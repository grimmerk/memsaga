//! One-way copy of a root's allowlisted entries into the repo subtree.
//!
//! Semantics match `rsync -a --delete` for what we need: copy a file when the
//! destination is missing or differs in size or mtime, preserve the source
//! mtime on the copy (so an unchanged file is never re-hashed by git), keep
//! symlinks as symlinks, and delete destination files that no longer exist in
//! the source. Deletion is confined to the entry's own destination directory;
//! nothing outside a tool-owned subtree is ever touched. The live root is only
//! ever read.

use crate::config::Root;
use crate::roots::{Entry, Scope};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub copied: usize,
    pub deleted: usize,
    pub linked: usize,
}

impl Stats {
    fn add(&mut self, o: Stats) {
        self.copied += o.copied;
        self.deleted += o.deleted;
        self.linked += o.linked;
    }
}

/// Mirror every entry of `root` into `repo/<label>/`, then prune project
/// directories whose memory vanished or became excluded.
pub fn sync_root(scope: &Scope, root: &Root, repo: &Path) -> Result<Stats> {
    let mut stats = Stats::default();
    let entries = scope.entries(root);
    for e in &entries {
        stats.add(sync_entry(scope, root, e, repo)?);
    }
    // Files/dirs in the allowlist that disappeared from the root: remove copies.
    let base = repo.join(&root.label);
    for f in &scope.cfg.files {
        let src = root.path.join(f);
        if src.symlink_metadata().is_err() {
            let dst = base.join(f);
            if dst.symlink_metadata().is_ok() {
                remove_any(&dst)?;
                stats.deleted += 1;
            }
        }
    }
    for d in &scope.cfg.dirs {
        let src = root.path.join(d);
        if src.symlink_metadata().is_err() {
            let dst = base.join(d);
            if dst.symlink_metadata().is_ok() {
                remove_any(&dst)?;
                stats.deleted += 1;
            }
        }
    }
    stats.add(prune_projects(scope, root, repo, &entries)?);
    Ok(stats)
}

fn sync_entry(scope: &Scope, root: &Root, entry: &Entry, repo: &Path) -> Result<Stats> {
    let src = entry.source(root);
    let dst = entry.dest(repo, root);
    let mut stats = Stats::default();
    let meta = match src.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return Ok(stats),
    };
    if meta.file_type().is_symlink() {
        stats.linked += mirror_symlink(scope, &src, &dst, repo)?;
        return Ok(stats);
    }
    if meta.is_file() {
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        if copy_if_changed(&src, &dst)? {
            stats.copied += 1;
        }
        return Ok(stats);
    }
    if meta.is_dir() {
        // A destination that used to be a symlink must be replaced by a real dir.
        if dst.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            fs::remove_file(&dst)?;
        }
        let prefix = match entry {
            Entry::Dir { rel } => PathBuf::from(rel),
            Entry::Memory { slug } => PathBuf::from("projects").join(slug).join("memory"),
            Entry::File { rel } => PathBuf::from(rel),
        };
        stats.add(sync_dir(scope, &src, &dst, &prefix)?);
    }
    Ok(stats)
}

/// Recursive directory mirror with delete. `prefix` is `src` relative to its
/// root, so root-relative `exclude_paths` can be applied while walking.
pub fn sync_dir(scope: &Scope, src: &Path, dst: &Path, prefix: &Path) -> Result<Stats> {
    let mut stats = Stats::default();
    fs::create_dir_all(dst)?;
    let mut keep: BTreeSet<PathBuf> = BTreeSet::new();
    for item in WalkDir::new(src).follow_links(false).min_depth(1) {
        let item = item?;
        let name = item.file_name().to_string_lossy().to_string();
        if scope.excluded_name(&name) {
            if item.file_type().is_dir() {
                // walkdir has no skip-subtree on the iterator form we use; excluded
                // dirs are rare (.DS_Store is a file), so just skip entries below.
            }
            continue;
        }
        let rel = item.path().strip_prefix(src)?;
        if rel.components().any(|c| scope.excluded_name(&c.as_os_str().to_string_lossy())) {
            continue;
        }
        if scope.excluded_path(&prefix.join(rel)) {
            continue;
        }
        let target = dst.join(rel);
        keep.insert(target.clone());
        let ft = item.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            // A link that stays inside the mirrored tree keeps its relative text;
            // one that escapes it (e.g. skills/foo -> ../../.agents/skills/foo)
            // is rewritten to the absolute target so the copy is not dangling.
            let link = fs::read_link(item.path())?;
            let resolved = if link.is_absolute() { link.clone() } else { normalize(&item.path().parent().unwrap_or(src).join(&link)) };
            let link = if resolved.starts_with(src) { link } else { resolved };
            if replace_symlink(&target, &link)? {
                stats.linked += 1;
            }
        } else if ft.is_file() {
            if copy_if_changed(item.path(), &target)? {
                stats.copied += 1;
            }
        }
    }
    // Delete what is no longer in the source. Deepest first so dirs empty out.
    let mut extra: Vec<PathBuf> = WalkDir::new(dst)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .filter(|p| !keep.contains(p))
        .collect();
    extra.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in extra {
        if p.symlink_metadata().is_ok() {
            remove_any(&p)?;
            stats.deleted += 1;
        }
    }
    Ok(stats)
}

/// Copy when size or mtime differ; keep the source mtime on the copy.
fn copy_if_changed(src: &Path, dst: &Path) -> Result<bool> {
    let sm = fs::metadata(src)?;
    if let Ok(dm) = fs::symlink_metadata(dst) {
        if dm.is_file()
            && dm.len() == sm.len()
            && same_second(dm.modified().ok(), sm.modified().ok())
        {
            return Ok(false);
        }
        if !dm.is_file() {
            remove_any(dst)?;
        }
    }
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    fs::copy(src, dst).with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    if let Ok(t) = sm.modified() {
        let f = fs::OpenOptions::new().write(true).open(dst)?;
        let _ = f.set_modified(t);
    }
    Ok(true)
}

fn same_second(a: Option<std::time::SystemTime>, b: Option<std::time::SystemTime>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let sa = a.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let sb = b.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            sa == sb
        }
        _ => false,
    }
}

/// Mirror a top-level symlink. A target that points into another mirrored
/// root is rewritten as a relative link inside the repo (`../claude/CLAUDE.md`)
/// so the repo is self-contained; anything else keeps its absolute target.
fn mirror_symlink(scope: &Scope, src: &Path, dst: &Path, repo: &Path) -> Result<usize> {
    let target = fs::read_link(src)?;
    let abs_target = if target.is_absolute() {
        target.clone()
    } else {
        src.parent().unwrap_or(Path::new("/")).join(&target)
    };
    let mut link = target;
    for other in &scope.cfg.roots {
        if let Ok(rest) = abs_target.strip_prefix(&other.path) {
            let repo_target = repo.join(&other.label).join(rest);
            if let Some(parent) = dst.parent() {
                if let Some(rel) = relative_path(parent, &repo_target) {
                    link = rel;
                }
            }
            break;
        }
    }
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    Ok(if replace_symlink(dst, &link)? { 1 } else { 0 })
}

/// Create or replace a symlink at `dst` pointing to `link`. Returns whether it changed.
fn replace_symlink(dst: &Path, link: &Path) -> Result<bool> {
    if let Ok(m) = dst.symlink_metadata() {
        if m.file_type().is_symlink() {
            if fs::read_link(dst).map(|l| l == link).unwrap_or(false) {
                return Ok(false);
            }
        }
        remove_any(dst)?;
    }
    std::os::unix::fs::symlink(link, dst)
        .with_context(|| format!("symlink {} -> {}", dst.display(), link.display()))?;
    Ok(true)
}

/// Repo project dirs whose source memory is gone, empty, or excluded.
fn prune_projects(_scope: &Scope, root: &Root, repo: &Path, entries: &[Entry]) -> Result<Stats> {
    let mut stats = Stats::default();
    let projects = repo.join(&root.label).join("projects");
    let live: BTreeSet<&str> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Memory { slug } => Some(slug.as_str()),
            _ => None,
        })
        .collect();
    if let Ok(rd) = fs::read_dir(&projects) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if !live.contains(name.as_str()) {
                remove_any(&e.path())?;
                stats.deleted += 1;
            }
        }
    }
    Ok(stats)
}

/// Remove a file, symlink, or directory tree. Refuses anything named `.git`.
fn remove_any(p: &Path) -> Result<()> {
    if p.file_name().map(|n| n == ".git").unwrap_or(false) {
        anyhow::bail!("refusing to remove {}", p.display());
    }
    let m = p.symlink_metadata()?;
    if m.is_dir() && !m.file_type().is_symlink() {
        fs::remove_dir_all(p)?;
    } else {
        fs::remove_file(p)?;
    }
    Ok(())
}

/// Lexically resolve `.` and `..` without touching the filesystem.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `relative_path(/r/claude-work, /r/claude/CLAUDE.md)` -> `../claude/CLAUDE.md`.
pub fn relative_path(from_dir: &Path, to: &Path) -> Option<PathBuf> {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from.iter().zip(to_c.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to_c[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_walks_up_then_down() {
        assert_eq!(
            relative_path(Path::new("/r/claude-work"), Path::new("/r/claude/CLAUDE.md")).unwrap(),
            PathBuf::from("../claude/CLAUDE.md")
        );
        assert_eq!(
            relative_path(Path::new("/r/a/b"), Path::new("/r/a/b/c.md")).unwrap(),
            PathBuf::from("c.md")
        );
    }
}
