//! Two-way sync: edits made on the repository side reach the live files.
//! Design and the reasons behind each choice: `docs/two-way-sync.md`.
//!
//! - `Incoming`: repository files newer than their live file, each with its
//!   base (the version both sides last agreed on). `.git/memsaga/incoming.json`.
//! - `Guard`: given to the mirror during a sweep. It refuses to overwrite or
//!   delete a file on the incoming list, and detects a hand edit in the clone
//!   before the mirror would overwrite it (step 1b).
//! - `integrate_remote`: fetch and merge origin, record what it changed (1a).
//! - `settle`: the three-way decision per incoming file (step 2).
//! - `check_remote`: the throttled "did origin move?" question the mod asks (3).

use crate::config::{config_path, Config};
use crate::git::Git;
use crate::mirror::{extra_repo_rel, strip_settings};
use crate::restore::{self, Located};
use crate::state::{self, Event, State};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Rebuilt by every sweep; in a merge conflict memsaga keeps its own copy.
pub const GENERATED: [&str; 2] = ["index.md", "paths.json"];

const INCOMING: &str = "incoming.json";
const REMOTE_CHECK: &str = "remote-check.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomingEntry {
    /// Blob id of the agreed version; None when the file did not exist then.
    pub base: Option<String>,
    /// Where the newer version came from: `merge <sha>` or `hand edit`.
    pub from: String,
}

/// Repo-relative path -> entry. Sorted, so the file and the reports are stable.
pub type Incoming = BTreeMap<String, IncomingEntry>;

pub fn load_incoming(state: &State) -> Incoming {
    std::fs::read_to_string(state.marker(INCOMING))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The caller holds the sweep lock. An empty list removes the file.
pub fn save_incoming(state: &State, incoming: &Incoming) -> Result<()> {
    if incoming.is_empty() {
        state.remove(INCOMING);
        return Ok(());
    }
    state.write_marker(INCOMING, &serde_json::to_string_pretty(incoming)?)
}

/// Where the live copy of a repository file is.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Inside a root (`claude/…`, `claude-work/…`).
    Root(Located),
    /// memsaga's own config file, or one `extra_files` entry.
    Exact(PathBuf),
}

impl Target {
    pub fn live(&self) -> &Path {
        match self {
            Target::Root(l) => &l.live,
            Target::Exact(p) => p,
        }
    }
}

/// The live location of `repo_rel`, or None for a file that has no live side
/// (`companion/`, `manual/`, the generated index) or is out of scope.
pub fn live_target(cfg: &Config, repo_rel: &str) -> Option<Target> {
    if GENERATED.contains(&repo_rel) {
        return None;
    }
    if repo_rel == "memsaga/config.toml" {
        return Some(Target::Exact(config_path()));
    }
    if repo_rel.starts_with("extra/") {
        return cfg
            .extra_files()
            .into_iter()
            .find(|f| extra_repo_rel(f) == Path::new(repo_rel))
            .map(Target::Exact);
    }
    let label = repo_rel.split('/').next()?;
    if !cfg.roots.iter().any(|r| r.label == label) {
        return None;
    }
    restore::locate(cfg, repo_rel).ok().map(Target::Root)
}

/// The live file as the mirror would store it (`settings.json` without its
/// volatile keys). None when it does not exist or is not a regular file.
pub fn mirror_form(cfg: &Config, target: &Target) -> Option<Vec<u8>> {
    let live = target.live();
    let meta = std::fs::symlink_metadata(live).ok()?;
    if !meta.is_file() {
        return None;
    }
    let bytes = std::fs::read(live).ok()?;
    if is_root_settings(cfg, target) {
        if let Some(s) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| strip_settings(t, &cfg.settings_ignore_keys))
        {
            return Some(s.into_bytes());
        }
    }
    Some(bytes)
}

fn is_root_settings(cfg: &Config, target: &Target) -> bool {
    match target {
        Target::Root(l) => {
            !cfg.settings_ignore_keys.is_empty()
                && l.repo_rel.iter().count() == 2
                && l.repo_rel.file_name() == Some(OsStr::new("settings.json"))
        }
        Target::Exact(_) => false,
    }
}

/// Blob id of `path` at `rev`, or None when it does not exist there.
fn blob_at(git: &Git, rev: &str, path: &str) -> Option<String> {
    let out = git
        .run(&["rev-parse", "-q", "--verify", &format!("{rev}:{path}")])
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn blob_content(git: &Git, id: &str) -> Option<Vec<u8>> {
    let out = git.run(&["cat-file", "blob", id]).ok()?;
    out.status.success().then_some(out.stdout)
}

/// Blob id the file's current bytes would have.
fn hash_of(git: &Git, abs: &Path) -> Option<String> {
    let out = git
        .run_os(&[OsStr::new("hash-object"), OsStr::new("--"), abs.as_os_str()])
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Handed to the mirror for one sweep (step 1b).
pub struct Guard {
    repo: PathBuf,
    /// Start of the previous sweep: a repo file modified later is a candidate
    /// hand edit (memsaga's own copies carry their source's older mtime).
    since: Option<SystemTime>,
    /// Absolute repo paths on the incoming list: never written or deleted.
    protected: BTreeSet<PathBuf>,
    /// Hand edits found during this sweep: repo-relative path -> base.
    found: RefCell<BTreeMap<String, Option<String>>>,
    /// Keys the mirror drops from `settings.json`, to compare like with like.
    settings_ignore_keys: Vec<String>,
}

impl Guard {
    pub fn new(cfg: &Config, since: Option<SystemTime>, incoming: &Incoming) -> Guard {
        Guard {
            repo: cfg.repo.clone(),
            since,
            protected: incoming.keys().map(|k| cfg.repo.join(k)).collect(),
            found: RefCell::new(BTreeMap::new()),
            settings_ignore_keys: cfg.settings_ignore_keys.clone(),
        }
    }

    /// May the mirror replace the repository file `dst` with the content of
    /// `src` (None when deleting or linking)?
    pub fn allows_write(&self, dst: &Path, src: Option<&Path>) -> bool {
        if self.protected.contains(dst) {
            return false;
        }
        let Ok(meta) = std::fs::symlink_metadata(dst) else {
            return true; // nothing there yet
        };
        if !meta.is_file() {
            return true;
        }
        // Fast path: not modified since the last sweep began, so not a hand
        // edit. Only the rare candidate costs two git calls.
        match (self.since, meta.modified()) {
            (Some(since), Ok(t)) if t > since => {}
            _ => return true,
        }
        let Ok(rel) = dst.strip_prefix(&self.repo) else {
            return true;
        };
        let rel = rel.to_string_lossy().to_string();
        let git = Git::new(&self.repo);
        let base = blob_at(&git, "HEAD", &rel);
        if base.is_some() && base == hash_of(&git, dst) {
            return true; // equal to what memsaga committed
        }
        if src.is_some_and(|s| self.is_mirror_of(dst, s)) {
            return true; // memsaga's own copy, e.g. left by a `diff` preview
        }
        self.found.borrow_mut().insert(rel, base);
        false
    }

    /// Does `dst` hold exactly what the mirror would make of `src`?
    fn is_mirror_of(&self, dst: &Path, src: &Path) -> bool {
        let (Ok(d), Ok(s)) = (std::fs::read(dst), std::fs::read(src)) else {
            return false;
        };
        if d == s {
            return true;
        }
        src.file_name() == Some(OsStr::new("settings.json"))
            && std::str::from_utf8(&s)
                .ok()
                .and_then(|t| strip_settings(t, &self.settings_ignore_keys))
                .is_some_and(|stripped| stripped.as_bytes() == d.as_slice())
    }

    /// May the mirror delete `path` (a file, or a directory and everything
    /// under it)? Refused when any file under it is protected or hand-edited.
    pub fn allows_remove(&self, path: &Path) -> bool {
        if self.protected.iter().any(|p| p.starts_with(path)) {
            return false;
        }
        let mut ok = true;
        for e in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            // Check every file, so each hand edit is recorded.
            if !self.allows_write(e.path(), None) {
                ok = false;
            }
        }
        ok
    }

    /// Hand edits found, as incoming entries.
    pub fn take_found(&self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut *self.found.borrow_mut())
            .into_iter()
            .collect()
    }
}

/// Add hand edits to the list. An entry already there keeps its base.
pub fn record_hand_edits(incoming: &mut Incoming, found: Vec<(String, Option<String>)>) -> usize {
    let mut added = 0;
    for (rel, base) in found {
        if let std::collections::btree_map::Entry::Vacant(slot) = incoming.entry(rel) {
            slot.insert(IncomingEntry {
                base,
                from: "hand edit".into(),
            });
            added += 1;
        }
    }
    added
}

/// What `integrate_remote` did.
#[derive(Debug, Default)]
pub struct Integrated {
    /// A merge (or fast-forward) brought commits in.
    pub merged: bool,
    /// Live-mapped files the merge changed, now on the incoming list.
    pub files: Vec<String>,
}

/// The merge stopped on files memsaga will not decide alone.
#[derive(Debug)]
pub struct MergeConflict(pub Vec<String>);

impl std::fmt::Display for MergeConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "origin has commits that conflict with local ones in {}; merge aborted, nothing pushed (resolve in the data repository by hand: git pull, fix, commit)",
            self.0.join(", ")
        )
    }
}

impl std::error::Error for MergeConflict {}

pub fn current_branch(git: &Git) -> Result<String> {
    Ok(git
        .run_ok(&["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string())
}

/// Step 1a: fetch origin and merge its branch when it has commits we lack.
/// Every file the merge changed that has a live side goes on the incoming
/// list with its pre-merge version as base. Takes the sweep lock.
pub fn integrate_remote(cfg: &Config) -> Result<Integrated> {
    integrate_remote_within(cfg, Duration::from_secs(60))
}

/// How long a hook waits for the sweep lock: well inside the hooks' 30 s
/// timeout, so a busy lock makes the hook skip its pull rather than be killed
/// half-way (Claude Code then runs the tool on the file as it is).
pub const HOOK_LOCK_WAIT: Duration = Duration::from_secs(20);

/// `integrate_remote`, waiting at most `wait` for the sweep lock.
pub fn integrate_remote_within(cfg: &Config, wait: Duration) -> Result<Integrated> {
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    if !git.has_remote("origin") {
        return Ok(Integrated::default());
    }
    let _guard = state.lock(wait).context("sweep lock")?;
    let branch = current_branch(&git)?;
    git.run_ok(&["fetch", "-q", "origin"])?;
    let remote = format!("origin/{branch}");
    if !git
        .run(&["rev-parse", "-q", "--verify", &remote])?
        .status
        .success()
    {
        return Ok(Integrated::default());
    }
    let behind = !git
        .run(&["merge-base", "--is-ancestor", &remote, "HEAD"])?
        .status
        .success();
    if !behind {
        return Ok(Integrated::default());
    }
    let pre = git.run_ok(&["rev-parse", "HEAD"])?.trim().to_string();
    let message = format!("sync: merge {remote}");
    let merge = git.run(&["merge", "--no-edit", "-q", "-m", &message, &remote])?;
    if !merge.status.success() {
        let conflicted: Vec<String> = git
            .run_ok(&["diff", "--name-only", "-z", "--diff-filter=U"])
            .unwrap_or_default()
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let only_generated =
            !conflicted.is_empty() && conflicted.iter().all(|f| GENERATED.contains(&f.as_str()));
        if only_generated {
            let mut args = vec!["checkout", "--ours", "--"];
            args.extend(conflicted.iter().map(String::as_str));
            git.run_ok(&args)?;
            let mut args = vec!["add", "--"];
            args.extend(conflicted.iter().map(String::as_str));
            git.run_ok(&args)?;
            git.run_ok(&["commit", "-q", "--no-edit"])?;
        } else {
            let _ = git.run(&["merge", "--abort"]);
            if conflicted.is_empty() {
                bail!(
                    "git merge {remote} failed: {}",
                    String::from_utf8_lossy(&merge.stderr).trim()
                );
            }
            return Err(MergeConflict(conflicted).into());
        }
    }
    let head = git
        .run_ok(&["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    let changed: Vec<String> = git
        .run_ok(&["diff", "--name-only", "-z", &pre, "HEAD"])?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut incoming = load_incoming(&state);
    let mut files = Vec::new();
    for f in changed {
        if live_target(cfg, &f).is_none() {
            continue;
        }
        if !incoming.contains_key(&f) {
            incoming.insert(
                f.clone(),
                IncomingEntry {
                    base: blob_at(&git, &pre, &f),
                    from: format!("merge {head}"),
                },
            );
        }
        files.push(f);
    }
    save_incoming(&state, &incoming)?;
    mark_integrated(&state);
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "merge".into(),
        event: "sync".into(),
        files: files.clone(),
        sha: head,
        message: format!("merged {remote}"),
        ..Default::default()
    })?;
    Ok(Integrated {
        merged: true,
        files,
    })
}

/// How `settle` left one file.
#[derive(Debug, Default, Serialize)]
pub struct Settled {
    /// The repository version was written to the live file.
    pub written: Vec<String>,
    /// Both sides changed different lines; the merged text was written.
    pub merged: Vec<String>,
    /// The repository deleted it and the live file was unchanged: deleted.
    pub deleted: Vec<String>,
    /// Already equal on both sides.
    pub in_step: Vec<String>,
    /// Both sides changed the same lines (or not text): left as they are.
    pub conflicts: Vec<String>,
    /// Not decided now, with the reason (no live location on this machine,
    /// a symlink, an error).
    pub skipped: Vec<(String, String)>,
    /// For each written, merged or deleted file: the change to the live file
    /// as changed lines (`-old` / `+new`), for the notice.
    pub diffs: BTreeMap<String, String>,
}

/// `--take`: settle one file by picking a side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Take {
    Live,
    Repo,
}

/// Step 2: decide every incoming file (docs/two-way-sync.md, diagram D2).
/// Takes the sweep lock; writes live files atomically.
pub fn settle(cfg: &Config, take: Option<(Take, &str)>) -> Result<Settled> {
    settle_within(cfg, take, Duration::from_secs(60))
}

/// `settle`, waiting at most `wait` for the sweep lock.
pub fn settle_within(cfg: &Config, take: Option<(Take, &str)>, wait: Duration) -> Result<Settled> {
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    let _guard = state.lock(wait).context("sweep lock")?;
    let mut incoming = load_incoming(&state);
    let mut out = Settled::default();
    let take = match take {
        Some((side, path)) => {
            let rel = repo_rel_of_input(cfg, path)?;
            if !incoming.contains_key(&rel) {
                bail!("{rel} is not waiting to be synced");
            }
            Some((side, rel))
        }
        None => None,
    };
    let keys: Vec<String> = incoming.keys().cloned().collect();
    for rel in keys {
        let entry = incoming[&rel].clone();
        let target = live_target(cfg, &rel);
        let before = target.as_ref().and_then(|t| mirror_form(cfg, t));
        match settle_one(cfg, &git, &state, &rel, &entry, take.as_ref()) {
            Ok(Outcome::Done(kind)) => {
                incoming.remove(&rel);
                if !matches!(kind, Done::InStep) {
                    let after = target.as_ref().and_then(|t| mirror_form(cfg, t));
                    out.diffs.insert(
                        rel.clone(),
                        changed_lines(&state, before.as_deref(), after.as_deref()),
                    );
                }
                match kind {
                    Done::Written => out.written.push(rel),
                    Done::Merged => out.merged.push(rel),
                    Done::Deleted => out.deleted.push(rel),
                    Done::InStep => out.in_step.push(rel),
                }
            }
            Ok(Outcome::Conflict) => out.conflicts.push(rel),
            Ok(Outcome::Skip(why)) => out.skipped.push((rel, why)),
            Err(e) => out.skipped.push((rel, format!("{e:#}"))),
        }
    }
    save_incoming(&state, &incoming)?;
    let touched: Vec<String> = out
        .written
        .iter()
        .chain(&out.merged)
        .chain(&out.deleted)
        .cloned()
        .collect();
    if !touched.is_empty() {
        // The live side changed: the next Stop sweeps and commits it.
        state.write_dirty(&format!(
            "{} sync {} files",
            state::now_rfc3339(),
            touched.len()
        ))?;
        state.append_event(&Event {
            ts: state::now_rfc3339(),
            kind: "pull".into(),
            event: "sync".into(),
            files: touched,
            message: format!(
                "{} written, {} merged, {} deleted, {} conflicts",
                out.written.len(),
                out.merged.len(),
                out.deleted.len(),
                out.conflicts.len()
            ),
            ..Default::default()
        })?;
    }
    Ok(out)
}

enum Done {
    Written,
    Merged,
    Deleted,
    InStep,
}

enum Outcome {
    Done(Done),
    Conflict,
    Skip(String),
}

fn settle_one(
    cfg: &Config,
    git: &Git,
    state: &State,
    rel: &str,
    entry: &IncomingEntry,
    take: Option<&(Take, String)>,
) -> Result<Outcome> {
    let Some(target) = live_target(cfg, rel) else {
        return Ok(Outcome::Skip("no live location on this machine".into()));
    };
    if let Target::Root(loc) = &target {
        let root_exists = cfg
            .roots
            .iter()
            .any(|r| loc.live.starts_with(&r.path) && r.path.is_dir());
        if !root_exists {
            return Ok(Outcome::Skip(
                "its root does not exist on this machine".into(),
            ));
        }
    }
    let repo_path = cfg.repo.join(rel);
    let repo_meta = std::fs::symlink_metadata(&repo_path).ok();
    if repo_meta.as_ref().is_some_and(|m| !m.is_file()) {
        return Ok(Outcome::Skip("not a regular file in the repository".into()));
    }
    if std::fs::symlink_metadata(target.live()).is_ok_and(|m| !m.is_file()) {
        return Ok(Outcome::Skip("the live path is not a regular file".into()));
    }
    let r = repo_meta.map(|_| std::fs::read(&repo_path)).transpose()?;
    let l = mirror_form(cfg, &target);
    if let Some((side, which)) = take {
        if which == rel {
            return match side {
                Take::Live => Ok(Outcome::Done(Done::InStep)),
                Take::Repo => apply(cfg, state, &target, &repo_path, r.as_deref()),
            };
        }
    }
    if l == r {
        return Ok(Outcome::Done(Done::InStep));
    }
    let b = match &entry.base {
        Some(id) => Some(blob_content(git, id).context("reading the base version")?),
        None => None,
    };
    if l == b {
        return apply(cfg, state, &target, &repo_path, r.as_deref());
    }
    // Both sides changed: a line merge when all three are text.
    let (Some(l), Some(r)) = (l, r) else {
        return Ok(Outcome::Conflict);
    };
    let b = b.unwrap_or_default();
    if [&l, &r, &b].iter().any(|v| v.contains(&0)) {
        return Ok(Outcome::Conflict);
    }
    let union = rel.ends_with("/MEMORY.md") || rel == "MEMORY.md";
    match merge_text(&cfg.repo, state, &l, &b, &r, union)? {
        Some(merged) => {
            write_live(cfg, &target, &merged, executable(&repo_path))?;
            Ok(Outcome::Done(Done::Merged))
        }
        None => Ok(Outcome::Conflict),
    }
}

/// Make the live file equal the repository file (or delete it).
fn apply(
    cfg: &Config,
    _state: &State,
    target: &Target,
    repo_path: &Path,
    r: Option<&[u8]>,
) -> Result<Outcome> {
    match r {
        Some(bytes) => {
            write_live(cfg, target, bytes, executable(repo_path))?;
            Ok(Outcome::Done(Done::Written))
        }
        None => {
            let live = target.live();
            if let Target::Root(_) = target {
                restore::verify_destination(cfg, live)?;
            }
            if std::fs::symlink_metadata(live).is_ok() {
                std::fs::remove_file(live)
                    .with_context(|| format!("deleting {}", live.display()))?;
            }
            Ok(Outcome::Done(Done::Deleted))
        }
    }
}

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// Atomically replace the live file with `content` (in mirror form: the live
/// `settings.json` keeps its own volatile keys).
fn write_live(cfg: &Config, target: &Target, content: &[u8], executable: bool) -> Result<()> {
    let (dest, content) = match target {
        Target::Root(loc) => {
            let dest = restore::verify_destination(cfg, &loc.live)?;
            let content = if is_root_settings(cfg, target) {
                restore::with_live_ignored_keys(cfg, loc, content.to_vec())
            } else {
                content.to_vec()
            };
            (dest, content)
        }
        Target::Exact(p) => (p.clone(), content.to_vec()),
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_file_name(format!(
        ".{}.memsaga-sync-{}",
        dest.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, &content).with_context(|| format!("writing {}", tmp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        // Keep a private file private: start from the current live mode.
        let mode = std::fs::metadata(&dest)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o644);
        let mode = if executable {
            mode | 0o111
        } else {
            mode & !0o111
        };
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

/// The lines that differ between `old` and `new` (absent = empty), as
/// `-line` / `+line` without diff headers or context: what a notice shows.
fn changed_lines(state: &State, old: Option<&[u8]>, new: Option<&[u8]>) -> String {
    if new.is_none() {
        return "(deleted)".into();
    }
    let pid = std::process::id();
    let a = state.marker(&format!("diff-old-{pid}"));
    let b = state.marker(&format!("diff-new-{pid}"));
    if std::fs::write(&a, old.unwrap_or_default()).is_err()
        || std::fs::write(&b, new.unwrap_or_default()).is_err()
    {
        return String::new();
    }
    let out = std::process::Command::new("git")
        .args(["diff", "--no-index", "--no-color", "-U0", "--"])
        .arg(&a)
        .arg(&b)
        .output();
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let Ok(out) = out else {
        return String::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `git merge-file`: Some(merged text) when the two sides changed different
/// lines, None on overlapping edits.
fn merge_text(
    repo: &Path,
    state: &State,
    live: &[u8],
    base: &[u8],
    other: &[u8],
    union: bool,
) -> Result<Option<Vec<u8>>> {
    state.ensure()?;
    let pid = std::process::id();
    let paths = ["live", "base", "repo"].map(|n| state.marker(&format!("merge-{n}-{pid}")));
    for (p, bytes) in paths.iter().zip([live, base, other]) {
        std::fs::write(p, bytes)?;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).arg("merge-file").arg("-p");
    if union {
        cmd.arg("--union");
    }
    let out = cmd.args(&paths).output();
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let out = out.context("running git merge-file")?;
    // Exit 0: clean; >0: number of conflicts; <0 (shown as >127): error.
    match out.status.code() {
        Some(0) => Ok(Some(out.stdout)),
        Some(n) if (1..=127).contains(&n) => Ok(None),
        _ => bail!(
            "git merge-file failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// Before reading or writing one file: if its repository copy was edited by
/// hand since the last sweep and is not yet on the incoming list, add it
/// (base = HEAD). Returns whether the file is on the list afterwards.
/// Takes the sweep lock.
pub fn note_hand_edit(cfg: &Config, repo_rel: &str) -> Result<bool> {
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    let _guard = state.lock(HOOK_LOCK_WAIT).context("sweep lock")?;
    let mut incoming = load_incoming(&state);
    if incoming.contains_key(repo_rel) {
        return Ok(true);
    }
    let path = cfg.repo.join(repo_rel);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Ok(false);
    }
    let base = blob_at(&git, "HEAD", repo_rel);
    if base.is_some() && base == hash_of(&git, &path) {
        return Ok(false);
    }
    // Equal to the live file: a sweep that stopped before committing, not an edit.
    if let Some(t) = live_target(cfg, repo_rel) {
        if mirror_form(cfg, &t).as_deref() == std::fs::read(&path).ok().as_deref() {
            return Ok(false);
        }
    }
    record_hand_edits(&mut incoming, vec![(repo_rel.to_string(), base)]);
    save_incoming(&state, &incoming)?;
    Ok(true)
}

/// A repo-relative path, a live path or `~/…`, to its repo-relative form.
pub fn repo_rel_of_input(cfg: &Config, input: &str) -> Result<String> {
    let p = crate::config::expand_tilde(input);
    if p == config_path() {
        return Ok("memsaga/config.toml".into());
    }
    if p.is_absolute() {
        if let Some(f) = cfg.extra_files().into_iter().find(|f| *f == p) {
            return Ok(extra_repo_rel(&f).to_string_lossy().to_string());
        }
        if let Ok(rel) = p.strip_prefix(&cfg.repo) {
            return Ok(rel.to_string_lossy().to_string());
        }
    }
    Ok(restore::locate(cfg, input)?
        .repo_rel
        .to_string_lossy()
        .to_string())
}

/// What the throttled remote check knows.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RemoteCheck {
    /// Unix seconds of the last question to origin.
    pub checked_at: u64,
    /// Origin's branch head as last seen.
    pub remote_sha: String,
    /// ETag of the GitHub API answer, for a 304 next time.
    #[serde(default)]
    pub etag: String,
    /// "github-api" or "ls-remote".
    #[serde(default)]
    pub via: String,
    /// Origin has commits the local branch lacks.
    pub moved: bool,
    /// Live-mapped files those commits change: (repo-relative, live path).
    pub changed: Vec<(String, String)>,
}

/// Step 3's question, throttled across every session on the machine: when
/// another check ran less than `max_age` ago its answer is reused. Otherwise
/// origin's branch head is read (GitHub API with an ETag, else
/// `git ls-remote`), fetched when it is new, and the changed files listed.
pub fn check_remote(cfg: &Config, max_age: Duration) -> Result<RemoteCheck> {
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    let cached: RemoteCheck = std::fs::read_to_string(state.marker(REMOTE_CHECK))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if cached.checked_at > 0 && now.saturating_sub(cached.checked_at) < max_age.as_secs() {
        return Ok(cached);
    }
    if !git.has_remote("origin") {
        return Ok(RemoteCheck::default());
    }
    let branch = current_branch(&git)?;
    let (remote_sha, etag, via) = match github_head(&git, &branch, &cached) {
        Some(found) => found,
        None => {
            let out = git.run_ok(&["ls-remote", "origin", &format!("refs/heads/{branch}")])?;
            let sha = out
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            (sha, String::new(), "ls-remote".to_string())
        }
    };
    let tracking = format!("origin/{branch}");
    let local_tracking = git
        .run(&["rev-parse", "-q", "--verify", &tracking])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if !remote_sha.is_empty() && local_tracking.as_deref() != Some(remote_sha.as_str()) {
        git.run_ok(&["fetch", "-q", "origin"])?;
    }
    let moved = !remote_sha.is_empty()
        && !git
            .run(&["merge-base", "--is-ancestor", &tracking, "HEAD"])?
            .status
            .success();
    let changed = if moved {
        git.run_ok(&["diff", "--name-only", "-z", &format!("HEAD...{tracking}")])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .filter_map(|f| {
                live_target(cfg, f).map(|t| (f.to_string(), t.live().display().to_string()))
            })
            .collect()
    } else {
        Vec::new()
    };
    let check = RemoteCheck {
        checked_at: now,
        remote_sha,
        etag,
        via,
        moved,
        changed,
    };
    state.write_marker(REMOTE_CHECK, &serde_json::to_string_pretty(&check)?)?;
    Ok(check)
}

/// Origin's branch head from the GitHub API, when origin is on github.com and
/// `gh` has a token: one request, and a 304 (free, no rate cost) when nothing
/// changed. None means "use git ls-remote instead".
fn github_head(git: &Git, branch: &str, cached: &RemoteCheck) -> Option<(String, String, String)> {
    if std::env::var_os("MEMSAGA_NO_GITHUB_API").is_some() {
        return None;
    }
    let url = git.run_ok(&["remote", "get-url", "origin"]).ok()?;
    let slug = github_slug(url.trim())?;
    let token = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|t| !t.is_empty())?;
    // The token goes in through stdin (`-K -`), never on the command line.
    let mut config = format!("header = \"Authorization: Bearer {token}\"\n");
    if !cached.etag.is_empty() && !cached.remote_sha.is_empty() {
        config.push_str(&format!("header = \"If-None-Match: {}\"\n", cached.etag));
    }
    let mut child = std::process::Command::new("curl")
        .args([
            "-s",
            "-m",
            "5",
            "-D",
            "-",
            "-H",
            "Accept: application/vnd.github.sha",
            "-K",
            "-",
            &format!("https://api.github.com/repos/{slug}/commits/{branch}"),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split_whitespace().nth(1)?;
    let etag = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("etag").then(|| v.trim().to_string())
        })
        .unwrap_or_default();
    match status {
        "304" => Some((
            cached.remote_sha.clone(),
            cached.etag.clone(),
            "github-api".into(),
        )),
        "200" => {
            let sha = body.trim().to_string();
            (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (sha, etag, "github-api".into()))
        }
        _ => None,
    }
}

/// `owner/repo` of a github.com remote URL (https or ssh form).
fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !repo.is_empty())
        .then(|| format!("{owner}/{repo}"))
}

/// The cached answer of `check_remote`, whatever its age.
fn cached_check(state: &State) -> RemoteCheck {
    std::fs::read_to_string(state.marker(REMOTE_CHECK))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// After a merge: origin's commits are in, so the cached answer no longer
/// says "moved" (otherwise every prompt until the next check would pull).
fn mark_integrated(state: &State) {
    let mut c = cached_check(state);
    if c.checked_at == 0 {
        return;
    }
    c.moved = false;
    c.changed.clear();
    if let Ok(text) = serde_json::to_string_pretty(&c) {
        let _ = state.write_marker(REMOTE_CHECK, &text);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Seconds a check's answer is reused by every session on the machine.
pub const CHECK_MAX_AGE: Duration = Duration::from_secs(120);

/// Start `memsaga pull --check` detached when the cached answer is older than
/// `CHECK_MAX_AGE`, without waiting for it. A marker keeps several sessions
/// from starting one each.
fn refresh_check_in_background(state: &State) {
    let c = cached_check(state);
    if now_secs().saturating_sub(c.checked_at) < CHECK_MAX_AGE.as_secs() {
        return;
    }
    if state
        .mtime("remote-check-started")
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(60))
    {
        return;
    }
    let _ = state.touch("remote-check-started");
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["pull", "--check"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid only detaches the child from the caller's session; it
    // takes no arguments and touches no shared state.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// Merge origin (when it moved) and settle the incoming list: what `memsaga
/// pull` does, inside a hook, so the lock waits are bounded by
/// `HOOK_LOCK_WAIT`. A merge that stops on a conflict is reported, not raised.
pub fn pull_now(cfg: &Config) -> Result<(Option<String>, Settled)> {
    let merge_error = integrate_remote_within(cfg, HOOK_LOCK_WAIT)
        .err()
        .map(|e| format!("{e:#}"));
    Ok((merge_error, settle_within(cfg, None, HOOK_LOCK_WAIT)?))
}

/// Files the session used (Read, Write, Edit), per session id.
fn session_file(state: &State, session: &str) -> Option<PathBuf> {
    let safe: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!safe.is_empty()).then(|| state.marker(&format!("session-files/{safe}")))
}

fn note_session_file(state: &State, session: &str, rel: &str) {
    let Some(p) = session_file(state, session) else {
        return;
    };
    let known = std::fs::read_to_string(&p).unwrap_or_default();
    if known.lines().any(|l| l == rel) {
        return;
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{rel}");
    }
}

/// Forget a session's files (SessionEnd).
pub fn forget_session(cfg: &Config, session: &str) {
    if let Some(p) = session_file(&State::new(cfg.state_dir()), session) {
        let _ = std::fs::remove_file(p);
    }
}

/// CLAUDE.md of a root, or a project's MEMORY.md: files Claude Code loads
/// into every session's context without a Read.
fn loaded_at_start(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    matches!(
        parts.as_slice(),
        [_, "CLAUDE.md"] | [_, "memory", _, "MEMORY.md"]
    )
}

/// Most changed lines a notice quotes per file; a longer change is named
/// with its size and the model is asked to read the file again.
const NOTICE_DIFF_LINES: usize = 20;

/// The notice for the model: which files changed, what changed in them, and
/// that the current file is the only version that counts from now on (a
/// session that re-read the file once still answered from the copy loaded at
/// start: 2026-10-06).
fn notice(
    cfg: &Config,
    settled: &Settled,
    files: &[String],
    conflicts: &[String],
) -> Option<String> {
    let live = |r: &String| -> String {
        live_target(cfg, r)
            .map(|t| t.live().display().to_string())
            .unwrap_or_else(|| r.clone())
    };
    let mut out = Vec::new();
    for f in files {
        let diff = settled.diffs.get(f).map(String::as_str).unwrap_or_default();
        let lines = diff.lines().count();
        let what = if diff == "(deleted)" {
            "It was deleted.".to_string()
        } else if lines == 0 {
            "Read it again to see the change.".to_string()
        } else if lines > NOTICE_DIFF_LINES {
            format!("{lines} lines changed: Read it again before you rely on any part of it.")
        } else {
            format!("What changed (- before, + now):\n{diff}")
        };
        out.push(format!(
            "memsaga: {} was changed elsewhere (another machine, GitHub, or the data repository) and the live file now holds the new text. The copy of it in your context is outdated: from now on the current file is the only version that counts, in answers and in instructions alike. {what}",
            live(f)
        ));
    }
    if !conflicts.is_empty() {
        out.push(format!(
            "memsaga: these files changed both here and elsewhere on the same lines and were left as they are: {}. Tell the user; `memsaga sync` explains how to settle them.",
            conflicts.iter().map(live).collect::<Vec<_>>().join(", ")
        ));
    }
    (!out.is_empty()).then(|| out.join("\n\n"))
}

/// SessionStart (start, `/compact`, `/clear`): CLAUDE.md is already loaded
/// when this runs (measured, docs/two-way-sync.md §7.1), so a pull here
/// updates the files and the returned notice makes the session read them.
pub fn on_session_start(cfg: &Config) -> Result<Option<String>> {
    let state = State::new(cfg.state_dir());
    let check = check_remote(cfg, CHECK_MAX_AGE)?;
    if !check.moved && load_incoming(&state).is_empty() {
        return Ok(None);
    }
    let (_, settled) = pull_now(cfg)?;
    let files: Vec<String> = changed_files(&settled)
        .into_iter()
        .filter(|f| loaded_at_start(f))
        .collect();
    Ok(notice(cfg, &settled, &files, &settled.conflicts))
}

/// UserPromptSubmit: once per turn, bring in what origin or a hand edit
/// changed, and name the files this session already used.
pub fn on_prompt(cfg: &Config, session: &str) -> Result<Option<String>> {
    let state = State::new(cfg.state_dir());
    refresh_check_in_background(&state);
    if !cached_check(&state).moved && load_incoming(&state).is_empty() {
        return Ok(None);
    }
    let (_, settled) = pull_now(cfg)?;
    let used: BTreeSet<String> = session_file(&state, session)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let files: Vec<String> = changed_files(&settled)
        .into_iter()
        .filter(|f| used.contains(f) || loaded_at_start(f))
        .collect();
    Ok(notice(cfg, &settled, &files, &settled.conflicts))
}

fn changed_files(s: &Settled) -> Vec<String> {
    s.written
        .iter()
        .chain(&s.merged)
        .chain(&s.deleted)
        .cloned()
        .collect()
}

/// What a PreToolUse hook answers.
#[derive(Debug, PartialEq)]
pub enum ToolVerdict {
    Allow,
    /// Refuse the call; the model reads the reason.
    Deny(String),
}

/// PreToolUse for Read, Write and Edit of `live`: when this very file changed
/// on origin or in the data repository, bring it in before the tool runs.
/// Anything outside memsaga's scope returns at once.
///
/// A Read never waits for the check itself (a stale answer starts one in the
/// background). A Write or Edit does when the shared answer is older than
/// `CHECK_MAX_AGE` (about 0.5 s, at most once per window): an edit built on an
/// outdated copy would otherwise end in a merge conflict or silently drop the
/// other side's change. A Write that would overwrite text just pulled in, or a
/// write to a file whose change from origin could not be merged, is refused
/// with the reason, so the model reads the file first.
pub fn before_tool(cfg: &Config, session: &str, live: &Path, tool: &str) -> Result<ToolVerdict> {
    let scope = crate::roots::Scope::new(cfg);
    let in_scope = scope.contains(live).is_some()
        || live == config_path()
        || cfg.extra_files().iter().any(|f| f == live);
    if !in_scope {
        return Ok(ToolVerdict::Allow);
    }
    let Ok(rel) = repo_rel_of_input(cfg, &live.to_string_lossy()) else {
        return Ok(ToolVerdict::Allow);
    };
    let writes = tool != "Read";
    let state = State::new(cfg.state_dir());
    note_session_file(&state, session, &rel);
    // A hand edit in the clone since the last sweep.
    let repo_copy = cfg.repo.join(&rel);
    let edited = match (
        std::fs::metadata(&repo_copy)
            .and_then(|m| m.modified())
            .ok(),
        state.mtime("last-sweep"),
    ) {
        (Some(t), Some(since)) => t > since,
        _ => false,
    };
    if edited {
        note_hand_edit(cfg, &rel)?;
    }
    let check = if writes {
        check_remote(cfg, CHECK_MAX_AGE)?
    } else {
        refresh_check_in_background(&state);
        cached_check(&state)
    };
    let from_origin = check.changed.iter().any(|(f, _)| *f == rel);
    let waiting = from_origin || load_incoming(&state).contains_key(&rel);
    if !waiting {
        return Ok(ToolVerdict::Allow);
    }
    let (merge_error, settled) = pull_now(cfg)?;
    if !writes {
        // A Read goes ahead either way: on the fresh file, or on the live
        // file as it is when the pull could not settle it (the next prompt's
        // notice reports that).
        return Ok(ToolVerdict::Allow);
    }
    let shown = live.display();
    if let (true, Some(e)) = (from_origin, &merge_error) {
        return Ok(ToolVerdict::Deny(format!(
            "memsaga: {shown} has a change on origin that could not be merged automatically ({e}). Do not write it now: tell the user, who can settle it with `memsaga sync`."
        )));
    }
    if settled.conflicts.contains(&rel) {
        return Ok(ToolVerdict::Deny(format!(
            "memsaga: {shown} changed both here and elsewhere on the same lines. Do not write it now: tell the user, who can settle it with `memsaga sync`."
        )));
    }
    let just_changed = changed_files(&settled).contains(&rel);
    if just_changed && tool == "Write" {
        return Ok(ToolVerdict::Deny(format!(
            "memsaga: {shown} was just updated from elsewhere, so content written from the copy you read earlier would undo that update. Read it again, then make your change on top of the current text."
        )));
    }
    Ok(ToolVerdict::Allow)
}

#[cfg(test)]
mod tests {
    use super::github_slug;

    #[test]
    fn github_slug_reads_https_and_ssh_remotes() {
        assert_eq!(
            github_slug("https://github.com/grimmerk/memories.git").as_deref(),
            Some("grimmerk/memories")
        );
        assert_eq!(
            github_slug("git@github.com:grimmerk/memories.git").as_deref(),
            Some("grimmerk/memories")
        );
        assert_eq!(github_slug("https://gitlab.com/a/b.git"), None);
        assert_eq!(github_slug("https://github.com/a/b/c"), None);
    }
}
