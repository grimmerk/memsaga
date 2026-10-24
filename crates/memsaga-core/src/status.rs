//! What `memsaga status` reports, as one value. The text view and
//! `status --json` both print this struct, so the two cannot disagree.
//! Read-only: building it never sweeps, commits, pushes or writes a marker.

use crate::config::{config_path, Config};
use crate::git::Git;
use crate::roots::{Entry, Scope};
use crate::state::{Event, State};
use serde::Serialize;
use std::path::PathBuf;
use std::time::SystemTime;

/// Bumped when a field is removed or changes meaning; adding fields does not.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Status {
    pub schema: u32,
    pub repo: PathBuf,
    /// False when the repo is missing or not a git repository; the remaining
    /// git-derived fields are then empty.
    pub initialised: bool,
    /// Short sha of HEAD, empty when there is none.
    pub head: String,
    pub config: ConfigStatus,
    pub last_sweep: Option<Stamp>,
    /// A Write/Edit marked the turn dirty and the Stop sweep has not run yet.
    pub dirty: bool,
    pub origin: Option<OriginStatus>,
    pub roots: Vec<RootStatus>,
    /// Repository files newer than their live file, waiting for
    /// `memsaga sync` (docs/two-way-sync.md): repo-relative path -> entry.
    pub incoming: crate::sync::Incoming,
    /// Repository files in scope that this machine never had live, kept by
    /// the last sweep instead of being deleted (grimmerk/memsaga#22).
    pub absent: Vec<String>,
    /// Repository links under which the last sweep mirrored nothing, because
    /// this machine has a folder there (grimmerk/memsaga#29).
    pub link_over_folder: Vec<String>,
    /// Top-level entries the repository holds that this machine's config
    /// does not mirror, as `<label>/<name>` (grimmerk/memsaga#23).
    pub uncovered: Vec<String>,
    /// Scripts a root's `settings.json` runs that this machine lacks
    /// (grimmerk/memsaga#25).
    pub missing_scripts: Vec<crate::differences::MissingScript>,
    /// Most recent first, from the current `events.jsonl` (rotated files are
    /// not read).
    pub events: Vec<Event>,
}

#[derive(Debug, Serialize)]
pub struct ConfigStatus {
    pub path: PathBuf,
    pub present: bool,
    /// The file is gone but the repo holds `memsaga/config.toml`, so a config
    /// existed before: memsaga is running on its defaults by accident.
    pub missing: bool,
    /// `extra_files` entries, as written, that are folders. The mirror takes
    /// files only, so nothing under them is mirrored.
    pub extra_folders: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Stamp {
    /// Local time, RFC 3339.
    pub at: String,
    pub age_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct OriginStatus {
    /// Commits on HEAD that origin's tracking branch lacks.
    pub ahead: usize,
    pub last_push: Option<Stamp>,
    /// The latest push attempt failed (marker `push-error`, cleared by the
    /// next successful push).
    pub push_failing: bool,
    /// Its error text, when failing.
    pub push_error: String,
    /// A deferred push is scheduled for this time.
    pub push_pending: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RootStatus {
    pub label: String,
    pub path: PathBuf,
    pub exists: bool,
    pub memory_dirs: usize,
}

/// `memsaga/config.toml` in the repo but no config file: the one-line
/// warning shared by the reporter and `status`, or None.
pub fn config_missing_warning(cfg: &Config) -> Option<String> {
    let path = config_path();
    if path.exists() || !cfg.repo.join("memsaga/config.toml").is_file() {
        return None;
    }
    let path = path.display().to_string();
    Some(format!(
        "memsaga: config file {path} is missing, running on defaults (fewer files are mirrored); restore it with: git -C {} show HEAD:memsaga/config.toml > {}",
        shell_quote(&cfg.repo.display().to_string()),
        shell_quote(&path)
    ))
}

/// `extra_files` entries, as written, that name a folder. The mirror skips
/// anything that is not a file, so without this a folder listed there would
/// look watched while nothing in it is ever committed.
pub fn extra_folders(cfg: &Config) -> Vec<String> {
    cfg.extra_files
        .iter()
        .zip(cfg.extra_files())
        .filter(|(_, path)| path.is_dir())
        .map(|(entry, _)| entry.clone())
        .collect()
}

/// One warning line per folder in `extra_files`, shared by `status` and
/// `config`.
pub fn extra_folder_warnings(cfg: &Config) -> Vec<String> {
    extra_folders(cfg)
        .into_iter()
        .map(|entry| {
            format!(
                "memsaga: extra_files entry {entry} is a folder, so nothing in it is mirrored; list its files one by one, or keep the folder under the config directory and add it to dirs"
            )
        })
        .collect()
}

/// `s` as one shell word: unchanged when it is plainly safe, else in single
/// quotes (with any `'` written as `'\''`).
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn collect(cfg: &Config, n_events: usize) -> Status {
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    let initialised = git.is_repo();
    let path = config_path();
    let config = ConfigStatus {
        present: path.exists(),
        missing: config_missing_warning(cfg).is_some(),
        extra_folders: extra_folders(cfg),
        path,
    };
    let scope = Scope::new(cfg);
    let roots = cfg
        .roots
        .iter()
        .map(|r| RootStatus {
            label: r.label.clone(),
            path: r.path.clone(),
            exists: r.path.is_dir(),
            memory_dirs: scope
                .entries(r)
                .iter()
                .filter(|e| matches!(e, Entry::Memory { .. }))
                .count(),
        })
        .collect();
    if !initialised {
        return Status {
            schema: SCHEMA_VERSION,
            repo: cfg.repo.clone(),
            initialised,
            head: String::new(),
            config,
            last_sweep: None,
            dirty: false,
            origin: None,
            roots,
            incoming: Default::default(),
            absent: Vec::new(),
            link_over_folder: Vec::new(),
            uncovered: Vec::new(),
            missing_scripts: crate::differences::missing_scripts(cfg),
            events: Vec::new(),
        };
    }
    let mut events = state.read_events();
    events.reverse();
    let origin = git.has_remote("origin").then(|| {
        // A marker, not the event log: rotation must not hide a failure.
        let push_error = state.read_marker("push-error");
        OriginStatus {
            ahead: git.ahead_of("origin"),
            last_push: state.mtime("last-push").map(stamp),
            push_failing: push_error.is_some(),
            push_error: push_error.unwrap_or_default(),
            push_pending: state
                .read_marker("push-pending")
                .and_then(|s| s.parse::<i64>().ok())
                .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
        }
    });
    events.truncate(n_events);
    Status {
        schema: SCHEMA_VERSION,
        repo: cfg.repo.clone(),
        initialised,
        head: git.head_short().unwrap_or_default(),
        config,
        last_sweep: state.mtime("last-sweep").map(stamp),
        dirty: state.exists("dirty"),
        origin,
        roots,
        incoming: crate::sync::load_incoming(&state),
        absent: crate::sync::load_absent(&state),
        link_over_folder: crate::sync::load_link_over_folder(&state),
        uncovered: crate::differences::uncovered(cfg, "HEAD"),
        missing_scripts: crate::differences::missing_scripts(cfg),
        events,
    }
}

fn stamp(t: SystemTime) -> Stamp {
    Stamp {
        at: chrono::DateTime::<chrono::Local>::from(t)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        age_secs: SystemTime::now()
            .duration_since(t)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn shell_quote_leaves_plain_paths_and_quotes_the_rest() {
        assert_eq!(
            shell_quote("/Users/me/git/memories"),
            "/Users/me/git/memories"
        );
        assert_eq!(shell_quote("/Users/me/My Repo"), "'/Users/me/My Repo'");
        assert_eq!(shell_quote("/a/it's"), "'/a/it'\\''s'");
        assert_eq!(shell_quote("~/x"), "'~/x'");
        assert_eq!(shell_quote("/a/$(rm)"), "'/a/$(rm)'");
    }
}
