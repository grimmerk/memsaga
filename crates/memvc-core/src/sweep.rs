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
use std::path::Path;
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

/// Any file or directory under `dir` (following the top-level symlink, not
/// inner ones) modified after `marker`. Directory mtimes change on create and
/// delete, so removals are caught too.
fn tree_newer_than(dir: &Path, marker: SystemTime) -> bool {
    // Follow a symlinked top-level dir (the 2nd account links skills/).
    let start = match std::fs::canonicalize(dir) {
        Ok(p) => p,
        Err(_) => return false,
    };
    for item in WalkDir::new(&start)
        .follow_links(false)
        .into_iter()
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
pub fn refresh_mirror(cfg: &Config, scope: &Scope) -> Result<(usize, usize)> {
    let mut copied = 0;
    let mut deleted = 0;
    for root in &cfg.roots {
        if !root.path.is_dir() {
            continue;
        }
        let s = mirror::sync_root(scope, root, &cfg.repo)
            .with_context(|| format!("mirroring {}", root.path.display()))?;
        copied += s.copied + s.linked;
        deleted += s.deleted;
    }
    index::write_index(cfg, scope)?;
    Ok((copied, deleted))
}

/// Refresh the mirror and return git's view of what a sweep would commit:
/// `(status --short, diff)`; `stat_only` asks for `--stat` instead of a full diff.
pub fn pending(cfg: &Config, stat_only: bool) -> Result<(String, String)> {
    let scope = Scope::new(cfg);
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        anyhow::bail!(
            "{} is not a git repository; run `memvc init` first",
            cfg.repo.display()
        );
    }
    let state = State::new(cfg.state_dir());
    let _guard = state.lock(Duration::from_secs(20)).context("sweep lock")?;
    refresh_mirror(cfg, &scope)?;
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
        .join(format!("memvc-index-{}", std::process::id()));
    let real = cfg.repo.join(".git").join("index");
    if real.exists() {
        std::fs::copy(&real, &tmp)?;
    }
    let run = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&cfg.repo)
            .env("GIT_INDEX_FILE", &tmp)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    let _ = temp_index; // keep the helper for callers that need a single command
    run(&["add", "-A", "--", "."])?;
    let status = run(&["status", "--short"])?;
    let diff = if stat_only {
        run(&["diff", "--cached", "--stat"])?
    } else {
        run(&["diff", "--cached"])?
    };
    let _ = std::fs::remove_file(&tmp);
    Ok((status, diff))
}

/// Mirror, index, commit. Returns what happened. Never writes into a root.
pub fn sweep(cfg: &Config, attrib: &Attribution) -> Result<Outcome> {
    let started = Instant::now();
    let scope = Scope::new(cfg);
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        anyhow::bail!(
            "{} is not a git repository; run `memvc init` first",
            cfg.repo.display()
        );
    }
    let state = State::new(cfg.state_dir());
    let _guard = state.lock(Duration::from_secs(20)).context("sweep lock")?;
    let sweep_start = SystemTime::now();

    let (copied, deleted) = refresh_mirror(cfg, &scope)?;

    let changed = git.add_all_and_check()?;
    let mut out = Outcome {
        copied,
        deleted,
        ..Default::default()
    };
    if changed {
        let name_status = git.staged_name_status()?;
        let files: Vec<String> = name_status
            .lines()
            // `M\tpath`, or `R100\told\tnew` for a rename: the last field is the
            // path that exists after the commit.
            .filter_map(|l| l.split('\t').next_back().map(|s| s.to_string()))
            .collect();
        let shortstat = git.staged_shortstat()?;
        let msg = commit_message(cfg, attrib, &files, &name_status, &shortstat);
        let sha = git.commit(&msg, None)?;
        out.committed = true;
        out.sha = sha;
        out.files = files;
        out.shortstat = shortstat;
    }
    // Promote the marker only after a successful pass; edits made during the
    // sweep have mtimes after `sweep_start` and are caught next time.
    set_marker(&state, sweep_start)?;
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
    Ok(out)
}

fn set_marker(state: &State, at: SystemTime) -> Result<()> {
    state.touch("last-sweep")?;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(state.marker("last-sweep"))?;
    f.set_modified(at)?;
    Ok(())
}

/// Subject: `<event>: <label>:<area>, …`; body: name-status; trailers.
pub fn commit_message(
    cfg: &Config,
    attrib: &Attribution,
    files: &[String],
    name_status: &str,
    shortstat: &str,
) -> String {
    let mut areas: Vec<String> = files.iter().map(|f| area_of(cfg, f)).collect();
    areas.sort();
    areas.dedup();
    let mut subject = format!("{}: {}", attrib.event, areas.join(", "));
    if subject.chars().count() > 72 {
        subject = subject.chars().take(69).collect::<String>() + "...";
    }
    let body: String = name_status.lines().take(40).collect::<Vec<_>>().join("\n");
    format!(
        "{subject}\n\n{shortstat}\n{body}\n\nEvent: {}\nAccount: {}\nSession: {}\nCwd: {}\nAgent: {}\n",
        attrib.event,
        or_none(&attrib.account),
        or_none(&attrib.session),
        or_none(&attrib.cwd),
        if attrib.agent.is_empty() { "main" } else { &attrib.agent },
    )
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
