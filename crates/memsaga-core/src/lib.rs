//! memsaga-core: everything memsaga does, as a library.
//!
//! The mental model, in one paragraph: a set of *roots* (Claude Code config
//! directories such as `~/.claude`) contain files we want under version control
//! (global CLAUDE.md, per-project auto-memory, skills). memsaga never edits those
//! live files. It copies an allowlisted subset into a *mirror* git repo, one
//! subtree per root, and commits whenever something changed. Claude Code hooks
//! call `hook` with a JSON event on stdin; `sweep` is the copy-and-commit step;
//! `init` does the first import (optionally preceded by a history `backfill`
//! replayed from session transcripts).
//!
//! Module map (read in this order):
//! - `config`   what to mirror, from a TOML file with env overrides
//! - `roots`    enumerating the allowlisted paths of one root, in-scope tests
//! - `slug`     Claude Code's project-directory encoding and the reverse map
//! - `mirror`   copying a root into its repo subtree (rsync-like, one way)
//! - `git`      thin wrapper around the system `git` binary
//! - `state`    small files under `.git/memsaga/`: lock, markers, events log
//! - `status`   the read-only state report behind `status` and `status --json`
//! - `sweep`    the orchestration: lock, mirror, index, commit, events
//! - `hook`     parsing a hook event and deciding what to do
//! - `index`    `index.md` and `paths.json` generation
//! - `install`  registering the hooks in a root's settings.json
//! - `restore`  copying one file from history back into a root (the only write)
//! - `skills`   the SKILL.md files bundled with the binary
//! - `backfill` replaying transcripts into dated commits

pub mod backfill;
pub mod companion;
pub mod config;
pub mod git;
pub mod hook;
pub mod index;
pub mod install;
pub mod mirror;
pub mod restore;
pub mod roots;
pub mod skills;
pub mod slug;
pub mod state;
pub mod status;
pub mod sweep;

pub use config::Config;
