---
name: companion-docs
description: Where to put a translated or annotated companion of a document that must not be committed to its own repository (for example a Traditional Chinese version of an English doc in a company repo). Use when the user asks for a translation, a 中文版 / 繁中版 / 繁中 / zh-tw version, a companion or side-by-side version of a document, when reading a document that may already have one, or when the user mentions a companion, or memsaga's data repo by any name (the memories repo, memsaga KB, memsaga memory, memsaga data, memsaga memories) and its companion/ directory. Not for a request to reply in Chinese (用繁中跟我對話): that is about the conversation language, not a document.
---

# Companion docs

A companion is a second file about the same document: a translation, a
commentary, a summary in another language. It must not land in the source
repository, so it lives in the memsaga data repo, which commits it automatically
at the end of the turn.

## Path convention

```
<memsaga repo>/companion/<repo name>/<path inside that repo>.<lang>.md
```

- `<memsaga repo>` is the `repo` value of `memsaga config` (default `~/git/memories`).
- `<repo name>` is the source repository's directory name under `~/git`
  (`webapp`, `docs`), not the GitHub full name.
- `<path inside that repo>` keeps the source's relative path and extension,
  then adds the language tag: `docs/api.md` -> `docs/api.zh-tw.md`.

Example: the companion of `~/git/webapp/docs/api.md` is
`~/git/memories/companion/webapp/docs/api.zh-tw.md`.

### When the source does not exist yet

A document may be drafted in the companion language first (a design written
in Traditional Chinese to discuss before any English version exists) in a
repository where it cannot be committed. Use the same path rule with the path
the source *will* have once it is written, so the companion is found the day
the source appears: `~/git/memories/companion/webapp/docs/new-design.zh-tw.md`
for a future `~/git/webapp/docs/new-design.md`. Set `status: draft` and leave
`sources` empty; fill `sources` in when the source is committed. Ask the user
where the source will live if it is not obvious.

A draft has no source file for a later session to read, so also add one line
to that repository's auto-memory index (`MEMORY.md` under
`~/.claude/projects/<slug>/memory/`): the topic, the companion's path, and
"no source yet". The pointer is what a fresh session in that repository
loads; the content stays in the companion. Remove the line once the source
is committed, since the path rule finds the companion from then on.

## Frontmatter

Every companion starts with YAML frontmatter following the Open Knowledge
Format core fields plus provenance, so the source and its version are
recorded and any OKF consumer can read it:

```yaml
---
type: Translation            # or Commentary, Summary
title: api (zh-TW)
description: Traditional Chinese translation of webapp docs/api.md
sources:
  - resource: ~/git/webapp/docs/api.md
    last_modified: 2026-09-18T09:12:00Z     # commit time of the source revision translated
    commit: 3f9a85f6                         # source repo commit the companion was made from
generated:
  by: claude-code/claude-fable-5-1
  at: 2026-09-18T10:05:00Z
status: stable                               # draft | stable | deprecated
---
```

Get the source commit with `git -C <source repo> log -1 --format='%h %cI' -- <path>`.

## When writing

1. Resolve the companion path from the source path as above; create parent
   directories as needed.
2. Write the frontmatter, then the content. Keep headings parallel to the
   source so the two files can be read side by side.
3. Do not add anything to the source repository, its `.gitignore`, or its
   docs; the companion is complete on its own.
4. Nothing else to do: memsaga commits `companion/` at the next `Stop`.

## When reading a document

Before summarising or translating a file under `~/git/<repo>/`, check whether
its companion exists at the path above and mention it to the user if it does.
If the source has moved, `sources[].resource` in the companion's frontmatter
names the old path; search by file name under `companion/<repo>/` and
suggest updating the frontmatter.

## Updating

When the source changes, regenerate or patch the companion and bump
`sources[].commit`, `sources[].last_modified` and `generated.at`. Mark a
companion that is no longer maintained with `status: deprecated` rather than
deleting it; history stays in git either way.
