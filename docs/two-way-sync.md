# Two-way sync: bringing repository-side edits back to the live files

Design for grimmerk/memsaga-private#19, steps 1 to 3 in one pull request. Status: proposal,
waiting for review before implementation.

## Contents

- [TL;DR](#tldr)
- [Glossary](#glossary)
- [1. The problem](#1-the-problem)
- [2. The idea in one paragraph](#2-the-idea-in-one-paragraph)
- [3. Step 1a: push, and merge only when rejected](#3-step-1a-push-and-merge-only-when-rejected)
  - Diagram D1: the push path (ASCII)
- [4. Step 1b: the sweep stops overwriting repository-side edits](#4-step-1b-the-sweep-stops-overwriting-repository-side-edits)
- [5. The incoming list](#5-the-incoming-list)
- [6. Step 2: `memsaga pull`](#6-step-2-memsaga-pull)
  - Diagram D2: deciding one file (ASCII)
- [7. Step 3: read before use, through a mod](#7-step-3-read-before-use-through-a-mod)
- [8. Edge cases](#8-edge-cases)
- [9. Decisions for the reviewer](#9-decisions-for-the-reviewer)
- [10. Tests](#10-tests)

The two diagrams are independent: D1 shows how a commit from elsewhere enters
the local repository, D2 shows what happens afterwards to each file it changed.
The [incoming list](#5-the-incoming-list) is the hand-off between them.

## TL;DR

| Step | What it does | What it fixes |
|---|---|---|
| 1a | When `git push` is rejected because origin has commits we lack, fetch, merge, push again. Stop and report if the merge conflicts. | Today one edit on GitHub makes every later push fail. |
| 1b | Before mirroring, find repository files that were edited in the repository (merged in, or edited by hand in the clone). The sweep leaves them alone instead of copying the live file over them. | Today the next sweep silently undoes such an edit. |
| 2 | `memsaga pull`: write the files from 1a and 1b to their live locations when the live file did not change meanwhile; merge text when both changed; list real conflicts. | Today an edit made on GitHub never reaches `~/.claude`. |
| 3 | A small Claude Code mod runs the safe part of `memsaga pull` before Claude Code reads CLAUDE.md and memory at the start of a session and after `/compact`. | Without it, the user has to remember to run step 2. |

Steps 1a and 1b produce one list of files whose repository version is newer
than the live file (the *incoming list*). Steps 2 and 3 only read that list.

## Glossary

| Term | Meaning |
|---|---|
| Live file | The file Claude Code reads, under a root such as `~/.claude`. |
| Repository file | Its copy in the data repository, for example `claude/CLAUDE.md` in `~/git/memories`. |
| Mirror form | What the live file looks like in the repository. Equal to the live file, except `settings.json`, whose volatile keys (`settings_ignore_keys`) are removed. |
| Base | The version of a file that the live side and the repository side last agreed on. A three-way decision compares the live file and the repository file with the base. |
| Incoming list | `.git/memsaga/incoming.json`: each repository file that is newer than its live file, with its base. Defined in §5. |
| Sweep | memsaga's copy-and-commit pass at the end of each turn (the Stop hook). |
| Mod | A Claude Code plugin with TypeScript function hooks (Claude Code 2.1.287 and later). The context-steward mod is one. |
| `prompt.context` | A mod hook that runs when Claude Code builds the context of the first message, before it reads CLAUDE.md and the memory files, and again after `/compact`. |

## 1. The problem

memsaga copies in one direction only: live files to the repository. Three
kinds of edit happen on the other side:

1. **Edited on GitHub** in the web editor. Rare but real, even with one machine.
2. **Edited by hand in the local clone**, for example `~/git/memories/claude/CLAUDE.md`.
3. **Pushed from another machine.**

What happens today:

- Cases 1 and 3: the local branch is now behind `origin`. memsaga never pulls,
  so **every later push is rejected** (non-fast-forward) and commits pile up
  locally. The edit never reaches the live file.
- Case 2: the next sweep copies the live file over the edit. If the edit was
  committed, history still has it; if not, it is gone.

## 2. The idea in one paragraph

Every file the repository changed on its own gets an entry in the incoming
list together with its base, the version both sides last agreed on. Merged
commits supply the base as the pre-merge version; a hand edit supplies it as
the committed version. As long as a file is on the list, the sweep does not
overwrite it. `memsaga pull` (by hand, step 2, or from the mod, step 3) then
compares three versions: live, base and repository. Live still equal to base
means only the repository changed, so the repository version is written to the
live file. Both changed means a text merge is tried, and a conflict is
reported, never resolved by picking a side.

## 3. Step 1a: push, and merge only when rejected

The push stays as it is. Only when origin rejects it because origin has
commits we lack does memsaga integrate them. Checking first (with
`git ls-remote` before every push) would add a network round trip to every
push; letting the push fail costs that round trip only when origin really
moved. The read side (step 3) is where an up-front check belongs.

**Diagram D1: the push path**

```
  ① git push origin <branch>
        │
        ├── accepted ─────────────────────────────▶ done
        │
        └── rejected: non-fast-forward
              │
  ② take the sweep lock (the merge touches the working tree)
  ③ git fetch origin
  ④ git merge --no-edit origin/<branch>
        │
        ├── conflict only in generated files (index.md, paths.json)
        │     └── keep ours, regenerate after the merge, finish the merge
        │
        ├── any other conflict
        │     └── git merge --abort; record a push error naming the files;
        │         nothing is pushed; status and the reporter say so
        │
        └── merged
              │
  ⑤ add every live-mapped file the merge changed to the incoming list,
     base = its version in ORIG_HEAD (the pre-merge commit)
  ⑥ git push origin <branch>  (once; a second rejection is reported, not retried)
```

Notes:

- `MEMORY.md` already merges line by line (`**/MEMORY.md merge=union` in the
  data repository's `.gitattributes`), so two machines adding index lines do
  not conflict.
- "Live-mapped" means files under a root subtree (`claude/…`,
  `claude-work/…`), the mirrored config (`memsaga/config.toml`) and
  `extra_files` (`extra/…`). Files only the repository owns (`companion/`,
  `manual/`) need nothing more: they have no live side.
- The merge commit is made by memsaga with signing off, like every memsaga
  commit, so it never waits for a signing prompt.
- ORIG_HEAD is the right base because the Stop hook sweeps before it pushes:
  at that moment the local HEAD matches the live files.

## 4. Step 1b: the sweep stops overwriting repository-side edits

At the start of each sweep, before mirroring:

1. Run `git status --porcelain -z` limited to the live-mapped paths. A sweep
   ends with a clean working tree, so anything listed here changed after the
   last sweep: a hand edit (case 2), or a sweep that crashed half-way.
2. For each listed file, compare the working-tree file with the mirror form of
   the live file. Equal means a crashed sweep had already copied it: nothing
   to do. Different means a hand edit: add the file to the incoming list with
   base = `HEAD:<path>`.
3. Mirror as today, **except for files on the incoming list**. Those are not
   copied over and not deleted.
4. Commit. A hand edit is committed by this sweep like any other change, so it
   enters history at once.

`git status` is how the hash comparison is done: git compares mtime and size
first and reads the content only when they differ, so the common case costs
one process start (a few milliseconds) and reads no file.

## 5. The incoming list

`.git/memsaga/incoming.json`, written only while the sweep lock is held:

```json
{
  "claude/CLAUDE.md": { "base": "<blob sha>", "from": "merge 1a2b3c4" },
  "claude/memory/-Users-me-git/notes.md": { "base": null, "from": "hand edit" }
}
```

- `base` is the blob of the agreed version; `null` means the file did not
  exist then (it was added on the repository side).
- An entry leaves the list when step 2 or 3 settles it: written to the live
  file, merged, or found equal to the live file.
- If a file on the list changes again on the repository side, the entry keeps
  its original base. If it changes again on the live side, that is a both-sides
  change and step 2 treats it as one.
- `memsaga status` and `status --json` show the list (count and paths), and
  the Stop reporter prints one line while it is non-empty, so a pending pull
  is never silent.

## 6. Step 2: `memsaga pull`

```
memsaga pull [--yes] [--auto]
```

1. Take the sweep lock.
2. If origin is ahead: fetch and merge exactly as §3 steps ③ to ⑤ (no push).
3. For each entry of the incoming list, decide with D2.
4. Print what was written, merged, left as a conflict, or skipped. Without
   `--yes`, show the diffs first and ask once, like `restore --all`.

**Diagram D2: deciding one file.** L = mirror form of the live file,
B = base, R = repository file.

```
  L == R ? ── yes ──▶ already in step; drop the entry
     │ no
  L == B ? ── yes ──▶ only the repository changed: write R to the live file
     │ no
  both changed:
     git merge-file L B R ── clean ──▶ write the merged text to the live file
     │                                  (the next sweep commits it)
     └── overlapping edits ──▶ conflict: write nothing, keep the entry,
                               report the file and how to resolve it
```

How it writes:

- Through the same code as `restore`: atomic replace, executable bit from the
  repository, the live `settings_ignore_keys` put back into `settings.json`,
  the destination checked against the roots.
- `--auto` (used by the mod) applies only the `L == B` case and the
  `L == R` case. Text merges and deletions wait for a run by hand, so nothing
  surprising happens unattended.
- A conflict is resolved by the user editing the live file (or the repository
  file) and running `memsaga pull` again; once L equals R, the entry is
  dropped. A `--take live|repo <path>` option settles one file explicitly.

## 7. Step 3: read before use, through a mod

A new mod in this repository (`mod/`), installed like context-steward:
a folder under `~/.claude/mods/` whose path is added to
`env.CLAUDE_CODE_PLUGIN_DIRS` (colon-separated) in both accounts' settings.

| Hook | What it does | Cost |
|---|---|---|
| `$.clock.every` (5 minutes), started at `session.start` | `memsaga remote-check`: `git ls-remote origin refs/heads/<branch>` compared with the local tracking ref; writes a marker when origin moved. A "last check" marker keeps several sessions from checking at once. | One small network request per 5 minutes per machine; no download. |
| `prompt.context` | When the marker is set or the incoming list is non-empty: `memsaga pull --auto` (fetch, merge, write the safe files), then `next(e)`, so Claude Code reads the updated CLAUDE.md and memory. | Nothing in the common case (one file stat). A fetch of a few kilobytes when origin moved. |

What it does not do:

- It does not replace any command hook; the sweep and push stay where they are.
- It does not resolve conflicts or delete files (`--auto`).
- It does not guard writes. A `tool.call` guard that refuses to edit a memory
  file with a pending incoming version is possible, and is left for later.

To verify during implementation, not yet measured: that `prompt.context` runs
before the memory files are read in practice (the types say it precedes the
`claudeMd` block), and that the mod loads in `claude -p` and the desktop app.

This is also the first step of the status mod in grimmerk/memsaga-private#18: the status line
and toasts can be added to the same mod later.

## 8. Edge cases

- **Generated files.** `index.md` and `paths.json` are rebuilt by every sweep.
  In a merge conflict memsaga keeps its own version and regenerates them; they
  never go on the incoming list.
- **Symlinks in the repository** (a second account sharing a file with the
  first) are skipped by pull, as `restore --all` already does for external
  links.
- **Deletions.** A file deleted on the repository side has R missing. With
  L == B the live file could be deleted; see decision 1 in §9.
- **A root missing on this machine** (the second account not signed in): its
  entries stay on the list and are reported, as `restore --all` already does.
- **Machines with different home directory names.** Memory directories are
  named after the absolute project path (`-Users-alice-git`), so the same
  project has a different directory on each machine and nothing is shared
  between them. Two-way sync does not change that; mapping those names across
  machines is separate work.
- **Two sessions at once.** Every write to the working tree, the incoming list
  and the live files happens under the sweep lock; pushes keep their own push
  lock, which is taken after the sweep lock and never the other way round.

## 9. Decisions for the reviewer

1. **Deletions from the repository side.** Proposal: `memsaga pull` deletes
   the live file only when L == B and only after the confirmation (or with
   `--yes`); `--auto` never deletes. Alternative: never delete, only report.
2. **`--auto` scope.** Proposal: only files where the live side did not change
   (L == B). Alternative: also clean text merges.
3. **One mod for 19 and 18.** Proposal: create the memsaga mod now with only
   the two hooks above; the status line of #18 joins it later.
4. **Version string.** Proposal: `memsaga --version` also prints the git
   commit it was built from, so it is clear whether a branch or `main` is
   installed. Small and optional.

## 10. Tests

End-to-end, against a bare local repository standing in for origin and a
second clone standing in for GitHub or another machine:

- A commit pushed by the second clone: the next push is rejected, merged,
  pushed; the file is on the incoming list; the live file is untouched; the
  next sweep does not undo it.
- A hand edit in the clone: kept by the next sweep and committed; on the list.
- `memsaga pull`: L == B writes the file; both sides with separate lines merge
  cleanly; overlapping lines conflict and leave both files as they were.
- A merge conflict in a non-generated file: merge aborted, nothing pushed,
  status reports the files; a conflict only in `index.md` resolves on its own.
- `settings.json` round-trip keeps the live volatile keys; an executable script
  keeps its mode.
- `--auto` never writes a merged text and never deletes.
- The mod: `claude plugin test` for the hook logic (marker set → pull called
  before `next(e)`), plus one manual session.

Each guard is mutation-verified: remove it, see its test fail, restore it.
