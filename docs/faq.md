# memsaga FAQ

Questions people ask before they install memsaga, mostly about doing without
it. For what memsaga adds over the simplest setup, see
[Do you need memsaga?](../README.md#do-you-need-memsaga) in the README.

- [Why not keep my own notes in a repository and skip Claude Code's memory?](#why-not-keep-my-own-notes-in-a-repository-and-skip-claude-codes-memory)
- [Why not replace the built-in memory with a skill that writes to a repository?](#why-not-replace-the-built-in-memory-with-a-skill-that-writes-to-a-repository)
- [How do I set up the do-it-yourself version?](#how-do-i-set-up-the-do-it-yourself-version)

## Why not keep my own notes in a repository and skip Claude Code's memory?

You can. Tell Claude in your `CLAUDE.md` to write what it learns into a folder
of your own, such as `~/notes/`, and to commit there.

**What you gain**

- The folder is yours: visible, organised the way you like, easy to browse.
- Nothing to install.

**What it costs**

- Commits happen only when Claude remembers to make them, unless you also add a
  hook that commits. Instructions in `CLAUDE.md` are followed most of the time,
  not every time.
- Claude reads the notes only when told to, through an `@` import in
  `CLAUDE.md` or an instruction to look there. Built-in memory is loaded for
  you at the start of every session.
- Claude Code keeps writing its own memory as well, unless you turn it off
  (`"autoMemoryEnabled": false` in settings, or the toggle in `/memory`), so
  the same kind of fact can end up in two places.
- Your `CLAUDE.md`, skills and hooks still have no history.

memsaga takes the other path: it leaves the built-in memory as it is, and gives
it and the rest of `~/.claude` a history.

## Why not replace the built-in memory with a skill that writes to a repository?

This is the previous question taken further: a skill that tells the agent when
and how to save a memory, writes it straight into a repository you chose, and
commits. Skills are an open format that Claude Code, Codex and Cursor all read,
so one memory could serve all three.

**What you gain**

- One memory shared by several coding agents. memsaga covers Claude Code only
  today.
- The repository is the original, not a copy, so there is nothing to mirror
  and nothing to sync back.
- You decide the layout and the writing rules.

**What it costs**

- Whether a memory gets saved depends on the model choosing to use the skill.
  The built-in memory is driven by Claude Code's own instructions, tuned by its
  makers.
- A commit that the skill asks for is again an instruction the model may skip.
  A hook that commits makes it certain, but each agent has its own hook system.
- Memories are read only when the agent is told to read them. The built-in
  memory is loaded at the start of every session.
- You now maintain a memory system of your own, and improvements to the
  built-in one do not reach you.
- Several sessions or agents committing to one repository at once need
  locking, or git refuses some of the commits.
- `CLAUDE.md`, skills and hooks still have no history.

| | A memory skill | memsaga |
|---|---|---|
| Changes how the agent remembers | Yes, it replaces the built-in memory | No, the agent works as before |
| Is every change recorded | When the model uses the skill | Always: a hook runs after every reply |
| Edits made by you or by other tools | Missed unless they go through the skill | Recorded too |
| Several agents | Yes, by design | Claude Code today; Codex is on the [roadmap](../README.md#roadmap) |
| What has history | Memory | All of `~/.claude` that you write, plus files you list |

Tools that take this path already exist and go much further, with search,
indexes and hand-offs between agents. One example is
[ai-memory](https://github.com/akitaonrails/ai-memory), which replaces each
agent's own memory with a shared, git-backed markdown wiki for twenty or more
coding agents. The two can be used together: such a tool keeps the memory, in
a repository that already has its own history, and memsaga keeps the history of
the rest of `~/.claude`.

## How do I set up the do-it-yourself version?

This is the setup the README describes for one account on one computer: git
in Claude Code's own folder, and a hook that commits after every reply. Read
[what it does not handle](../README.md#do-you-need-memsaga) first; above all,
the files Claude is using become the git work tree, so a `git checkout` or
`git clean` run in `~/.claude` changes them.

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
