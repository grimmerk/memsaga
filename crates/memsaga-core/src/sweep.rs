//! The sweep: take the lock, mirror every root, regenerate the index, commit
//! if anything changed, record an event. Also the fast-path question the hook
//! asks before sweeping: "did anything in scope change since the last sweep?"

use crate::config::Config;
use crate::git::Git;
use crate::index;
use crate::mirror;
use crate::roots::{Entry, Scope};
use crate::state::{self, Event, State};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use walkdir::WalkDir;

/// Who caused this sweep; goes into the commit trailers and the event log.
#[derive(Debug, Clone, Default)]
pub struct Attribution {
    pub event: String,
    pub account: String,
    pub session: String,
    pub cwd: String,
    pub agent: String,
    /// The session transcript, when the hook event named one: the commit
    /// message carries the session's title from it.
    pub transcript: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub committed: bool,
    pub sha: String,
    pub files: Vec<String>,
    pub shortstat: String,
    pub duration_ms: u64,
    pub copied: usize,
    pub deleted: usize,
    /// Files left out of this commit because a new line looks like a secret.
    pub held: Vec<crate::secrets::Hit>,
}

/// Would a sweep find anything? Cheap: stat calls plus one `git status`.
pub fn needs_sweep(cfg: &Config, scope: &Scope, state: &State) -> bool {
    if state.exists("dirty") {
        return true;
    }
    let marker = match state.mtime("last-sweep") {
        Some(m) => m,
        None => return true, // never swept
    };
    // Files the last sweep could not mirror: tried again every
    // `MIRROR_RETRY`, since fixing one (a `chmod`) need not change its mtime.
    if !load_mirror_errors(state).is_empty()
        && marker.elapsed().is_ok_and(|age| age >= MIRROR_RETRY)
    {
        return true;
    }
    // The scope itself changed (a file or dir added to the config, an env
    // override), or the config file was edited: the old marker says nothing
    // about files that were not in scope when it was set.
    if state.read_marker("config-fingerprint").as_deref() != Some(cfg.fingerprint().as_str())
        || state::newer_than(&crate::config::config_path(), marker)
    {
        return true;
    }
    for p in cfg.extra_files() {
        if state::newer_than(&p, marker) {
            return true;
        }
    }
    for root in &cfg.roots {
        // The pre-2026-09-18 layout: move it even when nothing live changed.
        if cfg
            .repo
            .join(&root.label)
            .join("projects")
            .symlink_metadata()
            .is_ok()
        {
            return true;
        }
        // A memory directory still under its live name (the names before
        // the home-relative ones): rename it even when nothing live changed.
        if memory_renames_pending(cfg, root) > 0 {
            return true;
        }
        for e in scope.entries(root) {
            let src = e.source(root);
            match e {
                Entry::File { .. } => {
                    if state::newer_than(&src, marker) {
                        return true;
                    }
                }
                Entry::Dir { .. } | Entry::Memory { .. } => {
                    if tree_newer_than(&src, marker) {
                        return true;
                    }
                }
            }
        }
    }
    // User-owned paths inside the repo (manual/, companion/, hand edits):
    // anything at the repo top level that is not a root subtree, git's own
    // directory, or a generated file. A stat walk, not `git status`, so the
    // no-change case stays a few milliseconds.
    if let Ok(rd) = std::fs::read_dir(&cfg.repo) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git"
                || name == "index.md"
                || name == "paths.json"
                || name == "memsaga"
                || name == "extra"
                || cfg.roots.iter().any(|r| r.label == name)
            {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                if tree_newer_than(&p, marker) {
                    return true;
                }
            } else if state::newer_than(&p, marker) {
                return true;
            }
        }
    }
    false
}

/// Memory directories of `root` in the repository still under a live name
/// that now maps to a home-relative one. A root missing on this machine
/// counts none: the mirror skips it, so nothing could rename them, and
/// counting them would sweep on every Stop.
fn memory_renames_pending(cfg: &Config, root: &crate::config::Root) -> usize {
    if !root.path.is_dir() {
        return 0;
    }
    std::fs::read_dir(cfg.repo.join(&root.label).join(crate::roots::MEMORY_DIR))
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    crate::roots::repo_name_of_slug(&n) != n
                })
                .count()
        })
        .unwrap_or(0)
}

/// Any file or directory under `dir` (following the top-level symlink, not
/// inner ones) modified after `marker`. Directory mtimes change on create and
/// delete, so removals are caught too. A nested git repository is not
/// mirrored, so nothing inside one is looked at (a `git fetch` in a clone
/// would otherwise start a sweep).
fn tree_newer_than(dir: &Path, marker: SystemTime) -> bool {
    // Follow a symlinked top-level dir (the 2nd account links skills/).
    let start = match std::fs::canonicalize(dir) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if mirror::is_nested_repo(&start) {
        return false;
    }
    for item in WalkDir::new(&start)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !(e.depth() > 0 && e.file_type().is_dir() && mirror::is_nested_repo(e.path()))
        })
        .filter_map(|e| e.ok())
    {
        if let Ok(m) = item.metadata() {
            if let Ok(t) = m.modified() {
                if t > marker {
                    return true;
                }
            }
        }
    }
    false
}

/// Copy every root into the repo and regenerate the index, without touching
/// git. Shared by `sweep` and `diff`. The caller holds the lock.
/// Mirror every root, memsaga's own config and `extra_files`, then rebuild the
/// index. Counts go into `stats` as each file is written, so a caller still
/// knows every file written so far when this returns an error part-way.
pub fn refresh_mirror(cfg: &Config, scope: &Scope, stats: &mut mirror::Stats) -> Result<()> {
    for root in &cfg.roots {
        if !root.path.is_dir() {
            continue;
        }
        mirror::sync_root(scope, root, &cfg.repo, stats)
            .with_context(|| format!("mirroring {}", root.path.display()))?;
    }
    mirror::sync_self_and_extras(scope, stats)?;
    index::write_index(cfg)?;
    Ok(())
}

/// Refresh the mirror and return git's view of what a sweep would commit:
/// `(status --short, diff, held)`; `stat_only` asks for `--stat` instead of a
/// full diff. A file the secret check would hold is left out of the diff, so
/// the preview never prints the key either.
pub fn pending(
    cfg: &Config,
    stat_only: bool,
) -> Result<(String, String, Vec<crate::secrets::Hit>)> {
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        anyhow::bail!(
            "{} is not a git repository; run `memsaga init` first",
            cfg.repo.display()
        );
    }
    let state = State::new(cfg.state_dir());
    let locked = crate::recorded::lock_reconciled(cfg, &state, Duration::from_secs(60))?;
    // The preview writes the working tree too: not over a conflict.
    crate::recorded::check_no_conflicts(cfg, &git)?;
    // The same guard as a sweep, so a preview never overwrites a repository-side
    // edit either; what it finds is recorded by the next sweep, not here.
    let incoming = crate::sync::load_incoming(&state)?;
    let scope = Scope::new(cfg).with_guard(crate::sync::Guard::new(
        cfg,
        state.mtime("last-sweep"),
        &incoming,
        &locked,
    ));
    let mut stats = mirror::Stats::default();
    let refreshed = refresh_mirror(cfg, &scope, &mut stats);
    // The copies stay in the clone uncommitted; remember them, or a live file
    // that changes again before the next sweep would make its preview copy
    // look like a hand edit. Recorded before the mirror's own error, if any,
    // is returned: what it wrote before failing is just as uncommitted.
    crate::sync::record_uncommitted(cfg, &state, &stats.written)?;
    crate::sync::mark_seen(cfg, &state, &stats.present)?;
    refreshed?;
    // Only after a complete mirror: a partial list would drop real entries.
    if let Some(g) = &scope.guard {
        crate::sync::save_absent(&state, &g.take_absent())?;
    }
    crate::sync::save_link_over_folder(&state, &scope.take_link_over_folder())?;
    let restat = stats.restat;
    // Stage into a throwaway index so new files show up in the diff without
    // touching whatever the user may have staged in the real one.
    let temp_index = |args: &[&str]| -> Result<String> {
        let out = git.run_with_temp_index(args)?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    // The temp index is recreated per call, so add and diff must be one process each
    // on the same file: use a tiny shell-free sequence through a fixed temp path.
    let tmp = cfg
        .repo
        .join(".git")
        .join(format!("memsaga-index-{}", std::process::id()));
    let real = cfg.repo.join(".git").join("index");
    if real.exists() {
        std::fs::copy(&real, &tmp)?;
    }
    let git_cmd = || {
        let mut c = std::process::Command::new("git");
        c.arg("-C")
            .arg(&cfg.repo)
            .env("GIT_INDEX_FILE", &tmp)
            .env("GIT_TERMINAL_PROMPT", "0");
        c
    };
    let run = |args: &[&str]| -> Result<String> {
        let out = git_cmd().args(args).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    let _ = temp_index; // keep the helper for callers that need a single command
    run(&["add", "-A", "--", "."])?;
    for args in crate::git::renormalize_args(&cfg.repo, &restat) {
        let out = git_cmd().args(&args).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "git add --renormalize failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let held = if cfg.scan_secrets {
        let run_out =
            |args: &[&str]| -> Result<std::process::Output> { Ok(git_cmd().args(args).output()?) };
        match crate::secrets::unstage_held(&run_out, &cfg.allow_secrets) {
            Ok(h) => h,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    } else {
        Vec::new()
    };
    let status = run(&["status", "--short"])?;
    let diff = if stat_only {
        run(&["diff", "--cached", "--stat"])?
    } else {
        run(&["diff", "--cached"])?
    };
    let _ = std::fs::remove_file(&tmp);
    Ok((status, diff, held))
}

/// Mirror, index, commit. Returns what happened. Never writes into a root.
/// A failure is kept in `sweep-error` until a sweep succeeds, so the reporter
/// and `status` say so: a hook's error otherwise reaches only memsaga.log.
pub fn sweep(cfg: &Config, attrib: &Attribution) -> Result<Outcome> {
    let state = State::new(cfg.state_dir());
    let result = sweep_once(cfg, attrib);
    match &result {
        Ok(_) => state.remove(SWEEP_ERROR),
        Err(e) => record_sweep_error(&state, e),
    }
    result
}

fn sweep_once(cfg: &Config, attrib: &Attribution) -> Result<Outcome> {
    let started = Instant::now();
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        anyhow::bail!(
            "{} is not a git repository; run `memsaga init` first",
            cfg.repo.display()
        );
    }
    let state = State::new(cfg.state_dir());
    // A change of HEAD memsaga did not record is recorded first, so the
    // guard below protects what it brought (docs/two-way-sync.md).
    let mut locked = crate::recorded::lock_reconciled(cfg, &state, Duration::from_secs(60))?;
    // `git add -A` below would stage a conflicted file, markers and all.
    crate::recorded::check_no_conflicts(cfg, &git)?;
    let sweep_start = SystemTime::now();

    // Files changed on the repository side are not overwritten (step 1b of
    // docs/two-way-sync.md); hand edits found on the way join the list.
    let mut incoming = crate::sync::load_incoming(&state)?;
    if crate::sync::rename_incoming(cfg, &mut incoming)? {
        crate::sync::save_incoming(&state, &incoming)?;
    }
    let scope = Scope::new(cfg).with_guard(crate::sync::Guard::new(
        cfg,
        state.mtime("last-sweep"),
        &incoming,
        &locked,
    ));
    let renames: usize = cfg
        .roots
        .iter()
        .map(|r| memory_renames_pending(cfg, r))
        .sum();
    let mut stats = mirror::Stats::default();
    let refreshed = refresh_mirror(cfg, &scope, &mut stats);
    // Until the commit below, what the mirror just wrote is memsaga's own
    // uncommitted copy. Record it first, so a sweep that fails or is stopped
    // before committing leaves nothing that looks like a hand edit once the
    // live file changes again: including a mirror that failed part-way, whose
    // error is returned only after the record. Costs one `git hash-object` call, only when
    // files changed: measured 2026-10-15 at +12 to +27 ms CPU and +33 to +66 ms
    // wall on a busy machine, on a sweep that runs in the background anyway
    // (docs/architecture.md, "Measured again"). If that ever matters, hash the
    // content in-process instead (about 30 lines, no git process); the cost is
    // a second kind of hash that the record and the check must both use, and
    // that must stay the same across versions.
    crate::sync::record_uncommitted(cfg, &state, &stats.written)?;
    // What exists live now, this machine has had: from here on its absence
    // is a deletion (grimmerk/memsaga#22).
    crate::sync::mark_seen(cfg, &state, &stats.present)?;
    log_set_aside(&state, attrib, &stats.set_aside)?;
    refreshed?;
    if let Some(g) = &scope.guard {
        if crate::sync::record_hand_edits(&mut incoming, g.take_found()) > 0 {
            crate::sync::save_incoming(&state, &incoming)?;
        }
        crate::sync::save_absent(&state, &g.take_absent())?;
    }
    crate::sync::save_link_over_folder(&state, &scope.take_link_over_folder())?;
    save_mirror_lists(cfg, &state, attrib, &stats)?;
    git.add_renormalize(&stats.restat)?;
    let mut changed = git.add_all_and_check()?;
    let mut out = Outcome {
        copied: stats.copied + stats.linked,
        deleted: stats.deleted,
        ..Default::default()
    };
    if changed && cfg.scan_secrets {
        match hold_secrets(cfg, &git, &stats.written) {
            Ok(held) => out.held = held,
            Err(e) => {
                // Failed closed: nothing is committed. A hook's error only
                // reaches the log, so leave the reason where the reporter and
                // `status` read it; it names files, never the key.
                let reason = format!("{e:#}");
                let _ = state.write_marker(HELD_ERROR, &reason);
                let _ = state.append_event(&Event {
                    ts: state::now_rfc3339(),
                    kind: "error".into(),
                    event: attrib.event.clone(),
                    session: attrib.session.clone(),
                    message: reason,
                    ..Default::default()
                });
                return Err(e);
            }
        }
        if !out.held.is_empty() {
            changed = git.has_staged_changes()?;
        }
    }
    state.remove(HELD_ERROR);
    record_held(&state, attrib, &out.held)?;
    // Whether this sweep's commit, if any, is recorded (`advance_after_commit`).
    let mut recorded = true;
    if changed {
        let name_status = git.staged_name_status()?;
        let files: Vec<String> = name_status
            .lines()
            // `M\tpath`, or `R100\told\tnew` for a rename: the last field is the
            // path that exists after the commit.
            .filter_map(|l| l.split('\t').next_back().map(|s| s.to_string()))
            .collect();
        let shortstat = git.staged_shortstat()?;
        let mut title = session_title(Path::new(&attrib.transcript));
        // The title goes into the commit message: one that holds a key
        // shape is left out rather than committed.
        if cfg.scan_secrets && crate::secrets::text_has_secret(&title) {
            title.clear();
        }
        let mut msg = commit_message(cfg, attrib, &title, &files, &name_status, &shortstat);
        if renames > 0 {
            // The one-time rename gets a subject that says what it is.
            let body = msg.split_once('\n').map(|(_, b)| b).unwrap_or_default();
            msg = format!("layout: {renames} memory directories renamed home-relative\n{body}");
        }
        let made = git.commit(&msg, None)?;
        // Recorded before the uncommitted record is cleared below: stopped in
        // between, the next run finds this commit unrecorded, and the record
        // still names every copy in it as memsaga's own. When someone else's
        // commit landed on top in the same instant, nothing is recorded and
        // the record is kept, so the next run sorts the two out.
        recorded = locked.advance_after_commit(&git, &made)?;
        if !recorded {
            state.log(&format!(
                "HEAD moved past commit {} while it was made; left for the next run to record",
                made.short
            ));
        }
        out.committed = true;
        out.sha = made.short;
        out.files = files;
        out.shortstat = shortstat;
    }
    // Every copy memsaga wrote (a preview's, this sweep's) is now committed, or
    // replaced and committed, or kept as a hand edit made since: the record
    // has done its job. Not earlier: a sweep that fails before this point
    // leaves copies uncommitted, and they must still be recognised next time.
    if recorded {
        crate::sync::clear_uncommitted(&state);
    }
    // Promote the marker only after a successful pass; edits made during the
    // sweep have mtimes after `sweep_start` and are caught next time.
    set_marker(&state, sweep_start)?;
    state.write_marker("config-fingerprint", &cfg.fingerprint())?;
    state.remove("dirty");
    out.duration_ms = started.elapsed().as_millis() as u64;
    let ev = Event {
        ts: state::now_rfc3339(),
        kind: if out.committed { "commit" } else { "noop" }.into(),
        event: attrib.event.clone(),
        account: attrib.account.clone(),
        session: attrib.session.clone(),
        cwd: attrib.cwd.clone(),
        files: out.files.clone(),
        sha: out.sha.clone(),
        duration_ms: out.duration_ms,
        message: out.shortstat.clone(),
    };
    state.append_event(&ev)?;
    // Still under the sweep lock, so no other sweep rotates at the same time.
    if let Err(e) = state.rotate_events_if_over(state::events_max_bytes()) {
        state.log(&format!("events.jsonl rotation failed: {e:#}"));
    }
    Ok(out)
}

/// The secret check on the real index (grimmerk/memsaga#60): take every
/// held file out of the commit, and put memsaga's own copy of it back to its
/// committed version, so the working tree keeps matching `HEAD` there.
///
/// Why the copy must not stay: once the sweep ends, nothing records it as
/// memsaga's own, so two-way sync would read the difference as a hand edit in
/// the repository, and after the user removed the key from the live file it
/// would write the repository copy, key included, back into the live file.
/// The next sweep copies the live file again and holds it again.
/// A file the user wrote in the repository (`manual/`, `companion/`, a hand
/// edit that differs from the live file) is only unstaged, never touched.
/// memsaga's own copy is one this sweep's mirror wrote (`written`), or one
/// equal to what the mirror would write now (an earlier preview's copy); the
/// first holds even when the live file changed again since the copy.
pub fn hold_secrets(
    cfg: &Config,
    git: &Git,
    written: &[std::path::PathBuf],
) -> Result<Vec<crate::secrets::Hit>> {
    let run = |a: &[&str]| git.run(a);
    let held = crate::secrets::unstage_held(&run, &cfg.allow_secrets)?;
    for h in &held {
        let path = cfg.repo.join(&h.path);
        let own = written.contains(&path)
            || crate::sync::live_target(cfg, &h.path)
                .and_then(|t| crate::sync::mirror_form(cfg, &t))
                .is_some_and(|live| std::fs::read(&path).ok().as_deref() == Some(live.as_slice()));
        if !own {
            continue;
        }
        let spec = format!(":(literal){}", h.path);
        if git.object_type(&format!("HEAD:{}", h.path))?.is_some() {
            git.run_ok(&["checkout", "-q", "HEAD", "--", &spec])?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(held)
}

/// Save the held set for `status` and the reporter, and log an event when it
/// changed, so a file held turn after turn is one event, not one per sweep.
fn record_held(state: &State, attrib: &Attribution, held: &[crate::secrets::Hit]) -> Result<()> {
    let before = load_held(state);
    if before == held {
        return Ok(());
    }
    if held.is_empty() {
        state.remove(HELD);
    } else {
        state.write_marker(HELD, &serde_json::to_string(held)?)?;
    }
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "held".into(),
        event: attrib.event.clone(),
        account: attrib.account.clone(),
        session: attrib.session.clone(),
        cwd: attrib.cwd.clone(),
        files: held.iter().map(|h| h.path.clone()).collect(),
        message: held
            .iter()
            .map(|h| format!("{}:{} {} {}", h.path, h.line, h.rule, h.fingerprint))
            .collect::<Vec<_>>()
            .join("; "),
        ..Default::default()
    })
}

/// Files the last sweep held back, under `.git/memsaga/`.
const HELD: &str = "held.json";

/// Why the last sweep failed, and since when sweeps fail (`SweepError`);
/// removed by the next sweep that succeeds.
const SWEEP_ERROR: &str = "sweep-error";

/// Repository paths the last sweep could not mirror, with the reason.
const MIRROR_ERRORS: &str = "mirror-errors.json";

/// Live files the last sweep left out for `max_file_mb`, with their size.
const TOO_LARGE: &str = "too-large.json";

/// Live folders the last sweep left out as git repositories of their own.
const NESTED: &str = "nested.json";

/// How often a sweep runs again while files could not be mirrored.
const MIRROR_RETRY: Duration = Duration::from_secs(600);

/// A failing sweep, as `status` and the reporter show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SweepError {
    /// When the first of the failures in a row happened (RFC 3339).
    pub since: String,
    /// The latest failure's message (file names, never file contents).
    pub message: String,
}

pub fn load_sweep_error(state: &State) -> Option<SweepError> {
    state
        .read_marker(SWEEP_ERROR)
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// Keep the failure, and the time of the first one in a row. Not for a
/// sweep that waited for another one's lock (that one is running), nor for
/// the secret check's own failure (`held-error` already says it).
fn record_sweep_error(state: &State, e: &anyhow::Error) {
    let message = format!("{e:#}");
    if message.contains("could not acquire sweep lock")
        || load_held_error(state).as_deref() == Some(message.as_str())
    {
        return;
    }
    let since = load_sweep_error(state)
        .map(|old| old.since)
        .unwrap_or_else(state::now_rfc3339);
    if let Ok(text) = serde_json::to_string(&SweepError { since, message }) {
        let _ = state.write_marker(SWEEP_ERROR, &text);
    }
}

/// One mirror problem, as `status` shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MirrorError {
    pub path: String,
    pub error: String,
}

/// One file over `max_file_mb`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TooLarge {
    pub path: String,
    pub bytes: u64,
}

pub fn load_mirror_errors(state: &State) -> Vec<MirrorError> {
    load_json(state, MIRROR_ERRORS)
}

pub fn load_too_large(state: &State) -> Vec<TooLarge> {
    load_json(state, TOO_LARGE)
}

pub fn load_nested(state: &State) -> Vec<String> {
    load_json(state, NESTED)
}

fn load_json<T: serde::de::DeserializeOwned + Default>(state: &State, name: &str) -> T {
    std::fs::read_to_string(state.marker(name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Written as the last complete mirror found it; removed when empty.
fn save_json<T: serde::Serialize>(state: &State, name: &str, list: &[T]) -> Result<()> {
    if list.is_empty() {
        state.remove(name);
        return Ok(());
    }
    state.write_marker(name, &serde_json::to_string(list)?)
}

/// Save what the mirror left out (errors, files over the limit, nested
/// repositories) for `status` and the reporter; an event when the set of
/// paths that could not be mirrored changed.
fn save_mirror_lists(
    cfg: &Config,
    state: &State,
    attrib: &Attribution,
    stats: &mirror::Stats,
) -> Result<()> {
    let rel = |p: &Path| {
        p.strip_prefix(&cfg.repo)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string()
    };
    let mut errors: Vec<MirrorError> = stats
        .errors
        .iter()
        .map(|(p, e)| MirrorError {
            path: rel(p),
            error: e.clone(),
        })
        .collect();
    errors.sort_by(|a, b| a.path.cmp(&b.path));
    let before: Vec<String> = load_mirror_errors(state)
        .into_iter()
        .map(|e| e.path)
        .collect();
    let now: Vec<String> = errors.iter().map(|e| e.path.clone()).collect();
    save_json(state, MIRROR_ERRORS, &errors)?;
    if before != now {
        state.append_event(&Event {
            ts: state::now_rfc3339(),
            kind: "mirror-error".into(),
            event: attrib.event.clone(),
            session: attrib.session.clone(),
            files: now,
            message: errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.error))
                .collect::<Vec<_>>()
                .join("; "),
            ..Default::default()
        })?;
    }
    let mut large: Vec<TooLarge> = stats
        .too_large
        .iter()
        .map(|(p, n)| TooLarge {
            path: rel(p),
            bytes: *n,
        })
        .collect();
    large.sort_by(|a, b| a.path.cmp(&b.path));
    save_json(state, TOO_LARGE, &large)?;
    let mut nested: Vec<String> = stats.nested.iter().map(|p| rel(p)).collect();
    nested.sort();
    save_json(state, NESTED, &nested)
}

/// An event for repository folders holding a `.git` copy that the mirror
/// moved aside (where to, in `message`).
fn log_set_aside(state: &State, attrib: &Attribution, moved: &[(PathBuf, PathBuf)]) -> Result<()> {
    if moved.is_empty() {
        return Ok(());
    }
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "set-aside".into(),
        event: attrib.event.clone(),
        session: attrib.session.clone(),
        files: moved
            .iter()
            .map(|(from, _)| from.display().to_string())
            .collect(),
        message: moved
            .iter()
            .map(|(from, to)| format!("{} -> {}", from.display(), to.display()))
            .collect::<Vec<_>>()
            .join("; "),
        ..Default::default()
    })
}

/// Why the last sweep committed nothing because the secret check failed
/// closed; removed by the next sweep that gets past the check.
const HELD_ERROR: &str = "held-error";

pub fn load_held_error(state: &State) -> Option<String> {
    state.read_marker(HELD_ERROR).filter(|s| !s.is_empty())
}

pub fn load_held(state: &State) -> Vec<crate::secrets::Hit> {
    std::fs::read_to_string(state.marker(HELD))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn set_marker(state: &State, at: SystemTime) -> Result<()> {
    state.touch("last-sweep")?;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(state.marker("last-sweep"))?;
    f.set_modified(at)?;
    Ok(())
}

/// The session's title (set by `/rename`, or generated), read backwards from
/// the end of its transcript: Claude Code appends `custom-title` and
/// `agent-name` entries as the session goes, so the latest one is the
/// current title. They usually sit within the last few kilobytes, but large
/// tool output can push them further back (measured up to 4.2 MB), so the
/// search walks back in 1 MiB chunks and gives up after 8 MiB. Not a
/// documented format; anything unexpected yields an empty title. The prompt
/// text is deliberately not read: a first line may carry a pasted secret,
/// and the repo is pushed.
pub fn session_title(transcript: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 1 << 20;
    const LIMIT: u64 = 8 << 20;
    let Ok(mut f) = std::fs::File::open(transcript) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let floor = len.saturating_sub(LIMIT);
    let mut end = len;
    // The start of the line that straddles the previous chunk's start.
    let mut carry: Vec<u8> = Vec::new();
    while end > floor {
        let start = end.saturating_sub(CHUNK).max(floor);
        let mut buf = vec![0u8; (end - start) as usize];
        if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut buf).is_err() {
            return String::new();
        }
        buf.extend_from_slice(&carry);
        // Unless this chunk starts the file, its first line may be partial:
        // keep it for the next (earlier) chunk.
        let first_complete = if start == 0 {
            0
        } else {
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => i + 1,
                None => buf.len(),
            }
        };
        carry = buf[..first_complete].to_vec();
        let text = String::from_utf8_lossy(&buf[first_complete..]);
        for line in text.lines().rev() {
            if let Some(title) = title_entry(line) {
                return title;
            }
        }
        end = start;
    }
    if floor == 0 {
        return title_entry(&String::from_utf8_lossy(&carry)).unwrap_or_default();
    }
    String::new()
}

/// The title a `custom-title` or `agent-name` entry carries, if `line` is one.
fn title_entry(line: &str) -> Option<String> {
    // Skip the bulk (messages, tool output) without parsing it.
    if !(line.contains("\"custom-title\"") || line.contains("\"agent-name\"")) {
        return None;
    }
    let v = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    let title = match field("type") {
        "custom-title" => one_line(field("customTitle")),
        "agent-name" => one_line(field("agentName")),
        _ => return None,
    };
    (!title.is_empty()).then_some(title)
}

/// The first non-blank line, trimmed. Kept whole for the trailer, so a
/// later search by title matches the full sentence; only a runaway value
/// is capped (1000 characters). The subject cuts its own copy.
fn one_line(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    truncate_chars(line, 1000)
}

/// Subject: `[<session title>] <label>:<area>, …`, or `<event>: …` when the
/// session has no title; at most 72 characters, the title cut to 32 so the
/// areas keep room (the full title is in the trailer). Body: name-status;
/// trailers, with `Session-Title` when the transcript gave one.
pub fn commit_message(
    cfg: &Config,
    attrib: &Attribution,
    title: &str,
    files: &[String],
    name_status: &str,
    shortstat: &str,
) -> String {
    let mut areas: Vec<String> = files.iter().map(|f| area_of(cfg, f)).collect();
    areas.sort();
    areas.dedup();
    let head = if title.is_empty() {
        format!("{}:", attrib.event)
    } else {
        format!("[{}]", truncate_chars(title, 32))
    };
    let subject = truncate_chars(&format!("{head} {}", areas.join(", ")), 72);
    let body: String = name_status.lines().take(40).collect::<Vec<_>>().join("\n");
    let mut msg = format!(
        "{subject}\n\n{shortstat}\n{body}\n\nEvent: {}\nAccount: {}\nSession: {}\nCwd: {}\nAgent: {}\n",
        attrib.event,
        or_none(&attrib.account),
        or_none(&attrib.session),
        or_none(&attrib.cwd),
        if attrib.agent.is_empty() { "main" } else { &attrib.agent },
    );
    if !title.is_empty() {
        msg.push_str(&format!("Session-Title: {title}\n"));
    }
    msg
}

/// At most `max` characters, ending in `...` when cut (with no space
/// before the dots).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars()
            .take(max - 3)
            .collect::<String>()
            .trim_end()
            .to_string()
            + "..."
    } else {
        s.to_string()
    }
}

fn or_none(s: &str) -> &str {
    if s.is_empty() {
        "none"
    } else {
        s
    }
}

/// `claude/memory/-x-a/f.md` -> `claude:memory:-x-a` (so does the pre-2026-09-18
/// `claude/projects/-x-a/memory/f.md`, seen as deletions in the migration commit);
/// `claude/skills/foo/SKILL.md` -> `claude:skills`; `claude/CLAUDE.md` -> `claude:CLAUDE.md`;
/// anything outside a root subtree -> its first component (`companion`, `manual`).
fn area_of(cfg: &Config, file: &str) -> String {
    let parts: Vec<&str> = file.split('/').collect();
    let label = parts.first().copied().unwrap_or("");
    let is_root = cfg.roots.iter().any(|r| r.label == label);
    if !is_root {
        return label.to_string();
    }
    match parts.get(1).copied() {
        Some("memory") | Some("projects") => {
            format!("{label}:memory:{}", parts.get(2).copied().unwrap_or("?"))
        }
        Some(dir) if cfg.dirs.iter().any(|d| d == dir) => format!("{label}:{dir}"),
        Some(f) => format!("{label}:{f}"),
        None => label.to_string(),
    }
}
