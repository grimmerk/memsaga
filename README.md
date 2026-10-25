# memsaga

Version control and two-way sync for the files your AI coding agent works
with: what it remembers, how you configure it, and anything else you choose.

**Website:** [memsaga.grimmer.io](https://memsaga.grimmer.io) ·
[Install](#quick-start) · [Do you need it?](#do-you-need-memsaga) ·
[FAQ](docs/faq.md) ·
[Discussions](https://github.com/grimmerk/memsaga/discussions)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/hero-dark.png">
  <img src="docs/assets/hero-light.png" width="100%" alt="memsaga: three sources (Claude Code's config directory in every account, files elsewhere you choose, and content kept only in the repository) become a readable history in a git repository you own, each change named after its session, from fixing a login redirect to restoring CLAUDE.md to Monday's version. The repository syncs both ways between this machine, a private GitHub repository and another machine, and edits made on the web or a phone come back.">
</picture>

Claude Code keeps your global instructions in `~/.claude/CLAUDE.md`, writes
what it learns about each project into `~/.claude/projects/<slug>/memory/`,
and reads your skills, hooks and scripts from `~/.claude/`. None of that has
history. Two sessions that edit the same file on one day leave no trace of the
earlier version, and a memory clean-up that deletes or merges files is final.

memsaga keeps one git repository you own, the data repository, and fills it
from three places, told apart by where a file lives:

1. **Claude Code's config directory** (`~/.claude`). What Claude Code reads
   and writes there: memory, `CLAUDE.md`, skills, hooks, scripts and (if you
   want) `settings.json`, plus folders of your own such as drafts or
   hand-written mods. They are mirrored without changing where they live or how
   Claude Code writes them.
2. **Files elsewhere that you choose.** A notes file, a plan in your home
   directory, a dotfile: whoever edits it, you or Claude, the change is
   committed.
3. **Content that lives only in the repository.** It is a normal git
   repository: add notes, companion documents (your notes, translations or
   drafts about other repositories), anything else; edit and push from
   anywhere. memsaga commits what you add and never deletes it.

At the end of every turn a hook copies what changed and commits it, one commit
per turn. Each commit names the session that made the change, by title and by
ID, so you can go from any change back to the conversation that caused it, and
from a conversation to everything it changed. The
repository can live on GitHub and be shared by several machines and Claude Code
accounts; an edit made in one place (another machine, GitHub's web editor, a
phone) reaches the live files everywhere else.

Status: early, in daily use by its author on macOS (CI also runs on Linux).
Claude Code is the only source today; other agents and sources are on the
[roadmap](#roadmap). Contributor notes: [`docs/architecture.md`](docs/architecture.md).
Two-way sync: [`docs/two-way-sync.md`](docs/two-way-sync.md). Design record:
[`docs/design.md`](docs/design.md).

## Contents

- [Do you need memsaga?](#do-you-need-memsaga)
- [Quick start](#quick-start)
- [What you can do with it](#what-you-can-do-with-it)
  - Claude Code's config directory
    1. [See what Claude remembered, and when it changed](#1-see-what-claude-remembered-and-when-it-changed)
    2. [Undo a bad edit or a memory clean-up](#2-undo-a-bad-edit-or-a-memory-clean-up)
    3. [Ask Claude about the history](#3-ask-claude-about-the-history)
    4. [Start with the history you already have](#4-start-with-the-history-you-already-have)
  - Files elsewhere, and content only in the repository
    5. [Version any file Claude edits for you](#5-version-any-file-claude-edits-for-you)
    6. [Add your own files to the repository](#6-add-your-own-files-to-the-repository)
    7. [Keep notes beside a repository without committing them](#7-keep-notes-beside-a-repository-without-committing-them)
  - Everywhere you work
    8. [Two Claude Code accounts](#8-two-claude-code-accounts)
    9. [Two machines](#9-two-machines)
    10. [Edit from anywhere](#10-edit-from-anywhere)
  - Going further
    11. [Let Claude reorganise its memory, safely](#11-let-claude-reorganise-its-memory-safely)
- [What memsaga does not cover, and what to use instead](#what-memsaga-does-not-cover-and-what-to-use-instead)
- [FAQ](docs/faq.md): other ways to keep this history, compared
- [How it works](#how-it-works)
- [When something looks wrong](#when-something-looks-wrong)
- [Reference](#reference): [commands](#commands), [what is mirrored](#what-is-mirrored), [memory directory names](#memory-directory-names), [configuration](#configuration), [two-way sync](#two-way-sync), [commit messages](#commit-messages), [updating](#updating), [setting up another machine](#setting-up-another-machine)
- [Tests](#tests)
- [Roadmap](#roadmap)
- [The name](#the-name)

Pictures: the image at the top (where files come from, and where the history
goes), and a sequence diagram in [use case 10](#10-edit-from-anywhere) (what happens
when a file changed elsewhere).

## Do you need memsaga?

**Maybe not.** If you use Claude Code with one account on one computer, a
do-it-yourself setup gets you most of the history:

1. Turn `~/.claude` into a git repository, a folder whose history git keeps
   (`git init`).
2. Add a `.gitignore` that lets in only what you want kept (memory,
   `CLAUDE.md`, skills) and keeps out conversations and credentials.
3. Add a hook that commits at the end of every reply (a `Stop` hook).

memsaga does this for you, out of the box. There is no list or hook to write
yourself: it already knows which files Claude Code reads (`CLAUDE.md`, memory,
skills, agents, commands, rules, hooks), following Claude Code's own directory
reference, and it leaves out credentials, conversations and caches.

It also handles what the do-it-yourself setup cannot easily handle:

- **A separate repository, away from the files Claude is using.** In the
  do-it-yourself setup, Claude Code's own folder *is* the repository, so a
  single git command run there (`git checkout`, `git clean`) or a merge
  conflict changes the files Claude is reading. memsaga keeps its copies in a
  normal folder you choose (by default `memories` in the folder where you keep
  code), not in the hidden `~/.claude`, with an `index.md` that lists every
  project by its real folder,
  so it is easy to browse in Finder, an editor or Obsidian.
- **Files you choose, even outside Claude Code's folder.** Beyond the files
  Claude Code uses, which are covered by default (above), a folder you create
  in `~/.claude` yourself, such as `~/.claude/drafts/`, takes one line in
  `dirs`. Outside `~/.claude`, name each file in `extra_files`,
  such as a plan in your home directory or another tool's config file, and
  every change to it is committed, whether you made it or Claude did
  ([use case 5](#5-version-any-file-claude-edits-for-you)). Whole folders
  outside `~/.claude` are not supported yet
  ([#20](https://github.com/grimmerk/memsaga/issues/20)).
- **Edits from anywhere, and more than one account or computer.** You can fix
  a memory in GitHub's web editor, on a phone, or on another computer. In the
  do-it-yourself setup, bringing such an edit back is a `git pull` that writes
  straight into the files in use, and a conflict leaves git's markers in a file
  Claude reads. memsaga brings the edit to the live file for you: it keeps the
  edits made on both sides, and if both changed the same line, it stops and
  tells you. It also tells Claude what changed. Every account lives in one
  repository, and another computer picks up changes the next time you use
  Claude there.
- **Which conversation made each change.** Every commit names the conversation
  behind it, and `claude --resume` reopens that conversation as long as Claude
  Code keeps it: 30 days after its last use, unless you raise
  `cleanupPeriodDays`.
- **History from before you installed it.** memsaga rebuilds it from the edits
  recorded in those same saved conversations.
- **Your own documents about other projects.** Some documents are about a
  project but must not go into its repository, for example notes on your
  employer's code. Ask Claude for one (a note, a translation of its docs, or a
  draft) and it is kept in memsaga's repository under `companion/`, filed by
  the project and file it is about, so a later conversation finds it again.
  memsaga calls these companion documents
  ([use case 7](#7-keep-notes-beside-a-repository-without-committing-them)).

Other ways to do this, compared, and the do-it-yourself setup step by step:
[FAQ](docs/faq.md).

## Quick start

You need macOS or Linux, git and Claude Code. `gh` (the GitHub command-line
tool) is optional, for creating the GitHub repository. The first line installs a
prebuilt binary into `~/.cargo/bin` (Apple silicon and Intel Macs, Linux on
x86_64 and arm64). To build from source instead, with Rust's `cargo` (Rust
1.88 or later): `cargo install memsaga --locked` from crates.io, or
`cargo install --git https://github.com/grimmerk/memsaga memsaga --locked` for
the latest `main`.
Rather not pipe a script into `sh`? Download the archive for your platform from
the [Releases page](https://github.com/grimmerk/memsaga/releases/latest) and
put `memsaga` on your `PATH`. On macOS a browser marks the download as
quarantined, and the binary is not notarized, so clear the mark once:
`xattr -d com.apple.quarantine memsaga`.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/grimmerk/memsaga/releases/latest/download/memsaga-installer.sh | sh
memsaga init --dry-run    # what would be imported, and how much history can be replayed
memsaga init              # create the data repository (see "Where the data repository goes"), replay history, import the current state
memsaga install           # register the hooks in each Claude Code config directory
gh repo create memories --private --source "$(memsaga repo)" --push   # optional: a private GitHub repository; any name works
memsaga status            # last sweep, pending changes, push state
```

**Where the data repository goes.** `init` creates it as `memories` in the
first of `~/git`, `~/code`, `~/src`, `~/Projects` and `~/dev` that exists, or as
`~/memories` when none does, and records the choice as `repo` in
`~/.config/memsaga/config.toml`. `memsaga init --repo <path>` puts it anywhere
else, and `init --dry-run` shows the choice first. A folder of that name that
already holds something else is never taken over: `init` stops and asks for
`--repo`. A git repository you name yourself, such as a fresh clone of an empty
GitHub repository, is used as it is. Until `init` has run, the hooks and every
other command leave all folders alone. `memsaga repo` prints its path, and the
commands below use it, so they work wherever yours is. Example
projects here live under `~/git` (for example `~/git/webapp`); read that as
the folder where you keep code.

From then on nothing else is needed. Every turn that changed something in
scope ends with a commit, and the terminal shows a one-line notice. Once the
data repository has an `origin` remote (the optional `gh repo create` line
above), pushes happen on their own, at most once every ten minutes and at the
end of a session; without one, the history stays on this machine. Without
`gh`, create an empty private repository on GitHub, then run
`git -C "$(memsaga repo)" remote add origin <its URL>` and
`git -C "$(memsaga repo)" push -u origin HEAD`.

**Keep the data repository private.** Memory holds whatever Claude learned
while working with you, which often includes details of your work. The default
scope leaves out `~/.claude.json` (where Claude Code keeps its login),
credentials and transcripts, and takes `settings.json` only if you ask. memsaga
does not scan what it copies for secrets, so keep credentials out of anything
you add to `files`, `dirs` or `extra_files`. To keep a project's memory out
entirely, for example client work, exclude it by its Claude Code directory
name. For `~/git/client` and every project below it:
`exclude_projects = ["*-git-client", "*-git-client-*"]`.

**To try it first on example data**, install memsaga, then run
`demo/setup.sh` from a clone of this repository. It builds a fake home directory under `/tmp` with a Claude Code
config, two projects' memory and a week of history (a bug fix, a memory
tidy-up, companion documents, a file added by hand, a restore), and never reads or writes your real
one; `source /tmp/memsaga-demo/env.sh` then points `memsaga` and `git` at it.
To look without installing anything, browse
[grimmerk/memsaga-example](https://github.com/grimmerk/memsaga-example): the
data repository that script builds, with its week of commits on GitHub.

## What you can do with it

Use cases 1 to 4 work right after the quick start (3 also needs
`memsaga install --skills`). 5 to 7 add files of your own. 8 to 10 are about
working in more than one place. 11 is one example of what a full history makes
safe to try.

### 1. See what Claude remembered, and when it changed

<img src="docs/assets/demo.gif" width="100%" alt="Terminal recording on example data: git log of a week of memsaga commits, each named after its session; the Session-Title and Session trailers of the commit that added a memory about the OAuth redirect; git log -S finding the session that removed a rule from CLAUDE.md and the restore that brought it back; the live CLAUDE.md with the rule again.">

Every memory file, skill and your global `CLAUDE.md` gets a normal git history,
in the data repository:

```sh
cd "$(memsaga repo)"
git log -p -- claude/CLAUDE.md
git log -p --follow -- claude/memory/HOME-git-webapp/MEMORY.md
```

Each commit says which session changed what, in which directory and account
(see [commit messages](#commit-messages)):

```
[fix login redirect] claude:memory:HOME-git-webapp

Event: Stop
Account: claude
Session: 08ad5ab3-28b7-4d51-bfeb-4cd32a95b22c
Cwd: /Users/me/git/webapp
Session-Title: fix login redirect after OAuth
```

The title and ID work in both directions:

- **From a change to the conversation.** The `Session:` line is the session ID.
  Its transcript is `~/.claude/projects/<slug>/<session>.jsonl`, and
  `claude --resume <session>`, run in the `Cwd:` directory, reopens it.
- **From a conversation to its changes.** Everything one session changed:
  `git -C "$(memsaga repo)" log -p --grep 'Session: <session>'`. By title, when
  you remember the topic but not the session:
  `git -C "$(memsaga repo)" log --grep 'Session-Title: .*login'`.

Claude Code deletes transcripts after 30 days by default; raise
`cleanupPeriodDays` in `settings.json` to keep them longer. The commits stay
either way.

Three more questions that git answers directly:

- **Why is this line in `CLAUDE.md`?**
  `git -C "$(memsaga repo)" blame claude/CLAUDE.md` gives the commit of each
  line, and that commit's `Session:` line gives the conversation.
- **When was a sentence added, or removed?**
  `git -C "$(memsaga repo)" log -S 'run the tests first' --oneline`
- **What changed this week?**
  `git -C "$(memsaga repo)" log --since=1.week --stat`

You also see changes as they happen. After a turn that committed, the terminal
shows one line such as `memsaga: 2 files committed (a1b2c3d)`, so a memory
Claude wrote on its own does not go unnoticed. Claude Code prints it as
`Stop says: memsaga: …`: the prefix is Claude Code's, naming the hook that runs
at the end of a turn (`Stop`). `reporter = false` turns the line off. For one
commit per edit instead of one per turn, set `granularity = "edit"`.

The repository is also easy to read on GitHub: `index.md` lists every project
with its real directory, and the memory files already use frontmatter and
`[[wikilinks]]`, so the repository opens as an Obsidian vault too.

### 2. Undo a bad edit or a memory clean-up

`memsaga restore` copies a file from history back to its live location. It
shows the diff first and asks before it writes:

```sh
memsaga restore claude/memory/HOME-git-webapp/notes.md --at HEAD~3
memsaga restore ~/.claude/CLAUDE.md --at a1b2c3d   # any git revision; the live path works too
memsaga restore --all --at a1b2c3d                 # every mirrored file as it was then
```

A deleted file comes back the same way. `--all` overwrites the files that
differ and never deletes anything.

### 3. Ask Claude about the history

`memsaga install --skills` adds the `memsaga-history` skill to each config
directory. Then questions such as "what did CLAUDE.md say about testing last
week?" or "when did this memory change, and why?" are answered from the
history: the skill knows the repository layout and the git commands, and it
restores a file only after you say yes.

### 4. Start with the history you already have

The first `memsaga init` reads your Claude Code session transcripts and replays
the edits they record as dated commits, so the repository starts with weeks of
history instead of a single snapshot. `memsaga init --dry-run` shows how much
can be replayed. (Transcripts only reach back as far as Claude Code keeps
them, 30 days by default.)

### 5. Version any file Claude edits for you

Claude often writes files outside its config directory: a plan in your home
directory, notes about a repository that you do not want to commit to it, a
config file it fixed. List each such file in `extra_files`, and memsaga commits
every change to it at the end of the turn, whoever made it:

```toml
extra_files = ["~/notes/plan.md", "~/.config/starship.toml"]
```

The copy is stored under `extra/` by its real path, is never deleted by
memsaga, and gets edits made elsewhere like any other file (use case 10). For a
group of files, make a folder under the config directory (for example
`~/.claude/drafts/`) and add it to `dirs`. The same goes for hand-written
Claude Code mods: add `"mods"` and their source is versioned and synced like
your skills, and add `"mods/*/.claude-plugin/types"` (the folder the mod engine
regenerates) to `exclude_paths`. Setting either key replaces its default list,
so start from the defaults in [configuration](#configuration) and append:

```toml
dirs = ["skills", "commands", "agents", "agent-memory", "rules", "output-styles",
        "themes", "workflows", "hooks", "scripts", "drafts", "mods"]
exclude_paths = ["skills/synced", "skills/.trash", "mods/*/.claude-plugin/types"]
```

Whole folders outside the config directory are not supported yet
([#20](https://github.com/grimmerk/memsaga/issues/20)); until then, list their
files one by one, or keep the folder under the config directory and add it to
`dirs`, as above. A folder listed in `extra_files` is skipped, and
`memsaga status` and `memsaga config` say so.

### 6. Add your own files to the repository

The data repository is a normal git repository, so it can hold anything else
you want versioned next to your agent's files: reference notes, prompts you
reuse, a log of decisions. Put files at the top level, outside the account
folders such as `claude/` (for example in a `manual/` folder). The next sweep
commits them, or commit them yourself; memsaga never deletes what it did not
copy. You can also commit and push from another machine or edit in GitHub's
web editor. An edit to a mirrored file made in the repository, say to
`claude/CLAUDE.md`, is not overwritten: it goes to the live file (use case 10).

### 7. Keep notes beside a repository without committing them

Some documents belong with a repository but must not be committed to it: a
translation of its docs, your annotated copy, a draft written before the source
exists, a note about the repository as a whole. Ask Claude for one in any
language, and the `companion-docs` skill puts it at
`<data repository>/companion/<repo>/<path>`, keyed by the source file's path
(a note goes under `companion/<repo>/_notes/`), so it is versioned and synced
with the rest of your memory. Before Claude summarises or translates a
document, the skill has it check for an existing companion. `memsaga companions
[repo]`, or `/companions` in a session, lists them with the status and source
recorded in each one's frontmatter. That frontmatter uses the fields of the
[Open Knowledge Format](https://github.com/GoogleCloudPlatform/open-knowledge-format)
(OKF, a Google Cloud specification for markdown knowledge files): `type`,
`title` and `description`, a translation's source file and commit in
`sources`, who wrote it in `generated`, and `status` (`draft`, `stable` or
`deprecated`), so any tool that reads OKF can read a companion. Memory files, skills and `CLAUDE.md` keep their own formats. The
example repository has
[three of them](https://github.com/grimmerk/memsaga-example/tree/main/companion/webapp).

### 8. Two Claude Code accounts

If you run more than one account, each with its own config directory (for
example `~/.claude` and `~/.claude-work` through `CLAUDE_CONFIG_DIR`), list
each as a root. Each gets its own subtree in the repository (`claude/`,
`claude-work/`), and each commit's `Account:` line says which account made the
change. Files the second account shares through symlinks (a common pattern for
`CLAUDE.md` and `skills/`) are stored once, as relative symlinks.

[CodeV](https://github.com/grimmerk/codev), a macOS launcher for Claude Code
accounts, keeps a list of accounts. When your config names no roots, memsaga
reads that list if it exists, and otherwise uses `~/.claude`.

### 9. Two machines

Clone the data repository on the second machine and restore from it (the steps
are in [setting up another machine](#setting-up-another-machine)). After that
both machines commit and push. A push that GitHub rejects because the other
machine pushed first merges the other machine's commits and pushes again, and
their edits reach your live files as described in use case 10.

Everything in scope travels this way, not only memory: a skill, hook or script
written on one machine, and a hand-written Claude Code mod once its folder is in
`dirs`, is on the other machine's disk after its next prompt there. Your global
`CLAUDE.md` stays the same on both.

A project shares its memory between machines when it sits at the same path
relative to the home directory on both, for example `~/git/webapp`, even when
the user names differ (see [memory directory names](#memory-directory-names)).

### 10. Edit from anywhere

You can fix a memory file in GitHub's web editor, on a phone, on another
machine, or by hand in the local clone. memsaga does not overwrite such an edit.
It brings it to the live file:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/sync-sequence-dark.png">
  <img src="docs/assets/sync-sequence-light.png" width="100%" alt="Sequence: you edit and commit a memory file in GitHub's web editor. At the next session start or prompt, the memsaga hooks ask GitHub whether origin moved (at most once a minute); it did, so they merge origin into the data repository, write the new version to the live file (merging if both sides changed), and send Claude a notice with the changed lines. Claude reads the file again before using it.">
</picture>

A real session (example data): just before it started, `deploys.md` was edited
on GitHub to say production deploys every Friday. Asked how webapp deploys,
Claude reads the file again and answers from the new text.

<img src="docs/assets/sync-demo.gif" width="100%" alt="Claude Code session on example data: the user asks how webapp deploys to production; Claude reads deploys.md and answers that production deploys every Friday from a git tag, the text edited on GitHub just before the session; the memsaga Stop notice reports the commit.">

1. At session start and at each prompt, a hook asks GitHub whether the
   repository moved. This takes about half a second and happens at most once a
   minute per machine.
2. If it moved, memsaga merges it. A file that changed only in the repository
   is written to the live file. If both sides changed different lines, the
   texts are merged. Edits to the same lines are left alone and reported.
3. The model gets a short notice with the changed lines, so it does not answer
   from the copy it loaded earlier.
4. Before a Write or Edit, the hook asks GitHub again unless it asked in the
   last ten seconds, and pulls a change it finds first. A Write that would
   replace text that just arrived is refused, and Claude is told to read the
   file again; a change that cannot be merged is refused and left to you. A
   Read uses the last answer and refreshes it in the background, so an edit
   made elsewhere within the last minute may reach a read only at the next
   prompt.

All of this happens inside Claude Code sessions: a turn that changed something
ends with a commit. With an `origin` remote, the end of a turn also pushes, at
most once per `push_min_interval_secs` (`0` turns these pushes off), and the
end of a session pushes when `push_on_session_end` is on (the default). Run it
by hand when no session will do it for you:

- **Before you use a machine that has been idle**, or before editing its live
  files outside Claude Code: `memsaga sync` (both directions) or
  `memsaga pull` (only inbound).
- **To push right away**, instead of waiting for the throttle or the end of
  the session, or when those pushes are turned off: `memsaga push`.
- **When a sync reports a conflict** (both sides changed the same lines):
  `memsaga sync --take live <path>` or `--take repo <path>`, or edit the
  live file and run `memsaga sync` again.
- **When setting up another machine**: see
  [Setting up another machine](#setting-up-another-machine).

Details: [two-way sync](#two-way-sync).

### 11. Let Claude reorganise its memory, safely

Memory grows by small additions: facts repeat across files, old ones go stale,
a `MEMORY.md` index drifts from the files it lists. Asking Claude to clean
that up is risky when the only copy is the live one. With every version kept,
it becomes a routine you can run boldly, a "dream" pass over what was learned:

1. Now and then, by hand or on a schedule, start a session with a prompt such
   as "read every file under `~/.claude/projects/*/memory/`, merge duplicates,
   drop what is no longer true, and keep `MEMORY.md` in step with the files".
2. Its edits land as ordinary commits, marked with that session's ID and title
   (use case 1). Review them like any change:
   `git -C "$(memsaga repo)" log -p --grep 'Session: <id>'`.
3. Keep what helps. Undo the rest one file at a time with `memsaga restore`, or
   everything with `memsaga restore --all --at <commit before the pass>`.
   `--all` never deletes, so a file the pass created stays until you remove it.

The same holds for any bold edit: a rewrite of `CLAUDE.md`, a skills refactor,
a new hook. Try it, read the diff, keep it or roll it back.

## What memsaga does not cover, and what to use instead

| What | Why memsaga leaves it | What to use instead |
|---|---|---|
| Session transcripts | Large, and mostly the content of your work rather than configuration | Raise `cleanupPeriodDays` in `settings.json` so Claude Code keeps them longer. To search or archive them, use a transcript indexer such as [funes](https://github.com/huggingface/funes) or [fable](https://github.com/grooverLab/fable) |
| Plugins installed from a marketplace | They are packages you can install again, not files you wrote | Install them again; with `settings.json` mirrored (opt-in, `files`), its `enabledPlugins` list records which ones |
| Hand-written Claude Code mods | Not in the default scope | Add `"mods"` to `dirs` (use case 5) to version and sync them; give a mod you share or develop seriously its own git repository too, since one commit per turn is a backup, not a development history |
| Credentials, OAuth tokens, `~/.claude.json` | Secrets and account state must never enter a repository | Your system keychain or a password manager |
| `settings.json` keys that change all the time (`model`, `effortLevel`) | Claude Code rewrites them on every `/model` and `/effort` | Nothing: memsaga leaves them out of the repository and keeps each machine's own values (`settings_ignore_keys`) |

## How it works

- **Hooks.** `memsaga install` registers memsaga in each config directory's
  `settings.json`. `PostToolUse` marks a turn that wrote a file, `Stop` copies
  and commits at the end of the turn, and `SessionEnd` pushes. `SessionStart`,
  `UserPromptSubmit` and `PreToolUse` bring in edits made elsewhere (use case 10).
- **Mirror.** The repository holds copies. Claude Code keeps reading and
  writing its own files in place; memsaga writes live files only when you
  restore, or when an edit made elsewhere arrives.
- **Cost.** A turn that changed nothing in scope costs a check of modification
  times. A turn that did costs a copy and a commit, well under a second, and
  runs in the background. Asking GitHub whether the repository moved costs
  about half a second, at most once a minute.
- **Safety.** One lock serialises concurrent sessions. A hook never fails a
  turn: errors go to a log and to `memsaga status`.

More: [`docs/architecture.md`](docs/architecture.md).

## When something looks wrong

- `memsaga status` shows the last sweep, the push state (a failed or deferred
  push, and why), files waiting to come in from elsewhere, and a missing config
  file. It also lists where this machine differs from the repository: files
  it never had (kept, not deleted), a folder where the repository has a link
  (nothing under it is mirrored, so nothing is written through the link),
  folders the repository has that this machine's config does not mirror, and
  scripts that `settings.json` runs but this machine lacks.
- A hook never stops a turn. Its errors go to
  `<data repository>/.git/memsaga/memsaga.log`,
  and every sweep, push and merge to `events.jsonl` next to it.
- `memsaga diff` shows what the next sweep would commit; `memsaga sweep`
  commits now.
- An edit from elsewhere that conflicts with yours waits, and `memsaga sync`
  reports it. Pick a side with `memsaga sync --take live <path>` or
  `--take repo <path>`, or edit the live file and run `memsaga sync` again.
- To stop, `memsaga uninstall` removes the hooks. The repository and its
  history stay.
- Questions and ideas go to
  [GitHub Discussions](https://github.com/grimmerk/memsaga/discussions); a bug
  goes to [Issues](https://github.com/grimmerk/memsaga/issues).

## Reference

### Commands

| Command | What it does |
|---|---|
| `memsaga status [--json]` | Last sweep, pending dirty marker, push state (including a failing or deferred push), a missing config file, where this machine differs from the repository (files it never had, a folder where the repository has a link, folders its config does not mirror, missing hook scripts), recent events; `--json` prints the same as one object for scripts and tools. Read-only |
| `memsaga log` | Recent commits |
| `memsaga diff [--stat]` | What the next sweep would commit (refreshes the mirror, commits nothing) |
| `memsaga restore <path> [--at <rev>] [-y]` | Copy one file from history back to its live location, diff first |
| `memsaga restore --all --at <rev> [--prune]` | Every mirrored (in-scope) file of every root as it was at `<rev>`: overwrites what differs, never deletes, recreates links as they are (never writes through one), keeps the live `settings_ignore_keys`; shows the diffs, one confirmation. Lists live files in scope that `<rev>` does not have; `--prune` moves them to `<data repository>/.git/memsaga/stale/<time>/` instead of leaving them. Before asking, also names folders the repository has that this machine's config does not mirror, and scripts `settings.json` runs that this machine lacks |
| `memsaga sweep` | Mirror and commit now |
| `memsaga push` | Push to `origin` now (pushes also happen on `Stop`, throttled, and on `SessionEnd`); a push rejected because origin moved merges origin first |
| `memsaga sync [--take live\|repo <path>]` | Both directions: commit live changes, merge origin, bring repository-side edits to the live files, commit, push |
| `memsaga pull [--check] [--path <file>]` | Inbound only (what the hooks run); `--check` asks whether origin moved, throttled across sessions |
| `memsaga paths` | The memory name → real directory map |
| `memsaga config` | The effective configuration |
| `memsaga repo` | The data repository's path and nothing else, for commands such as `cd "$(memsaga repo)"` or `open "$(memsaga repo)"`; fails before `init` |
| `memsaga companions [repo]` | List companion documents (status, path, source); `/companions` in a session prints the same |
| `memsaga install [--skills]` | Register the hooks in each config directory; `--skills` also writes the bundled skills (`memsaga-history`, `companion-docs`, `companions`) |
| `memsaga uninstall` | Remove the hooks |

Paths for `restore` can be repository-relative (`claude/memory/<name>/x.md`)
or the live path. A shorter command name is one alias away:
`alias mem=memsaga`.

### What is mirrored

Per config directory (root), by default, what you write there that Claude Code
reads ([its directory reference](https://code.claude.com/docs/en/claude-directory)):
`CLAUDE.md`, `keybindings.json`, `skills/`, `commands/`, `agents/`,
`agent-memory/`, `rules/`, `output-styles/`, `themes/`, `workflows/`, and every
non-empty `projects/<slug>/memory/`. Also `hooks/` and `scripts/`, which Claude
Code does not read itself but which hold what `settings.json` points at by
convention. A folder you add yourself, such as `drafts/` or hand-written
`mods/`, goes in `dirs`. `settings.json` is opt-in
(`files = ["CLAUDE.md", "keybindings.json", "settings.json"]`). Never:
transcripts, `~/.claude.json`, caches, `file-history`, plugins, and Claude
Code's own synced skills under `skills/synced/` and `skills/.trash/`.

Repository layout:

```
<data repository>/
  claude/CLAUDE.md
  claude/skills/…  claude/hooks/…  claude/scripts/…
  claude/memory/<name>/{MEMORY.md, *.md}   <name>: see the next section
  claude-work/…                      second account; shared files stay symlinks
  index.md                           generated table of every project
  paths.json                         memory name → real working directory (~ for home)
  memsaga/config.toml                this config file, mirrored automatically
  extra/home/…  extra/root/…         extra_files, by their real path
  companion/  manual/  …             yours: committed, never deleted by the tool
```

### Memory directory names

A project's memory directory is named after its path. Claude Code's own name
(the "slug") is the absolute path with every non-alphanumeric character
replaced by `-` (`/Users/me/git/webapp` → `-Users-me-git-webapp`). In the
repository, a project under the home directory drops the home and starts with
`HOME` (`HOME-git-webapp`), so machines with different homes (another user
name, Linux's `/home/...`) share one directory per project. Anything outside
home keeps Claude Code's name.

One exception: if another directory on the machine encodes like a path under
home (`/Users/me-old` next to `/Users/me`), memsaga cannot tell them apart and
keeps Claude Code's names on that machine. Names from before this rule are
renamed by the next sweep, as one commit of renames, and older revisions stay
restorable by the new name. Before 2026-09-18 memory was stored at
`<label>/projects/<slug>/memory/`; the first sweep after upgrading moves it in
one commit, and `restore --at` still reads older revisions from the old path.

### Configuration

`~/.config/memsaga/config.toml`. Every key has a default, and `memsaga config`
prints the effective values.

```toml
repo = "~/git/memories"          # where init put it (example); `memsaga repo` prints yours
files = ["CLAUDE.md", "keybindings.json"]
dirs = ["skills", "commands", "agents", "agent-memory", "rules", "output-styles", "themes", "workflows", "hooks", "scripts"]
memory = true
exclude = ["*.codev-bak*", "*.codev-tmp-*", ".DS_Store", "__pycache__"]
settings_ignore_keys = ["model", "effortLevel", "modelSettings"]  # left out of the mirrored settings.json: Claude Code rewrites them on every /model and /effort
exclude_projects = []            # slug globs
exclude_paths = ["skills/synced", "skills/.trash"] # root-relative subtrees
extra_files = []                  # single files outside every root, e.g. "~/.config/codev/accounts.json"
granularity = "turn"             # or "edit": one commit per Write/Edit
reporter = true                  # one-line notice in the terminal after a commit
push_min_interval_secs = 600     # at most one push per interval on Stop; a throttled push is deferred to the end of the window, not dropped. 0 disables pushes on Stop
push_on_session_end = true

[[roots]]
path = "~/.claude"
label = "claude"
```

Without `[[roots]]`, memsaga reads CodeV's `~/.config/codev/accounts.json` if
present, else uses `~/.claude`. Without `repo`, it uses an existing memsaga
repository called `memories` in one of the folders the quick start names, else
the place `init` would choose. If the config file disappears while the
repository still holds its mirrored copy, memsaga runs on its defaults: fewer
files are mirrored, and copies of files that drop out of scope are left in the
repository. The reporter prints a warning with the restore command after every
turn until the file is back.

### Two-way sync

An edit made outside the live files (in GitHub's web editor, on another
machine, or by hand in the local clone) is not overwritten by the next sweep.
It waits on a list (`memsaga status` shows it) until it is brought to the live
file. When only the repository changed, its version is written; when both
sides changed different lines (or made the same change), the texts are merged;
conflicting edits to the same lines are left alone and reported, and
`memsaga sync --take live|repo <path>` settles one by picking a side. A file
deleted in the repository is deleted live when the live file was unchanged.
The other way round, a live file counts as deleted only on a machine that has
had it: a file the repository has but this machine never had (a fresh clone, a
folder another machine added to the config) is kept, and `memsaga status`
lists it.

The three synchronous hooks of use case 10 ask origin at most once a minute per
machine, and before a Write or Edit at most every ten seconds. Design and
measurements: [`docs/two-way-sync.md`](docs/two-way-sync.md).

### Commit messages

The subject is `[<session title>] <areas>`, for example
`[fix login redirect] claude:memory:HOME-git-webapp`. The title is cut to 32
characters there so the subject stays within 72; the `Session-Title:` line keeps
the whole title. The other lines: `Event:` (the hook or command), `Account:`
(from `CLAUDE_CONFIG_DIR`), `Session:` (the session ID), `Cwd:` and `Agent:`.
The prompt text is never copied into a commit.

### Updating

1. Run the install line from the [quick start](#quick-start) again: it
   downloads the latest release and replaces `~/.cargo/bin/memsaga`. If you
   built from source, add `--force` to the `cargo install` line.
2. `memsaga install`, with `--skills` if you installed the bundled skills.
   It is safe to run again: it updates memsaga's own hook entries in place and
   adds any hook the new version needs. `--skills` overwrites the bundled
   skills; if you edited one, the old text is in the history.

`memsaga --version` shows what is installed. Running sessions need no restart:
every hook starts a new process, so the next one uses the new binary. If
several machines share one data repository, update them together.

### Setting up another machine

Tested with the same user name and the same folder layout on both machines:

1. Install Claude Code and sign in once, so `~/.claude` exists. Create the
   directory of every other account in your config (for example
   `~/.claude-work`); a root that does not exist is skipped, with a count.
2. Clone the data repository to the same path as on the first machine
   (`memsaga repo` prints it there), and copy its config. Do **not** run
   `memsaga init` on the new machine: it would commit its empty state on top
   of your history.

   ```sh
   D=~/git/memories        # the path memsaga repo prints on the first machine
   git clone git@github.com:<you>/memories.git "$D"   # or: gh repo clone <you>/memories "$D"
   mkdir -p ~/.config/memsaga && cp "$D/memsaga/config.toml" ~/.config/memsaga/config.toml
   ```
3. Install memsaga (the install line in the [quick start](#quick-start)).
4. `memsaga restore --all --at HEAD`: read the list and the diffs, answer
   `n`; then run it again with `--yes`. Files get their content, executable
   bits and commit time back; links that share files between accounts are
   recreated, and so are links to another tool's folder, as they are, even
   when that folder is not on this machine yet. Nothing is deleted. Before
   it asks, the restore also names folders the repository has that your
   config does not mirror (copy the config, step 2), and scripts that
   `settings.json` runs but this machine lacks.
5. Verify: `memsaga sweep --event verify` should report `no changes`; then
   `memsaga diff --stat` should print `no changes since the last commit`. If
   the verify sweep commits anything, look at it with
   `git -C "$(memsaga repo)" show --stat` before you start Claude Code there,
   whose hooks push, and see the next paragraph.

**A machine that ran memsaga before** (an older data repository or config):
move the old data repository aside rather than deleting it, then do steps 2
to 5. Live files from before may still be there after the other machine
removed or moved them: a memory file moved to another project, a folder
that became a link. `restore --all` lists them as not in the repository and
leaves them, since on any other machine such a file may simply be new; add
`--prune` to move them to `<data repository>/.git/memsaga/stale/<time>/`
(nothing is deleted), which also lets a link come back where an old folder
stood. Until then a folder where the repository has a link is left alone by
every sweep, and `memsaga status` lists it.

If `settings.json` is in your `files` (it is not by default), restoring it
also registers the hooks, so new sessions on that machine start committing at
once; otherwise run `memsaga install` after the restore. The hooks may run
scripts from outside memsaga's scope (another tool's status hook, for
example); `restore` and `memsaga status` name any that this machine lacks.
Mirror such a script from the machine that has it (`files` or `dirs` for a
path in a config directory, `extra_files` for one elsewhere), or install the
tool that writes it. Do not remove the hook on the machine that lacks it: two-way
sync would remove it on every machine.

## Tests

`cargo test` runs behaviour tests that drive the real binary against fake
config directories in a temporary directory (`crates/memsaga/tests/e2e.rs`),
plus unit tests for pure logic. CI runs them on Ubuntu and macOS with
`cargo fmt --check` and `cargo clippy -D warnings`.

## Roadmap

Plans, not promises:

- More sources: other coding agents that keep memory or instruction files on
  disk (Codex first: its opt-in memories under `~/.codex/memories/`,
  `AGENTS.md` and skills), whole folders outside the config directory
  ([#20](https://github.com/grimmerk/memsaga/issues/20)), and remote knowledge
  bases, such as a server hosting a tree of markdown documents.
- A Claude Code mod that shows the sync state in the status line, and later
  runs the hook work inside Claude Code instead of a new process per event.
- Folder aliases for machines whose project folders differ below home
  (`~/code` on one, `~/git` on the other).
- Files that belong to one machine only, kept per machine in the repository.

## The name

mem, for memory, and saga, from Old Norse for "story" or "history", the word
for the medieval Icelandic narratives that followed people and events across
generations. A repository of memory that keeps every version is the story of
how that memory came to be, one change at a time.

## License

MIT
