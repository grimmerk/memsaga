---
name: memvc-history
description: Look up the version history of Claude Code's own memory and config — global CLAUDE.md, per-project auto-memory files (MEMORY.md and topic files), and user skills — which memvc mirrors into a git repo (`memvc config` shows the path; default ~/git/ccmem) on every turn. Use when the user asks what a memory or CLAUDE.md said last week, when a memory was added/changed/deleted and by which session, wants to diff two versions, wants to recover a deleted or overwritten memory file, or mentions memvc, ccmem, or "memory 歷史/版本/還原".
---

# memvc history

Live files stay where Claude Code keeps them (`~/.claude/...`, `~/.claude-work/...`). memvc
copies an allowlist into `~/git/ccmem` and commits once per turn; the repo is a one-way
mirror plus history back to 2026-07 (replayed from transcripts). Never edit the mirror
expecting it to flow back: restore is an explicit, user-approved copy.

## Layout of ~/git/ccmem

```
claude/CLAUDE.md                                  global CLAUDE.md (account main)
claude/skills/<name>/SKILL.md                     user skills
claude/projects/<slug>/memory/{MEMORY.md,*.md}    auto-memory per project (main account)
claude-work/...                                   same for the second account (work); CLAUDE.md and skills are symlinks to claude/
paths.json                                        slug -> real cwd, e.g. "-Users-me-git-webapp": "/Users/me/git/webapp"
index.md                                          browsable table of every project
```

Slug = the cwd with every non-alphanumeric character replaced by `-`. The memory for
`~/git/webapp` is `claude/projects/-Users-me-git-webapp/memory/`; `~/git` itself is
`claude/projects/-Users-me-git/memory/`.

Commit messages carry trailers `Event:` (PostToolUse / Stop / SessionEnd / backfill / import),
`Account:`, `Session:` (the Claude session id), `Cwd:`, `Agent:`.

## Recipes (all read-only)

```bash
R=~/git/ccmem
memvc status                                            # last sweep, pending dirty marker, recent events
git -C $R log --since='7 days ago' --date=short --format='%h %ad %s' -- claude/CLAUDE.md
git -C $R log -p --follow -- claude/projects/-Users-me-git-webapp/memory/MEMORY.md
git -C $R log --diff-filter=D --since='30 days ago' --name-only --format='%h %ad' -- '*/memory/*'   # deleted memories
git -C $R show <sha>^:claude/projects/<slug>/memory/<file>.md        # content before a commit
git -C $R diff <sha1> <sha2> -- claude/CLAUDE.md
git -C $R log --format='%h %ad %s%n  %(trailers:key=Session,valueonly)' --date=short -n 20   # which session changed what
```

Backfilled history has gaps: an edit made through Bash is invisible to the replay, so a
file may jump versions between two `backfill:` commits. The `import:` commit of
2026-09-18 is the first exact snapshot; everything after it is exact.

## Restoring

Restoring writes into a live config directory, so: show the user the diff first, get an
explicit yes, then copy the blob out of git into place, e.g.
`git -C $R show <sha>:claude/projects/<slug>/memory/<file>.md > ~/.claude/projects/<slug>/memory/<file>.md`.
Never `git checkout` inside `~/.claude`; it is not a git repo.
