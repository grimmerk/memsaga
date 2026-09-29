---
name: companions
description: List the companion documents (translations, annotated copies, drafts) memsaga keeps under its data repo's companion/ directory, optionally for one source repository. User-invoked as /companions [repo name]; the output is printed verbatim so the user can read it directly.
argument-hint: [repo dir name under ~/git, e.g. webapp; empty = all]
disable-model-invocation: true
---

Run this in one Bash call, replacing ARGS with: $ARGUMENTS (may be empty):

```bash
memsaga companions ARGS
```

Print the command's output verbatim inside one fenced code block and nothing
else: no summary, no reordering, no omitted lines. The user asked for the
list so they can read it themselves.

Each line is `<status>  <path under companion/>  <- <source>`; a note about
the repository shows `~ <what it relates to>` instead; `draft` with
`(no source yet)` means the source document has not been written. The path
rule and frontmatter are in the `companion-docs` skill.
