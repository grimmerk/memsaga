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
use anyhow::Result;
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
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Ignored,
    MarkedDirty,
    Swept(Outcome),
    Pushed,
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
            if sweep::needs_sweep(cfg, &scope, &state) {
                action = Action::Swept(sweep::sweep(cfg, &input.attribution(cfg))?);
            }
            let session_end = input.hook_event_name == "SessionEnd";
            if maybe_push(cfg, &state, session_end, &input.attribution(cfg))?
                && matches!(action, Action::Ignored)
            {
                action = Action::Pushed;
            }
            Ok(action)
        }
        _ => Ok(Action::Ignored),
    }
}

/// Push when a remote exists, the branch is ahead, and either this is a
/// SessionEnd or the throttle interval has elapsed. Failures are logged as
/// events, never raised: the next opportunity retries.
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
            if elapsed < Duration::from_secs(cfg.push_min_interval_secs) {
                return Ok(false);
            }
        }
    }
    if git.ahead_of("origin") == 0
        && git
            .run(&["rev-parse", "--verify", "-q", "origin/HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false)
    {
        return Ok(false);
    }
    let mut ev = Event {
        ts: state::now_rfc3339(),
        event: attrib.event.clone(),
        account: attrib.account.clone(),
        session: attrib.session.clone(),
        cwd: attrib.cwd.clone(),
        ..Default::default()
    };
    match git.push("origin") {
        Ok(()) => {
            state.touch("last-push")?;
            ev.kind = "push".into();
            state.append_event(&ev)?;
            Ok(true)
        }
        Err(e) => {
            ev.kind = "push-failed".into();
            ev.message = e.to_string();
            state.append_event(&ev)?;
            state.log(&format!("push failed: {e}"));
            Ok(false)
        }
    }
}

/// The reporter: commits since the last report, as a one-line notice, or None.
/// Kept to a few milliseconds: reads a byte offset and the tail of events.jsonl.
pub fn report(cfg: &Config) -> Option<String> {
    if !cfg.reporter {
        return None;
    }
    let state = State::new(cfg.state_dir());
    let offset: u64 = std::fs::read_to_string(state.marker("report-offset"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let (events, new_offset) = state.read_events_since(offset);
    let _ = std::fs::write(state.marker("report-offset"), new_offset.to_string());
    let commits: Vec<&Event> = events.iter().filter(|e| e.kind == "commit").collect();
    if commits.is_empty() {
        return None;
    }
    let files: usize = commits.iter().map(|e| e.files.len()).sum();
    let shas: Vec<&str> = commits.iter().map(|e| e.sha.as_str()).collect();
    Some(format!(
        "memsaga: {} file{} committed ({})",
        files,
        if files == 1 { "" } else { "s" },
        shas.join(", ")
    ))
}
