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
| `memsaga status [--json]` | Last sweep, pending dirty marker, push state (including a failing or deferred push), a missing config file, recent events; `--json` prints the same as one object for scripts and tools. Read-only |
| `memsaga log` | Recent commits |
| `memsaga diff [--stat]` | What the next sweep would commit (refreshes the mirror, commits nothing) |
| `memsaga restore <path> [--at <rev>] [-y]` | Copy one file from history back to its live location, diff first |
| `memsaga sweep` | Mirror and commit now |
| `memsaga push` | Push to `origin` now (pushes also happen on `Stop`, throttled, and on `SessionEnd`) |
| `memsaga paths` | The slug → real-directory map |
| `memsaga restore --all --at <rev>` | Every mirrored (in-scope) file of every root as it was at `<rev>`: overwrites what differs, never deletes, skips symlinks, keeps the live `settings_ignore_keys`; shows the diffs, one confirmation |
| `memsaga companions [repo]` | List companion documents (status, path, source); `/companions` in a session prints the same |
| `memsaga install --skills` | Write the bundled skills (`memsaga-history`, `companion-docs`, `companions`) into each config directory |
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
  memsaga/config.toml                this config file, mirrored automatically
  extra/home/…  extra/root/…         extra_files, by their real path
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
exclude = ["*.codev-bak*", "*.codev-tmp-*", ".DS_Store", "__pycache__"]
settings_ignore_keys = ["model", "effortLevel", "modelSettings"]  # dropped from the mirrored settings.json: Claude Code rewrites them on every /model and /effort
exclude_projects = []            # slug globs
exclude_paths = ["skills/synced"] # root-relative subtrees
extra_files = []                  # single files outside every root, e.g. "~/.config/codev/accounts.json"
granularity = "turn"             # or "edit": one commit per Write/Edit
reporter = true                  # one-line notice in the terminal after a commit
push_min_interval_secs = 600     # at most one push per interval on Stop; a throttled push is deferred to the end of the window, not dropped. 0 disables pushes on Stop
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
trailer comes from `CLAUDE_CONFIG_DIR`, and `Session-Title:` (when the session
has one) from the end of the session's transcript. The title also starts the
subject line, `[<title>] claude:memory:<slug>`, cut to 32 characters there so
the subject stays within 72; the trailer keeps the whole title. The prompt text is never copied into a commit.

If the config file disappears while the repo still holds its mirrored copy,
memsaga runs on its defaults: fewer files are mirrored, and copies of files
that drop out of scope are left in the repo (the usual mirror deletions still
apply to what stays in scope). The reporter prints a warning with the restore
command after every turn until the file is back.

## Restoring on another machine

To set up a new machine from the data repository (tested with the same
username and the same `~/git` layout, so memory directory names match):

1. Install Claude Code and sign in once, so `~/.claude` exists. Create the
   directory of every other account in your config (for example
   `~/.claude-work`); a root that does not exist is skipped, with a count.
2. Clone the data repository: `gh repo clone <you>/memories ~/git/memories`.
   Do **not** run `memsaga init` there: it would commit the new machine's
   empty state on top of your history.
3. Install memsaga and copy the config:
   `cp ~/git/memories/memsaga/config.toml ~/.config/memsaga/config.toml`.
4. `memsaga restore --all --at HEAD`: read the list and the diffs, answer
   `n`; then run it again with `--yes`. Files get their content, executable
   bits and commit time back; links that share files between accounts are
   recreated; links that point outside the mirrored directories are listed
   and skipped. Nothing is deleted.
5. Verify: `memsaga sweep --event verify` should report `no changes`, and
   `memsaga diff --stat` should print `no changes since the last commit`.

If `settings.json` is in your `files` (it is not by default), restoring it
also registers the hooks, so new sessions on that machine start committing at
once; otherwise run `memsaga install` after the restore. Sweeps are one-way (live files into the
repository) and never pull, so do not use two machines at the same time: the
second one's pushes would be rejected. If the new machine is only a backup,
run `memsaga uninstall` there.

## Tests

`cargo test` runs behaviour tests that drive the real binary against fake
config directories in a temp dir (`crates/memsaga/tests/e2e.rs`) plus unit
tests for pure logic. CI runs them on ubuntu and macOS with `cargo fmt --check`
and `cargo clippy -D warnings`. `hook/config-vc.sh` is the original bash
prototype, kept as a reference implementation.
