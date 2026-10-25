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
//!
//! The data repository is found the same way when the file does not set
//! `repo` ([`detect_repo`]); `memsaga init` then records it in the file, so a
//! folder created later never moves it.

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

/// Where the value of [`Config::repo`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum RepoOrigin {
    /// The config file sets `repo`.
    File,
    /// `MEMSAGA_REPO` overrides it for this run.
    Env,
    /// Nothing sets it: [`detect_repo`] chose it.
    #[default]
    Detected,
}

/// Where the roots came from.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum RootsOrigin {
    /// The config file lists `[[roots]]`.
    File,
    /// `MEMSAGA_ROOTS` overrides them for this run.
    Env,
    /// Nothing lists them: [`discover_roots`] found them.
    #[default]
    Discovered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The mirror git repository.
    pub repo: PathBuf,
    /// Where `repo` came from; not part of the file.
    #[serde(skip)]
    pub repo_origin: RepoOrigin,
    /// The config file's own `repo`, even when `MEMSAGA_REPO` overrides it
    /// for this run; `None` when the file does not set one.
    #[serde(skip)]
    pub file_repo: Option<PathBuf>,
    /// Whether the config file has a `roots` key, even an empty one; `init`
    /// then leaves the roots to the user.
    #[serde(skip)]
    pub file_sets_roots: bool,
    /// Roots to mirror. Empty means "discover".
    pub roots: Vec<Root>,
    /// Where `roots` came from; not part of the file.
    #[serde(skip)]
    pub roots_origin: RootsOrigin,
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
    /// Top-level keys removed from the mirrored copy of `settings.json`.
    /// Claude Code rewrites `model`, `effortLevel` and the per-model
    /// `modelSettings` on every `/model` and `/effort`, which would otherwise
    /// be a commit per toggle (measured: 17 such commits in two days).
    pub settings_ignore_keys: Vec<String>,
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
            // Filled in by `load`: the file's value, or `detect_repo()` only
            // when the file has none (hooks then pay for no search).
            repo: PathBuf::new(),
            repo_origin: RepoOrigin::Detected,
            file_repo: None,
            file_sets_roots: false,
            roots: Vec::new(),
            roots_origin: RootsOrigin::Discovered,
            // What the user writes in a config directory. Claude Code reads
            // CLAUDE.md, keybindings.json and the directories from skills to
            // agent-memory itself (code.claude.com/docs/en/claude-directory);
            // hooks/ and scripts/ are only a convention for what settings.json
            // points at. A missing one costs nothing.
            files: vec!["CLAUDE.md".into(), "keybindings.json".into()],
            dirs: [
                "skills",
                "commands",
                "agents",
                "agent-memory",
                "rules",
                "output-styles",
                "themes",
                "workflows",
                "hooks",
                "scripts",
            ]
            .map(String::from)
            .to_vec(),
            memory: true,
            exclude: vec![
                "*.codev-bak*".into(),
                "*.codev-tmp-*".into(),
                ".DS_Store".into(),
                "__pycache__".into(),
            ],
            exclude_projects: Vec::new(),
            // Claude Code's own copies of skills synced from claude.ai, and
            // where it moves the ones the sync removed.
            exclude_paths: vec!["skills/synced".into(), "skills/.trash".into()],
            extra_files: Vec::new(),
            settings_ignore_keys: vec![
                "model".into(),
                "effortLevel".into(),
                "modelSettings".into(),
            ],
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
        let (mut cfg, file_sets_repo) = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let table = toml::from_str::<toml::Table>(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            let sets_repo = table.contains_key("repo");
            let sets_roots = table.contains_key("roots");
            let mut c: Config = toml::Value::Table(table)
                .try_into()
                .with_context(|| format!("parsing {}", path.display()))?;
            c.file_sets_roots = sets_roots;
            (c, sets_repo)
        } else {
            (Config::default(), false)
        };
        if file_sets_repo {
            cfg.repo = normalize(&expand_tilde_path(&cfg.repo));
            cfg.repo_origin = RepoOrigin::File;
            cfg.file_repo = Some(cfg.repo.clone());
        } else {
            cfg.repo = detect_repo();
        }
        if !cfg.roots.is_empty() {
            cfg.roots_origin = RootsOrigin::File;
        }
        for r in &mut cfg.roots {
            r.path = expand_tilde_path(&r.path);
            if r.label.is_empty() {
                r.label = label_for(&r.path);
            }
        }
        if let Ok(v) = std::env::var("MEMSAGA_REPO") {
            cfg.repo = normalize(&expand_tilde(&v));
            cfg.repo_origin = RepoOrigin::Env;
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
            cfg.roots_origin = RootsOrigin::Env;
        }
        if cfg.roots.is_empty() {
            cfg.roots = discover_roots();
            cfg.roots_origin = RootsOrigin::Discovered;
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
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| expand_tilde("~/.claude"));
        let dir = normalize(&dir);
        self.roots.iter().find(|r| normalize(&r.path) == dir)
    }

    /// The folder `CLAUDE_CONFIG_DIR` names, when it is set, exists and is
    /// not one of the roots. Root discovery never reads the variable (hooks
    /// run in every account's sessions, and each would see a different set),
    /// so `init` adds it and records it, and `install` and `status` warn.
    pub fn unlisted_config_dir(&self) -> Option<PathBuf> {
        let dir = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|v| !v.is_empty())?;
        let dir = normalize(&expand_tilde(&dir));
        if !dir.is_dir() || self.roots.iter().any(|r| normalize(&r.path) == dir) {
            return None;
        }
        Some(dir)
    }

    /// For `init`: add the folder `CLAUDE_CONFIG_DIR` names to discovered
    /// roots, with a label no other root has. Returns the new root; `None`
    /// when the file has a `roots` key (even `roots = []`) or `MEMSAGA_ROOTS`
    /// is set, or nothing is missing.
    pub fn add_config_dir_root(&mut self) -> Option<Root> {
        if self.roots_origin != RootsOrigin::Discovered || self.file_sets_roots {
            return None;
        }
        let path = self.unlisted_config_dir()?;
        let label = unique_label(&self.roots, &label_for(&path));
        let root = Root { path, label };
        self.roots.push(root.clone());
        Some(root)
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

/// Folders under the home directory where people commonly keep repositories,
/// in the order [`detect_repo`] tries them.
pub const CODE_FOLDERS: [&str; 5] = ["git", "code", "src", "Projects", "dev"];

/// The `.gitattributes` line `init` writes into every data repository.
pub const GITATTRIBUTES_LINE: &str = "**/MEMORY.md merge=union";

/// Whether `p` is a memsaga data repository: a git repository with memsaga's
/// state directory, or a fresh clone of one, which carries the generated
/// `paths.json` and the `.gitattributes` line `init` wrote. Someone's
/// unrelated project that happens to be called `memories` is neither. Only
/// `init` creates the state directory in a repository that is not one yet:
/// every other command refuses to run there (`main.rs`, before dispatch), so
/// the mark cannot appear by accident. The file read happens only for a
/// clone memsaga has not used yet.
pub fn is_data_repo(p: &Path) -> bool {
    if p.join(".git/memsaga").is_dir() {
        return true;
    }
    p.join(".git").exists()
        && p.join("paths.json").is_file()
        && std::fs::read_to_string(p.join(".gitattributes"))
            .is_ok_and(|t| t.lines().any(|l| l.trim() == GITATTRIBUTES_LINE))
}

/// The data repository when the config file does not set `repo`: an existing
/// `memories` data repository in one of [`CODE_FOLDERS`] or the home
/// directory; else `memories` in the first of those folders that exists; else
/// `~/memories`. Only a few `stat` calls. The fallback is a proposal for
/// `init`; no other command writes there until `init` has made it a data
/// repository.
pub fn detect_repo() -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("memories");
    };
    let candidates: Vec<PathBuf> = CODE_FOLDERS
        .iter()
        .map(|f| home.join(f).join("memories"))
        .chain(std::iter::once(home.join("memories")))
        .collect();
    if let Some(existing) = candidates.iter().find(|p| is_data_repo(p)) {
        return existing.clone();
    }
    CODE_FOLDERS
        .iter()
        .map(|f| home.join(f))
        .find(|d| d.is_dir())
        .map(|d| d.join("memories"))
        .unwrap_or_else(|| home.join("memories"))
}

/// Write `repo = "<path>"` into the config file, creating it if needed. Used by
/// `init` when nothing set `repo`, so the choice stays fixed. The line goes at
/// the top, where a key is always top-level; the rest of the file is kept as
/// it is. A path under the home directory is written with `~/`, so a config
/// copied to another machine keeps working.
pub fn record_repo(repo: &Path) -> Result<()> {
    let path = config_path();
    let shown =
        match dirs::home_dir().and_then(|h| repo.strip_prefix(h).ok().map(Path::to_path_buf)) {
            Some(rel) => format!("~/{}", rel.display()),
            None => repo.display().to_string(),
        };
    let line = format!("repo = {}\n", toml::Value::String(shown));
    let text = match std::fs::read_to_string(&path) {
        Ok(old) => {
            if toml::from_str::<toml::Table>(&old).is_ok_and(|t| t.contains_key("repo")) {
                anyhow::bail!("{} already sets repo", path.display());
            }
            format!("{line}{old}")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("{CONFIG_HEADER}{line}"),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    write_config(&path, &text)
}

const CONFIG_HEADER: &str =
    "# memsaga configuration. Keys left out keep their defaults; `memsaga config` prints them all.\n";

/// Append `roots` to the config file as `[[roots]]` tables, so every session
/// sees the same roots whatever its `CLAUDE_CONFIG_DIR`. Refuses when the file
/// already lists roots. Paths under the home folder are written with `~/`.
pub fn record_roots(roots: &[Root]) -> Result<()> {
    let path = config_path();
    let old = match std::fs::read_to_string(&path) {
        Ok(old) => {
            if toml::from_str::<toml::Table>(&old).is_ok_and(|t| t.contains_key("roots")) {
                anyhow::bail!("{} already lists roots", path.display());
            }
            old
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CONFIG_HEADER.to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut text = old;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# found by `memsaga init`; listing roots turns off the automatic search\n");
    text.push_str(&roots_toml(roots));
    write_config(&path, &text)
}

/// `roots` as `[[roots]]` tables, paths under the home folder written with `~/`.
pub fn roots_toml(roots: &[Root]) -> String {
    let home = dirs::home_dir();
    let mut out = String::new();
    for r in roots {
        let shown = match home
            .as_ref()
            .and_then(|h| r.path.strip_prefix(h).ok().map(Path::to_path_buf))
        {
            Some(rel) => format!("~/{}", rel.display()),
            None => r.path.display().to_string(),
        };
        out.push_str(&format!(
            "[[roots]]\npath = {}\nlabel = {}\n",
            toml::Value::String(shown),
            toml::Value::String(r.label.clone())
        ));
    }
    out
}

fn write_config(path: &Path, text: &str) -> Result<()> {
    let path = path.to_path_buf();
    // A config kept in a dotfiles repository is often a symlink: write the file
    // it points at, so the link (and the dotfiles copy) stays the source.
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Write beside it, then rename: an interrupted write never leaves half a
    // config. The pid keeps two concurrent runs apart; the mode is kept.
    let tmp = target.with_extension(format!("toml.memsaga-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        std::fs::set_permissions(&tmp, meta.permissions())
            .with_context(|| format!("setting the mode of {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))
}

/// `p` as an absolute path with `.` and `..` resolved lexically (symlinks are
/// left alone), so two spellings of one folder compare equal.
pub fn normalize(p: &Path) -> PathBuf {
    let abs = if p.is_relative() {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `.claude` -> `claude`, `.claude-work` -> `claude-work`, `foo` -> `foo`.
/// `base`, or `base-2`, `base-3`, … when a root already has that label: each
/// root's label names its own subtree in the repository.
pub fn unique_label(roots: &[Root], base: &str) -> String {
    let mut label = base.to_string();
    let mut n = 2;
    while roots.iter().any(|r| r.label == label) {
        label = format!("{base}-{n}");
        n += 1;
    }
    label
}

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
