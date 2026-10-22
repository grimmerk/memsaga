---
name: memsaga-history
description: Look up the version history of Claude Code's own memory and config — global CLAUDE.md, per-project auto-memory files (MEMORY.md and topic files), and user skills — which memsaga mirrors into a git repo (`memsaga config` shows the path, for example ~/git/memories) on every turn. Use when the user asks what a memory or CLAUDE.md said last week, when a memory was added/changed/deleted and by which session, wants to diff two versions, wants to recover a deleted or overwritten memory file, or mentions memsaga, memories, or the old names memvc / ccmem. The user may ask in any language (for example 記憶の履歴, memory 歷史/版本/還原; examples only).
---

# memsaga history

Live files stay where Claude Code keeps them (`~/.claude/...`, and one directory per further
account, such as `~/.claude-work/...`). memsaga copies an allowlist into its data repository
(`memsaga repo` prints its path; `~/git/memories` in the examples below) and commits once
per turn; the repo is a mirror plus whatever earlier history `memsaga init` could replay from
transcripts. An edit made in the mirror (or on
GitHub) is kept and waits until `memsaga sync` (or the session-start / prompt hooks) brings it
to the live file; `memsaga status` lists what waits. Restoring an older version is an explicit
`memsaga restore`.

## Layout of the data repository

```
claude/CLAUDE.md                                  global CLAUDE.md (the ~/.claude account)
claude/skills/<name>/SKILL.md                     user skills
claude/memory/<name>/{MEMORY.md,*.md}             auto-memory per project; live path is projects/<slug>/memory/
claude-work/...                                   a further account, named after its directory; files it shares through symlinks stay symlinks to claude/
paths.json                                        memory name -> real cwd, e.g. "HOME-git-webapp": "~/git/webapp"
index.md                                          browsable table of every project
```

A project slug (Claude Code's live directory name) is a path with every non-alphanumeric
character replaced by `-`. For transcripts that path is the cwd; for auto-memory since Claude
Code 2.1.275 it is the git common dir, so every worktree and subdirectory of `~/git/webapp`
shares one directory, and a non-repo directory such as `~/git` keeps its own. In the
repository a project under home is named without the home: `claude/memory/HOME-git-webapp/`
(live `-Users-me-git-webapp`), `claude/memory/HOME-git/` for `~/git`; a project outside home
keeps its slug, and so does every project on a machine where another directory encodes like
a path under home (`/Users/me-old` next to `/Users/me`), where the home cannot be told
apart in the slug. Repositories started before October 2026 used the slugs themselves
(`claude/memory/-Users-me-git-webapp/`); that is a rename, so use `--follow`. Older slugs such as
`-Users-me-git-webapp-<worktree>-packages-api` are per-cwd leftovers from
before that change; their files are still mirrored but no longer loaded.

A commit's subject is `[<session title>] <account>:<area>, …` (the title cut to 32
characters), or `<event>: …` when the session had no title. Commit messages carry trailers
`Event:` (PostToolUse / Stop / SessionEnd / backfill / import), `Account:`, `Session:` (the
Claude session id), `Cwd:`, `Agent:`, and `Session-Title:` (full title, since 2026-10-05).

## Recipes (all read-only)

`<repo>` below is the data repository: read it once with `memsaga repo`,
then write the path itself into each command (a shell variable does not survive from one tool
call to the next).

```bash
memsaga status                                            # last sweep, pending dirty marker, recent events
git -C <repo> log --since='7 days ago' --date=short --format='%h %ad %s' -- claude/CLAUDE.md
git -C <repo> log -p --follow -- claude/memory/HOME-git-webapp/MEMORY.md
git -C <repo> log --diff-filter=D --since='30 days ago' --name-only --format='%h %ad' -- '*/memory/*'   # deleted memories
git -C <repo> show <sha>^:claude/memory/<name>/<file>.md        # content before a commit (older commits: the slug as name)
git -C <repo> diff <sha1> <sha2> -- claude/CLAUDE.md
git -C <repo> log --format='%h %ad %s%n  %(trailers:key=Session,valueonly)' --date=short -n 20   # which session changed what
git -C <repo> log --grep='^Session-Title: <name>$' --format='%h %ad %s' --date=short   # every commit of one named session (a regex: escape . [ * in the name)
```

Repositories started before 2026-09-18 stored memory at `claude/projects/<slug>/memory/`; the
commit that moved it is a pure rename, so use `--follow` (as above) to see history across it, and `git show
<old sha>:claude/projects/<slug>/memory/<file>.md` for content from before it. `memsaga restore
--at <old sha>` handles the old path by itself.

Backfilled history has gaps: an edit made through Bash is invisible to the replay, so a
file may jump versions between two `backfill:` commits. The `import:` commit that
`memsaga init` made is the first exact snapshot; everything after it is exact.

## Restoring

Restoring writes into a live config directory, so: show the user the diff first, get an
explicit yes, then run `memsaga restore claude/memory/<name>/<file>.md --at <sha>`,
which shows the diff, asks, writes the file atomically and records a `restore` event so the
next turn commits the restored state. Never `git checkout` inside `~/.claude`; it is not a git repo.
