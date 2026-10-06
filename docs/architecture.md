# memsaga architecture

Engineering notes for contributors. The user-facing description is the
[README](../README.md); the Traditional Chinese design history is
[`design-zh-tw.md`](design-zh-tw.md).

## The one-paragraph model

A *root* is a Claude Code config directory (`~/.claude`, or any directory
launched with `CLAUDE_CONFIG_DIR`). Each root holds files worth versioning
(global `CLAUDE.md`, per-project auto-memory under `projects/<slug>/memory/`,
`skills/`, `hooks/`, `scripts/`) next to a lot that is not (transcripts,
caches, OAuth state). memsaga writes into a root only on purpose (`restore`,
and two-way sync's `sync` / `pull`, see `two-way-sync.md`). It copies an allowlist from
every root into a *mirror* git repo, one subtree per root, and commits when the
copy differs from HEAD. Claude Code hooks trigger it; a transcript replay gives
the repo history from before memsaga existed.

```
                 PostToolUse(Write|Edit)           Stop / SessionEnd
Claude Code ─────────┬──────────────────────────────────┬───────────────▶  memsaga hook (stdin JSON)
                     │ in scope? → touch .git/memsaga/dirty │ dirty or mtime newer than last sweep?
                     │ (5 ms, no git)                    │   → lock → mirror → index → commit
                     ▼                                    │   → throttled push
   roots (read only)                                      ▼
   ~/.claude, ~/.claude-work  ──── rsync-like copy ────▶  ~/git/memories  (private git repo)
```

## Modules (`crates/memsaga-core/src`)

| Module | Owns | Key facts |
|---|---|---|
| `config` | `Config`, `Root`, `Granularity`; TOML load + env overrides; root discovery | Precedence: `[[roots]]` in the file, else codev's `accounts.json`, else `~/.claude`. `current_root()` reads `CLAUDE_CONFIG_DIR`, which hooks inherit from the `claude` process. |
| `roots` | `Scope`: what is in scope for a root (`entries`) and whether an absolute path is (`contains`) | `contains` never touches the filesystem: the PostToolUse fast path must stay at process-startup cost. Exclusions: basename globs (`exclude`), slug globs (`exclude_projects`), root-relative subtrees (`exclude_paths`, default `skills/synced`). |
| `slug` | Claude Code's project-dir encoding and the reverse map | `encode` = every non-alphanumeric byte → `-`. The reverse map comes from each root's `history.jsonl` (`project` field per prompt), never from parsing the slug. |
| `mirror` | One-way copy of a root into `repo/<label>/` | rsync semantics: copy when size or mtime differ, preserve mtime, delete extras, keep symlinks. A top-level symlink into another root is rewritten relative (`../claude/CLAUDE.md`); a symlink inside a mirrored dir that escapes the tree becomes absolute. Deletion is confined to tool-owned subtrees; `.git` is never removed. |
| `git` | Spawning the system `git` | Always `-c commit.gpgsign=false` and `GIT_TERMINAL_PROMPT=0`: a hook has no TTY and must never wait on a signing or credential prompt. No libgit2, so the user's credential helpers and config apply. |
| `state` | `.git/memsaga/`: flock, `last-sweep`, `dirty`, `last-push`, `push-pending` (due time of the deferred push), `config-fingerprint`, `report-offset`, `events.jsonl`, `memsaga.log` | The lock is `flock(2)` on an open fd: a crashed process releases it, so there is no stale-lock logic. A sweep (holding the lock) renames `events.jsonl` to `events-<time>.jsonl` past 1 MiB; the reporter's `report-offset` is `<inode>:<offset>`, and a rename keeps the inode, so the reporter finishes the rotated file and every later one before the new file. |
| `status` | The `Status` value behind `status` and `status --json` | Read-only. One struct for both outputs, so the text and the JSON cannot disagree. |
| `sweep` | `needs_sweep`, `refresh_mirror`, `pending`, `sweep`, commit message | `needs_sweep` is stat-only: allowlist and `extra_files` mtimes vs `last-sweep`, the config file's mtime, a digest of the effective config (`config-fingerprint` marker, so widening the scope sweeps even when nothing in the old scope is newer), a walk of user-owned top-level dirs in the repo, and a check for a pre-2026-09-18 `<label>/projects/`. The marker is set to the sweep's *start* time so edits made during a sweep are caught next time. |
| `sync` | Two-way sync (`two-way-sync.md`): the incoming list, the mirror's `Guard`, merging origin, settling each file, the throttled remote check, and the SessionStart / UserPromptSubmit / PreToolUse entry points | Every write to a live file is atomic, under the sweep lock, and only for a path a root, the config file or `extra_files` maps. The sweep lock and the push lock are never held together. |
| `hook` | Event → action; throttled push (a rejected push merges origin and retries once); the reporter (commit notice, missing-config warning, files waiting for `memsaga sync`) | `Turn` granularity: PostToolUse only marks dirty, Stop commits once per turn. `Edit` granularity sweeps per edit. Push: on Stop when ahead and `push_min_interval_secs` elapsed; inside the window a detached `memsaga push --after N` (marker `push-pending`, one waiter at a time) pushes when it ends, so a session's last commit does not wait for another Stop; on SessionEnd always. |
| `index` | `index.md`, `paths.json` at the repo root | Regenerated every sweep; the flat slug layout stays browsable on GitHub. |
| `install` | Hook registration in `settings.json` | Read-merge-write with `serde_json` `preserve_order`; foreign entries and key order survive; a `.memsaga-bak` is taken before the first change; ours are the entries whose command mentions `memsaga`. |
| `restore` | Repo path ↔ live path; a deliberate write into a root (the other is `sync`) | Diff first (`git diff --no-index`), then confirmation, then write, then `dirty` so the next Stop commits the live state. Refuses paths outside the allowlist. Accepts the old `projects/<slug>/memory` spelling and reads pre-move revisions from it. |
| `skills` | `SKILL.md` files compiled into the binary (`include_str!`) | Source of truth is `skills/<name>/SKILL.md` in this repo; `install --skills` writes them into a root. |
| `backfill` | Transcript replay into dated commits | Two passes per file: forward from each `Write` applying later `Edit`s; backward from the live file undoing `Edit`s while each op's result is still visible. Ops of one session within `group_window` seconds share a commit; `GIT_AUTHOR_DATE` = the last op's timestamp. Failed tool calls (`is_error`) are dropped. |

## Invariants

1. **A sweep never writes a root.** Only two paths do, on purpose: `restore`,
   which shows a diff and asks, and two-way sync (`sync` / `pull` and the
   session hooks), which writes a live file only when the repository holds a
   newer version and the three-way decision allows it (`two-way-sync.md`).
   Test: `sweeps_never_touch_roots_and_keep_a_repository_side_edit`
   snapshots both roots before and after sweeps.
2. **Deletion never leaves a tool-owned subtree.** `mirror::sync_dir` only
   deletes under the destination it was given; `prune_memory` only under
   `<label>/memory/` (and the pre-2026-09-18 `<label>/projects/`, which it
   removes once). User-owned paths (`companion/`, `manual/`, anything
   else at the repo root) are committed but never deleted by the tool.
3. **Hooks exit 0.** `hook` and `report` log errors to `.git/memsaga/memsaga.log`
   and never fail a Claude Code tool call.
4. **The fast paths do no git work.** PostToolUse out of scope: parse stdin,
   match path, exit. Stop with nothing newer: stat walk, exit.
5. **Concurrent sessions serialise on the flock**, and a sweep that finds
   nothing new after waiting commits nothing.

## CLI contract (what the tests pin)

- `memsaga hook` reads one JSON object on stdin with the Claude Code hook fields
  (`hook_event_name`, `session_id`, `cwd`, `tool_input.file_path`,
  `tool_calls[]`, `agent_type`, `file_path`).
- Commit subject: `[<session title>] <label>:<area>, …` (title cut to 32 characters, whole line to 72), or `<event>: …` without a title.
- Commit trailers: `Event`, `Account`, `Session`, `Cwd`, `Agent`, and `Session-Title` when the transcript names one.
- Repo layout: `<label>/CLAUDE.md`, `<label>/<dir>/…`,
  `<label>/memory/<name>/…` (live `projects/<slug>/memory/`; `<name>` is the
  slug with the home directory replaced by `HOME`, `roots::repo_name_of_slug`;
  the mapping is `roots::repo_rel_of_live` and its inverse), plus `index.md`, `paths.json`,
  `.gitattributes`.
- Config: `~/.config/memsaga/config.toml` (`MEMSAGA_CONFIG` overrides the path;
  `MEMSAGA_REPO` and `MEMSAGA_ROOTS` override values).

Anything that keeps this contract can be dropped in for the binary and the
behaviour tests will still pass.

## Tests

`crates/memsaga/tests/e2e.rs` builds fake roots in a temp dir, runs the real
binary, and asserts on the git repo it produces: import scope and exclusions,
symlink handling, no-op sweeps, out-of-scope hooks, per-turn batching, Bash
edits and deletions caught at Stop, eight concurrent hooks, user-owned paths,
roots untouched by sweeps, two-way sync (origin edits, hand edits, merges,
conflicts, deletions, the hooks of step 3), the reporter, install/uninstall,
backfill, and status.
Unit tests cover the pure functions only (slug encoding, scope matching,
relative-path computation, backfill reconstruction).

Run `cargo test`; CI runs the same on ubuntu-latest and macos-latest with
`cargo fmt --check` and `cargo clippy -D warnings`.

## Measured on the author's machine (2026-09-18)

| | |
|---|---|
| PostToolUse, file out of scope | 5 ms |
| Stop, nothing changed | 24 ms |
| Stop with a change: copy + commit | 0.2–0.5 s |
| `init` on two roots, 1,222 transcript ops → 503 commits | 78 s |
| Share of turns that produce a commit | about 11 % |
