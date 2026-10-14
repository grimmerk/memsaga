#!/bin/bash
# The terminal scene recorded by demo/record.sh: browse a week of memsaga
# history in the demo data repository, find the session behind a change, find
# when a rule vanished from CLAUDE.md, and see it restored. Runs inside the demo
# (source <demo-dir>/env.sh first); every command is typed out, then run.
set -u
cd "$HOME/git/memories"
export GIT_PAGER=cat LESS=FRX
PS="\033[1;38;5;178mmemories\033[0m \033[38;5;244m\$\033[0m "

type_run() { # command [pause-after]
  printf "$PS"
  local cmd="$1" i
  for ((i = 0; i < ${#cmd}; i++)); do printf '%s' "${cmd:i:1}"; sleep 0.035; done
  sleep 0.4; printf '\n'
  eval "$cmd"
  sleep "${2:-2.2}"
}
say() { printf '\033[38;5;37m# %s\033[0m\n' "$1"; sleep 1.1; }

clear
say "A week of Claude Code memory, one commit per turn, named after its session"
type_run "git log --format='%C(178)%h%Creset %C(244)%ad%Creset %s' --date=format:'%a %H:%M'" 3
say "Which conversation taught it the OAuth rule?"
type_run "git log -1 --format='%(trailers:key=Session-Title,key=Session)' -- claude/memory/HOME-git-webapp/auth-flow.md" 2.8
say "When did 'Run the tests' disappear from CLAUDE.md, and in which session?"
type_run "git log -S 'Run the tests' --format='%C(178)%h%Creset %s' -- claude/CLAUDE.md" 2.8
say "It was restored: the live file has the rule again"
type_run "head -3 ~/.claude/CLAUDE.md" 3
