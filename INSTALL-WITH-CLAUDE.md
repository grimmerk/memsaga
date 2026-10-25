# Install memsaga with Claude Code

You can ask Claude Code to install memsaga for you. Start Claude Code in any
folder and paste this prompt:

```text
Install memsaga for me. Follow
https://raw.githubusercontent.com/grimmerk/memsaga/main/INSTALL-WITH-CLAUDE.md
and ask me before each step that changes my machine.
```

Claude reads this file, checks what you have, and walks you through the same
steps as the [Quick start](README.md#quick-start). It asks before each step,
and you can stop at any point. Some steps also show a permission prompt from
Claude Code itself; [What to expect](#what-to-expect-from-permission-prompts)
says which ones.

The rest of this file is written for Claude.

---

## Instructions for Claude

You are installing memsaga, a tool that keeps the history of Claude Code's
memory and configuration in a git repository the user owns. Read the
[README](https://github.com/grimmerk/memsaga#readme) if the user asks what it
does.

Rules for the whole install:

- **Ask before every step that changes the machine** (steps 2 to 5 below).
  Say in one or two plain sentences what the step does and what it changes,
  then wait for a yes. Checking commands (step 1, `--dry-run`, `status`) need
  no question.
- **Never type a password, token or key** for the user, and never run a login
  command (`gh auth login`, `ssh-keygen`) on their behalf. If one is needed,
  ask the user to run it themselves by typing `! <command>` in the prompt.
- **The GitHub repository must be private.** Memory holds details of the
  user's work. Never create a public one, and ask for the name.
- **If a command is blocked or denied** by Claude Code's permissions, do not
  look for another way to run it. Tell the user which command it was and give
  them two choices: approve it (in auto mode: `/permissions`, then "Recently
  denied", then `r` on that command) or run it themselves with `! <command>`.
- Run one command per tool call, so each prompt the user sees is about one
  thing.

### Step 1: check what is there

Run these and summarise the result in a few lines:

```sh
uname -sm
git --version
command -v memsaga && memsaga --version
command -v gh && gh auth status
```

- memsaga runs on macOS (Apple silicon and Intel) and Linux (x86_64, arm64).
  On anything else, stop and say so.
- git is required. If it is missing, tell the user how to install it for
  their system and stop.
- If memsaga is already installed, ask whether to update it (step 2) or skip
  to step 3. If `memsaga status` shows a data repository already, this machine
  is set up: stop and show the status. A second machine that shares an
  existing data repository needs "Setting up another machine" in the README,
  not `memsaga init`; ask the user which case this is.
- `gh` is optional. It only makes step 5 shorter.

### Step 2: install the binary

Ask first. The usual way downloads a prebuilt binary into `~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/grimmerk/memsaga/releases/latest/download/memsaga-installer.sh | sh
```

Tell the user that this runs an install script from the memsaga GitHub
releases page, and offer the alternatives from the Quick start if they would
rather not: `cargo install memsaga --locked` (needs Rust 1.88 or later), or
downloading the archive from the Releases page themselves.

The installer may add `~/.cargo/bin` to the shell's startup file, but the
shell that Claude Code runs commands in does not pick that up. Until the user
opens a new terminal, call the binary by its full path, `~/.cargo/bin/memsaga`,
in every step below. Check with `~/.cargo/bin/memsaga --version`.

### Step 3: create the data repository

First, without asking:

```sh
~/.cargo/bin/memsaga init --dry-run
```

Show the user where the data repository would go and how much would be
imported, then ask whether that place is fine. Then run one of:

```sh
~/.cargo/bin/memsaga init
~/.cargo/bin/memsaga init --repo <path the user chose>
```

If `init` stops because the folder already holds something else, ask the user
for another path and use `--repo`. Never delete or move their folder.

### Step 4: register the hooks

Ask first. Explain that this adds memsaga's hooks to Claude Code's
`settings.json` in each config directory (usually only `~/.claude`), so that
every turn which changes Claude Code's memory or configuration ends with a
commit. Ask whether they also want the bundled skills, which let Claude answer
questions about the history (`--skills`):

```sh
~/.cargo/bin/memsaga install
~/.cargo/bin/memsaga install --skills
```

### Step 5 (optional): a private GitHub repository

Ask whether the user wants the history backed up to GitHub, and pushed
automatically. If yes, ask for the repository name (`memories` is a good
default). With `gh` logged in:

```sh
gh repo create <name> --private --source "$(~/.cargo/bin/memsaga repo)" --push
```

Without `gh`, ask the user to create an empty private repository on
github.com, give you its URL, and then run:

```sh
git -C "$(~/.cargo/bin/memsaga repo)" remote add origin <URL>
git -C "$(~/.cargo/bin/memsaga repo)" push -u origin HEAD
```

If the push fails for lack of credentials, the user has to set up git access
to GitHub themselves; do not try to fix it for them.

### Step 6: check and finish

```sh
~/.cargo/bin/memsaga status
```

Then tell the user, in a few lines:

- where the data repository is (`memsaga repo`);
- that from now on every turn that changes Claude Code's memory or
  configuration ends with a commit, with a one-line notice in the terminal;
- whether pushes to GitHub are set up;
- that `git -C "$(memsaga repo)" log` shows the history, and the README's
  "What you can do with it" has more;
- to open a new terminal so that `memsaga` works without the full path.

---

## What to expect from permission prompts

<!-- filled in from the test runs -->
