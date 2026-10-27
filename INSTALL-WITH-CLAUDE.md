# Install memsaga with Claude Code

You can ask Claude Code to install memsaga for you. Start Claude Code in any
folder and paste this prompt:

```text
Install memsaga for me. Follow
https://raw.githubusercontent.com/grimmerk/memsaga/main/INSTALL-WITH-CLAUDE.md
and ask me before each step that changes my machine.
```

Claude reads this file, checks what you have, and walks you through the same
steps as [Install by hand](README.md#install-by-hand). It asks before each step,
and you can stop at any point. Some steps also show a permission prompt from
Claude Code itself; [What to expect](#what-to-expect-from-permission-prompts)
says which ones.

The same file covers updating and uninstalling: start the prompt with "Update
memsaga for me" or "Uninstall memsaga for me" instead.

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
command -v memsaga && memsaga --version || echo "memsaga: not installed"
command -v gh && gh auth status || echo "gh: not installed or not logged in"
```

The `|| echo` keeps a missing tool from showing up as a failed command; tell
the user that "not installed" is an expected answer on a new machine.

- memsaga runs on macOS (Apple silicon and Intel) and Linux (x86_64, arm64).
  On anything else, stop and say so.
- git is required. If it is missing, tell the user how to install it for
  their system and stop.
- If memsaga is already installed, ask whether the user wants an update
  ([If the user asks to update](#if-the-user-asks-to-update)) or to go on
  with the steps below. A data repository alone does not mean the hooks,
  the skills or the GitHub backup are set up; step 3 checks for one.
- `gh` is optional. It only makes step 5 shorter.

### Step 2: install the binary

Ask first. The usual way downloads a prebuilt binary into `~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/grimmerk/memsaga/releases/latest/download/memsaga-installer.sh | sh
```

Tell the user that this runs an install script from the memsaga GitHub
releases page, and offer the alternatives from the Quick start if they would
rather not: `cargo install memsaga --locked` (needs Rust 1.88 or later), or
downloading the archive from the Releases page themselves.

The installer may add `~/.cargo/bin` to the shell's startup file, but the
shell that Claude Code runs commands in does not pick that up. Until the user
opens a new terminal, call the binary by its full path, `~/.cargo/bin/memsaga`,
in every step below. Check with `~/.cargo/bin/memsaga --version`. If the
binary is somewhere else (an install from before, or an archive the user
unpacked), use the path `command -v memsaga` printed, or the one the user
gives, wherever this file writes `~/.cargo/bin/memsaga`.

### Step 3: create the data repository

First, without asking:

```sh
~/.cargo/bin/memsaga init --dry-run
```

If the first line of the output says the location is "set in the config file"
or "an existing memsaga repository", there is a memsaga config or data
repository on this machine already. Do not run `init` yet; ask the user which
case it is:

- memsaga ran here before and was removed: skip to step 4, since the data
  repository and its history are still there. If `~/.config/memsaga/config.toml`
  is gone (the README's uninstall steps delete it), restore it first from the
  copy in the data repository:
  `mkdir -p ~/.config/memsaga && git -C "<data repository path>" show HEAD:memsaga/config.toml > ~/.config/memsaga/config.toml.tmp && mv ~/.config/memsaga/config.toml.tmp ~/.config/memsaga/config.toml`;
- this is a second machine that shares a data repository with another one:
  follow "Setting up another machine" in the README instead of the rest of
  this file, because `init` would commit this machine's state on top of the
  shared history;
- the user wrote the config file for this install: go on.

Otherwise, show the user where the data repository would go and how much would
be imported. The dry run lists each Claude Code config directory it found
(`root …` lines). Look for others with `ls -d ~/.claude*/` (folders only),
and ask whether the user runs Claude Code with `CLAUDE_CONFIG_DIR` set to
another folder, which may be anywhere (`echo "$CLAUDE_CONFIG_DIR"` shows it
for the current session). When this session has `CLAUDE_CONFIG_DIR` set,
memsaga 0.1.2 and later list that folder as a root and `init` records it, when
the folder exists and the config file has no `roots` key (not even
`roots = []`); memsaga 0.1.1 does not, and no version finds the folder of an account this session
does not use. If a folder the user uses is not listed, and above all
if no `root` line is listed at all, ask first, then add it to
`~/.config/memsaga/config.toml` as below, run the dry run again, and only
then go on:

```toml
[[roots]]
path = "<the folder>"
label = "claude"          # a second one gets its own label, e.g. "claude-work"
```

Listing roots replaces the automatic search, so list `~/.claude` too when the
user also uses it.

Ask whether any project must stay out of the history, for example client
work. If so, add it to `exclude_projects` in `~/.config/memsaga/config.toml`
now, keeping any entries already there, by the Claude Code directory name the
dry run lists (for `~/git/client` and the projects below it, add
`"*-git-client"` and `"*-git-client-*"` to the list): what `init` commits
stays in the history.

Ask whether that place and folder name are fine, or where they want it
instead; any path works, and the folder does not have to be called
`memories`. Then run one of:

```sh
~/.cargo/bin/memsaga init
~/.cargo/bin/memsaga init --repo "<path the user chose>"
```

Quote every path you fill in, here and below, since it may contain spaces.
If `init` stops because the folder already holds something else, ask the user
for another path and use `--repo`. Never delete or move their folder. (When
the user's own config file sets `repo`, `init` refuses a different `--repo`;
change the line in `~/.config/memsaga/config.toml` instead.)

### Step 4: register the hooks

Ask first. Explain that this adds memsaga's hooks to Claude Code's
`settings.json` in each config directory (usually only `~/.claude`), so that
every turn which changes Claude Code's memory or configuration ends with a
commit. Ask whether they also want the bundled skills, which let Claude answer
questions about the history (`--skills`). `--skills` writes the folders
`memsaga-history`, `companion-docs` and `companions` under `skills/`; if one of
them already exists and memsaga did not write it, ask before overwriting it.
Then run one of:

```sh
~/.cargo/bin/memsaga install
~/.cargo/bin/memsaga install --skills
```

### Step 5 (optional): a private GitHub repository

Ask whether the user wants the history backed up to GitHub, and pushed
automatically (with the default settings, at most once every ten minutes
and at the end of a session). If yes, ask for the repository name
(`memories` is a good default). Get the data repository's path with
`~/.cargo/bin/memsaga repo` and write that path into the commands below,
rather than `"$(memsaga repo)"`:
Claude Code cannot check a command with `$(…)` before it runs, so it always
asks, and offers no "don't ask again" choice. With `gh` logged in:

```sh
gh repo create <name> --private --source "<data repository path>" --push
```

Without `gh`, ask the user to create an empty private repository on
github.com and give you its URL. Before pushing, check that it is not public:
an unauthenticated request for a private repository answers 404. Push only on
404. On anything else (200 means public; a rate limit, an error or no answer
proves nothing), stop and ask the user to confirm the repository is private.

```sh
curl -s -o /dev/null -w '%{http_code}\n' https://api.github.com/repos/<owner>/<name>
git -C "<data repository path>" remote add origin "<URL>"
git -C "<data repository path>" push -u origin HEAD
```

If the push fails for lack of credentials, the user has to set up git access
to GitHub themselves; do not try to fix it for them.

If `memsaga status` in step 6 then says `last push never`, that is expected:
memsaga records only its own pushes, and its first one runs when your reply
ends.

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

### If the user asks to update

Ask first, then:

1. Run the install script of step 2 again; it replaces `~/.cargo/bin/memsaga`
   with the latest release. If memsaga was built with cargo, add `--force` to
   the `cargo install` line instead. If the user installed an archive by
   hand, ask them to replace that binary with the new release's.
2. Run `~/.cargo/bin/memsaga install` again, with `--skills` if the folder
   `memsaga-history` exists under `skills/` in the config directory. This
   updates memsaga's hook entries and adds any the new version needs;
   `--skills` overwrites the bundled skills (an edited one keeps its old text
   in the history).
3. Show `~/.cargo/bin/memsaga --version`. Running Claude Code sessions need no
   restart. If several machines share the data repository, tell the user to
   update them all.

Never run `memsaga init` for an update.

### If the user asks to uninstall

Ask before each part, and never delete the data repository or the GitHub
repository on your own: they are the history. List these, in this order, and
let the user choose:

1. `memsaga uninstall` removes the hooks from each config directory. The
   backup `settings.json.memsaga-bak` that `memsaga install` took stays;
   offer to delete it.
2. The bundled skills, if `--skills` was used: the folders `memsaga-history`,
   `companion-docs` and `companions` under `skills/` in each config directory
   (skip a config directory whose `skills` is a link to another one's).
   The README's "Uninstalling" section has the commands.
3. The binary (`~/.cargo/bin/memsaga`, or wherever `command -v memsaga`
   points; `cargo uninstall memsaga` if it was built with cargo), and
   `~/.config/memsaga/` (the config file and the installer's receipt).
4. The line `. "$HOME/.cargo/env"` that the install script added to the
   shell startup files (on macOS usually `~/.zshenv` and `~/.profile`; check
   `~/.zshrc`, `~/.bashrc` and `~/.bash_profile` too), and `~/.cargo`: only if
   nothing else the user has is installed in `~/.cargo`.
5. The data repository (`memsaga repo` prints where), and the GitHub
   repository if there is one. Suggest keeping both.

---

## What to expect from permission prompts

Tested on 2026-10-25 with Claude Code 2.1.310 and Sonnet 5.5, with a fresh
Claude Code configuration on macOS, using the prompt at the top of this file.
`gh` was replaced by a stand-in, so no GitHub repository was created.

**Auto mode** (the default permission mode on Pro, Max and Team plans): no
prompt and nothing blocked. Claude asked its own questions before each step,
as this file tells it to, and every command ran once the user said yes:
the install script, `memsaga init`, `memsaga install` (which edits
`~/.claude/settings.json`) and `gh repo create`.

**Manual mode** (Claude Code asks before each command): 11 permission prompts,
one for nearly every command, on top of Claude's own questions. They were for:

| Step | Commands that asked |
|---|---|
| Reading this file | `curl` of this file |
| 1. Check | `git --version`, `memsaga --version`, `gh auth status` (`uname` did not ask) |
| 2. Install | the `curl … \| sh` install script, then `memsaga --version` |
| 3. Data repository | `memsaga init --dry-run`, `memsaga init` |
| 4. Hooks | `memsaga install --skills` |
| 5. GitHub | `gh repo create …` |
| 6. Check | `memsaga status` |

To answer fewer of them, choose "Yes, and don't ask again for
`~/.cargo/bin/memsaga *`" when step 2 asks about `~/.cargo/bin/memsaga
--version`; that covers every memsaga command after it.

The hooks work in the same session as soon as step 4 has run: the reply that
ran `memsaga install` ended with a memsaga notice, and its own changes (the
bundled skills) were committed. No restart is needed.

A second run, on a Mac in daily use with memsaga moved aside (Opus 5.5, auto
mode, no `gh`), gave the same result for install and uninstall: nothing was
blocked.
