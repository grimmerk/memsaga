# Two-way sync: bringing repository-side edits back to the live files

Design for grimmerk/memsaga-private#19, steps 1 to 3 in one pull request. Status: reviewed;
the decisions of §9 are settled (2026-10-06).

## Contents

- [TL;DR](#tldr)
- [Glossary](#glossary)
- [1. The problem](#1-the-problem)
- [2. The idea in one paragraph](#2-the-idea-in-one-paragraph)
- [3. Step 1a: push, and merge only when rejected](#3-step-1a-push-and-merge-only-when-rejected)
  - Diagram D1: the push path (ASCII)
- [4. Step 1b: the sweep stops overwriting repository-side edits](#4-step-1b-the-sweep-stops-overwriting-repository-side-edits)
- [5. The incoming list](#5-the-incoming-list)
- [6. Step 2: `memsaga sync` and `memsaga pull`](#6-step-2-memsaga-sync-and-memsaga-pull)
  - Diagram D2: deciding one file (ASCII)
- [7. Step 3: read before use, through command hooks](#7-step-3-read-before-use-through-command-hooks)
  - [7.1 What the hooks rely on (measured)](#71-what-the-hooks-rely-on-measured)
  - [7.2 The mod alternative](#72-the-mod-alternative)
- [8. Edge cases](#8-edge-cases)
- [9. Decisions](#9-decisions)
- [10. Tests](#10-tests)

The two diagrams are independent: D1 shows how a commit from elsewhere enters
the local repository, D2 shows what happens afterwards to each file it changed.
The [incoming list](#5-the-incoming-list) is the hand-off between them.

## TL;DR

| Step | What it does | What it fixes |
|---|---|---|
| 1a | When `git push` is rejected because origin has commits we lack, fetch, merge, push again. Stop and report if the merge conflicts. | Today one edit on GitHub makes every later push fail. |
| 1b | Before mirroring, find repository files that were edited in the repository (merged in, or edited by hand in the clone). The sweep leaves them alone instead of copying the live file over them. | Today the next sweep silently undoes such an edit. |
| 2 | `memsaga sync` (both directions, for people) and `memsaga pull` (inbound only, for the hooks of step 3): write the files from 1a and 1b to their live locations when the live file did not change meanwhile; merge text when both changed; delete what the repository deleted; list real conflicts. | Today an edit made on GitHub never reaches `~/.claude`. |
| 3 | Three more command hooks: at session start and at each prompt, `memsaga pull` when origin moved, and a notice that tells the model to re-read the changed files it already has in context; before a Read, Write or Edit of a file that changed on origin, the pull first. | Without it, the user has to remember to run step 2, and a session could read or edit a stale file. |

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
| `prompt.context` | A mod hook that runs when Claude Code builds the context of the first message (and again after `/compact`); it receives CLAUDE.md and the memory files already loaded and may replace their text. Only §7.2 uses it. |
| Notice | Text a hook returns as `additionalContext`: the model reads it, the person does not see it. |

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
overwrite it. `memsaga pull` (inside `memsaga sync`, or from the hooks of step 3) then
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
        │     └── keep ours and finish the merge; the next sweep rebuilds
        │         them (the push that follows may carry our older copy)
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

The sweep hands the mirror a guard, and the mirror asks it before it
overwrites or deletes any repository file:

1. A file on the incoming list is never overwritten or deleted.
2. Any other file whose mtime is older than the start of the last sweep was
   not touched since: go ahead. memsaga's own copies carry their source's
   mtime, so this is the common case, and it costs one `stat`.
3. A newer file is a candidate. It is not a hand edit when its content equals
   `HEAD` (memsaga committed it), equals what a `memsaga diff` preview wrote
   there (the preview records each copy's blob in `.git/memsaga/preview.json`,
   and the next sweep clears the record; needed because a live file that
   changed again after the preview no longer matches that copy), or equals
   what the mirror would write now (memsaga copied it without committing).
   Otherwise it is a hand edit: it goes on the incoming list with
   base = `HEAD:<path>` (none for a new file), and is left as it is.
4. Commit. A hand edit is committed by this sweep like any other change, so it
   enters history at once.

The same check runs before a Read, Write or Edit of one file (step 3), so a
hand edit made since the last sweep reaches the live file before it is used.
A deletion made by hand in the clone is not detected: the next sweep copies
the live file back. Delete on the live side, or on GitHub.

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

## 6. Step 2: `memsaga sync` and `memsaga pull`

```
memsaga sync     # both directions: what a person runs
memsaga pull     # inbound only: what the hooks run
```

`memsaga sync`:

1. Sweep: commit what changed live, so pending live edits are kept and the
   merge base below matches the live files.
2. If origin has commits we lack: fetch and merge exactly as §3 steps ③ to ⑤.
3. Settle each entry of the incoming list with D2 (this part, with step 2, is
   `memsaga pull`).
4. Sweep again: commit what step 3 wrote (merged texts, written files).
5. Push. If the push itself merged a commit that reached origin meanwhile,
   settle, sweep and push once more.
6. Print what was written, merged, deleted, and left as a conflict; exit 1
   when a conflict is left (the push has happened: it is safe either way).

Each step takes the sweep lock for itself; the push lock is never held at the
same time.

There is no confirmation prompt: every case D2 settles on its own is safe to
apply (only one side changed, or the two sides changed different lines), and
every overwritten or deleted live version is in the repository's history, so
`memsaga restore` brings it back.

**Diagram D2: deciding one file.** L = mirror form of the live file,
B = base, R = repository file.

```
  L == R ? ── yes ──▶ already in step; drop the entry
     │ no
  L == B ? ── yes ──▶ only the repository changed:
     │                write R to the live file, or delete the live file
     │                when the repository deleted it
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
- A conflict is resolved by the user editing the live file (or the repository
  file) and running `memsaga sync` again; once L equals R, the entry is
  dropped. `memsaga sync --take live|repo <path>` settles one file explicitly.
- A both-sides change in a file that is not text (no line structure that
  `git merge-file` can use) is a conflict.

## 7. Step 3: read before use, through command hooks

memsaga has no daemon and no timer, and this step does not add one: it checks
origin only at the moments a stale file could be used (a check a Read or a
prompt does not wait for runs as a short-lived detached process), and a shared
answer throttles the check across every session on the machine. Everything runs as
settings hooks registered by `memsaga install`, the same mechanism as the
sweep, so it works in the terminal, `claude -p`, the desktop app and SDK
hosts, in both accounts.

**The check** (`memsaga pull --check`):

1. If any session checked within the last minute (10 seconds before a Write
   or Edit)
   (`.git/memsaga/remote-check.json`), use its answer.
2. Otherwise ask origin for its branch head: the GitHub API with an ETag
   (one request, about 0.5 s; a 304 when nothing changed costs no rate
   limit), or `git ls-remote` when origin is not on GitHub or `gh` has no
   token (three HTTPS round trips, about 1 s).
3. If origin moved: `git fetch`, and record which live-mapped files the new
   commits change.

| Hook | What it does | Cost |
|---|---|---|
| `SessionStart` (start, `/compact`, `/clear`) | The check, waiting for it. When origin moved or the incoming list is non-empty: `memsaga pull`, and a notice (`additionalContext`) naming the changed CLAUDE.md and MEMORY.md files. | Reading the shared answer when another session checked recently; else about 0.5 s; a fetch and a pull only when origin moved. |
| `PreToolUse` for Read, Write and Edit | Ignore a file outside memsaga's scope. Otherwise: note the file as used by this session; note a hand edit when its repository copy is newer than the last sweep; when the shared answer is older than a minute, a Read starts a background check without waiting, while a Write or Edit waits for the check when the answer is older than 10 seconds (an edit built on an outdated copy would end in a merge conflict, or drop the other side's change); and when **this** file is on the incoming list or among the files origin changed, `memsaga pull` first. Then a Read goes ahead on the fresh file. A Write is refused when the pull just changed the file (its content was prepared from the old copy), and a Write or Edit is refused when origin's change could not be merged; the reason tells the model to read again or to tell the user. An Edit otherwise goes ahead: it fails on its own when its old text is gone. | One `memsaga` process start (about 10 ms) per Read, Write or Edit, since a hook matcher selects tools, not paths; about 0.5 s for a Write or Edit when the shared answer is old; a pull only when this very file changed. |
| `UserPromptSubmit` | Start a background check when the shared answer is old. When origin moved or the incoming list is non-empty: `memsaga pull`, and a notice naming the changed files this session used plus any changed CLAUDE.md and MEMORY.md. | Nothing in the common case beyond the process start; a pull only after origin moved. |

Notices go to the model only; the Stop reporter (§5) tells the person. A
notice names each file, quotes the changed lines (up to 20 per file; a longer
change asks for a fresh Read), and says that from now on the current file is
the only version that counts, in answers and in instructions alike. The last
part was added after a session that had re-read the file still answered from
the copy loaded at start; a probe could not reproduce that in short sessions
(old and new wording alike led with the new text, 3 of 3 each), so the
wording is a precaution, not a measured fix. Each hook waits at most 20 s for the sweep lock, well
inside its 30 s timeout: a busy lock makes it skip the pull rather than be
stopped half-way.
A change that lands in the middle of a turn is announced at the next prompt;
until then, touching that file blocks on the pull as above. The guarantee is
bounded by the shared answer's age: a change made on origin less than a
minute after the last check (10 seconds for a Write or Edit) is not seen
until the next check, and a Read in that window gets the live copy as it is.
A file counts as used by the session when it went through Read, Write or Edit,
or when the session transcript names its path (a read through Bash).

### 7.1 What the hooks rely on (measured)

Probe: `claude -p --model opus`, Claude Code 2.1.291, 3 runs per arm, with
and without the notice (scripts kept outside this repository).

| Question | Result |
|---|---|
| Does `SessionStart` run before CLAUDE.md is loaded? | No: a SessionStart hook that rewrote CLAUDE.md left the old text in context, 3 of 3. So pulling at session start updates the file but not the session's copy, and the notice is what makes the session see the new text. |
| After a notice on an unrelated tool's result, does the model re-read a file it read before? | Yes, 3 of 3, and it answered from the new text. Without the notice: 3 of 3 answered from the stale copy. |
| After a `SessionStart` notice, does the model re-read CLAUDE.md when the prompt never mentions it, and follow the new instruction? | Yes, 3 of 3. Without the notice: 3 of 3 followed the old instruction. |

Not measured: the interactive session's ordering, and whether a notice works
as well deep into a long session.

### 7.2 The mod alternative

A Claude Code mod (a plugin with in-process TypeScript hooks, Claude Code
2.1.287 and later) could do the same work, and was the first design:

- `prompt.context` receives the instruction files already loaded and can
  replace their text, so the session would start with the new CLAUDE.md and
  MEMORY.md without a notice and without an extra Read.
- `tool.call` filters by path inside the process, so a Read or Edit outside
  memsaga's scope would cost nothing instead of one process start; its result
  can carry the same notice (`context`).

It was set aside for this change because the measured gain is small (about
10 ms per call, and a notice the model followed every time) while the costs
are real: the mod API is early access and changes between releases (a failing
mod hook is skipped, so the guard would silently stop working), loading it
needs `CLAUDE_CODE_PLUGIN_DIRS` in each account's settings, and it adds a
second language and test stack. It is the natural home for this logic once a
memsaga mod exists for the status line (grimmerk/memsaga-private#18) and for moving the other
hooks in process (grimmerk/memsaga-private#20); the command hooks' entry points are kept small
so they can be called from a mod unchanged.

## 8. Edge cases

- **Generated files.** `index.md` and `paths.json` are rebuilt by every sweep.
  In a merge conflict memsaga keeps its own version, and the next sweep
  rebuilds them from both sides' files; they never go on the incoming list.
- **Symlinks in the repository** (a second account sharing a file with the
  first) are skipped by pull, as `restore --all` already does for external
  links.
- **Deletions.** A file deleted on the repository side has R missing. With
  L == B the live file is deleted and the deletion is listed in the output;
  with L != B it is a conflict.
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

## 9. Decisions

Settled with author on 2026-10-06:

1. **Deletions from the repository side** are applied directly when the live
   file did not change (L == B), and listed. History keeps the old version.
2. **No separate unattended mode.** A clean three-way merge is applied
   automatically; only overlapping edits stop and are reported.
3. **Commands:** `memsaga sync` (both directions) for people,
   `memsaga pull` (inbound only) for the hooks.
4. **Step 3** checks origin only at session start, at each prompt and before
   a Read, Write or Edit of a live-mapped file, throttled across sessions; no
   timers, no background process.
5. **Blocking** before a Read, Write or Edit, and only when that file changed
   on origin. A file already in the session's context is announced by a
   notice the model was measured to act on (§7.1).
6. **Command hooks, not a mod**, for this change (§7.2); a memsaga mod may
   take the logic over later together with grimmerk/memsaga-private#18 and grimmerk/memsaga-private#20.
7. `memsaga --version` prints the commit it was built from, for example
   `memsaga 0.1.0 (0570369)`, so it is clear whether a branch or `main` is
   installed.

## 10. Tests

End-to-end, against a bare local repository standing in for origin and a
second clone standing in for GitHub or another machine:

- A commit pushed by the second clone: the next push is rejected, merged,
  pushed; the file is on the incoming list; the live file is untouched; the
  next sweep does not undo it.
- A hand edit in the clone: kept by the next sweep and committed; on the list.
- `memsaga sync`: L == B writes the file; both sides with separate lines merge
  cleanly; overlapping lines conflict and leave both files as they were; a
  repository-side deletion deletes the unchanged live file; the live changes
  are committed and pushed in the same run.
- A merge conflict in a non-generated file: merge aborted, nothing pushed,
  status reports the files; a conflict only in `index.md` resolves on its own.
- `settings.json` round-trip keeps the live volatile keys; an executable script
  keeps its mode.
- The check: a second call within the window does not reach the network;
  `remote-changed` lists exactly the files the new commits touch.
- The hooks: a session start after origin moved pulls and returns a notice
  naming CLAUDE.md; a Read of a file origin changed waits for the pull; a
  Read of an unchanged file does not; a prompt after a change to a file the
  session used returns a notice naming it.

Each guard is mutation-verified: remove it, see its test fail, restore it.
