#!/bin/bash
# Test harness for config-vc.sh using fake config roots in the scratchpad (never touches ~/.claude).
set -u
P="$(cd "$(dirname "$0")" && pwd)"
T="$P/t"; rm -rf "$T"; mkdir -p "$T"
export CCVC_REPO="$T/repo" CCVC_ROOTS="$T/.claude:$T/.claude-work" CCVC_LOG="$T/vc.log" CCVC_DIRS="skills commands hooks"
SCRIPT="$P/../hook/config-vc.sh"; chmod +x "$SCRIPT"

# fake roots: allowlisted content + noise that must NOT be mirrored
mkdir -p "$T/.claude/skills/foo" "$T/.claude/hooks" "$T/.claude/projects/-Users-x-git-a/memory" "$T/.claude/projects/-Users-x-git-a/sess" "$T/.claude/projects/-Users-x-git-b/memory" "$T/.claude/file-history/s1" "$T/.claude/plugins"
echo "# global" > "$T/.claude/CLAUDE.md"; echo '{"a":1}' > "$T/.claude/settings.json"; echo "skill" > "$T/.claude/skills/foo/SKILL.md"; echo "hook" > "$T/.claude/hooks/x.sh"
echo "- idx" > "$T/.claude/projects/-Users-x-git-a/memory/MEMORY.md"; echo "fact" > "$T/.claude/projects/-Users-x-git-a/memory/one.md"
echo '{"transcript":true}' > "$T/.claude/projects/-Users-x-git-a/sess.jsonl"; echo "big" > "$T/.claude/file-history/s1/h@v1"; echo "plugin" > "$T/.claude/plugins/p"; echo '{"oauth":"SECRET"}' > "$T/.claude/.claude.json"
mkdir -p "$T/.claude-work/projects/-Users-x-git-a/memory"; ln -s "$T/.claude/CLAUDE.md" "$T/.claude-work/CLAUDE.md"; ln -s "$T/.claude/skills" "$T/.claude-work/skills"; echo '{"w":1}' > "$T/.claude-work/settings.json"; echo "work fact" > "$T/.claude-work/projects/-Users-x-git-a/memory/w.md"

git init -q "$T/repo"; git -C "$T/repo" config commit.gpgsign false

echo "== 1. initial sweep (manual)"; time "$SCRIPT" manual </dev/null
git -C "$T/repo" log --oneline | head; echo "-- mirrored tree:"; (cd "$T/repo" && find . -path ./.git -prune -o -type f -print -o -type l -print | sort)
echo "-- noise present? (expect none):"; (cd "$T/repo" && find . -name '*.jsonl' -o -name '.claude.json' -o -path '*file-history*' -o -path '*plugins*' | grep -v '^./.git' )

echo; echo "== 2. no-op sweep (expect no new commit)"; n0=$(git -C "$T/repo" rev-list --count HEAD); "$SCRIPT" manual </dev/null; n1=$(git -C "$T/repo" rev-list --count HEAD); echo "commits before=$n0 after=$n1"

echo; echo "== 3. PostToolUse fast path: out-of-scope path must exit without touching git"
echo "edit" >> "$T/.claude/CLAUDE.md"   # dirty, but the hook is told about an unrelated file
time (printf '{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/Users/x/git/a/src/foo.ts"},"session_id":"s1","cwd":"/Users/x/git/a"}' | "$SCRIPT")
n2=$(git -C "$T/repo" rev-list --count HEAD); echo "commits after out-of-scope PostToolUse: $n2 (expect $n1)"

echo; echo "== 4. PostToolUse in-scope path (memory write) -> commit with attribution"
echo "new memory" > "$T/.claude/projects/-Users-x-git-b/memory/b.md"
printf '{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"%s"},"session_id":"1e82bad5-aaaa","cwd":"/Users/x/git/b","agent_type":"Explore"}' "$T/.claude/projects/-Users-x-git-b/memory/b.md" | "$SCRIPT"
git -C "$T/repo" log -1 --format='%B' | sed 's/^/    /'

echo; echo "== 5. deletion propagates (memory file removed by Claude via Bash, caught by Stop sweep)"
rm "$T/.claude/projects/-Users-x-git-a/memory/one.md"
printf '{"hook_event_name":"Stop","session_id":"s3","cwd":"/Users/x/git/a"}' | "$SCRIPT"
git -C "$T/repo" log -1 --stat --format='%s' | sed 's/^/    /'

echo; echo "== 6. concurrency: 8 parallel Stop hooks from different sessions, each after its own edit"
n3=$(git -C "$T/repo" rev-list --count HEAD)
for i in 1 2 3 4 5 6 7 8; do
  ( echo "fact $i" > "$T/.claude/projects/-Users-x-git-a/memory/c$i.md"; printf '{"hook_event_name":"Stop","session_id":"par-%s","cwd":"/Users/x/git/a"}' "$i" | "$SCRIPT" ) &
done; wait
n4=$(git -C "$T/repo" rev-list --count HEAD)
echo "commits added by 8 parallel hooks: $((n4-n3)) (1..8 all fine; 0 = lost update)"
echo "-- all 8 files in HEAD? $(git -C "$T/repo" ls-tree -r --name-only HEAD | grep -c 'memory/c[0-9].md') (expect 8)"
echo "-- index.lock errors in log:"; grep -c 'commit failed\|lock timeout' "$T/vc.log"
echo "-- stale lock left behind? $([ -d "$T/repo/.git/config-vc.lock" ] && echo YES || echo no)"

echo; echo "== 7. symlinks in claude-work mirrored as symlinks"; ls -l "$T/repo/claude-work/" | grep -- '->' | sed 's/^/    /'
echo; echo "== log:"; cat "$T/vc.log"
