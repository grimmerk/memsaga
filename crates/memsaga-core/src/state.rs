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
//!   A sweep moves it aside as `events-<time>.jsonl` past 1 MiB.
//! - `report-offset`: how far the reporter has read (`<inode>:<offset>`).
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

    /// Events appended after position `from` (from an earlier call; empty
    /// means the start), and the position to pass next time.
    ///
    /// A position is `<inode>:<offset>` of the file it points into. When
    /// `events.jsonl` has another inode, the log was rotated in between: the
    /// rest of the rotated file it pointed into (a rename keeps the inode) is
    /// read, then every file rotated after it, then the current file from its
    /// start. A bare offset from before rotation existed is still accepted;
    /// one past the end is taken as "rotated, which file unknown".
    pub fn read_events_since(&self, from: &str) -> (Vec<Event>, String) {
        use std::os::unix::fs::MetadataExt;
        let (from_ino, from_off) = match from.split_once(':') {
            Some((i, o)) => (i.parse::<u64>().ok(), o.parse::<u64>().unwrap_or(0)),
            None => (None, from.parse::<u64>().unwrap_or(0)),
        };
        let current = std::fs::metadata(self.marker("events.jsonl")).ok();
        let cur_ino = current.as_ref().map(|m| m.ino());
        let bytes = std::fs::read(self.marker("events.jsonl")).unwrap_or_default();
        let mut events = Vec::new();
        let mut position = None;
        let start = match from_ino {
            Some(ino) if Some(ino) != cur_ino => {
                let rotated = self.rotated_logs();
                if let Some(i) = rotated.iter().position(|(_, rino)| *rino == ino) {
                    for (k, (path, rino)) in rotated[i..].iter().enumerate() {
                        let b = std::fs::read(path).unwrap_or_default();
                        let off = if k == 0 { from_off } else { 0 };
                        events.extend(parse_events(tail(&b, off)));
                        position = Some(format!("{rino}:{}", b.len()));
                    }
                }
                0
            }
            _ if from_off > bytes.len() as u64 => 0,
            _ => from_off,
        };
        events.extend(parse_events(tail(&bytes, start)));
        let position = match cur_ino {
            Some(ino) => format!("{ino}:{}", bytes.len()),
            // No current file (just rotated): stay where the reading ended.
            None => position.unwrap_or_else(|| from.to_string()),
        };
        (events, position)
    }

    /// Rotated logs (`events-*.jsonl`) with their inodes, oldest first.
    fn rotated_logs(&self) -> Vec<(PathBuf, u64)> {
        use std::os::unix::fs::MetadataExt;
        let mut out: Vec<(SystemTime, PathBuf, u64)> = std::fs::read_dir(&self.dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy())
                    .is_some_and(|n| n.starts_with("events-") && n.ends_with(".jsonl"))
            })
            .filter_map(|p| {
                let m = std::fs::metadata(&p).ok()?;
                Some((m.modified().ok()?, p, m.ino()))
            })
            .collect();
        out.sort();
        out.into_iter().map(|(_, p, ino)| (p, ino)).collect()
    }

    /// Move `events.jsonl` aside as `events-<local time>.jsonl` once it is
    /// larger than `max` bytes; the next event starts a new file. The caller
    /// holds the sweep lock, so two sweeps cannot rotate at once. Rotated
    /// files are kept: about 20 MB a year at the measured rate.
    pub fn rotate_events_if_over(&self, max: u64) -> Result<Option<PathBuf>> {
        let current = self.marker("events.jsonl");
        let len = std::fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
        if len <= max {
            return Ok(None);
        }
        let stem = format!(
            "events-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
        );
        let mut target = self.dir.join(format!("{stem}.jsonl"));
        let mut n = 1;
        while target.exists() {
            target = self.dir.join(format!("{stem}-{n}.jsonl"));
            n += 1;
        }
        std::fs::rename(&current, &target)
            .with_context(|| format!("rotating {}", current.display()))?;
        Ok(Some(target))
    }
}

/// Size above which a sweep rotates `events.jsonl`: 1 MiB (about three
/// weeks at the measured 45 KB a day), or `MEMSAGA_EVENTS_MAX_BYTES`.
pub fn events_max_bytes() -> u64 {
    std::env::var("MEMSAGA_EVENTS_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1 << 20)
}

/// `bytes` from `offset` on, or nothing when the offset is past the end.
fn tail(bytes: &[u8], offset: u64) -> &[u8] {
    bytes.get(offset as usize..).unwrap_or_default()
}

fn parse_events(bytes: &[u8]) -> Vec<Event> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
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
