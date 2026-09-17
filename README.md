# memvc

Version control for Claude Code's memory and config. A hook-driven, one-way
mirror of `~/.claude/CLAUDE.md`, every project's auto-memory
(`~/.claude/projects/<slug>/memory/`), and your skills into a private git repo,
committed once per turn with the session that made the change recorded in the
commit. Multiple config directories (accounts) are supported, and the first
`init` replays your session transcripts so the repo starts with weeks of
history instead of a single snapshot.

Status: personal tool, early. Working notes (Traditional Chinese) in
[`docs/design-zh-tw.md`](docs/design-zh-tw.md); an English version will precede any public release.

## How it works

```
Claude Code ── PostToolUse(Write|Edit) / Stop / SessionEnd ──▶ memvc hook
                                                                 │ mirror allowlist → ~/git/ccmem/<account>/…
                                                                 │ git commit (trailers: Event, Account, Session, Cwd, Agent)
~/.claude, ~/.claude-work  (read only)                          ▼
                                                        ~/git/ccmem  (private git repo)
```

- Live files are never written. The repo is a copy; restore is an explicit action.
- `PostToolUse` only marks the turn dirty (5 ms); `Stop` sweeps and commits once
  per turn; `SessionEnd` sweeps again and pushes. Edits made through Bash are
  caught by an mtime scan at `Stop`.
- Concurrency across sessions is handled with `flock`.

## Install

```sh
cargo install --path crates/memvc --locked
memvc init --dry-run        # what would be imported, how much history is replayable
memvc init                  # create the repo, backfill from transcripts, import
memvc install               # register hooks in each account's settings.json
memvc status
```

Config lives at `~/.config/memvc/config.toml`; every key has a default
(`memvc config` prints the effective values). Roots are discovered from
codev's `~/.config/codev/accounts.json` when present, else `~/.claude`.

## Tests

`cargo test` runs behaviour tests that drive the real binary against fake
config directories in a temp dir (`crates/memvc/tests/e2e.rs`) plus a few
unit tests for pure logic. `hook/config-vc.sh` is the original bash prototype,
kept as the reference implementation.
