#!/bin/bash
# Build a self-contained demo of memsaga: a fake home directory with a Claude
# Code config directory, two projects' memory, a note outside the config
# directory, and a data repository holding a few days of history (a bug fix, a
# memory tidy-up, a file added by hand, a restore). Everything is example data;
# nothing outside the demo directory is read or written, so screenshots and
# recordings made from it show no real memory.
#
# Usage: demo/setup.sh [demo-dir]     (default /tmp/memsaga-demo; replaced if it exists)
# Then:  source <demo-dir>/env.sh     (HOME, MEMSAGA_CONFIG, ... point into the demo)
set -euo pipefail

DEMO=${1:-/tmp/memsaga-demo}
case "$DEMO" in /tmp/*|/private/tmp/*|"$TMPDIR"*) ;; *) echo "demo dir must be under /tmp or \$TMPDIR" >&2; exit 2;; esac
rm -rf "${DEMO:?}"
mkdir -p "$DEMO/home"
DEMO=$(cd "$DEMO" && pwd -P)

export HOME="$DEMO/home"
export CLAUDE_CONFIG_DIR="$HOME/.claude"
export MEMSAGA_CONFIG="$HOME/.config/memsaga/config.toml"
export MEMSAGA_NO_GITHUB_API=1
export GIT_AUTHOR_NAME="Ada Example" GIT_AUTHOR_EMAIL="ada@example.com"
export GIT_COMMITTER_NAME="Ada Example" GIT_COMMITTER_EMAIL="ada@example.com"
export GIT_CONFIG_GLOBAL="$DEMO/gitconfig"
printf '[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n' > "$GIT_CONFIG_GLOBAL"

C="$HOME/.claude"
slug() { printf '%s' "$1" | sed 's/[^A-Za-z0-9]/-/g'; }
WEBAPP="$HOME/git/webapp"
BLOG="$HOME/git/blog"
MEM_WEBAPP="$C/projects/$(slug "$WEBAPP")/memory"
MEM_BLOG="$C/projects/$(slug "$BLOG")/memory"
mkdir -p "$WEBAPP" "$BLOG" "$MEM_WEBAPP" "$MEM_BLOG" "$C/skills/release-notes" "$C/agents" "$HOME/notes" \
  "$(dirname "$MEMSAGA_CONFIG")"

cat > "$MEMSAGA_CONFIG" <<EOF
repo = "~/git/memories"
extra_files = ["~/notes/plan.md"]
push_min_interval_secs = 0

[[roots]]
path = "~/.claude"
label = "claude"
EOF

# --- Monday: the state memsaga starts from ---------------------------------
cat > "$C/CLAUDE.md" <<'EOF'
# Global instructions

- Run the tests before saying a change works.
- Prefer small pull requests; one topic each.
- Ask before deleting files outside the repository.
EOF
cat > "$C/skills/release-notes/SKILL.md" <<'EOF'
---
name: release-notes
description: Draft release notes from the merged pull requests since the last tag.
---
List the merged pull requests since the last tag, group them by area, and write
one plain sentence per change.
EOF
cat > "$C/agents/reviewer.md" <<'EOF'
---
name: reviewer
description: Reviews a diff for correctness bugs before a pull request is opened.
---
Read the whole diff, then the callers of every changed function.
EOF
cat > "$MEM_WEBAPP/MEMORY.md" <<'EOF'
- [Testing](testing.md) — run `npm test -- --runInBand`; the e2e suite needs `npm run db:up`
- [Deploys](deploys.md) — staging deploys on merge to main
EOF
cat > "$MEM_WEBAPP/testing.md" <<'EOF'
---
name: testing
description: How to run webapp's tests
---
Run `npm test -- --runInBand`; parallel runs share one database and flake.
The e2e suite needs `npm run db:up` first.
EOF
cat > "$MEM_WEBAPP/deploys.md" <<'EOF'
---
name: deploys
description: How webapp ships
---
Staging deploys on merge to main. Production deploys from a tag.
Run the tests in parallel to save time.
EOF
cat > "$MEM_BLOG/MEMORY.md" <<'EOF'
- [Style](style.md) — short paragraphs, British spelling
EOF
cat > "$MEM_BLOG/style.md" <<'EOF'
---
name: style
description: House style for the blog
---
Short paragraphs. British spelling. One idea per post.
EOF
cat > "$HOME/notes/plan.md" <<'EOF'
# Q4 plan

- Ship OAuth login
EOF

at() { export GIT_AUTHOR_DATE="$1" GIT_COMMITTER_DATE="$1"; }
at "2026-10-05T09:12:00+01:00"
memsaga init --no-backfill >/dev/null

# One Claude Code turn: a transcript that carries the session's title, then
# the Stop hook, exactly as Claude Code would call it.
turn() { # session-id title cwd date
  local t="$C/projects/$(slug "$3")/$1.jsonl"
  mkdir -p "$(dirname "$t")"
  printf '{"type":"custom-title","customTitle":"%s","sessionId":"%s"}\n' "$2" "$1" > "$t"
  at "$4"
  printf '{"hook_event_name":"Stop","session_id":"%s","transcript_path":"%s","cwd":"%s"}' "$1" "$t" "$3" |
    memsaga hook >/dev/null
  sleep 1 # separate mtimes from the next turn's last-sweep marker
}

# --- Tuesday: fixing a bug teaches Claude something --------------------------
cat > "$MEM_WEBAPP/auth-flow.md" <<'EOF'
---
name: auth-flow
description: Why the login redirect broke after OAuth
---
The OAuth callback must return to the page the user started from, not to `/`.
The original path is kept in the `state` parameter; never trust a `next` query.
EOF
cat >> "$MEM_WEBAPP/MEMORY.md" <<'EOF'
- [Auth flow](auth-flow.md) — OAuth callback returns to the starting page via `state`
EOF
printf -- '- Fix the login redirect (done)\n' >> "$HOME/notes/plan.md"
turn 08ad5ab3-28b7-4d51-bfeb-4cd32a95b22c "fix login redirect after OAuth" "$WEBAPP" "2026-10-06T14:02:00+01:00"

# --- Wednesday morning: a tidy-up pass over memory ---------------------------
cat > "$MEM_WEBAPP/deploys.md" <<'EOF'
---
name: deploys
description: How webapp ships
---
Staging deploys on merge to main. Production deploys from a tag.
EOF
cat > "$MEM_WEBAPP/MEMORY.md" <<'EOF'
- [Testing](testing.md) — run `npm test -- --runInBand`; the e2e suite needs `npm run db:up`
- [Deploys](deploys.md) — staging on merge to main, production from a tag
- [Auth flow](auth-flow.md) — OAuth callback returns to the starting page via `state`
EOF
turn 5f1c9e20-7a4d-4b8e-9c31-2d6e8f0a4b17 "weekly memory tidy-up" "$HOME/git" "2026-10-07T09:40:00+01:00"

# --- Wednesday evening: a file added by hand, straight into the repository ---
mkdir -p "$HOME/git/memories/manual"
cat > "$HOME/git/memories/manual/decisions.md" <<'EOF'
# Decisions

- 2026-10-07: OAuth state carries the return path; no `next` parameter.
EOF
at "2026-10-07T18:15:00+01:00"
memsaga sweep --event manual >/dev/null

# --- Thursday: a bad edit to CLAUDE.md, then a restore -----------------------
before=$(git -C "$HOME/git/memories" rev-parse HEAD)
cat > "$C/CLAUDE.md" <<'EOF'
# Global instructions

- Prefer small pull requests; one topic each.
EOF
turn 9c2e4d51-3b6a-4f0e-8d27-1a5c7e9f3b62 "shorten global instructions" "$HOME/git" "2026-10-08T09:30:00+01:00"
at "2026-10-08T10:03:00+01:00"
memsaga restore claude/CLAUDE.md --at "$before" --yes >/dev/null
memsaga sweep --event restore >/dev/null
unset GIT_AUTHOR_DATE GIT_COMMITTER_DATE

cat > "$DEMO/env.sh" <<EOF
# Source this to work inside the demo (example data only).
export HOME="$HOME"
export CLAUDE_CONFIG_DIR="$C"
export MEMSAGA_CONFIG="$MEMSAGA_CONFIG"
export MEMSAGA_NO_GITHUB_API=1
export GIT_CONFIG_GLOBAL="$GIT_CONFIG_GLOBAL"
export GIT_AUTHOR_NAME="Ada Example" GIT_AUTHOR_EMAIL="ada@example.com"
export GIT_COMMITTER_NAME="Ada Example" GIT_COMMITTER_EMAIL="ada@example.com"
cd "$HOME"
EOF
echo "demo ready: $DEMO (source $DEMO/env.sh)"
git -C "$HOME/git/memories" log --format='%h %ad %s' --date=format:'%a %H:%M'
