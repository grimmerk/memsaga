//! Where this machine differs from the repository in ways the mirror does not
//! settle by itself. `memsaga status` and `restore --all` print the same
//! findings (grimmerk/memsaga#23, #25); none of them is ever fixed silently.

use crate::config::{Config, Root};
use crate::git::Git;
use crate::roots::{self, Entry, Scope};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Top-level entries the repository holds at `rev` under a root that this
/// machine's config does not mirror, as `<label>/<name>`: a config older or
/// narrower than the one that wrote the repository.
pub fn uncovered(cfg: &Config, rev: &str) -> Vec<String> {
    let git = Git::new(&cfg.repo);
    let mut out = Vec::new();
    for root in &cfg.roots {
        let Ok(list) = git.run_ok(&[
            "ls-tree",
            "-z",
            "--name-only",
            rev,
            &format!("{}/", root.label),
        ]) else {
            continue;
        };
        for path in list.split('\0').filter(|s| !s.is_empty()) {
            let name = path.rsplit('/').next().unwrap_or(path);
            let covered = cfg.files.iter().any(|f| f == name)
                || cfg.dirs.iter().any(|d| d == name)
                // `projects/` is the memory layout before 2026-09-18.
                || (cfg.memory && (name == roots::MEMORY_DIR || name == "projects"));
            if !covered {
                out.push(format!("{}/{name}", root.label));
            }
        }
    }
    out
}

/// A script that a root's `settings.json` runs and this machine does not have.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MissingScript {
    /// The live `settings.json`.
    pub settings: PathBuf,
    /// The path as written in the command.
    pub script: String,
}

/// Hook and status-line commands in each root's `settings.json` that name a
/// file under the home directory or the root which does not exist. A restored
/// `settings.json` carries its registrations, not the scripts they run
/// (grimmerk/memsaga#25). A warning only: memsaga cannot tell whether a
/// script is meant to exist on every machine.
pub fn missing_scripts(cfg: &Config) -> Vec<MissingScript> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for root in &cfg.roots {
        let settings = root.path.join("settings.json");
        let Some(json) = std::fs::read_to_string(&settings)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        else {
            continue;
        };
        let mut commands = Vec::new();
        if let Some(events) = json.get("hooks").and_then(|h| h.as_object()) {
            for groups in events.values().filter_map(|v| v.as_array()) {
                for hooks in groups
                    .iter()
                    .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
                {
                    commands.extend(
                        hooks
                            .iter()
                            .filter_map(|h| h.get("command").and_then(|c| c.as_str())),
                    );
                }
            }
        }
        if let Some(c) = json
            .get("statusLine")
            .and_then(|s| s.get("command"))
            .and_then(|c| c.as_str())
        {
            commands.push(c);
        }
        let mut seen = BTreeSet::new();
        for token in commands.iter().flat_map(|c| c.split_whitespace()) {
            let token = token.trim_matches(|c: char| "\"'();&|".contains(c));
            let Some(path) = expand(token, &home, root) else {
                continue;
            };
            // Under the home or the root (which may live elsewhere); a link
            // whose target is gone cannot run either, so follow it.
            if (path.starts_with(&home) || path.starts_with(&root.path))
                && !path.exists()
                && seen.insert(token.to_string())
            {
                out.push(MissingScript {
                    settings: settings.clone(),
                    script: token.to_string(),
                });
            }
        }
    }
    out
}

/// A command word as a path, when it is one: absolute, or starting with `~/`,
/// `$HOME/`, `${HOME}/` or `$CLAUDE_CONFIG_DIR/` (the root it runs in).
fn expand(token: &str, home: &Path, root: &Root) -> Option<PathBuf> {
    for (prefix, base) in [
        ("~/", home),
        ("$HOME/", home),
        ("${HOME}/", home),
        ("$CLAUDE_CONFIG_DIR/", root.path.as_path()),
        ("${CLAUDE_CONFIG_DIR}/", root.path.as_path()),
    ] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return Some(base.join(rest));
        }
    }
    token.starts_with('/').then(|| PathBuf::from(token))
}

/// A live file in scope that the repository does not have at a revision.
#[derive(Debug, Clone)]
pub struct Stale {
    pub repo_rel: PathBuf,
    pub live: PathBuf,
}

/// Live files and links in scope that the repository does not have at `rev`.
/// On a machine being set up from the repository these are leftovers (a
/// memory file another machine moved, a folder that became a link there);
/// `restore --all` lists them and `--prune` moves them aside
/// (grimmerk/memsaga#23). Anywhere else they may simply be new, so nothing
/// else uses this list.
pub fn stale(cfg: &Config, rev: &str) -> Result<Vec<Stale>> {
    let git = Git::new(&cfg.repo);
    let tree: BTreeSet<String> = git.ls_tree(rev)?.into_iter().map(|(_, _, p)| p).collect();
    let scope = Scope::new(cfg);
    let mut out = Vec::new();
    for root in cfg.roots.iter().filter(|r| r.path.is_dir()) {
        for entry in scope.entries(root) {
            let src = entry.source(root);
            let prefix = match &entry {
                Entry::File { rel } | Entry::Dir { rel } => PathBuf::from(rel),
                Entry::Memory { slug } => PathBuf::from("projects").join(slug).join("memory"),
            };
            let mut found = Vec::new();
            let is_link = src
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());
            // An entry that is a link (the second account's `skills`) is one
            // item: walkdir would follow it into the folder it points at.
            if matches!(entry, Entry::File { .. }) || is_link {
                found.push(prefix.clone());
            } else {
                for item in WalkDir::new(&src).follow_links(false).min_depth(1) {
                    let item = item?;
                    if item.file_type().is_dir() {
                        continue;
                    }
                    let rel = item.path().strip_prefix(&src)?;
                    if rel
                        .components()
                        .any(|c| scope.excluded_name(&c.as_os_str().to_string_lossy()))
                        || scope.excluded_path(&prefix.join(rel))
                    {
                        continue;
                    }
                    found.push(prefix.join(rel));
                }
            }
            for live_rel in found {
                let repo_rel = Path::new(&root.label).join(roots::repo_rel_of_live(&live_rel));
                // An older revision may hold it under an older layout, which
                // restore reads as the same file.
                let held = std::iter::once(repo_rel.clone())
                    .chain(roots::legacy_repo_paths(&repo_rel))
                    .any(|p| tree.contains(&p.to_string_lossy().to_string()));
                if !held {
                    out.push(Stale {
                        repo_rel,
                        live: root.path.join(&live_rel),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Live paths where `rev` has a link and this machine a folder holding only
/// files in `stale`: after `prune` the folder is empty, and `restore --all`
/// puts the link back there.
pub fn links_freed(cfg: &Config, rev: &str, stale: &[Stale]) -> Result<Vec<PathBuf>> {
    let moved: BTreeSet<&Path> = stale.iter().map(|s| s.live.as_path()).collect();
    let mut out = Vec::new();
    for (mode, _, path) in Git::new(&cfg.repo).ls_tree(rev)? {
        if mode != "120000" {
            continue;
        }
        let Ok(loc) = crate::restore::locate(cfg, &path) else {
            continue;
        };
        let is_dir = loc.live.symlink_metadata().is_ok_and(|m| m.is_dir());
        if !is_dir || crate::restore::is_empty_dir(&loc.live) {
            continue; // not a folder, or already planned as a link
        }
        let only_stale = WalkDir::new(&loc.live)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_type().is_dir())
            .all(|e| moved.contains(e.path()));
        if only_stale {
            out.push(loc.live);
        }
    }
    Ok(out)
}

/// Move stale files to `<state>/stale/<time>/<repository path>`, never
/// deleting them, then remove the folders the move left empty (up to the
/// entry they were in). Returns the folder they went to.
pub fn prune(cfg: &Config, stale: &[Stale]) -> Result<PathBuf> {
    let dest = cfg
        .state_dir()
        .join("stale")
        .join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
    for s in stale {
        let root = cfg
            .roots
            .iter()
            .find(|r| s.live.starts_with(&r.path))
            .with_context(|| format!("{} is not under a root", s.live.display()))?;
        // The folder it sits in, reached without passing through a link:
        // otherwise the move would take a file from wherever the link points.
        let parent = s.live.parent().context("no parent")?;
        let direct = std::fs::canonicalize(&root.path)?.join(parent.strip_prefix(&root.path)?);
        if std::fs::canonicalize(parent)? != direct {
            anyhow::bail!("{} is reached through a link; not moved", s.live.display());
        }
        let to = dest.join(&s.repo_rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        if std::fs::rename(&s.live, &to).is_err() {
            // Another volume: copy (a link stays a link), then remove.
            let meta = std::fs::symlink_metadata(&s.live)?;
            if meta.file_type().is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(&s.live)?, &to)?;
            } else {
                std::fs::copy(&s.live, &to)
                    .with_context(|| format!("moving {} aside", s.live.display()))?;
            }
            std::fs::remove_file(&s.live)?;
        }
        // Never the entry itself (a configured folder, a memory folder):
        // a missing entry would read as deleted.
        let mut dir = s.live.parent();
        while let Some(d) = dir {
            let depth = d
                .strip_prefix(&root.path)
                .map_or(0, |r| r.components().count());
            let entry_level = if d
                .strip_prefix(&root.path)
                .is_ok_and(|r| r.starts_with("projects"))
            {
                3
            } else {
                1
            };
            if depth <= entry_level || std::fs::remove_dir(d).is_err() {
                break; // the entry, or not empty
            }
            dir = d.parent();
        }
    }
    Ok(dest)
}
