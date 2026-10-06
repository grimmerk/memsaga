---
name: memsaga-history
description: Look up the version history of Claude Code's own memory and config — global CLAUDE.md, per-project auto-memory files (MEMORY.md and topic files), and user skills — which memsaga mirrors into a git repo (`memsaga config` shows the path; default ~/git/memories) on every turn. Use when the user asks what a memory or CLAUDE.md said last week, when a memory was added/changed/deleted and by which session, wants to diff two versions, wants to recover a deleted or overwritten memory file, or mentions memsaga, memories, the old names memvc / ccmem, or "memory 歷史/版本/還原".
---

# memsaga history

Live files stay where Claude Code keeps them (`~/.claude/...`, `~/.claude-work/...`). memsaga
copies an allowlist into `~/git/memories` and commits once per turn; the repo is a mirror
plus history back to 2026-07 (replayed from transcripts). An edit made in the mirror (or on
GitHub) is kept and waits until `memsaga sync` (or the session-start / prompt hooks) brings it
to the live file; `memsaga status` lists what waits. Restoring an older version is an explicit
`memsaga restore`.

## Layout of ~/git/memories

```
claude/CLAUDE.md                                  global CLAUDE.md (account main)
claude/skills/<name>/SKILL.md                     user skills
claude/memory/<slug>/{MEMORY.md,*.md}             auto-memory per project (main account); live path is projects/<slug>/memory/
claude-work/...                                   same for the second account (work); CLAUDE.md and skills are symlinks to claude/
paths.json                                        slug -> real cwd, e.g. "-Users-me-git-webapp": "/Users/me/git/webapp"
index.md                                          browsable table of every project
```

A project slug is a path with every non-alphanumeric character replaced by `-`. For
transcripts that path is the cwd; for auto-memory since Claude Code 2.1.275 it is the git
common dir, so every worktree and subdirectory of `~/git/webapp` shares
`claude/memory/-Users-me-git-webapp/`, and a non-repo directory such as
`~/git` keeps its own (`claude/memory/-Users-me-git/`). Older slugs such as
`-Users-me-git-webapp-<worktree>-packages-api` are per-cwd leftovers from
before that change; their files are still mirrored but no longer loaded.

A commit's subject is `[<session title>] <account>:<area>, …` (the title cut to 32
characters), or `<event>: …` when the session had no title. Commit messages carry trailers
`Event:` (PostToolUse / Stop / SessionEnd / backfill / import), `Account:`, `Session:` (the
Claude session id), `Cwd:`, `Agent:`, and `Session-Title:` (full title, since 2026-10-05).

## Recipes (all read-only)

```bash
R=~/git/memories
memsaga status                                            # last sweep, pending dirty marker, recent events
git -C $R log --since='7 days ago' --date=short --format='%h %ad %s' -- claude/CLAUDE.md
git -C $R log -p --follow -- claude/memory/-Users-me-git-webapp/MEMORY.md
git -C $R log --diff-filter=D --since='30 days ago' --name-only --format='%h %ad' -- '*/memory/*'   # deleted memories
git -C $R show <sha>^:claude/memory/<slug>/<file>.md        # content before a commit
git -C $R diff <sha1> <sha2> -- claude/CLAUDE.md
git -C $R log --format='%h %ad %s%n  %(trailers:key=Session,valueonly)' --date=short -n 20   # which session changed what
git -C $R log --grep='^Session-Title: <name>$' --format='%h %ad %s' --date=short   # every commit of one named session (a regex: escape . [ * in the name)
```

Before 2026-09-18 the repo stored memory at `claude/projects/<slug>/memory/`; that commit is
a pure rename, so use `--follow` (as above) to see history across it, and `git show
<old sha>:claude/projects/<slug>/memory/<file>.md` for content from before it. `memsaga restore
--at <old sha>` handles the old path by itself.

Backfilled history has gaps: an edit made through Bash is invisible to the replay, so a
file may jump versions between two `backfill:` commits. The `import:` commit of
2026-09-18 is the first exact snapshot; everything after it is exact.

## Restoring

Restoring writes into a live config directory, so: show the user the diff first, get an
explicit yes, then run `memsaga restore claude/memory/<slug>/<file>.md --at <sha>`,
which shows the diff, asks, writes the file atomically and records a `restore` event so the
next turn commits the restored state. Never `git checkout` inside `~/.claude`; it is not a git repo.
