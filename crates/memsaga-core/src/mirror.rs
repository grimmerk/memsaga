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

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub copied: usize,
    pub deleted: usize,
    pub linked: usize,
    /// Copies git's stat cache cannot see: the new contents have the old
    /// size and an mtime in the same second. `git add` skips such a file
    /// unless told to re-read it (`git add --renormalize`).
    pub restat: Vec<PathBuf>,
}

impl Stats {
    fn add(&mut self, o: Stats) {
        self.copied += o.copied;
        self.deleted += o.deleted;
        self.linked += o.linked;
        self.restat.extend(o.restat);
    }

    fn count(&mut self, c: Copied, dst: &Path) {
        match c {
            Copied::No => {}
            Copied::Yes => self.copied += 1,
            Copied::SameStat => {
                self.copied += 1;
                self.restat.push(dst.to_path_buf());
            }
        }
    }
}

/// What a copy did to the destination.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Copied {
    No,
    Yes,
    /// Rewritten with the same size and an mtime in the same second.
    SameStat,
}

impl Copied {
    /// After writing `dst`: same size and second as before the write?
    fn of(before: Option<&fs::Metadata>, dst: &Path) -> Copied {
        let after = fs::metadata(dst).ok();
        match (before, after) {
            (Some(b), Some(a))
                if b.is_file()
                    && b.len() == a.len()
                    && same_second(b.modified().ok(), a.modified().ok()) =>
            {
                Copied::SameStat
            }
            _ => Copied::Yes,
        }
    }
}

/// Repo path for an `extra_files` entry: `extra/home/<path under $HOME>` or
/// `extra/root/<absolute path>`. Collision-free, and readable on GitHub.
pub fn extra_repo_rel(p: &Path) -> PathBuf {
    // `..` in a configured path must not let the copy leave the repo.
    let p = &normalize(p);
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = p.strip_prefix(&home) {
            return PathBuf::from("extra/home").join(rel);
        }
    }
    let rel = p.strip_prefix("/").unwrap_or(p);
    PathBuf::from("extra/root").join(rel)
}

/// Copy memsaga's own config file to `<repo>/memsaga/config.toml` and every
/// `extra_files` entry to its `extra/` path. Missing sources are skipped, and
/// nothing under `memsaga/` or `extra/` is ever deleted here: an entry removed
/// from the config keeps its last mirrored copy until the user deletes it.
pub fn sync_self_and_extras(scope: &Scope) -> Result<Stats> {
    let cfg = scope.cfg;
    let mut stats = Stats::default();
    let own = crate::config::config_path();
    if own.is_file() {
        let dst = cfg.repo.join("memsaga/config.toml");
        if scope.may_write(&dst, Some(&own)) {
            stats.count(copy_if_changed(&own, &dst)?, &dst);
        }
    }
    for src in cfg.extra_files() {
        if !src.is_file() {
            continue;
        }
        let dst = cfg.repo.join(extra_repo_rel(&src));
        if scope.may_write(&dst, Some(&src)) {
            stats.count(copy_if_changed(&src, &dst)?, &dst);
        }
    }
    Ok(stats)
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
            if dst.symlink_metadata().is_ok() && scope.may_remove(&dst) {
                remove_any(&dst)?;
                stats.deleted += 1;
            }
        }
    }
    for d in &scope.cfg.dirs {
        let src = root.path.join(d);
        if src.symlink_metadata().is_err() {
            let dst = base.join(d);
            if dst.symlink_metadata().is_ok() && scope.may_remove(&dst) {
                remove_any(&dst)?;
                stats.deleted += 1;
            }
        }
    }
    stats.add(prune_memory(scope, root, repo, &entries)?);
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
        if scope.may_write(&dst, None) {
            stats.linked += mirror_symlink(scope, &src, &dst, repo)?;
        }
        return Ok(stats);
    }
    if meta.is_file() {
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        if !scope.may_write(&dst, Some(&src)) {
            return Ok(stats);
        }
        let is_settings = matches!(entry, Entry::File { rel } if rel == "settings.json");
        let copied = if is_settings && !scope.cfg.settings_ignore_keys.is_empty() {
            copy_settings_stripped(&src, &dst, &scope.cfg.settings_ignore_keys)?
        } else {
            copy_if_changed(&src, &dst)?
        };
        stats.count(copied, &dst);
        return Ok(stats);
    }
    if meta.is_dir() {
        // A destination that used to be a symlink must be replaced by a real dir.
        if dst
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            fs::remove_file(&dst)?;
        }
        // Relative to the root (the live side): `exclude_paths` are root-relative.
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
        if rel
            .components()
            .any(|c| scope.excluded_name(&c.as_os_str().to_string_lossy()))
        {
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
            let resolved = if link.is_absolute() {
                link.clone()
            } else {
                normalize(&item.path().parent().unwrap_or(src).join(&link))
            };
            let link = if resolved.starts_with(src) {
                link
            } else {
                resolved
            };
            if scope.may_write(&target, None) && replace_symlink(&target, &link)? {
                stats.linked += 1;
            }
        } else if ft.is_file() && scope.may_write(&target, Some(item.path())) {
            stats.count(copy_if_changed(item.path(), &target)?, &target);
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
        if p.symlink_metadata().is_ok() && scope.may_remove(&p) {
            remove_any(&p)?;
            stats.deleted += 1;
        }
    }
    Ok(stats)
}

/// Copy when size or mtime differ; keep the source mtime on the copy.
/// Mirror `settings.json` without the volatile top-level keys, so toggling
/// `/model` or `/effort` does not produce a commit. Falls back to a plain
/// copy when the file is not a JSON object. Compared by content, not mtime.
fn copy_settings_stripped(src: &Path, dst: &Path, ignore: &[String]) -> Result<Copied> {
    let text = fs::read_to_string(src)?;
    let Some(out) = strip_settings(&text, ignore) else {
        return copy_if_changed(src, dst);
    };
    // Only a regular file counts as "already current"; a symlink is replaced.
    let before = fs::symlink_metadata(dst).ok();
    if let Some(dm) = &before {
        if dm.is_file() && fs::read_to_string(dst).map(|d| d == out).unwrap_or(false) {
            return Ok(Copied::No);
        }
        if !dm.is_file() {
            remove_any(dst)?;
        }
    }
    fs::write(dst, out).with_context(|| format!("writing {}", dst.display()))?;
    // Keep the source's mode (a private settings file stays private).
    if let Ok(sm) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, sm.permissions());
    }
    Ok(Copied::of(before.as_ref(), dst))
}

/// `settings.json` as the mirror stores it: the `ignore` keys removed,
/// pretty-printed. None when the text is not a JSON object.
pub fn strip_settings(text: &str, ignore: &[String]) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object_mut()?;
    for k in ignore {
        obj.remove(k);
    }
    Some(serde_json::to_string_pretty(&value).ok()? + "\n")
}

fn copy_if_changed(src: &Path, dst: &Path) -> Result<Copied> {
    let sm = fs::metadata(src)?;
    let before = fs::symlink_metadata(dst).ok();
    if let Some(dm) = &before {
        if dm.is_file()
            && dm.len() == sm.len()
            && same_second(dm.modified().ok(), sm.modified().ok())
        {
            // Equal to the nanosecond, with a sub-second part: this is our
            // own copy (we set the source mtime on it). Otherwise, a second
            // edit of the same length within that second, or a filesystem
            // that keeps whole seconds (where equal says nothing): the
            // contents decide (the files are small).
            let exact =
                dm.modified().ok() == sm.modified().ok() && has_subsecond(sm.modified().ok());
            if exact || same_contents(src, dst) {
                return Ok(Copied::No);
            }
        }
        if !dm.is_file() {
            remove_any(dst)?;
        }
    }
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    if let Ok(t) = sm.modified() {
        let f = fs::OpenOptions::new().write(true).open(dst)?;
        let _ = f.set_modified(t);
    }
    Ok(Copied::of(before.as_ref(), dst))
}

fn has_subsecond(t: Option<std::time::SystemTime>) -> bool {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|d| d.subsec_nanos() != 0)
}

/// Byte-for-byte equal, read in 64 KiB chunks so a large file never sits in
/// memory twice. Any read error counts as "different" (the caller copies).
fn same_contents(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let mut ba = vec![0u8; 64 << 10];
    let mut bb = vec![0u8; 64 << 10];
    loop {
        let Ok(na) = read_full(&mut fa, &mut ba) else {
            return false;
        };
        let Ok(nb) = read_full(&mut fb, &mut bb) else {
            return false;
        };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }

    /// Fill `buf` as far as the file allows; fewer bytes only at the end.
    fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
}

fn same_second(a: Option<std::time::SystemTime>, b: Option<std::time::SystemTime>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let sa = a
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let sb = b
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
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
            let repo_target = repo
                .join(&other.label)
                .join(crate::roots::repo_rel_of_live(rest));
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
        if m.file_type().is_symlink() && fs::read_link(dst).map(|l| l == link).unwrap_or(false) {
            return Ok(false);
        }
        remove_any(dst)?;
    }
    std::os::unix::fs::symlink(link, dst)
        .with_context(|| format!("symlink {} -> {}", dst.display(), link.display()))?;
    Ok(true)
}

/// Repo memory dirs (`<label>/memory/<slug>`) whose source memory is gone,
/// empty, or excluded. Also removes `<label>/projects/`, the layout used
/// before 2026-09-18: the same files are re-synced under `memory/`, so the
/// next commit is a pure rename and `git log --follow` crosses it.
fn prune_memory(scope: &Scope, root: &Root, repo: &Path, entries: &[Entry]) -> Result<Stats> {
    let mut stats = Stats::default();
    let legacy = repo.join(&root.label).join("projects");
    if legacy.symlink_metadata().is_ok() && scope.may_remove(&legacy) {
        remove_any(&legacy)?;
        stats.deleted += 1;
    }
    let projects = repo.join(&root.label).join(crate::roots::MEMORY_DIR);
    // Repository names: a directory under an older name (a live name, before
    // grimmerk/memsaga-private#25) is removed here once its new name is written, which git
    // records as a rename.
    let live: BTreeSet<String> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Memory { slug } => Some(crate::roots::repo_name_of_slug(slug)),
            _ => None,
        })
        .collect();
    if let Ok(rd) = fs::read_dir(&projects) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if !live.contains(name.as_str()) && scope.may_remove(&e.path()) {
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
    let common = from
        .iter()
        .zip(to_c.iter())
        .take_while(|(a, b)| a == b)
        .count();
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
    fn same_contents_compares_across_chunk_boundaries() {
        let dir = std::env::temp_dir().join(format!("memsaga-same-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        let mut big = vec![b'x'; (64 << 10) * 2 + 7];
        fs::write(&a, &big).unwrap();
        fs::write(&b, &big).unwrap();
        assert!(same_contents(&a, &b));
        // Differ only in the last byte, past two full chunks.
        *big.last_mut().unwrap() = b'y';
        fs::write(&b, &big).unwrap();
        assert!(!same_contents(&a, &b));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_repo_rel_stays_inside_the_repo() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            extra_repo_rel(&home.join(".config/codev/accounts.json")),
            PathBuf::from("extra/home/.config/codev/accounts.json")
        );
        assert_eq!(
            extra_repo_rel(Path::new("/etc/hosts")),
            PathBuf::from("extra/root/etc/hosts")
        );
        // `..` is resolved lexically first, so it cannot climb out of extra/.
        let tricky = home.join("a/../../../etc/passwd");
        let rel = extra_repo_rel(&tricky);
        assert!(!rel.components().any(|c| c.as_os_str() == ".."), "{rel:?}");
        assert!(rel.starts_with("extra/"), "{rel:?}");
    }

    #[test]
    fn relative_path_walks_up_then_down() {
        assert_eq!(
            relative_path(
                Path::new("/r/claude-work"),
                Path::new("/r/claude/CLAUDE.md")
            )
            .unwrap(),
            PathBuf::from("../claude/CLAUDE.md")
        );
        assert_eq!(
            relative_path(Path::new("/r/a/b"), Path::new("/r/a/b/c.md")).unwrap(),
            PathBuf::from("c.md")
        );
    }
}
