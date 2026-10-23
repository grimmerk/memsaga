//! What, inside one root, is in scope: the allowlisted top-level files and
//! directories, and every non-empty `projects/<slug>/memory/` directory,
//! which the repo stores as `<label>/memory/<slug>/`.
//!
//! Two questions are answered here: "is this absolute path something we
//! mirror?" (`Scope::contains`, used by the hook fast path) and "list every
//! source path to copy" (`Scope::entries`, used by the sweep).

use crate::config::{Config, Root};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// One thing to mirror from a root: a top-level file, a top-level directory,
/// or a project's memory directory.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    File { rel: String },
    Dir { rel: String },
    Memory { slug: String },
}

impl Entry {
    /// Path of the entry inside the root.
    pub fn source(&self, root: &Root) -> PathBuf {
        match self {
            Entry::File { rel } | Entry::Dir { rel } => root.path.join(rel),
            Entry::Memory { slug } => root.path.join("projects").join(slug).join("memory"),
        }
    }
    /// Path of the entry inside the repo subtree for this root.
    pub fn dest(&self, repo: &Path, root: &Root) -> PathBuf {
        let base = repo.join(&root.label);
        match self {
            Entry::File { rel } | Entry::Dir { rel } => base.join(rel),
            Entry::Memory { slug } => base.join(MEMORY_DIR).join(repo_name_of_slug(slug)),
        }
    }
}

/// Stands for the home directory at the start of a repository memory name:
/// `/Users/me/git/webapp`, whose live directory is `-Users-me-git-webapp`, is
/// stored as `HOME-git-webapp`, so the same project shares one directory on
/// machines whose home differs (another user name, Linux's `/home/...`).
/// A live name always starts with `-` (an absolute path starts with `/`), so
/// a name starting with the token is never a live one. grimmerk/memsaga-private#25.
pub const HOME_TOKEN: &str = "HOME";

struct HomeNames {
    /// The live-name encoding of the home directory, e.g. `-Users-me`.
    encoded: String,
    /// Whether a live name under home can be told apart from every other
    /// path on this machine (see `prefix_is_ambiguous`). When it cannot,
    /// live names are stored unchanged, and nothing is shared.
    forward: bool,
}

fn home_names() -> Option<&'static HomeNames> {
    static CELL: std::sync::OnceLock<Option<HomeNames>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let home = dirs::home_dir()?;
        let encoded = crate::slug::encode(&home.to_string_lossy());
        if encoded.len() < 2 {
            return None; // home is `/`: nothing to shorten
        }
        let forward = !prefix_is_ambiguous(&home, &encoded);
        Some(HomeNames { encoded, forward })
    })
    .as_ref()
}

/// The live-name encoding loses information (`/`, `.`, ` ` all become `-`),
/// so `-Users-me-old-x` could be `/Users/me/old/x` or `/Users/me-old/x`.
/// Ambiguous when another directory on this machine, outside home, encodes to
/// a name that starts like a path under home. Found by walking from `/` only
/// where an encoding still matches home's; gives up (ambiguous) after
/// 20,000 entries.
fn prefix_is_ambiguous(home: &Path, encoded: &str) -> bool {
    let target = format!("{encoded}-");
    let mut budget = 20_000usize;
    fn walk(
        dir: &Path,
        enc: &str,
        home: &Path,
        encoded: &str,
        target: &str,
        budget: &mut usize,
    ) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in rd.flatten() {
            if *budget == 0 {
                return true;
            }
            *budget -= 1;
            let path = e.path();
            if path == home {
                continue;
            }
            if !std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let enc_here = format!(
                "{enc}-{}",
                crate::slug::encode(&e.file_name().to_string_lossy())
            );
            if home.starts_with(&path) {
                if walk(&path, &enc_here, home, encoded, target, budget) {
                    return true;
                }
            } else if enc_here == encoded
                || enc_here.starts_with(target)
                || (target.starts_with(&format!("{enc_here}-"))
                    && walk(&path, &enc_here, home, encoded, target, budget))
            {
                return true;
            }
        }
        false
    }
    walk(Path::new("/"), "", home, encoded, &target, &mut budget)
}

/// A live project directory name -> its name in the repository.
pub fn repo_name_of_slug(slug: &str) -> String {
    let Some(h) = home_names().filter(|h| h.forward) else {
        return slug.to_string();
    };
    if slug == h.encoded {
        return HOME_TOKEN.to_string();
    }
    match slug.strip_prefix(&format!("{}-", h.encoded)) {
        Some(rest) if !rest.is_empty() => format!("{HOME_TOKEN}-{rest}"),
        _ => slug.to_string(),
    }
}

/// A repository memory name -> the live directory name on this machine.
/// Always unambiguous, so it works even where the forward direction is off.
pub fn slug_of_repo_name(name: &str) -> String {
    let Some(h) = home_names() else {
        return name.to_string();
    };
    if name == HOME_TOKEN {
        return h.encoded.clone();
    }
    match name.strip_prefix(&format!("{HOME_TOKEN}-")) {
        Some(rest) if !rest.is_empty() => format!("{}-{rest}", h.encoded),
        _ => name.to_string(),
    }
}

/// Repo-side directory under each root's subtree that holds every project's
/// memory: `<label>/memory/<name>/…`, the name from `repo_name_of_slug`. Live
/// memory stays at `projects/<slug>/memory/`; the two mappings below convert
/// between them.
pub const MEMORY_DIR: &str = "memory";

/// Root-relative live path -> path relative to the root's repo subtree.
/// `projects/<slug>/memory/x.md` becomes `memory/<name>/x.md`; every other
/// allowlisted path is stored where it lives.
pub fn repo_rel_of_live(rel: &Path) -> PathBuf {
    let parts: Vec<_> = rel.iter().collect();
    if parts.len() >= 3 && parts[0] == "projects" && parts[2] == "memory" {
        let mut out = PathBuf::from(MEMORY_DIR);
        out.push(repo_name_of_slug(&parts[1].to_string_lossy()));
        out.extend(&parts[3..]);
        return out;
    }
    rel.to_path_buf()
}

/// Inverse of `repo_rel_of_live`: `memory/<name>/x.md` -> `projects/<slug>/memory/x.md`.
/// A name stored before grimmerk/memsaga-private#25 (a live name) maps to itself.
pub fn live_rel_of_repo(rel: &Path) -> PathBuf {
    let parts: Vec<_> = rel.iter().collect();
    if parts.len() >= 2 && parts[0] == MEMORY_DIR {
        let mut out = PathBuf::from("projects");
        out.push(slug_of_repo_name(&parts[1].to_string_lossy()));
        out.push("memory");
        out.extend(&parts[2..]);
        return out;
    }
    rel.to_path_buf()
}

/// Where a memory file was stored in older layouts, so revisions from before
/// a change can still be read: under its live name (before grimmerk/memsaga-private#25), and
/// at `<label>/projects/<slug>/memory/x.md` (before 2026-09-18). Empty for
/// anything that is not memory.
pub fn legacy_repo_paths(repo_rel: &Path) -> Vec<PathBuf> {
    let parts: Vec<_> = repo_rel.iter().collect();
    if parts.len() < 3 || parts[1] != MEMORY_DIR {
        return Vec::new();
    }
    let slug = slug_of_repo_name(&parts[2].to_string_lossy());
    let mut out = Vec::new();
    if slug != parts[2].to_string_lossy() {
        let mut live_named = PathBuf::from(parts[0]);
        live_named.push(MEMORY_DIR);
        live_named.push(&slug);
        live_named.extend(&parts[3..]);
        out.push(live_named);
    }
    let mut old_layout = PathBuf::from(parts[0]);
    old_layout.push("projects");
    old_layout.push(&slug);
    old_layout.push("memory");
    old_layout.extend(&parts[3..]);
    out.push(old_layout);
    out
}

pub struct Scope<'a> {
    pub cfg: &'a Config,
    exclude_names: GlobSet,
    exclude_projects: GlobSet,
    exclude_paths: GlobSet,
    /// During a sweep: which repository files the mirror must leave alone
    /// (`sync::Guard`). None everywhere else.
    pub guard: Option<crate::sync::Guard>,
}

impl<'a> Scope<'a> {
    pub fn new(cfg: &'a Config) -> Scope<'a> {
        Scope {
            cfg,
            exclude_names: build_globset(&cfg.exclude),
            exclude_projects: build_globset(&cfg.exclude_projects),
            exclude_paths: build_globset(&cfg.exclude_paths),
            guard: None,
        }
    }

    pub fn with_guard(mut self, guard: crate::sync::Guard) -> Scope<'a> {
        self.guard = Some(guard);
        self
    }

    /// May the mirror overwrite `dst` (a repository path) with what it makes of
    /// `src` (None for a symlink)? Always, without a guard.
    pub fn may_write(&self, dst: &Path, src: Option<&Path>) -> bool {
        self.guard.as_ref().is_none_or(|g| g.allows_write(dst, src))
    }

    /// May the mirror delete `path` and everything under it?
    pub fn may_remove(&self, path: &Path) -> bool {
        self.guard.as_ref().is_none_or(|g| g.allows_remove(path))
    }

    /// May the mirror delete `path`, a repository copy that moved (an older
    /// layout, a memory directory under its older name) rather than a live
    /// file that vanished? The same checks without "has this machine had it".
    pub fn may_remove_moved(&self, path: &Path) -> bool {
        self.guard
            .as_ref()
            .is_none_or(|g| g.allows_remove_moved(path))
    }

    /// Is a file name excluded (matched against the basename only)?
    pub fn excluded_name(&self, name: &str) -> bool {
        self.exclude_names.is_match(name)
    }

    pub fn excluded_project(&self, slug: &str) -> bool {
        self.exclude_projects.is_match(slug)
    }

    /// Is a root-relative path (or any ancestor of it) in `exclude_paths`?
    pub fn excluded_path(&self, rel: &Path) -> bool {
        let mut cur = PathBuf::new();
        for c in rel.components() {
            cur.push(c.as_os_str());
            if self.exclude_paths.is_match(&cur) {
                return true;
            }
        }
        false
    }

    /// Everything to mirror from `root`, in a stable order.
    pub fn entries(&self, root: &Root) -> Vec<Entry> {
        let mut out = Vec::new();
        for f in &self.cfg.files {
            let p = root.path.join(f);
            if p.exists() || p.symlink_metadata().is_ok() {
                out.push(Entry::File { rel: f.clone() });
            }
        }
        for d in &self.cfg.dirs {
            let p = root.path.join(d);
            if p.exists() || p.symlink_metadata().is_ok() {
                out.push(Entry::Dir { rel: d.clone() });
            }
        }
        if self.cfg.memory {
            let projects = root.path.join("projects");
            if let Ok(rd) = std::fs::read_dir(&projects) {
                let mut slugs: Vec<String> = rd
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().join("memory").is_dir())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|s| !self.excluded_project(s))
                    .filter(|s| dir_has_files(&projects.join(s).join("memory")))
                    .collect();
                slugs.sort();
                out.extend(slugs.into_iter().map(|slug| Entry::Memory { slug }));
            }
        }
        out
    }

    /// Which root, if any, does an absolute path belong to, and is it in scope?
    /// Used by the hook fast path, so it must not touch the filesystem.
    pub fn contains(&self, path: &Path) -> Option<&'a Root> {
        for root in &self.cfg.roots {
            let rel = match path.strip_prefix(&root.path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if self.excluded_path(rel) {
                continue;
            }
            let mut comps = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string());
            let first = match comps.next() {
                Some(f) => f,
                None => continue,
            };
            if rel.components().count() == 1 && self.cfg.files.contains(&first) {
                return Some(root);
            }
            if self.cfg.dirs.contains(&first) {
                return Some(root);
            }
            if self.cfg.memory && first == "projects" {
                let slug = comps.next();
                let memory = comps.next();
                if let (Some(slug), Some(m)) = (slug, memory) {
                    if m == "memory" && !self.excluded_project(&slug) {
                        return Some(root);
                    }
                }
            }
        }
        None
    }
}

fn dir_has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| !e.file_name().to_string_lossy().starts_with('.'))
        })
        .unwrap_or(false)
}

fn build_globset(patterns: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_root(root: &Path) -> Config {
        Config {
            roots: vec![Root {
                path: root.to_path_buf(),
                label: "claude".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn contains_matches_allowlist_only() {
        let root = Path::new("/h/.claude");
        let cfg = cfg_with_root(root);
        let s = Scope::new(&cfg);
        assert!(s.contains(Path::new("/h/.claude/CLAUDE.md")).is_some());
        assert!(s
            .contains(Path::new("/h/.claude/skills/x/SKILL.md"))
            .is_some());
        assert!(s
            .contains(Path::new("/h/.claude/projects/-h-git-a/memory/MEMORY.md"))
            .is_some());
        assert!(s
            .contains(Path::new("/h/.claude/projects/-h-git-a/session.jsonl"))
            .is_none());
        assert!(s.contains(Path::new("/h/.claude/settings.json")).is_none());
        assert!(s.contains(Path::new("/h/git/a/src/x.ts")).is_none());
        assert!(s
            .contains(Path::new("/h/.claude/CLAUDE.md.codev-bak"))
            .is_none());
    }

    #[test]
    fn excluded_project_is_out_of_scope() {
        let root = Path::new("/h/.claude");
        let mut cfg = cfg_with_root(root);
        cfg.exclude_projects = vec!["-h-git-scratch*".into()];
        let s = Scope::new(&cfg);
        assert!(s
            .contains(Path::new(
                "/h/.claude/projects/-h-git-scratch-1/memory/a.md"
            ))
            .is_none());
        assert!(s
            .contains(Path::new("/h/.claude/projects/-h-git-a/memory/a.md"))
            .is_some());
    }

    #[test]
    fn memory_paths_map_between_live_and_repo_layouts() {
        let live = Path::new("projects/-h-git-a/memory/x.md");
        let repo = Path::new("memory/-h-git-a/x.md");
        assert_eq!(repo_rel_of_live(live), repo);
        assert_eq!(live_rel_of_repo(repo), live);
        assert_eq!(
            repo_rel_of_live(Path::new("projects/-h-git-a/memory")),
            Path::new("memory/-h-git-a")
        );
        // Non-memory paths are stored where they live.
        assert_eq!(
            repo_rel_of_live(Path::new("skills/foo/SKILL.md")),
            Path::new("skills/foo/SKILL.md")
        );
        assert_eq!(
            live_rel_of_repo(Path::new("CLAUDE.md")),
            Path::new("CLAUDE.md")
        );
        assert_eq!(
            legacy_repo_paths(Path::new("claude/memory/-h-git-a/x.md")),
            vec![PathBuf::from("claude/projects/-h-git-a/memory/x.md")]
        );
        assert!(legacy_repo_paths(Path::new("claude/CLAUDE.md")).is_empty());
    }
}
