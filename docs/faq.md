# memsaga FAQ

Questions people ask before they install memsaga, mostly about doing without
it. For what memsaga adds over the simplest setup, see
[Do you need memsaga?](../README.md#do-you-need-memsaga) in the README.

- [Why not skip Claude Code's memory and keep my own in a repository?](#why-not-skip-claude-codes-memory-and-keep-my-own-in-a-repository)
  Replacing the built-in memory.
- [How do I give the built-in memory a history myself?](#how-do-i-give-the-built-in-memory-a-history-myself)
  Keeping the built-in memory and versioning it, without memsaga.
- [Does it slow Claude Code down?](#does-it-slow-claude-code-down)
  What each hook costs, measured.

## Why not skip Claude Code's memory and keep my own in a repository?

You can, at two levels:

- **Simple:** tell Claude in your `CLAUDE.md` to write what it learns into a
  folder of your own, such as `~/notes/`, and to commit there.
- **Further:** package those rules as a skill that says when and how to save a
  memory, writes it into the repository and commits. Skills are an open format
  that Claude Code, Codex and Cursor all read, so one memory could serve all
  three.

**What you gain**

- The folder is yours: the original rather than a copy, organised the way you
  like, easy to browse.
- At the simple level, nothing to install. With a skill, one memory shared by
  several coding agents (memsaga covers Claude Code only today).

**What it costs, at both levels**

- Whether a memory gets saved and committed depends on the model following the
  instruction or choosing to use the skill. A hook that commits makes it
  certain, but each agent has its own hook system.
- Memories are read only when the agent is told to read them, through an `@`
  import in `CLAUDE.md` or an instruction to look there. The built-in memory is
  loaded at the start of every session.
- Claude Code keeps writing its own memory as well, unless you turn it off
  (`"autoMemoryEnabled": false` in settings, or the toggle in `/memory`), so
  the same kind of fact can end up in two places.
- You maintain a memory system of your own, and improvements to the built-in
  one do not reach you.
- Several sessions or agents committing to one repository at once need
  locking, or git refuses some of the commits.
- `CLAUDE.md`, skills and hooks still have no history.

| | Your own memory, by instruction or skill | memsaga |
|---|---|---|
| Changes how the agent remembers | Yes, it replaces the built-in memory | No, the agent works as before |
| Is every change recorded | When the model follows the instruction or uses the skill | Always: a hook runs after every reply |
| Edits made by you or by other tools | Missed unless they go through the same path | Recorded too |
| Several agents | Possible with a skill | Claude Code today; Codex is on the [roadmap](../README.md#roadmap) |
| What has history | Memory | All of `~/.claude` that you write, plus files you list |

Tools that take this path already exist and go much further, with search,
indexes and hand-offs between agents. One example is
[ai-memory](https://github.com/akitaonrails/ai-memory), which replaces each
agent's own memory with a shared, git-backed markdown wiki for twenty or more
coding agents. The two can be used together: such a tool keeps the memory, in
a repository that already has its own history, and memsaga keeps the history of
everything else in `~/.claude`, such as `CLAUDE.md`, skills and hooks.

## How do I give the built-in memory a history myself?

This keeps Claude Code's built-in memory exactly as it is and adds version
control to it, without memsaga: git in Claude Code's own folder, and a hook that
commits after every reply. It is the do-it-yourself setup that the README
describes for one account on one computer.

What this setup does not handle, in short (each is explained in
[Do you need memsaga?](../README.md#do-you-need-memsaga) in the README):

- **The files Claude is using become the git work tree**, so a `git checkout`,
  a `git clean` or a merge conflict in `~/.claude` changes them. Know this one
  before you start.
- **One account, one computer.** A second account needs a second repository.
  An edit made on GitHub, on a phone or on another computer comes back only
  through a `git pull` that writes straight into the files in use, and a
  conflict leaves git's markers in a file Claude reads.
- **Files outside `~/.claude`** stay out of this history.
- **Commits do not say which conversation made them**, unless you extend the
  hook.
- **History starts the day you set it up**; nothing earlier is rebuilt.
- **You keep the allowlist up to date** when Claude Code adds a folder you want
  kept.

1. Turn the folder into a git repository:

   ```sh
   git init ~/.claude
   ```

2. Create `~/.claude/.gitignore` as an allowlist. `/*` leaves out everything at
   the top level, and each `!` line lets one thing back in. Inside `projects/`,
   only the `memory/` folders are let back in, so conversation transcripts stay
   out:

   ```gitignore
   /*
   !/.gitignore
   !/CLAUDE.md
   !/skills/
   !/projects/
   /projects/*/*
   !/projects/*/memory/
   ```

   Add a line such as `!/agents/`, `!/commands/` or `!/hooks/` for anything
   else you keep there. Prefer an allowlist to a list of what to leave out:
   with the second kind, a folder a later Claude Code version adds, or a
   credentials file (`~/.claude/.credentials.json`, used on Linux, and on macOS
   when the Keychain cannot be written), is committed until you notice.

3. Add a `Stop` hook to `~/.claude/settings.json` that commits whatever
   changed, and does nothing when nothing did:

   ```json
   {
     "hooks": {
       "Stop": [
         {
           "hooks": [
             {
               "type": "command",
               "command": "git -C ~/.claude add -A && git -C ~/.claude diff --cached --quiet || git -C ~/.claude commit -q -m 'claude: turn'"
             }
           ]
         }
       ]
     }
   }
   ```

   If `settings.json` already has a `hooks` key, add the `Stop` entry inside it
   rather than a second `hooks`.

`git -C ~/.claude log -p` then shows every change. To push to a private GitHub
repository, add a remote and run `git push` yourself, or add it to the hook.

## Does it slow Claude Code down?

Barely. memsaga is a small compiled program (written in Rust); each hook starts
it, does one check and exits.

- **Most tool calls:** about 5 ms when the file is not one memsaga keeps.
- **End of a reply with nothing to commit:** about 30–45 ms.
- **End of a reply that commits:** runs in the background, so the reply is not
  held up.
- **Pushing to GitHub:** in the background, at most once every ten minutes by
  default (`push_min_interval_secs`).
- **Checking GitHub for edits made elsewhere (two-way sync):** about half a
  second, at most once a minute, at the start of a session or a prompt; before
  Claude writes a file memsaga keeps, at most every ten seconds. Without a
  GitHub remote, memsaga uses no network at all.

The numbers were measured on a busy machine;
[`docs/architecture.md`](architecture.md#measured-again-2026-10-15-and-how-to-repeat-it)
says how to repeat them.
