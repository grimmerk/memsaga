//! What, inside one root, is in scope: the allowlisted top-level files and
//! directories, and every non-empty `projects/<slug>/memory/` directory.
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
            Entry::Memory { slug } => base.join("projects").join(slug).join("memory"),
        }
    }
}

pub struct Scope<'a> {
    pub cfg: &'a Config,
    exclude_names: GlobSet,
    exclude_projects: GlobSet,
    exclude_paths: GlobSet,
}

impl<'a> Scope<'a> {
    pub fn new(cfg: &'a Config) -> Scope<'a> {
        Scope {
            cfg,
            exclude_names: build_globset(&cfg.exclude),
            exclude_projects: build_globset(&cfg.exclude_projects),
            exclude_paths: build_globset(&cfg.exclude_paths),
        }
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
            let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string());
            let first = match comps.next() {
                Some(f) => f,
                None => continue,
            };
            if rel.components().count() == 1 && self.cfg.files.iter().any(|f| *f == first) {
                return Some(root);
            }
            if self.cfg.dirs.iter().any(|d| *d == first) {
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
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| !e.file_name().to_string_lossy().starts_with('.')))
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
        let mut c = Config::default();
        c.roots = vec![Root { path: root.to_path_buf(), label: "claude".into() }];
        c
    }

    #[test]
    fn contains_matches_allowlist_only() {
        let root = Path::new("/h/.claude");
        let cfg = cfg_with_root(root);
        let s = Scope::new(&cfg);
        assert!(s.contains(Path::new("/h/.claude/CLAUDE.md")).is_some());
        assert!(s.contains(Path::new("/h/.claude/skills/x/SKILL.md")).is_some());
        assert!(s.contains(Path::new("/h/.claude/projects/-h-git-a/memory/MEMORY.md")).is_some());
        assert!(s.contains(Path::new("/h/.claude/projects/-h-git-a/session.jsonl")).is_none());
        assert!(s.contains(Path::new("/h/.claude/settings.json")).is_none());
        assert!(s.contains(Path::new("/h/git/a/src/x.ts")).is_none());
        assert!(s.contains(Path::new("/h/.claude/CLAUDE.md.codev-bak")).is_none());
    }

    #[test]
    fn excluded_project_is_out_of_scope() {
        let root = Path::new("/h/.claude");
        let mut cfg = cfg_with_root(root);
        cfg.exclude_projects = vec!["-h-git-scratch*".into()];
        let s = Scope::new(&cfg);
        assert!(s.contains(Path::new("/h/.claude/projects/-h-git-scratch-1/memory/a.md")).is_none());
        assert!(s.contains(Path::new("/h/.claude/projects/-h-git-a/memory/a.md")).is_some());
    }
}
