#!/bin/bash
# Record a real Claude Code session for use case 10 (edit from anywhere):
# a memory file is edited on GitHub, then a session in the demo project asks
# about it; memsaga's hooks pull the edit, write it to the live file and tell
# the model, which answers from the new text.
#
# Needs, once:
#   - a logged-in Claude Code account whose config directory is ~/.claude-demo
#     with no sharing (CodeV: Add account "demo"; `claude demo`, log in, /exit);
#   - a GitHub repository for the demo data (default grimmerk/memsaga-demo-live).
# Builds the demo in that config directory (its login is kept), pushes the
# demo history to the repository, registers memsaga's hooks in ~/.claude-demo
# only, edits the file on GitHub, records with asciinema driving tmux, and
# renders docs/assets/sync-demo.gif and sync-demo.mp4. Uses Claude Code (and so
# the account's usage) for one short session.
# Usage: demo/record-claude.sh [owner/repo]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
assets="$here/../docs/assets"
REPO=${1:-grimmerk/memsaga-demo-live}
DEMO=/tmp/memsaga-demo
CLAUDE_BIN=$(zsh -c 'whence -p claude')
REAL_HOME=$HOME
export GH_TOKEN=${GH_TOKEN:-$(gh auth token)}
export GIT_TERMINAL_PROMPT=0

DEMO_CLAUDE_DIR="$REAL_HOME/.claude-demo" "$here/setup.sh" "$DEMO" >/dev/null
DEMO=$(cd "$DEMO" && pwd -P)   # /tmp is a symlink on macOS; Claude Code uses the real path
FAKE_HOME="$DEMO/home"
data="$FAKE_HOME/git/memories"
gitenv() { env HOME="$FAKE_HOME" GIT_CONFIG_GLOBAL="$DEMO/gitconfig" "$@"; }
gitenv git -C "$data" remote add origin "https://github.com/$REPO.git"
gitenv git -C "$data" push -q --force -u origin main

# memsaga runs under the demo home (names, data repository, git config);
# Claude Code runs under the real home so it finds its login.
(source "$DEMO/env.sh"
 memsaga install --root "$CLAUDE_CONFIG_DIR" --bin "env HOME=$FAKE_HOME $REAL_HOME/.cargo/bin/memsaga" >/dev/null)

# The edit on GitHub, as the web editor would make it.
path=claude/memory/HOME-git-webapp/deploys.md
sha=$(gh api "repos/$REPO/contents/$path" --jq .sha)
new=$'---\nname: deploys\ndescription: How webapp ships\n---\nStaging deploys on merge to main. Production deploys every Friday from a tag.\n'
gh api -X PUT "repos/$REPO/contents/$path" -f message="Production deploys on Fridays" \
  -f content="$(printf '%s' "$new" | base64)" -f sha="$sha" >/dev/null

session=memsaga-demo
tmux kill-session -t "$session" 2>/dev/null || true
tmux new-session -d -s "$session" -x 110 -y 32 -c "$FAKE_HOME/git/webapp" \
  env CLAUDE_CONFIG_DIR="$REAL_HOME/.claude-demo" MEMSAGA_CONFIG="$FAKE_HOME/.config/memsaga/config.toml" \
  MEMSAGA_NO_GITHUB_API=1 GIT_CONFIG_GLOBAL="$DEMO/gitconfig" GH_TOKEN="$GH_TOKEN" \
  GIT_AUTHOR_NAME="Ada Example" GIT_AUTHOR_EMAIL=ada@example.com \
  GIT_COMMITTER_NAME="Ada Example" GIT_COMMITTER_EMAIL=ada@example.com \
  "$CLAUDE_BIN"
tmux set -t "$session" status off
# First run in this folder: the trust prompt defaults to "No, exit"; accept it
# before the recording starts so it stays out of the picture.
sleep 6
if tmux capture-pane -t "$session" -p | grep -q 'trust this folder'; then
  tmux send-keys -t "$session" Down; sleep 0.3; tmux send-keys -t "$session" Enter; sleep 8
fi
cast="$DEMO/claude.cast"
asciinema rec --overwrite --quiet --cols 110 --rows 32 -c "tmux attach -t $session" "$cast" &
rec=$!
sleep 8
prompt="How does webapp deploy to production? Check its memory."
for ((i = 0; i < ${#prompt}; i++)); do tmux send-keys -t "$session" -l "${prompt:i:1}"; sleep 0.04; done
sleep 0.6
tmux send-keys -t "$session" Enter
sleep 45
tmux send-keys -t "$session" -l "/exit"; sleep 0.5; tmux send-keys -t "$session" Enter
sleep 3
tmux kill-session -t "$session" 2>/dev/null || true
wait "$rec" || true

agg --font-size 18 --theme asciinema --idle-time-limit 2 --last-frame-duration 4 "$cast" "$assets/sync-demo.gif"
ffmpeg -loglevel error -y -i "$assets/sync-demo.gif" -movflags faststart -pix_fmt yuv420p \
  -vf "fps=25,scale=trunc(iw/2)*2:trunc(ih/2)*2" "$assets/sync-demo.mp4"
ls -l "$assets/sync-demo.gif" "$assets/sync-demo.mp4"
