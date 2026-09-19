//! Configuration: which roots to mirror, into which repo, with what allowlist.
//!
//! Resolution order for the config file: `MEMSAGA_CONFIG` env var, else
//! `~/.config/memsaga/config.toml`. A missing file is fine: every field has a
//! default. Env overrides (used by tests and one-off runs): `MEMSAGA_REPO`,
//! `MEMSAGA_ROOTS` (colon-separated paths).
//!
//! Roots are discovered when the file names none: codev's account registry
//! (`~/.config/codev/accounts.json`) if present, else `~/.claude`. codev is an
//! optional discovery source, never a dependency.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One Claude Code config directory to mirror.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Root {
    /// Absolute path after `~` expansion, e.g. `/Users/me/.claude`.
    pub path: PathBuf,
    /// Subtree name inside the repo. Defaults to the directory's basename
    /// without a leading dot (`.claude` -> `claude`).
    pub label: String,
}

/// Commit granularity: one commit per turn (default) or one per edit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Granularity {
    #[default]
    Turn,
    Edit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The mirror git repository.
    pub repo: PathBuf,
    /// Roots to mirror. Empty means "discover".
    pub roots: Vec<Root>,
    /// Top-level files of each root to mirror (relative names).
    pub files: Vec<String>,
    /// Top-level directories of each root to mirror recursively.
    pub dirs: Vec<String>,
    /// Mirror `projects/*/memory/` of each root (stored as `<label>/memory/<slug>/`).
    pub memory: bool,
    /// File-name globs excluded everywhere (matched against the basename).
    pub exclude: Vec<String>,
    /// Project slugs excluded entirely (exact names or globs).
    pub exclude_projects: Vec<String>,
    /// Root-relative paths excluded with their whole subtree, e.g. `skills/synced`
    /// (Claude Code's own synced skills from claude.ai are not the user's files).
    pub exclude_paths: Vec<String>,
    /// Single files outside every root to mirror as well, absolute or
    /// `~/`-prefixed (for example codev's `~/.config/codev/accounts.json`).
    /// Stored under `extra/` in the repo; never deleted by the tool.
    pub extra_files: Vec<String>,
    pub granularity: Granularity,
    /// Print a one-line terminal notice at the end of a turn that committed.
    pub reporter: bool,
    /// Minimum seconds between throttled pushes on `Stop`. 0 disables Stop pushes.
    pub push_min_interval_secs: u64,
    /// Push on `SessionEnd` when a remote named `origin` exists.
    pub push_on_session_end: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            repo: expand_tilde("~/git/memories"),
            roots: Vec::new(),
            files: vec!["CLAUDE.md".into()],
            dirs: vec!["skills".into(), "hooks".into(), "scripts".into()],
            memory: true,
            exclude: vec![
                "*.codev-bak*".into(),
                "*.codev-tmp-*".into(),
                ".DS_Store".into(),
            ],
            exclude_projects: Vec::new(),
            exclude_paths: vec!["skills/synced".into()],
            extra_files: Vec::new(),
            granularity: Granularity::Turn,
            reporter: true,
            push_min_interval_secs: 600,
            push_on_session_end: true,
        }
    }
}

impl Config {
    /// Load the config file (if any), apply env overrides, discover roots.
    pub fn load() -> Result<Config> {
        let path = config_path();
        let mut cfg = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str::<Config>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
        } else {
            Config::default()
        };
        cfg.repo = expand_tilde_path(&cfg.repo);
        for r in &mut cfg.roots {
            r.path = expand_tilde_path(&r.path);
            if r.label.is_empty() {
                r.label = label_for(&r.path);
            }
        }
        if let Ok(v) = std::env::var("MEMSAGA_REPO") {
            cfg.repo = expand_tilde(&v);
        }
        if let Ok(v) = std::env::var("MEMSAGA_ROOTS") {
            cfg.roots = v
                .split(':')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let p = expand_tilde(s);
                    Root {
                        label: label_for(&p),
                        path: p,
                    }
                })
                .collect();
        }
        if cfg.roots.is_empty() {
            cfg.roots = discover_roots();
        }
        for d in &cfg.dirs {
            if d == "memory" || d == "projects" {
                anyhow::bail!(
                    "`dirs` must not contain {d:?}: per-project memory is mirrored by \
                     `memory = true` into <label>/memory/, and projects/ holds transcripts"
                );
            }
        }
        Ok(cfg)
    }

    /// `extra_files` with `~` expanded.
    pub fn extra_files(&self) -> Vec<PathBuf> {
        self.extra_files.iter().map(|s| expand_tilde(s)).collect()
    }

    /// A stable digest of the effective configuration (file plus env
    /// overrides). Stored in the state dir after each sweep; a different value
    /// at the next Stop means the scope changed, so a sweep runs even when no
    /// file in the old scope is newer than the marker.
    pub fn fingerprint(&self) -> String {
        use std::hash::{Hash, Hasher};
        let text = serde_json::to_string(self).unwrap_or_default();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        format!("{:016x}", h.finish())
    }

    /// The root whose path equals `CLAUDE_CONFIG_DIR` (or `~/.claude` when unset).
    /// Hooks inherit that variable from the `claude` process, which is how the
    /// account of the session that fired the hook is known.
    pub fn current_root(&self) -> Option<&Root> {
        let dir = std::env::var("CLAUDE_CONFIG_DIR")
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|_| expand_tilde("~/.claude"));
        self.roots.iter().find(|r| r.path == dir)
    }

    /// Where memsaga keeps its own state inside the repo.
    pub fn state_dir(&self) -> PathBuf {
        self.repo.join(".git").join("memsaga")
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(v) = std::env::var("MEMSAGA_CONFIG") {
        return PathBuf::from(v);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config/memsaga/config.toml")
}

/// `.claude` -> `claude`, `.claude-work` -> `claude-work`, `foo` -> `foo`.
pub fn label_for(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().trim_start_matches('.').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "root".to_string())
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(s)
}

pub fn expand_tilde_path(p: &Path) -> PathBuf {
    expand_tilde(&p.to_string_lossy())
}

/// Roots from codev's registry, else `~/.claude` if it exists.
fn discover_roots() -> Vec<Root> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let reg = home.join(".config/codev/accounts.json");
        if let Ok(text) = std::fs::read_to_string(&reg) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(accounts) = v.get("accounts").and_then(|a| a.as_array()) {
                    for a in accounts {
                        if let Some(dir) = a.get("dir").and_then(|d| d.as_str()) {
                            let p = expand_tilde(dir);
                            if p.is_dir() {
                                out.push(Root {
                                    label: label_for(&p),
                                    path: p,
                                });
                            }
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            let p = home.join(".claude");
            if p.is_dir() {
                out.push(Root {
                    label: "claude".into(),
                    path: p,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_strips_leading_dot() {
        assert_eq!(label_for(Path::new("/x/.claude")), "claude");
        assert_eq!(label_for(Path::new("/x/.claude-work")), "claude-work");
        assert_eq!(label_for(Path::new("/x/plain")), "plain");
    }
}
