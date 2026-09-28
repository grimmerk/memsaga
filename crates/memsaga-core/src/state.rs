//! Small state files under `<repo>/.git/memsaga/`. Nothing here is versioned.
//!
//! - `lock`: exclusive advisory lock (flock) held during a sweep.
//!   Several Claude sessions fire hooks at the same moment; the
//!   lock serialises their sweeps. It lives on an open file
//!   descriptor, so a crashed process releases it automatically
//!   and there is no stale-lock case to handle.
//! - `last-sweep`: empty file whose mtime is the start of the last sweep; the
//!   fast path compares source mtimes against it.
//! - `dirty`: appended by the hook when an in-scope file was edited during
//!   the turn; the Stop sweep consumes it.
//! - `events.jsonl`: one JSON object per sweep or push; `status` reads it.
//! - `last-push`: mtime = last successful push (throttling).
//! - `memsaga.log`: free-form log lines for debugging.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct State {
    pub dir: PathBuf,
}

/// Holding this value holds the sweep lock; dropping it releases it.
pub struct LockGuard {
    _file: File,
}

/// One line of `events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Event {
    pub ts: String,
    /// "commit" | "noop" | "push" | "push-failed" | "error" | "backfill" | "import"
    pub kind: String,
    /// Hook event name, or "manual" / "init".
    pub event: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub message: String,
}

impl State {
    pub fn new(state_dir: PathBuf) -> State {
        State { dir: state_dir }
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))
    }

    /// Acquire the sweep lock, polling until `timeout`.
    pub fn lock(&self, timeout: Duration) -> Result<LockGuard> {
        self.lock_named("lock", timeout)
    }

    /// The push lock: pushes serialise among themselves without blocking
    /// sweeps behind a slow remote.
    pub fn push_lock(&self, timeout: Duration) -> Result<LockGuard> {
        self.lock_named("push-lock", timeout)
    }

    fn lock_named(&self, name: &str, timeout: Duration) -> Result<LockGuard> {
        self.ensure()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(name))?;
        let start = SystemTime::now();
        loop {
            // flock(2): exclusive, non-blocking. EWOULDBLOCK while another process holds it.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(LockGuard { _file: file });
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::WouldBlock {
                return Err(err).context("flock on sweep lock");
            }
            if start.elapsed().unwrap_or_default() > timeout {
                anyhow::bail!("could not acquire sweep lock within {:?}", timeout);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn marker(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn touch(&self, name: &str) -> Result<()> {
        self.ensure()?;
        let p = self.marker(name);
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)?;
        f.set_modified(SystemTime::now())?;
        Ok(())
    }

    pub fn mtime(&self, name: &str) -> Option<SystemTime> {
        std::fs::metadata(self.marker(name))
            .and_then(|m| m.modified())
            .ok()
    }

    pub fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.marker(name));
    }

    pub fn exists(&self, name: &str) -> bool {
        self.marker(name).exists()
    }

    /// Contents of a marker file, trimmed; None when absent.
    pub fn read_marker(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.marker(name))
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Replace a marker file's contents.
    pub fn write_marker(&self, name: &str, value: &str) -> Result<()> {
        self.ensure()?;
        std::fs::write(self.marker(name), value)?;
        Ok(())
    }

    pub fn write_dirty(&self, info: &str) -> Result<()> {
        self.ensure()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.marker("dirty"))?;
        writeln!(f, "{info}")?;
        Ok(())
    }

    pub fn log(&self, line: &str) {
        if self.ensure().is_err() {
            return;
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.marker("memsaga.log"))
        {
            let _ = writeln!(f, "{} {}", now_rfc3339(), line);
        }
    }

    pub fn append_event(&self, ev: &Event) -> Result<()> {
        self.ensure()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.marker("events.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(ev)?)?;
        Ok(())
    }

    pub fn read_events(&self) -> Vec<Event> {
        let text = std::fs::read_to_string(self.marker("events.jsonl")).unwrap_or_default();
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Events appended after byte offset `from`, plus the new end offset.
    pub fn read_events_since(&self, from: u64) -> (Vec<Event>, u64) {
        let text = std::fs::read_to_string(self.marker("events.jsonl")).unwrap_or_default();
        let total = text.len() as u64;
        if from as usize >= text.len() {
            return (Vec::new(), total);
        }
        let slice = &text[from as usize..];
        (
            slice
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
            total,
        )
    }
}

pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Is `p` (following symlinks) newer than `marker`?
pub fn newer_than(p: &Path, marker: SystemTime) -> bool {
    std::fs::metadata(p)
        .and_then(|md| md.modified())
        .map(|t| t > marker)
        .unwrap_or(false)
}
