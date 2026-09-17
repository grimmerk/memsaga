#!/bin/bash
# config-vc.sh — mirror allowlisted Claude Code config + memory files into a git repo and commit.
#
# Runs as a Claude Code hook (PostToolUse / Stop / SessionEnd / FileChanged) reading the hook
# JSON on stdin, or manually:  config-vc.sh manual
#
# Config file (optional, sourced): ~/.config/claude-config-vc/config.sh — may set:
#   CCVC_REPO        mirror repo path                      (default ~/git/claude-config)
#   CCVC_ROOTS       colon-separated config dirs           (default: codev accounts.json dirs, else ~/.claude)
#   CCVC_FILES       top-level files to mirror             (default "CLAUDE.md")
#   CCVC_DIRS        top-level dirs to mirror, opt-in      (default ""; e.g. "skills commands hooks")
#   CCVC_MEMORY      1 = mirror projects/*/memory          (default 1)
set -u
[ -f "$HOME/.config/claude-config-vc/config.sh" ] && . "$HOME/.config/claude-config-vc/config.sh"
REPO="${CCVC_REPO:-$HOME/git/claude-config}"
FILES="${CCVC_FILES:-CLAUDE.md}"
DIRS="${CCVC_DIRS:-}"
MEMORY="${CCVC_MEMORY:-1}"
if [ -z "${CCVC_ROOTS:-}" ]; then
  ROOTS="$(jq -r '.accounts[].dir' "$HOME/.config/codev/accounts.json" 2>/dev/null | paste -sd: -)"
  ROOTS="${ROOTS:-$HOME/.claude}"
else ROOTS="$CCVC_ROOTS"; fi
LOG="${CCVC_LOG:-$REPO/.git/config-vc.log}"
LOCK="$REPO/.git/config-vc.lock"
MARK="$REPO/.git/config-vc.last-sweep"

log() { printf '%s %s\n' "$(date '+%F %T')" "$*" >> "$LOG" 2>/dev/null; }
dbg() { [ -n "${CCVC_DEBUG:-}" ] && log "dbg[$$] $*"; return 0; }
# No `set -e`/ERR trap on purpose (bash 3.2 fires ERR inside ||/&& lists); every step tolerates failure.

INPUT="$(cat 2>/dev/null || true)"
EVENT="$(printf '%s' "$INPUT" | jq -r '.hook_event_name // empty' 2>/dev/null)"
[ -z "$EVENT" ] && EVENT="${1:-manual}"

# ---- in-scope test for a path
in_scope() {
  local p="$1" r f d
  for r in ${ROOTS//:/ }; do
    for f in $FILES; do [ "$p" = "$r/$f" ] && return 0; done
    for d in $DIRS;  do case "$p" in "$r/$d"/*) return 0;; esac; done
    [ "$MEMORY" = 1 ] && case "$p" in "$r"/projects/*/memory/*) return 0;; esac
  done
  return 1
}
# PostToolUse / FileChanged: exit fast unless the touched path is ours
case "$EVENT" in
  PostToolUse|PostToolBatch|FileChanged)
    hit=0
    while IFS= read -r p; do [ -n "$p" ] && in_scope "$p" && { hit=1; break; }; done \
      < <(printf '%s' "$INPUT" | jq -r '[.file_path?, .tool_input.file_path?, (.tool_calls[]?.tool_input.file_path?)] | .[] | select(. != null)' 2>/dev/null)
    [ "$hit" = 1 ] || exit 0 ;;
esac
[ -d "$REPO/.git" ] || { log "no repo at $REPO"; exit 0; }

# ---- Stop/SessionEnd sweep: exit fast when nothing under our paths is newer than the last sweep
if [ -f "$MARK" ] && [ "$EVENT" != manual ]; then
  newer=""
  for r in ${ROOTS//:/ }; do
    for f in $FILES; do [ -e "$r/$f" ] && [ "$r/$f" -nt "$MARK" ] && newer=1; done
    for d in $DIRS;  do [ -d "$r/$d" ] && [ -n "$(find "$r/$d/" -newer "$MARK" -print -quit 2>/dev/null)" ] && newer=1; done   # trailing slash: follow a symlinked dir (2nd account links skills/)
    [ "$MEMORY" = 1 ] && [ -n "$(find "$r"/projects/*/memory -newer "$MARK" -print -quit 2>/dev/null)" ] && newer=1
    [ -n "$newer" ] && break
  done
  [ -n "$newer" ] || { dbg "nothing newer ($EVENT)"; exit 0; }
fi

# ---- lock (macOS has no flock; mkdir is atomic). Stale after 120 s.
i=0
while ! mkdir "$LOCK" 2>/dev/null; do
  # stale only if the lock dir is really there AND old; a vanished lock (stat fails) just means retry.
  # (An earlier version treated stat failure as age 0 → "stale" and removed a lock another run had just taken.)
  lm="$(stat -f %m "$LOCK" 2>/dev/null)"
  if [ -n "$lm" ] && [ "$(( $(date +%s) - lm ))" -gt 120 ]; then rmdir "$LOCK" 2>/dev/null; continue; fi
  i=$((i+1)); [ "$i" -gt 100 ] && { log "lock timeout ($EVENT)"; exit 0; }
  sleep 0.1
done
trap 'dbg "leave"; rmdir "$LOCK" 2>/dev/null; exit 0' EXIT   # log before release so the log order is the lock order
dbg "enter ($EVENT)"
touch "$MARK.tmp"   # sweep start time; promoted to $MARK on success so edits during the sweep are caught next time

# ---- mirror with rsync (preserves mtimes → git's stat cache stays warm; --delete propagates removals)
RS="rsync -a --delete --exclude=*.codev-bak* --exclude=*.codev-tmp-* --exclude=.DS_Store"   # codev collision backups never enter history
for r in ${ROOTS//:/ }; do
  [ -d "$r" ] || continue
  name="$(basename "$r")"; name="${name#.}"          # .claude → claude, .claude-work → claude-work
  dst="$REPO/$name"; mkdir -p "$dst"
  for f in $FILES; do
    if [ -e "$r/$f" ] || [ -L "$r/$f" ]; then $RS "$r/$f" "$dst/$f" 2>>"$LOG"; else rm -f "$dst/$f"; fi
  done
  for d in $DIRS; do
    if [ -L "$r/$d" ]; then rm -rf "$dst/$d"; cp -R "$r/$d" "$dst/$d" 2>>"$LOG"      # keep symlink as symlink
    elif [ -d "$r/$d" ]; then $RS "$r/$d/" "$dst/$d/" 2>>"$LOG"
    else rm -rf "$dst/$d"; fi
  done
  if [ "$MEMORY" = 1 ]; then
    mkdir -p "$dst/projects"
    for m in "$r"/projects/*/memory; do
      [ -d "$m" ] && [ -n "$(ls -A "$m" 2>/dev/null)" ] || continue
      slug="$(basename "$(dirname "$m")")"
      mkdir -p "$dst/projects/$slug" && $RS "$m/" "$dst/projects/$slug/memory/" 2>>"$LOG"
    done
    for md in "$dst"/projects/*/memory; do                    # prune slugs whose memory vanished or emptied
      [ -d "$md" ] || continue
      slug="$(basename "$(dirname "$md")")"
      [ -n "$(ls -A "$r/projects/$slug/memory" 2>/dev/null)" ] || rm -rf "$dst/projects/$slug"
    done
  fi
done

# ---- commit if anything changed
cd "$REPO" || exit 0
git add -A -- . >/dev/null 2>&1
if git diff --cached --quiet; then mv -f "$MARK.tmp" "$MARK"; exit 0; fi

SID="$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)"
CWD="$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)"
AGENT="$(printf '%s' "$INPUT" | jq -r '.agent_type // empty' 2>/dev/null)"
# account label: codev launches non-anchor accounts with CLAUDE_CONFIG_DIR set; hooks inherit that env
ACCT="$(jq -r --arg d "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" '.accounts[] | select(.dir==$d) | .label' "$HOME/.config/codev/accounts.json" 2>/dev/null | head -1)"
[ -z "$ACCT" ] && { ACCT="$(basename "${CLAUDE_CONFIG_DIR:-$HOME/.claude}")"; ACCT="${ACCT#.}"; }
STAT="$(git diff --cached --shortstat | sed 's/^ //')"
FILELIST="$(git diff --cached --name-status | head -30)"
# subject: "<event>: <account>:<area>[, ...]" e.g. "Stop: claude:CLAUDE.md, claude:memory/-Users-me-git-webapp"
SUBJ="$(git diff --cached --name-only \
  | sed -E 's#^([^/]+)/projects/([^/]+)/memory/.*#\1:memory/\2#; s#^([^/]+)/(skills|commands|hooks|agents|rules)/.*#\1:\2#; s#^([^/]+)/#\1:#' \
  | sort -u | paste -sd, - | sed 's/,/, /g' | cut -c1-68)"
MSG="$EVENT: $SUBJ

$STAT
$FILELIST

Event: $EVENT
Account: $ACCT
Session: ${SID:-none}
Cwd: ${CWD:-none}
Agent: ${AGENT:-main}"
if git -c commit.gpgsign=false commit -q -m "$MSG" >/dev/null 2>&1; then
  mv -f "$MARK.tmp" "$MARK"; log "committed ($EVENT) $STAT"
else
  rm -f "$MARK.tmp"; log "commit failed ($EVENT)"
fi
exit 0
