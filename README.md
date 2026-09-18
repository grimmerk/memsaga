# memsaga

Version control for what Claude Code learns and how you configure it.

Claude Code keeps your global instructions in `~/.claude/CLAUDE.md`, writes
what it learns about each project into `~/.claude/projects/<slug>/memory/`,
and reads your skills, hooks and scripts from `~/.claude/`. None of that has
history: two sessions editing the same file in one day leave no trace of the
earlier version, and a memory clean-up that deletes or merges files is final.

memsaga gives those files a git history without changing where they live or how
Claude Code writes them. A hook fires at the end of every turn, memsaga copies
the allowlisted files into a private git repo and commits, one commit per turn,
with the session that made the change recorded in the commit. Several config
directories (accounts) are supported, and the first `init` replays your
session transcripts so the repo starts with weeks of history instead of a
single snapshot.

Status: personal tool, early, in daily use by its author. Contributor notes in
[`docs/architecture.md`](docs/architecture.md); design history (Traditional
Chinese) in [`docs/design-zh-tw.md`](docs/design-zh-tw.md).

## What you get

- `git log -p` on any memory file, skill, or your global CLAUDE.md.
- Commits that say which session, working directory and account changed what.
- A repo you can browse on GitHub or open as an Obsidian vault (memory files
  already use frontmatter and `[[wikilinks]]`).
- `memsaga restore` to bring back an earlier version, or a deleted file, after
  seeing the diff.
- History from before you installed memsaga, replayed from transcripts.

Live files are never edited by the tool except through `restore`, which shows
the diff and asks first.

## Install

```sh
cargo install --path crates/memsaga --locked   # binary lands in ~/.cargo/bin
memsaga init --dry-run                          # what would be imported, how much history is replayable
memsaga init                                    # create the repo, backfill, import the current state
memsaga install                                 # register the hooks in each config directory
memsaga status                                  # last sweep, pending changes, recent events
```

From then on nothing else is needed: every turn that changed something in
scope ends with a commit, and the terminal shows a one-line notice.

## Everyday commands

| Command | What it does |
|---|---|
| `memsaga status` | Last sweep, pending dirty marker, push state, recent events |
| `memsaga log` | Recent commits |
| `memsaga diff [--stat]` | What the next sweep would commit (refreshes the mirror, commits nothing) |
| `memsaga restore <path> [--at <rev>] [-y]` | Copy one file from history back to its live location, diff first |
| `memsaga sweep` | Mirror and commit now |
| `memsaga push` | Push to `origin` now (pushes also happen on `Stop`, throttled, and on `SessionEnd`) |
| `memsaga paths` | The slug → real-directory map |
| `memsaga install --skills` | Write the bundled skills (`memsaga-history`, `companion-docs`) into each config directory |
| `memsaga uninstall` | Remove the hooks |

Paths for `restore` can be repo-relative (`claude/memory/<slug>/x.md`)
or the live path.

## What is mirrored

Per config directory (root), by default: `CLAUDE.md`, `skills/`, `hooks/`,
`scripts/`, and every non-empty `projects/<slug>/memory/`; `settings.json` is
opt-in (`files = ["CLAUDE.md", "settings.json"]`). Never: transcripts, `~/.claude.json`, caches, `file-history`, plugins, and Claude Code's
own synced skills under `skills/synced/`.

Repo layout:

```
<repo>/
  claude/CLAUDE.md
  claude/skills/…  claude/hooks/…  claude/scripts/…
  claude/memory/<slug>/{MEMORY.md, *.md}
  claude-work/…                      second account; shared files stay symlinks
  index.md                           generated table of every project
  paths.json                         slug → real working directory
  companion/  manual/  …             yours: committed, never deleted by the tool
```

Before 2026-09-18 memory was stored at `<label>/projects/<slug>/memory/`. The
first sweep after upgrading moves it in one commit (renames plus the regenerated
`index.md`); `git log --follow` crosses the move for every file whose content did
not also change a lot in that same sweep, and `restore --at` reads older
revisions from the old path regardless.

## Configuration

`~/.config/memsaga/config.toml`; every key has a default and `memsaga config`
prints the effective values.

```toml
repo = "~/git/memories"
files = ["CLAUDE.md"]
dirs = ["skills", "hooks", "scripts"]
memory = true
exclude = ["*.codev-bak*", "*.codev-tmp-*", ".DS_Store"]
exclude_projects = []            # slug globs
exclude_paths = ["skills/synced"] # root-relative subtrees
granularity = "turn"             # or "edit": one commit per Write/Edit
reporter = true                  # one-line notice in the terminal after a commit
push_min_interval_secs = 600     # 0 disables pushes on Stop
push_on_session_end = true

[[roots]]
path = "~/.claude"
label = "claude"
```

Without `[[roots]]`, memsaga reads codev's `~/.config/codev/accounts.json` if
present, else uses `~/.claude`.

## Two accounts

Each Claude Code config directory is a root with its own subtree. Files the
second account shares through symlinks (a common pattern for `CLAUDE.md` and
`skills/`) are mirrored as relative symlinks inside the repo, so content is
stored once. Hooks are registered per directory; the commit's `Account:`
trailer comes from `CLAUDE_CONFIG_DIR`.

## Tests

`cargo test` runs behaviour tests that drive the real binary against fake
config directories in a temp dir (`crates/memsaga/tests/e2e.rs`) plus unit
tests for pure logic. CI runs them on ubuntu and macOS with `cargo fmt --check`
and `cargo clippy -D warnings`. `hook/config-vc.sh` is the original bash
prototype, kept as a reference implementation.
