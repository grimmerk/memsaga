# memvc architecture

Engineering notes for contributors. The user-facing description is the
[README](../README.md); the Traditional Chinese design history is
[`design-zh-tw.md`](design-zh-tw.md).

## The one-paragraph model

A *root* is a Claude Code config directory (`~/.claude`, or any directory
launched with `CLAUDE_CONFIG_DIR`). Each root holds files worth versioning
(global `CLAUDE.md`, per-project auto-memory under `projects/<slug>/memory/`,
`skills/`, `hooks/`, `scripts/`) next to a lot that is not (transcripts,
caches, OAuth state). memvc never edits a root. It copies an allowlist from
every root into a *mirror* git repo, one subtree per root, and commits when the
copy differs from HEAD. Claude Code hooks trigger it; a transcript replay gives
the repo history from before memvc existed.

```
                 PostToolUse(Write|Edit)           Stop / SessionEnd
Claude Code ─────────┬──────────────────────────────────┬───────────────▶  memvc hook (stdin JSON)
                     │ in scope? → touch .git/memvc/dirty │ dirty or mtime newer than last sweep?
                     │ (5 ms, no git)                    │   → lock → mirror → index → commit
                     ▼                                    │   → throttled push
   roots (read only)                                      ▼
   ~/.claude, ~/.claude-work  ──── rsync-like copy ────▶  ~/git/ccmem  (private git repo)
```

## Modules (`crates/memvc-core/src`)

| Module | Owns | Key facts |
|---|---|---|
| `config` | `Config`, `Root`, `Granularity`; TOML load + env overrides; root discovery | Precedence: `[[roots]]` in the file, else codev's `accounts.json`, else `~/.claude`. `current_root()` reads `CLAUDE_CONFIG_DIR`, which hooks inherit from the `claude` process. |
| `roots` | `Scope`: what is in scope for a root (`entries`) and whether an absolute path is (`contains`) | `contains` never touches the filesystem: the PostToolUse fast path must stay at process-startup cost. Exclusions: basename globs (`exclude`), slug globs (`exclude_projects`), root-relative subtrees (`exclude_paths`, default `skills/synced`). |
| `slug` | Claude Code's project-dir encoding and the reverse map | `encode` = every non-alphanumeric byte → `-`. The reverse map comes from each root's `history.jsonl` (`project` field per prompt), never from parsing the slug. |
| `mirror` | One-way copy of a root into `repo/<label>/` | rsync semantics: copy when size or mtime differ, preserve mtime, delete extras, keep symlinks. A top-level symlink into another root is rewritten relative (`../claude/CLAUDE.md`); a symlink inside a mirrored dir that escapes the tree becomes absolute. Deletion is confined to tool-owned subtrees; `.git` is never removed. |
| `git` | Spawning the system `git` | Always `-c commit.gpgsign=false` and `GIT_TERMINAL_PROMPT=0`: a hook has no TTY and must never wait on a signing or credential prompt. No libgit2, so the user's credential helpers and config apply. |
| `state` | `.git/memvc/`: flock, `last-sweep`, `dirty`, `last-push`, `report-offset`, `events.jsonl`, `memvc.log` | The lock is `flock(2)` on an open fd: a crashed process releases it, so there is no stale-lock logic. |
| `sweep` | `needs_sweep`, `refresh_mirror`, `pending`, `sweep`, commit message | `needs_sweep` is stat-only: allowlist mtimes vs `last-sweep`, plus a walk of user-owned top-level dirs in the repo. The marker is set to the sweep's *start* time so edits made during a sweep are caught next time. |
| `hook` | Event → action; throttled push; the reporter | `Turn` granularity: PostToolUse only marks dirty, Stop commits once per turn. `Edit` granularity sweeps per edit. Push: on Stop when ahead and `push_min_interval_secs` elapsed; on SessionEnd always. |
| `index` | `index.md`, `paths.json` at the repo root | Regenerated every sweep; the flat slug layout stays browsable on GitHub. |
| `install` | Hook registration in `settings.json` | Read-merge-write with `serde_json` `preserve_order`; foreign entries and key order survive; a `.memvc-bak` is taken before the first change; ours are the entries whose command mentions `memvc`. |
| `restore` | Repo path ↔ live path; the only write into a root | Diff first (`git diff --no-index`), then confirmation, then write, then `dirty` so the next Stop commits the live state. Refuses paths outside the allowlist. |
| `skills` | `SKILL.md` files compiled into the binary (`include_str!`) | Source of truth is `skills/<name>/SKILL.md` in this repo; `install --skills` writes them into a root. |
| `backfill` | Transcript replay into dated commits | Two passes per file: forward from each `Write` applying later `Edit`s; backward from the live file undoing `Edit`s while each op's result is still visible. Ops of one session within `group_window` seconds share a commit; `GIT_AUTHOR_DATE` = the last op's timestamp. Failed tool calls (`is_error`) are dropped. |

## Invariants

1. **A root is read-only** except in `restore`, which shows a diff and asks.
   Test: `mirror_is_one_way_and_never_touches_roots` snapshots both roots
   before and after sweeps.
2. **Deletion never leaves a tool-owned subtree.** `mirror::sync_dir` only
   deletes under the destination it was given; `prune_projects` only under
   `<label>/projects/`. User-owned paths (`companion/`, `manual/`, anything
   else at the repo root) are committed but never deleted by the tool.
3. **Hooks exit 0.** `hook` and `report` log errors to `.git/memvc/memvc.log`
   and never fail a Claude Code tool call.
4. **The fast paths do no git work.** PostToolUse out of scope: parse stdin,
   match path, exit. Stop with nothing newer: stat walk, exit.
5. **Concurrent sessions serialise on the flock**, and a sweep that finds
   nothing new after waiting commits nothing.

## CLI contract (what the tests pin)

- `memvc hook` reads one JSON object on stdin with the Claude Code hook fields
  (`hook_event_name`, `session_id`, `cwd`, `tool_input.file_path`,
  `tool_calls[]`, `agent_type`, `file_path`).
- Commit trailers: `Event`, `Account`, `Session`, `Cwd`, `Agent`.
- Repo layout: `<label>/CLAUDE.md`, `<label>/<dir>/…`,
  `<label>/projects/<slug>/memory/…`, plus `index.md`, `paths.json`,
  `.gitattributes`.
- Config: `~/.config/memvc/config.toml` (`MEMVC_CONFIG` overrides the path;
  `MEMVC_REPO` and `MEMVC_ROOTS` override values).

Anything that keeps this contract can be dropped in for the binary and the
behaviour tests will still pass.

## Tests

`crates/memvc/tests/e2e.rs` builds fake roots in a temp dir, runs the real
binary, and asserts on the git repo it produces: import scope and exclusions,
symlink handling, no-op sweeps, out-of-scope hooks, per-turn batching, Bash
edits and deletions caught at Stop, eight concurrent hooks, user-owned paths,
one-way mirroring, the reporter, install/uninstall, backfill, and status.
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
