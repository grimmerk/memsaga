//! Turning a Claude Code hook event into an action.
//!
//! - `PostToolUse` (Write/Edit), `PostToolBatch`, `FileChanged`: if any path in
//!   the event is in scope, either sweep now (granularity = edit) or just mark
//!   the turn dirty (granularity = turn, the default).
//! - `Stop`, `SessionEnd`: sweep if dirty or anything is newer than the last
//!   sweep; then push, throttled on Stop and unconditionally on SessionEnd.
//! - `report`: the small synchronous hook that prints a one-line notice for
//!   commits made since the last report.

use crate::config::{Config, Granularity};
use crate::git::Git;
use crate::roots::Scope;
use crate::state::{self, Event, State};
use crate::sweep::{self, Attribution, Outcome};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Deserialize, Default)]
pub struct HookInput {
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_calls: Vec<serde_json::Value>,
    #[serde(default)]
    pub agent_type: String,
    /// FileChanged only.
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub transcript_path: String,
}

impl HookInput {
    pub fn parse(text: &str) -> HookInput {
        serde_json::from_str(text).unwrap_or_default()
    }

    /// Every file path the event mentions.
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !self.file_path.is_empty() {
            out.push(PathBuf::from(&self.file_path));
        }
        if let Some(p) = self.tool_input.get("file_path").and_then(|v| v.as_str()) {
            out.push(PathBuf::from(p));
        }
        for c in &self.tool_calls {
            if let Some(p) = c
                .get("tool_input")
                .and_then(|t| t.get("file_path"))
                .and_then(|v| v.as_str())
            {
                out.push(PathBuf::from(p));
            }
        }
        out
    }

    pub fn attribution(&self, cfg: &Config) -> Attribution {
        Attribution {
            event: if self.hook_event_name.is_empty() {
                "manual".into()
            } else {
                self.hook_event_name.clone()
            },
            account: cfg
                .current_root()
                .map(|r| r.label.clone())
                .unwrap_or_default(),
            session: self.session_id.clone(),
            cwd: self.cwd.clone(),
            agent: self.agent_type.clone(),
            transcript: self.transcript_path.clone(),
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Ignored,
    MarkedDirty,
    Swept(Outcome),
    Pushed,
    /// A notice for the model (`additionalContext`) from the hook named.
    Notice {
        event: String,
        text: String,
    },
    /// PreToolUse refuses the call; the model reads the reason.
    Deny(String),
}

pub fn handle(cfg: &Config, input: &HookInput) -> Result<Action> {
    let scope = Scope::new(cfg);
    let state = State::new(cfg.state_dir());
    match input.hook_event_name.as_str() {
        "PostToolUse" | "PostToolBatch" | "FileChanged" => {
            let hit = input.paths().iter().any(|p| scope.contains(p).is_some());
            if !hit {
                return Ok(Action::Ignored);
            }
            match cfg.granularity {
                Granularity::Edit => Ok(Action::Swept(sweep::sweep(cfg, &input.attribution(cfg))?)),
                Granularity::Turn => {
                    state.write_dirty(&format!(
                        "{} {} {} {}",
                        state::now_rfc3339(),
                        input.hook_event_name,
                        input.session_id,
                        input
                            .paths()
                            .first()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    ))?;
                    Ok(Action::MarkedDirty)
                }
            }
        }
        "Stop" | "SessionEnd" => {
            let mut action = Action::Ignored;
            let attrib = input.attribution(cfg);
            if sweep::needs_sweep(cfg, &scope, &state) {
                action = Action::Swept(sweep::sweep(cfg, &attrib)?);
            }
            let session_end = input.hook_event_name == "SessionEnd";
            if session_end {
                crate::sync::forget_session(cfg, &input.session_id);
            }
            if maybe_push(cfg, &state, session_end, &attrib)? && matches!(action, Action::Ignored) {
                action = Action::Pushed;
            }
            Ok(action)
        }
        // Two-way sync, step 3 (docs/two-way-sync.md §7): synchronous hooks
        // that bring repository-side edits in before a stale copy is used.
        // A Claude Code mod could do the same in process (§7.2: `prompt.context`
        // replacing the loaded CLAUDE.md, `tool.call` filtering by path without
        // a process per call); these entry points are kept small for that.
        "SessionStart" => notice_action(input, crate::sync::on_session_start(cfg)?),
        "UserPromptSubmit" => notice_action(
            input,
            crate::sync::on_prompt(
                cfg,
                &input.session_id,
                std::path::Path::new(&input.transcript_path),
            )?,
        ),
        "PreToolUse" => {
            let Some(p) = input.paths().first().cloned() else {
                return Ok(Action::Ignored);
            };
            Ok(
                match crate::sync::before_tool(cfg, &input.session_id, &p, &input.tool_name)? {
                    crate::sync::ToolVerdict::Allow => Action::Ignored,
                    crate::sync::ToolVerdict::Deny(reason) => Action::Deny(reason),
                },
            )
        }
        _ => Ok(Action::Ignored),
    }
}

fn notice_action(input: &HookInput, text: Option<String>) -> Result<Action> {
    Ok(match text {
        Some(text) => Action::Notice {
            event: input.hook_event_name.clone(),
            text,
        },
        None => Action::Ignored,
    })
}

/// Push when a remote exists, the branch is ahead, and either this is a
/// SessionEnd or the throttle interval has elapsed. Inside the interval the
/// push is deferred, not dropped: a detached `memsaga push --after N` waits
/// out the remainder and pushes, so a commit made by a session's last turn
/// still reaches origin within one interval even if no Stop ever follows
/// (measured 2026-09-20: a commit 30 s inside the window sat unpushed for
/// 16 minutes). Failures are logged as events, never raised.
pub fn maybe_push(
    cfg: &Config,
    state: &State,
    session_end: bool,
    attrib: &Attribution,
) -> Result<bool> {
    let git = Git::new(&cfg.repo);
    if !git.has_remote("origin") {
        return Ok(false);
    }
    if session_end && !cfg.push_on_session_end {
        return Ok(false);
    }
    if !session_end {
        if cfg.push_min_interval_secs == 0 {
            return Ok(false);
        }
        if let Some(last) = state.mtime("last-push") {
            let elapsed = last.elapsed().unwrap_or(Duration::ZERO);
            let interval = Duration::from_secs(cfg.push_min_interval_secs);
            if elapsed < interval {
                if !nothing_to_push(&git) {
                    defer_push(state, interval - elapsed, attrib)?;
                }
                return Ok(false);
            }
        }
    }
    if nothing_to_push(&git) {
        return Ok(false);
    }
    push_now(cfg, state, attrib)
}

/// True when origin's tracking branch already has everything on HEAD.
fn nothing_to_push(git: &Git) -> bool {
    git.ahead_of("origin") == 0
        && git
            .run(&["rev-parse", "--verify", "-q", "origin/HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// `git push origin` now, recording the outcome as an event. The caller has
/// decided that a push is wanted; this does not consult the throttle.
pub fn push_now(cfg: &Config, state: &State, attrib: &Attribution) -> Result<bool> {
    let first = try_push(cfg, state)?;
    let error = match first {
        Ok(()) => return record_push(state, attrib, None),
        Err(e) => e,
    };
    // Rejected because origin has commits we lack (an edit on GitHub,
    // another machine): merge them and push once more. Any other failure
    // (no network, no permission) is recorded as it is. The merge takes the
    // sweep lock; the push lock is not held meanwhile, so the two locks are
    // never held together (docs/two-way-sync.md, step 1a).
    if !rejected_as_behind(&error) {
        return record_push(state, attrib, Some(error));
    }
    match crate::sync::integrate_remote(cfg) {
        Ok(i) if i.merged => {
            let second = try_push(cfg, state)?;
            record_push(state, attrib, second.err())
        }
        Ok(_) => record_push(state, attrib, Some(error)),
        Err(e) => record_push(state, attrib, Some(e)),
    }
}

/// Did origin refuse the push because it has commits we lack? git says
/// "non-fast-forward" or "fetch first" for that.
fn rejected_as_behind(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    text.contains("non-fast-forward") || text.contains("fetch first")
}

/// One `git push origin` under the push lock. One push at a time: a Stop
/// push and a deferred push racing each other made GitHub reject the later
/// one (2026-09-28). A push-specific lock, so a stalled remote never blocks
/// sweeps.
fn try_push(cfg: &Config, state: &State) -> Result<std::result::Result<(), anyhow::Error>> {
    let _guard = state
        .push_lock(Duration::from_secs(60))
        .context("push lock")?;
    Ok(Git::new(&cfg.repo).push("origin"))
}

/// Record a push outcome as an event and in the `push-error` marker.
fn record_push(state: &State, attrib: &Attribution, error: Option<anyhow::Error>) -> Result<bool> {
    let mut ev = Event {
        ts: state::now_rfc3339(),
        event: attrib.event.clone(),
        account: attrib.account.clone(),
        session: attrib.session.clone(),
        cwd: attrib.cwd.clone(),
        ..Default::default()
    };
    match error {
        None => {
            state.touch("last-push")?;
            state.remove("push-error");
            ev.kind = "push".into();
            state.append_event(&ev)?;
            Ok(true)
        }
        Some(e) => {
            let text = format!("{e:#}");
            ev.kind = "push-failed".into();
            ev.message = text.clone();
            state.append_event(&ev)?;
            state.log(&format!("push failed: {text}"));
            state.write_marker("push-error", &text)?;
            Ok(false)
        }
    }
}

/// Schedule one push for when the throttle window ends. The `push-pending`
/// marker holds the scheduled unix time; while it lies in the future a waiter
/// is assumed alive and no second one is started. The waiter is a detached
/// process (its own session, no stdio), so it outlives the hook that spawned
/// it and whatever process group Claude Code tears down.
fn defer_push(state: &State, wait: Duration, attrib: &Attribution) -> Result<()> {
    // Check-and-claim under the state lock so two concurrent Stops cannot
    // both start a waiter.
    let _guard = state.lock(Duration::from_secs(5))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(due) = state
        .read_marker("push-pending")
        .and_then(|s| s.parse::<u64>().ok())
    {
        if due > now {
            return Ok(());
        }
    }
    let secs = (wait.as_secs_f64().ceil() as u64).max(1);
    state.write_marker("push-pending", &(now + secs).to_string())?;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["push", "--after", &secs.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid only detaches the child from the caller's session and
    // controlling terminal; it takes no arguments and touches no shared state.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_) => {
            state.append_event(&Event {
                ts: state::now_rfc3339(),
                kind: "push-deferred".into(),
                event: attrib.event.clone(),
                account: attrib.account.clone(),
                session: attrib.session.clone(),
                cwd: attrib.cwd.clone(),
                message: format!("push in {secs}s"),
                ..Default::default()
            })?;
            state.log(&format!("{}: push deferred {secs}s", attrib.event));
        }
        Err(e) => {
            state.remove("push-pending");
            state.log(&format!("could not start the deferred push: {e}"));
        }
    }
    Ok(())
}

/// Body of `memsaga push --after N`: sleep, then push whatever is ahead.
/// Runs detached; nothing is printed, everything goes to the state dir.
pub fn deferred_push(cfg: &Config, after_secs: u64) -> Result<()> {
    std::thread::sleep(Duration::from_secs(after_secs));
    let state = State::new(cfg.state_dir());
    // A SessionEnd or manual push may have happened while sleeping; keep the
    // at-most-one-push-per-interval promise relative to that newer push.
    let interval = Duration::from_secs(cfg.push_min_interval_secs);
    loop {
        let remaining = state
            .mtime("last-push")
            .and_then(|t| t.elapsed().ok())
            .map(|e| interval.saturating_sub(e))
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining);
    }
    let git = Git::new(&cfg.repo);
    let attrib = Attribution {
        event: "deferred".into(),
        ..Default::default()
    };
    if git.has_remote("origin") && !nothing_to_push(&git) {
        push_now(cfg, &state, &attrib)?;
    }
    state.remove("push-pending");
    Ok(())
}

/// The reporter: a one-line notice for commits since the last report, and
/// a warning while the config file is missing; None when there is neither.
/// Kept to a few milliseconds: reads a byte offset and the tail of events.jsonl.
pub fn report(cfg: &Config) -> Option<String> {
    let warning = crate::status::config_missing_warning(cfg);
    if !cfg.reporter {
        return warning;
    }
    let state = State::new(cfg.state_dir());
    let offset = state.read_marker("report-offset").unwrap_or_default();
    let (events, new_offset) = state.read_events_since(&offset);
    let _ = state.write_marker("report-offset", &new_offset);
    let commits: Vec<&Event> = events.iter().filter(|e| e.kind == "commit").collect();
    let notice = (!commits.is_empty()).then(|| {
        let files: usize = commits.iter().map(|e| e.files.len()).sum();
        let shas: Vec<&str> = commits.iter().map(|e| e.sha.as_str()).collect();
        format!(
            "memsaga: {} file{} committed ({})",
            files,
            if files == 1 { "" } else { "s" },
            shas.join(", ")
        )
    });
    let lines: Vec<String> = [warning, notice, incoming_notice(&state)]
        .into_iter()
        .flatten()
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// One line when the set of files waiting for `memsaga sync` changed since
/// the last report (a new merge, a hand edit, a conflict left over), so a
/// pending sync is never silent but a lingering one does not repeat.
fn incoming_notice(state: &State) -> Option<String> {
    let incoming = crate::sync::load_incoming(state);
    let key = incoming.keys().cloned().collect::<Vec<_>>().join("\n");
    if state.read_marker("incoming-reported").unwrap_or_default() == key {
        return None;
    }
    let _ = state.write_marker("incoming-reported", &key);
    if incoming.is_empty() {
        return None;
    }
    let n = incoming.len();
    let names: Vec<&str> = incoming.keys().take(3).map(String::as_str).collect();
    Some(format!(
        "memsaga: {n} file{} changed in the repository and not yet in the live files ({}{}); run `memsaga sync`",
        if n == 1 { "" } else { "s" },
        names.join(", "),
        if n > 3 { ", ..." } else { "" }
    ))
}
