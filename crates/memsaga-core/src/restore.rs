//! `restore`: copy one file from the repo's history back into a live config
//! directory. This is the only place memsaga ever writes into a root, and it
//! does so only for a path inside the allowlist, only after showing the diff,
//! and only with explicit confirmation (`--yes` or an interactive "y").
//!
//! Also the path mapping both directions: `claude/memory/<slug>/x.md` in the
//! repo is `<root of label "claude">/projects/<slug>/memory/x.md` live. The
//! pre-2026-09-18 repo path (`claude/projects/<slug>/memory/x.md`) is still
//! accepted as input and is where old revisions are read from.

use crate::config::{expand_tilde, Config};
use crate::git::Git;
use crate::roots::{self, Scope};
use crate::state::{self, Event, State};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A file named either way, resolved to both of its locations.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    /// Path inside the repo, e.g. `claude/CLAUDE.md`.
    pub repo_rel: PathBuf,
    /// Absolute live path.
    pub live: PathBuf,
}

/// Accepts a repo-relative path (`claude/...`), an absolute live path, or a
/// `~/...` live path. Refuses anything outside the mirrored scope.
pub fn locate(cfg: &Config, input: &str) -> Result<Located> {
    let scope = Scope::new(cfg);
    let p = expand_tilde(input);
    if p.is_absolute() {
        let root = scope.contains(&p).with_context(|| {
            format!(
                "{} is not inside any mirrored root or allowlist",
                p.display()
            )
        })?;
        let rel = p.strip_prefix(&root.path)?;
        return Ok(Located {
            repo_rel: Path::new(&root.label).join(roots::repo_rel_of_live(rel)),
            live: p,
        });
    }
    let mut comps = p.components();
    let label = comps
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default();
    let root = cfg
        .roots
        .iter()
        .find(|r| r.label == label)
        .with_context(|| format!("no mirrored root is labelled {label:?} (path {input})"))?;
    let rest: PathBuf = comps.collect();
    // Accept both layouts as input; the repo path is always the current one.
    let live_rel = roots::live_rel_of_repo(&rest);
    let live = root.path.join(&live_rel);
    if scope.contains(&live).is_none() {
        bail!(
            "{} is not inside the allowlist of root {}",
            live.display(),
            root.label
        );
    }
    Ok(Located {
        repo_rel: Path::new(&root.label).join(roots::repo_rel_of_live(&live_rel)),
        live,
    })
}

/// One file of a whole-tree restore.
#[derive(Debug)]
pub struct PlannedFile {
    pub loc: Located,
    pub content: Vec<u8>,
}

/// What `restore --all` would do at `rev`: every blob under a root's subtree
/// whose content differs from the live file (or which is missing live).
/// Symlinks and paths outside the allowlist are counted and skipped; nothing
/// is ever deleted.
#[derive(Debug, Default)]
pub struct Plan {
    pub changed: Vec<PlannedFile>,
    pub identical: usize,
    pub symlinks: usize,
    pub skipped: usize,
    /// Roots (by label) whose directory does not exist on this machine, with
    /// how many files of theirs were skipped. A restore never creates a
    /// config directory: that is setting up an account, not restoring one.
    pub missing_roots: Vec<(String, usize)>,
}

pub fn plan_all(cfg: &Config, rev: &str) -> Result<Plan> {
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        bail!(
            "{} is not a git repository. On a new machine, clone the data repository there first (do not run `memsaga init`, which would commit this machine's empty state)",
            cfg.repo.display()
        );
    }
    let mut plan = Plan::default();
    for (mode, kind, path) in git.ls_tree(rev)? {
        let label = path.split('/').next().unwrap_or("");
        let root = match cfg.roots.iter().find(|r| r.label == label) {
            Some(r) => r,
            None => continue, // index.md, paths.json, companion/, extra/, memsaga/
        };
        if !root.path.is_dir() {
            match plan.missing_roots.iter_mut().find(|(l, _)| l == label) {
                Some((_, n)) => *n += 1,
                None => plan.missing_roots.push((label.to_string(), 1)),
            }
            continue;
        }
        if mode == "120000" {
            plan.symlinks += 1;
            continue;
        }
        if kind != "blob" {
            continue;
        }
        let loc = match locate(cfg, &path) {
            Ok(l) => l,
            Err(_) => {
                plan.skipped += 1;
                continue;
            }
        };
        // A live symlink (the second account's CLAUDE.md, a skill linked
        // elsewhere) is never written through, whatever the tree had.
        if std::fs::symlink_metadata(&loc.live)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            plan.symlinks += 1;
            continue;
        }
        let content = match content_at(cfg, &loc.repo_rel, rev)? {
            Some(c) => c,
            None => {
                plan.skipped += 1;
                continue;
            }
        };
        let content = with_live_ignored_keys(cfg, &loc, content);
        if std::fs::read(&loc.live).ok().as_deref() == Some(content.as_slice()) {
            plan.identical += 1;
        } else {
            plan.changed.push(PlannedFile { loc, content });
        }
    }
    Ok(plan)
}

/// The mirror stores `settings.json` without `settings_ignore_keys`
/// (`model`, `effortLevel`, ...). Restoring it must not delete those from
/// the live file: carry the live values of the ignored keys into the
/// candidate. Anything that is not a JSON object is returned unchanged.
pub fn with_live_ignored_keys(cfg: &Config, loc: &Located, candidate: Vec<u8>) -> Vec<u8> {
    let is_settings = loc
        .repo_rel
        .file_name()
        .map(|n| n == "settings.json")
        .unwrap_or(false);
    if !is_settings || cfg.settings_ignore_keys.is_empty() {
        return candidate;
    }
    let live: serde_json::Value = match std::fs::read_to_string(&loc.live)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(v) => v,
        None => return candidate,
    };
    let mut cand: serde_json::Value = match serde_json::from_slice(&candidate) {
        Ok(v) => v,
        Err(_) => return candidate,
    };
    let (Some(live_obj), Some(cand_obj)) = (live.as_object(), cand.as_object_mut()) else {
        return candidate;
    };
    for k in &cfg.settings_ignore_keys {
        if let Some(v) = live_obj.get(k) {
            cand_obj.insert(k.clone(), v.clone());
        }
    }
    match serde_json::to_string_pretty(&cand) {
        Ok(s) => (s + "\n").into_bytes(),
        Err(_) => candidate,
    }
}

/// Where a write to `live` would really land: the path with every symlink
/// resolved. The file, or any number of its parents, may not exist yet (a
/// restore into a deleted memory directory): the nearest existing ancestor
/// is resolved and the missing tail appended. Refuses a destination outside
/// every mirrored root, e.g. a skill directory that is a symlink into
/// `~/.agents/`.
pub fn verify_destination(cfg: &Config, live: &Path) -> Result<PathBuf> {
    let resolved = if live.exists() {
        std::fs::canonicalize(live)?
    } else {
        let mut existing = live.to_path_buf();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        while !existing.exists() {
            let name = existing
                .file_name()
                .context("live path has no existing ancestor")?
                .to_os_string();
            tail.push(name);
            existing = existing
                .parent()
                .context("live path has no existing ancestor")?
                .to_path_buf();
        }
        let mut p = std::fs::canonicalize(&existing)?;
        for name in tail.iter().rev() {
            p.push(name);
        }
        p
    };
    let inside = cfg.roots.iter().any(|r| {
        std::fs::canonicalize(&r.path)
            .map(|rp| resolved.starts_with(rp))
            .unwrap_or(false)
    });
    if !inside {
        bail!(
            "{} resolves to {}, which is outside every mirrored root; refusing to write through it",
            live.display(),
            resolved.display()
        );
    }
    Ok(resolved)
}

/// Content of `repo_rel` at `rev`, or None when it did not exist there.
/// A memory file absent at `rev` is also looked up at its pre-2026-09-18
/// path, so `--at` works across the layout change. A directory (tree) at
/// the path is an error, not content.
pub fn content_at(cfg: &Config, repo_rel: &Path, rev: &str) -> Result<Option<Vec<u8>>> {
    let git = Git::new(&cfg.repo);
    let mut candidates = vec![repo_rel.to_path_buf()];
    candidates.extend(roots::legacy_repo_path(repo_rel));
    for path in candidates {
        let spec = format!("{rev}:{}", path.to_string_lossy());
        match git.object_type(&spec)? {
            None => continue,
            Some(t) if t == "blob" => {}
            Some(t) => bail!("{spec} is a {t}, not a file"),
        }
        let out = git.run(&["show", &spec])?;
        return Ok(out.status.success().then_some(out.stdout));
    }
    Ok(None)
}

/// A unified diff between the live file (or nothing) and `candidate`.
pub fn diff_against_live(cfg: &Config, live: &Path, candidate: &[u8]) -> Result<String> {
    let state = State::new(cfg.state_dir());
    state.ensure()?;
    let tmp = state.marker(&format!("restore-candidate-{}", std::process::id()));
    std::fs::write(&tmp, candidate)?;
    let live_arg = if live.exists() {
        live.to_string_lossy().to_string()
    } else {
        "/dev/null".to_string()
    };
    let git = Git::new(&cfg.repo);
    // --no-index exits 1 when the files differ; that is not an error here.
    let out = git.run(&[
        "diff",
        "--no-index",
        "--color=never",
        &live_arg,
        &tmp.to_string_lossy(),
    ])?;
    let _ = std::fs::remove_file(&tmp);
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    // Make the header readable: the temp path is an implementation detail.
    text = text.replace(
        &tmp.to_string_lossy().to_string(),
        &format!("{} (restored)", live.display()),
    );
    Ok(text)
}

/// Write `candidate` to the live path and record the event. The caller has
/// already shown the diff and obtained confirmation.
pub fn write_live(cfg: &Config, loc: &Located, rev: &str, candidate: &[u8]) -> Result<()> {
    let state = State::new(cfg.state_dir());
    // Hold the sweep lock so a concurrent Stop never mirrors a half-written file,
    // and replace atomically for any other reader.
    let _guard = state
        .lock(std::time::Duration::from_secs(20))
        .context("sweep lock")?;
    write_one(cfg, &state, loc, rev, candidate)
}

/// `restore --all`: every file under one lock, so a concurrent Stop cannot
/// commit a half-restored tree between two files.
pub fn write_live_batch(cfg: &Config, files: &[PlannedFile], rev: &str) -> Result<usize> {
    let state = State::new(cfg.state_dir());
    let _guard = state
        .lock(std::time::Duration::from_secs(60))
        .context("sweep lock")?;
    for f in files {
        write_one(cfg, &state, &f.loc, rev, &f.content)?;
    }
    Ok(files.len())
}

fn write_one(
    cfg: &Config,
    state: &State,
    loc: &Located,
    rev: &str,
    candidate: &[u8],
) -> Result<()> {
    let dest = verify_destination(cfg, &loc.live)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_file_name(format!(
        ".{}.memsaga-restore-{}",
        dest.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, candidate).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).with_context(|| format!("writing {}", dest.display()))?;
    // The live file now differs from the mirror: make sure the next Stop commits it.
    state.write_dirty(&format!(
        "{} restore {} {}",
        state::now_rfc3339(),
        rev,
        loc.live.display()
    ))?;
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "restore".into(),
        event: "manual".into(),
        files: vec![loc.repo_rel.to_string_lossy().to_string()],
        sha: rev.to_string(),
        message: format!("restored {} from {rev}", loc.live.display()),
        ..Default::default()
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Root;

    fn cfg() -> Config {
        Config {
            roots: vec![
                Root {
                    path: PathBuf::from("/h/.claude"),
                    label: "claude".into(),
                },
                Root {
                    path: PathBuf::from("/h/.claude-work"),
                    label: "claude-work".into(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn locate_maps_both_directions_and_refuses_out_of_scope() {
        let c = cfg();
        let a = locate(&c, "claude/memory/-h-git-a/x.md").unwrap();
        assert_eq!(
            a.live,
            PathBuf::from("/h/.claude/projects/-h-git-a/memory/x.md")
        );
        // The pre-2026-09-18 repo path and the live path name the same file.
        let old = locate(&c, "claude/projects/-h-git-a/memory/x.md").unwrap();
        assert_eq!(old, a);
        let abs = locate(&c, "/h/.claude/projects/-h-git-a/memory/x.md").unwrap();
        assert_eq!(abs, a);
        assert_eq!(a.repo_rel, PathBuf::from("claude/memory/-h-git-a/x.md"));
        let b = locate(&c, "/h/.claude-work/CLAUDE.md").unwrap();
        assert_eq!(b.repo_rel, PathBuf::from("claude-work/CLAUDE.md"));
        assert!(
            locate(&c, "claude/settings.json").is_err(),
            "settings.json is not mirrored"
        );
        assert!(locate(&c, "/h/git/a/src/x.ts").is_err());
        assert!(locate(&c, "nope/CLAUDE.md").is_err());
    }
}
