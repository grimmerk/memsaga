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
    /// Every repository file a copy created or replaced (not links or
    /// deletions): what a `diff` preview leaves uncommitted in the clone.
    pub written: Vec<PathBuf>,
    /// Every repository file or link whose live source exists now, changed or
    /// not: what this machine has had (`sync::mark_seen`).
    pub present: Vec<PathBuf>,
    /// Repository paths the mirror could not copy, link or delete, with the
    /// reason. The rest of the sweep went on without them, and nothing under
    /// a folder that could not be read was deleted.
    pub errors: Vec<(PathBuf, String)>,
    /// Live files over `max_file_mb`, not copied: repository path and size
    /// in bytes. A repository copy of an earlier version is left as it is.
    pub too_large: Vec<(PathBuf, u64)>,
    /// Live folders holding `.git` (git repositories of their own), not
    /// mirrored: their repository paths, left as they are.
    pub nested: Vec<PathBuf>,
    /// Repository folders holding a `.git` copy (made by versions that copied
    /// one), moved aside instead of deleted: from, to.
    pub set_aside: Vec<(PathBuf, PathBuf)>,
}

impl Stats {
    fn count(&mut self, c: Copied, dst: &Path) {
        match c {
            Copied::No => {}
            Copied::Yes => {
                self.copied += 1;
                self.written.push(dst.to_path_buf());
            }
            Copied::SameStat => {
                self.copied += 1;
                self.restat.push(dst.to_path_buf());
                self.written.push(dst.to_path_buf());
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
pub fn sync_self_and_extras(scope: &Scope, stats: &mut Stats) -> Result<()> {
    let cfg = scope.cfg;
    let own = crate::config::config_path();
    let mut sources = vec![(own.clone(), cfg.repo.join("memsaga/config.toml"))];
    for src in cfg.extra_files() {
        let dst = cfg.repo.join(extra_repo_rel(&src));
        sources.push((src, dst));
    }
    for (src, dst) in sources {
        let Ok(meta) = fs::metadata(&src) else {
            continue;
        };
        if !meta.is_file() || over_limit(scope, &dst, meta.len(), stats) {
            continue;
        }
        if scope.may_write(&dst, Some(&src)) {
            match copy_if_changed(scope, &src, &dst) {
                Ok(c) => stats.count(c, &dst),
                Err(e) => stats.errors.push((dst, format!("{e:#}"))),
            }
        }
    }
    Ok(())
}

/// Is a live file of `len` bytes over `max_file_mb`? Then it is listed and
/// not copied (`max_file_mb = 0` turns the limit off).
fn over_limit(scope: &Scope, dst: &Path, len: u64, stats: &mut Stats) -> bool {
    let max = scope.cfg.max_file_mb;
    if max == 0 || len <= max.saturating_mul(1 << 20) {
        return false;
    }
    stats.too_large.push((dst.to_path_buf(), len));
    true
}

/// A folder holding `.git` (a folder, or a file as in a worktree or a
/// submodule): a git repository of its own. The mirror leaves it out on both
/// sides (docs/architecture.md, invariant 8).
pub fn is_nested_repo(dir: &Path) -> bool {
    dir.join(".git").symlink_metadata().is_ok()
}

/// Is `live`, a path under `root`, inside a nested repository: does a folder
/// between `root` (not included) and `live` hold `.git`?
pub fn in_nested_repo(root: &Path, live: &Path) -> bool {
    let mut cur = live.parent();
    while let Some(d) = cur {
        if d == root || !d.starts_with(root) {
            return false;
        }
        if is_nested_repo(d) {
            return true;
        }
        cur = d.parent();
    }
    false
}

/// Mirror every entry of `root` into `repo/<label>/`, then prune project
/// directories whose memory vanished or became excluded. Counts go into
/// `stats` as each file is written, so the caller still has the list of
/// files written so far when a later step fails.
pub fn sync_root(scope: &Scope, root: &Root, repo: &Path, stats: &mut Stats) -> Result<()> {
    // A root that cannot be listed would read as empty, and everything
    // mirrored from it as deleted.
    if let Err(e) = fs::read_dir(&root.path) {
        stats.errors.push((
            repo.join(&root.label),
            format!("listing {}: {e}", root.path.display()),
        ));
        return Ok(());
    }
    let entries = scope.entries(root);
    for e in &entries {
        sync_entry(scope, root, e, repo, stats)?;
    }
    // Files/dirs in the allowlist that disappeared from the root: remove copies.
    let base = repo.join(&root.label);
    for name in scope.cfg.files.iter().chain(&scope.cfg.dirs) {
        let src = root.path.join(name);
        // Gone only when the file system says so: a permission error must
        // not delete the repository copy.
        if src
            .symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            let dst = base.join(name);
            if dst.symlink_metadata().is_ok() && scope.may_remove(&dst) {
                remove_path(scope, &dst, stats);
            }
        }
    }
    prune_memory(scope, root, repo, &entries, stats);
    Ok(())
}

/// Remove a repository file or folder for the mirror, counting it; a
/// failure is recorded for that path and the mirror goes on. A folder
/// holding a `.git` copy inside goes aside first, never deleted.
fn remove_path(scope: &Scope, p: &Path, stats: &mut Stats) {
    if !set_aside_git_copies(scope, p, stats).is_empty() {
        return; // recorded; what is left of the folder stays
    }
    if p.symlink_metadata().is_err() {
        stats.deleted += 1; // the folder itself went aside
        return;
    }
    match remove_any(p) {
        Ok(()) => stats.deleted += 1,
        Err(e) => stats.errors.push((p.to_path_buf(), format!("{e:#}"))),
    }
}

/// Move every repository folder at or under `top` that holds a `.git` copy
/// to `<state>/stale/<time>/<repository path>`. git would take such a folder
/// for a repository of its own (a gitlink, or `git add` failing when it has
/// no commit yet), and its `.git` may hold the only copy of commits.
/// Returns the folders that could not be moved (recorded in `stats.errors`).
fn set_aside_git_copies(scope: &Scope, top: &Path, stats: &mut Stats) -> Vec<PathBuf> {
    let mut failed = Vec::new();
    if !top.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return failed;
    }
    let mut found: Vec<PathBuf> = Vec::new();
    let mut walk = WalkDir::new(top).follow_links(false).into_iter();
    while let Some(item) = walk.next() {
        let Ok(item) = item else { continue };
        if item.file_type().is_dir() && is_nested_repo(item.path()) {
            found.push(item.path().to_path_buf());
            walk.skip_current_dir();
        }
    }
    if found.is_empty() {
        return failed;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let base = scope.cfg.state_dir().join("stale").join(stamp);
    for from in found {
        // Files under it wait for the live side: moved, they would read as
        // deleted in the repository.
        if scope
            .guard
            .as_ref()
            .is_some_and(|g| g.protects_under(&from))
        {
            stats.errors.push((
                from.clone(),
                "holds a .git copy and files waiting for `memsaga sync`; left as it is".into(),
            ));
            failed.push(from);
            continue;
        }
        // Only inside a root's subtree (`<repo>/<label>/…`): never the data
        // repository itself, whose own `.git` this would otherwise match.
        let Some(rel) = from
            .strip_prefix(&scope.cfg.repo)
            .ok()
            .filter(|r| r.components().count() >= 2)
        else {
            stats.errors.push((
                from.clone(),
                "not inside a root's subtree; left as it is".into(),
            ));
            failed.push(from);
            continue;
        };
        let mut to = base.join(rel);
        let mut n = 1;
        while to.symlink_metadata().is_ok() {
            to = base.join(format!("{}.{n}", rel.display()));
            n += 1;
        }
        let moved = to
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::rename(&from, &to));
        match moved {
            Ok(()) => stats.set_aside.push((from, to)),
            Err(e) => {
                stats.errors.push((
                    from.clone(),
                    format!("moving its .git copy aside to {}: {e}", to.display()),
                ));
                failed.push(from);
            }
        }
    }
    failed
}

fn sync_entry(
    scope: &Scope,
    root: &Root,
    entry: &Entry,
    repo: &Path,
    stats: &mut Stats,
) -> Result<()> {
    let src = entry.source(root);
    let dst = entry.dest(repo, root);
    let meta = match src.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if meta.file_type().is_symlink() {
        stats.present.push(dst.clone());
        if scope.may_write(&dst, None) {
            match mirror_symlink(scope, &src, &dst, repo) {
                Ok(n) => stats.linked += n,
                Err(e) => stats.errors.push((dst, format!("{e:#}"))),
            }
        }
        return Ok(());
    }
    if meta.is_file() {
        stats.present.push(dst.clone());
        if over_limit(scope, &dst, meta.len(), stats) || !scope.may_write(&dst, Some(&src)) {
            return Ok(());
        }
        let is_settings = matches!(entry, Entry::File { rel } if rel == "settings.json");
        let copied = if is_settings && !scope.cfg.settings_ignore_keys.is_empty() {
            copy_settings_stripped(scope, &src, &dst, &scope.cfg.settings_ignore_keys)
        } else {
            copy_if_changed(scope, &src, &dst)
        };
        match copied {
            Ok(c) => stats.count(c, &dst),
            Err(e) => stats.errors.push((dst, format!("{e:#}"))),
        }
        return Ok(());
    }
    if meta.is_dir() {
        // A folder where the repository has a link: `sync_dir` records it and
        // mirrors nothing under it. Replacing the link here would commit the
        // folder over it, and on a machine with a stale folder that spreads
        // the stale copy (grimmerk/memsaga#29).
        // Relative to the root (the live side): `exclude_paths` are root-relative.
        let prefix = match entry {
            Entry::Dir { rel } => PathBuf::from(rel),
            Entry::Memory { slug } => PathBuf::from("projects").join(slug).join("memory"),
            Entry::File { rel } => PathBuf::from(rel),
        };
        sync_dir(scope, &src, &dst, &prefix, stats)?;
    }
    Ok(())
}

/// Is `rel`, a path inside a mirrored folder whose root-relative path is
/// `prefix`, left out by `exclude` (any component) or `exclude_paths`? One
/// rule for the mirror and for the counts `init --dry-run` prints.
pub fn excluded_in_dir(scope: &Scope, prefix: &Path, rel: &Path) -> bool {
    rel.components()
        .any(|c| scope.excluded_name(&c.as_os_str().to_string_lossy()))
        || scope.excluded_path(&prefix.join(rel))
}

/// How many files and links the mirror would keep for `src`: one for a link
/// (the second account's `skills`, mirrored as a link) or a file, else what
/// `sync_dir` would mirror.
pub fn count_mirrored(scope: &Scope, src: &Path, prefix: &Path) -> usize {
    if src
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink() || m.is_file())
    {
        return 1;
    }
    if is_nested_repo(src) {
        return 0;
    }
    WalkDir::new(src)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && is_nested_repo(e.path())))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        .filter(|e| {
            e.path()
                .strip_prefix(src)
                .is_ok_and(|rel| !excluded_in_dir(scope, prefix, rel))
        })
        .count()
}

/// Recursive directory mirror with delete. `prefix` is `src` relative to its
/// root, so root-relative `exclude_paths` can be applied while walking.
///
/// One path that cannot be copied, linked or deleted is recorded in
/// `stats.errors` and the rest goes on; a folder that cannot be read keeps
/// everything under its repository copy (its contents are unknown, so
/// nothing there may read as deleted). A folder holding `.git` is a
/// repository of its own: neither side of it is touched.
pub fn sync_dir(
    scope: &Scope,
    src: &Path,
    dst: &Path,
    prefix: &Path,
    stats: &mut Stats,
) -> Result<()> {
    // Never write or delete through a link in the repository: it can point
    // at a live folder (grimmerk/memsaga#29). walkdir also follows a root
    // that is a link, which would make the deletion pass walk the live side.
    if dst
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        scope.note_link_over_folder(dst);
        return Ok(());
    }
    if scope.through_link(dst) {
        return Ok(());
    }
    // A repository copy holding `.git` goes aside before anything else, so
    // git never sees it and the deletion pass never has to delete in it.
    let stuck = set_aside_git_copies(scope, dst, stats);
    if is_nested_repo(src) {
        stats.nested.push(dst.to_path_buf());
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    let mut keep: BTreeSet<PathBuf> = BTreeSet::new();
    // Repository links found where the live side has a folder: nothing under
    // them is mirrored or counted as had.
    let mut blocked: Vec<PathBuf> = stuck.clone();
    // Repository folders whose contents stay as they are: a nested
    // repository's, a folder's that could not be read, and a `.git` copy's
    // that could not be moved aside.
    let mut untouched: Vec<PathBuf> = stuck;
    let mut walk = WalkDir::new(src)
        .follow_links(false)
        .min_depth(1)
        .into_iter();
    while let Some(item) = walk.next() {
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                let at = e.path().unwrap_or(src);
                let target = dst.join(at.strip_prefix(src).unwrap_or(Path::new("")));
                stats.errors.push((target.clone(), e.to_string()));
                untouched.push(target);
                continue;
            }
        };
        let rel = item.path().strip_prefix(src)?;
        if excluded_in_dir(scope, prefix, rel) {
            continue;
        }
        let target = dst.join(rel);
        if blocked.iter().any(|b| target.starts_with(b)) {
            continue;
        }
        keep.insert(target.clone());
        let ft = item.file_type();
        if ft.is_dir()
            && target
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            scope.note_link_over_folder(&target);
            blocked.push(target);
            continue;
        }
        if ft.is_dir() && is_nested_repo(item.path()) {
            stats.nested.push(target.clone());
            untouched.push(target.clone());
            blocked.push(target);
            walk.skip_current_dir();
            continue;
        }
        if !ft.is_dir() {
            stats.present.push(target.clone());
        }
        let done = (|| -> Result<()> {
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
                if scope.may_write(&target, None) && replace_symlink(scope, &target, &link)? {
                    stats.linked += 1;
                }
            } else if ft.is_file() {
                let len = item.metadata()?.len();
                if !over_limit(scope, &target, len, stats)
                    && scope.may_write(&target, Some(item.path()))
                {
                    stats.count(copy_if_changed(scope, item.path(), &target)?, &target);
                }
            }
            Ok(())
        })();
        if let Err(e) = done {
            stats.errors.push((target, format!("{e:#}")));
        }
    }
    // Delete what is no longer in the source. Deepest first so dirs empty out.
    let mut extra: Vec<PathBuf> = WalkDir::new(dst)
        .follow_links(false)
        .follow_root_links(false)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .filter(|p| !keep.contains(p) && !untouched.iter().any(|u| p.starts_with(u)))
        .collect();
    extra.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in extra {
        if p.symlink_metadata().is_ok() && scope.may_remove(&p) {
            remove_path(scope, &p, stats);
        }
    }
    Ok(())
}

/// Copy when size or mtime differ; keep the source mtime on the copy.
/// Mirror `settings.json` without the volatile top-level keys, so toggling
/// `/model` or `/effort` does not produce a commit. Falls back to a plain
/// copy when the file is not a JSON object. Compared by content, not mtime.
fn copy_settings_stripped(
    scope: &Scope,
    src: &Path,
    dst: &Path,
    ignore: &[String],
) -> Result<Copied> {
    let text = fs::read_to_string(src)?;
    let Some(out) = strip_settings(&text, ignore) else {
        return copy_if_changed(scope, src, dst);
    };
    // Only a regular file counts as "already current"; a symlink is replaced.
    let before = fs::symlink_metadata(dst).ok();
    if let Some(dm) = &before {
        if dm.is_file() && fs::read_to_string(dst).map(|d| d == out).unwrap_or(false) {
            return Ok(Copied::No);
        }
    }
    let sm = fs::metadata(src)?;
    put_in_place(scope, dst, |tmp| {
        fs::write(tmp, &out).with_context(|| format!("writing {}", dst.display()))?;
        // Keep the source's mode (a private settings file stays private).
        owner_writable(tmp, &sm)
    })?;
    Ok(Copied::of(before.as_ref(), dst))
}

/// `settings.json` as the mirror stores it: the `ignore` keys removed,
/// memsaga's own hook commands written with one binary path for every machine
/// (`install::MIRRORED_BIN`; each machine registers its own, grimmerk/memsaga#55)
/// unless `ignore` is empty, pretty-printed. None when the text is not a JSON object.
pub fn strip_settings(text: &str, ignore: &[String]) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object_mut()?;
    for k in ignore {
        obj.remove(k);
    }
    // Same gate as the live direction (`restore::with_live_ignored_keys`):
    // `settings_ignore_keys = []` keeps settings.json as it is.
    if !ignore.is_empty() {
        crate::install::set_memsaga_bin(&mut value, crate::install::MIRRORED_BIN);
    }
    Some(serde_json::to_string_pretty(&value).ok()? + "\n")
}

fn copy_if_changed(scope: &Scope, src: &Path, dst: &Path) -> Result<Copied> {
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
    }
    put_in_place(scope, dst, |tmp| {
        fs::copy(src, tmp)
            .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
        owner_writable(tmp, &sm)?;
        if let Ok(t) = sm.modified() {
            let f = fs::OpenOptions::new().write(true).open(tmp)?;
            let _ = f.set_modified(t);
        }
        Ok(())
    })?;
    Ok(Copied::of(before.as_ref(), dst))
}

/// Write `dst` by writing a temporary file in the state directory (the same
/// file system) and renaming it over `dst` only once it is complete. A copy
/// that fails (a source that cannot be read, a full disk) leaves the old
/// repository copy as it was: the sweep goes on past one failed path and
/// stages everything, so a missing or half-written copy would be committed
/// as a deletion or a damaged file. A rename also replaces a read-only copy
/// (a git clone's packs are 0444), which cannot be opened for writing. A
/// folder in the way is cleared first, its `.git` copies moved aside.
fn put_in_place(scope: &Scope, dst: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    use std::os::unix::fs::MetadataExt;
    let parent = dst.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(parent)?;
    let dir = scope.cfg.state_dir().join("tmp");
    fs::create_dir_all(&dir)?;
    // The rename must not cross file systems: with `.git` elsewhere (a link
    // to another volume), the copy is written beside its destination.
    let same_fs = match (fs::metadata(&dir), fs::metadata(parent)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    };
    let name = format!(
        ".memsaga-tmp-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = if same_fs {
        dir.join(name)
    } else {
        parent.join(name)
    };
    let _ = fs::remove_file(&tmp);
    let done = write(&tmp).and_then(|()| {
        if dst
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            clear_folder(scope, dst)?;
        }
        fs::rename(&tmp, dst).with_context(|| format!("writing {}", dst.display()))?;
        Ok(())
    });
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

/// Remove a repository folder that a file or link replaces, after moving
/// any `.git` copy inside it aside (invariant 8); refused when one cannot be.
fn clear_folder(scope: &Scope, dir: &Path) -> Result<()> {
    let mut stats = Stats::default();
    let stuck = set_aside_git_copies(scope, dir, &mut stats);
    if let Some(first) = stuck.first() {
        anyhow::bail!(
            "{} holds a .git copy that could not be moved aside",
            first.display()
        );
    }
    if dir.symlink_metadata().is_ok() {
        remove_any(dir)?;
    }
    Ok(())
}

/// Give a mirror copy the source's mode plus owner-write: git keeps only the
/// executable bit, and a read-only copy would stop the next copy and its
/// mtime from being written.
fn owner_writable(dst: &Path, src: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = src.permissions().mode() | 0o200;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting the mode of {}", dst.display()))
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
    Ok(if replace_symlink(scope, dst, &link)? {
        1
    } else {
        0
    })
}

/// Create or replace a symlink at `dst` pointing to `link`. Returns whether it changed.
fn replace_symlink(scope: &Scope, dst: &Path, link: &Path) -> Result<bool> {
    if let Ok(m) = dst.symlink_metadata() {
        if m.file_type().is_symlink() && fs::read_link(dst).map(|l| l == link).unwrap_or(false) {
            return Ok(false);
        }
    }
    put_in_place(scope, dst, |tmp| {
        std::os::unix::fs::symlink(link, tmp)
            .with_context(|| format!("symlink {} -> {}", dst.display(), link.display()))?;
        Ok(())
    })?;
    Ok(true)
}

/// Repo memory dirs (`<label>/memory/<slug>`) whose source memory is gone,
/// empty, or excluded. Also removes `<label>/projects/`, the layout used
/// before 2026-09-18: the same files are re-synced under `memory/`, so the
/// next commit is a pure rename and `git log --follow` crosses it.
fn prune_memory(scope: &Scope, root: &Root, repo: &Path, entries: &[Entry], stats: &mut Stats) {
    let legacy = repo.join(&root.label).join("projects");
    if legacy.symlink_metadata().is_ok() && scope.may_remove_moved(&legacy) {
        remove_path(scope, &legacy, stats);
    }
    // Live memory folders that cannot be listed would read as gone.
    if let Err(e) = fs::read_dir(root.path.join("projects")) {
        if e.kind() != std::io::ErrorKind::NotFound {
            stats.errors.push((
                repo.join(&root.label).join(crate::roots::MEMORY_DIR),
                format!("listing {}: {e}", root.path.join("projects").display()),
            ));
            return;
        }
    }
    let projects = repo.join(&root.label).join(crate::roots::MEMORY_DIR);
    // Repository names: a directory under an older name (its live name, as
    // versions before the home-relative names stored it) is removed here
    // once its new name is written, which git records as a rename.
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
            if live.contains(name.as_str()) {
                continue;
            }
            // Under an older name of a directory that is live under its new
            // one: a move. Otherwise the memory is gone, or excluded.
            let moved = live.contains(crate::roots::repo_name_of_slug(&name).as_str());
            let allowed = if moved {
                scope.may_remove_moved(&e.path())
            } else {
                scope.may_remove(&e.path())
            };
            if allowed {
                remove_path(scope, &e.path(), stats);
            }
        }
    }
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
