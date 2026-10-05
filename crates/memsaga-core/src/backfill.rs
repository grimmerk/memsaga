//! Reconstructing history from session transcripts.
//!
//! Every Claude Code transcript (`projects/<slug>/<session>.jsonl`, plus
//! subagent transcripts) records each tool call with its input and a
//! timestamp. `Write` carries the full new content of a file and `Edit`
//! carries an old/new string pair, so in-scope files can be replayed into
//! dated commits. Two passes recover as much as possible:
//!
//! - **forward**: from each `Write` onwards, apply the following `Edit`s; a
//!   file whose `old_string` cannot be found (a Bash edit happened in between)
//!   becomes unknown until the next `Write`.
//! - **backward**: from the file's current content, undo the `Edit`s in
//!   reverse (`new_string` -> `old_string`) back to the first one that cannot
//!   be undone. This is what recovers `MEMORY.md`-style files that are only
//!   ever appended to and never rewritten whole.
//!
//! Where both passes know a version the backward one wins: it is anchored on
//! the real current file. The final import sweep then commits the live state,
//! which corrects any remaining drift.

use crate::config::{Config, Root};
use crate::git::Git;
use crate::roots::{self, Scope};
use crate::state::{self, Event, State};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
enum Kind {
    Write { content: String },
    Edit { old: String, new: String, all: bool },
}

#[derive(Debug, Clone)]
struct Op {
    ts: String, // RFC 3339 from the transcript
    session: String,
    cwd: String,
    rel: PathBuf, // path inside the repo (label/...)
    abs: PathBuf, // live path, to read the current content for the backward pass
    label: String,
    kind: Kind,
    id: String, // tool_use_id, to drop failed calls
}

#[derive(Debug, Default)]
pub struct Report {
    pub ops: usize,
    pub failed_dropped: usize,
    pub known_versions: usize,
    pub unknown_versions: usize,
    pub commits: usize,
    pub first_ts: String,
    pub last_ts: String,
}

/// Replay into `cfg.repo`, an initialised repo with no commits yet.
pub fn backfill(cfg: &Config, group_window_secs: i64) -> Result<Report> {
    let scope = Scope::new(cfg);
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    let mut ops: Vec<Op> = Vec::new();
    let mut failed: HashSet<String> = HashSet::new();
    for root in &cfg.roots {
        collect_root(&scope, root, &mut ops, &mut failed)?;
    }
    let mut report = Report {
        failed_dropped: ops.iter().filter(|o| failed.contains(&o.id)).count(),
        ..Default::default()
    };
    ops.retain(|o| !failed.contains(&o.id));
    ops.sort_by(|a, b| a.ts.cmp(&b.ts));
    report.ops = ops.len();
    if ops.is_empty() {
        return Ok(report);
    }
    report.first_ts = ops.first().map(|o| o.ts.clone()).unwrap_or_default();
    report.last_ts = ops.last().map(|o| o.ts.clone()).unwrap_or_default();

    // after[i] = content of ops[i].rel right after ops[i], when known.
    let after = reconstruct(&ops);
    report.known_versions = after.iter().filter(|a| a.is_some()).count();
    report.unknown_versions = after.len() - report.known_versions;

    // Emit commits: ops of one session within the window form one commit,
    // whose tree holds the latest known version of every touched file.
    let mut latest: HashMap<PathBuf, String> = HashMap::new(); // rel -> content written so far
    let mut group_end = 0usize;
    while group_end < ops.len() {
        let start = group_end;
        let mut end = start + 1;
        while end < ops.len()
            && ops[end].session == ops[start].session
            && seconds_between(&ops[end - 1].ts, &ops[end].ts) <= group_window_secs
        {
            end += 1;
        }
        group_end = end;
        // Latest known version per file within this group.
        let mut versions: HashMap<PathBuf, String> = HashMap::new();
        for i in start..end {
            if let Some(c) = &after[i] {
                versions.insert(ops[i].rel.clone(), c.clone());
            }
        }
        let mut wrote = false;
        for (rel, content) in versions {
            if latest.get(&rel) == Some(&content) {
                continue;
            }
            let dst = cfg.repo.join(&rel);
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&dst, &content)?;
            latest.insert(rel, content);
            wrote = true;
        }
        if wrote && git.add_all_and_check()? {
            let last = &ops[end - 1];
            let name_status = git.staged_name_status()?;
            let files: Vec<String> = name_status
                .lines()
                .filter_map(|l| l.split('\t').nth(1).map(|s| s.to_string()))
                .collect();
            let attrib = crate::sweep::Attribution {
                event: "backfill".into(),
                account: last.label.clone(),
                session: last.session.clone(),
                cwd: last.cwd.clone(),
                agent: String::new(),
                transcript: String::new(),
            };
            let shortstat = git.staged_shortstat()?;
            let msg =
                crate::sweep::commit_message(cfg, &attrib, "", &files, &name_status, &shortstat);
            git.commit(&msg, Some(&last.ts))?;
            report.commits += 1;
        }
    }

    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "backfill".into(),
        event: "init".into(),
        message: format!(
            "ops={} commits={} known={} unknown={} dropped={} range={}..{}",
            report.ops,
            report.commits,
            report.known_versions,
            report.unknown_versions,
            report.failed_dropped,
            report.first_ts,
            report.last_ts
        ),
        ..Default::default()
    })?;
    Ok(report)
}

/// Per-op "content after this op", from a forward pass anchored on Writes and
/// a backward pass anchored on the live file. Backward wins where both know.
fn reconstruct(ops: &[Op]) -> Vec<Option<String>> {
    let n = ops.len();
    let mut after: Vec<Option<String>> = vec![None; n];

    // Forward pass.
    let mut cur: HashMap<PathBuf, Option<String>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        let entry = cur.entry(op.rel.clone()).or_insert(None);
        match &op.kind {
            Kind::Write { content } => *entry = Some(content.clone()),
            Kind::Edit { old, new, all } => {
                *entry = match entry.take() {
                    Some(c) if c.contains(old.as_str()) => Some(apply_edit(&c, old, new, *all)),
                    _ => None,
                }
            }
        }
        after[i] = entry.clone();
    }

    // Backward pass, per file, starting from the live content.
    let mut by_file: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        by_file.entry(op.rel.clone()).or_default().push(i);
    }
    for (_rel, idxs) in by_file {
        let live = match std::fs::read_to_string(&ops[idxs[0]].abs) {
            Ok(s) => s,
            Err(_) => continue, // deleted since: only the forward pass can help
        };
        // `state` = content right after ops[idx] for the idx we are at.
        let mut state: Option<String> = Some(live);
        for &i in idxs.iter().rev() {
            let s = match &state {
                Some(s) => s.clone(),
                None => break,
            };
            // `s` can only be "the content after op i" if op i's result is
            // visible in it; otherwise something outside the transcript (a
            // Bash edit) changed the file afterwards and the chain is broken.
            let consistent = match &ops[i].kind {
                Kind::Write { content } => *content == s,
                Kind::Edit { new, .. } => !new.is_empty() && s.contains(new.as_str()),
            };
            if !consistent {
                break;
            }
            after[i] = Some(s.clone());
            // Compute the content before op i, i.e. after op i-1 of this file.
            state = match &ops[i].kind {
                Kind::Write { .. } => None, // nothing before a whole rewrite is knowable here
                Kind::Edit { old, new, all } => {
                    if s.contains(new.as_str()) && !new.is_empty() {
                        Some(if *all {
                            s.replace(new.as_str(), old)
                        } else {
                            s.replacen(new.as_str(), old, 1)
                        })
                    } else {
                        // An empty `new_string` is a deletion: where the text was is
                        // ambiguous, so the chain stops here.
                        None
                    }
                }
            };
        }
    }
    after
}

fn apply_edit(cur: &str, old: &str, new: &str, all: bool) -> String {
    if all {
        cur.replace(old, new)
    } else {
        cur.replacen(old, new, 1)
    }
}

fn seconds_between(a: &str, b: &str) -> i64 {
    let pa = chrono::DateTime::parse_from_rfc3339(a).ok();
    let pb = chrono::DateTime::parse_from_rfc3339(b).ok();
    match (pa, pb) {
        (Some(a), Some(b)) => (b - a).num_seconds().abs(),
        _ => i64::MAX,
    }
}

/// Walk a root's transcripts and collect in-scope Write/Edit ops.
fn collect_root(
    scope: &Scope,
    root: &Root,
    ops: &mut Vec<Op>,
    failed: &mut HashSet<String>,
) -> Result<()> {
    let projects = root.path.join("projects");
    if !projects.is_dir() {
        return Ok(());
    }
    for item in WalkDir::new(&projects)
        .follow_links(false)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = item.path();
        if !item.file_type().is_file() || p.extension().map(|e| e != "jsonl").unwrap_or(true) {
            continue;
        }
        if p.components().any(|c| c.as_os_str() == "memory") {
            continue;
        }
        collect_transcript(scope, root, p, ops, failed)
            .with_context(|| format!("reading {}", p.display()))?;
    }
    Ok(())
}

fn collect_transcript(
    scope: &Scope,
    root: &Root,
    path: &Path,
    ops: &mut Vec<Op>,
    failed: &mut HashSet<String>,
) -> Result<()> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path)?;
    let mut reader = BufReader::with_capacity(1 << 20, f);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        // Cheap prefilters before parsing 1 GB of JSON.
        let is_assistant = line.contains("\"type\":\"assistant\"") && line.contains("\"tool_use\"");
        let is_result = line.contains("\"tool_result\"") && line.contains("\"is_error\":true");
        if !is_assistant && !is_result {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let content = match v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(c) => c,
            None => continue,
        };
        if is_result {
            for blk in content {
                if blk.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    && blk.get("is_error").and_then(|e| e.as_bool()) == Some(true)
                {
                    if let Some(id) = blk.get("tool_use_id").and_then(|i| i.as_str()) {
                        failed.insert(id.to_string());
                    }
                }
            }
            continue;
        }
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let session = v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let cwd = v
            .get("cwd")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        for blk in content {
            if blk.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = blk.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let input = match blk.get("input") {
                Some(i) => i,
                None => continue,
            };
            let fp = match input.get("file_path").and_then(|p| p.as_str()) {
                Some(p) => PathBuf::from(p),
                None => continue,
            };
            let hit_root = match scope.contains(&fp) {
                Some(r) if r.path == root.path => r,
                _ => continue,
            };
            let rel_in_root = match fp.strip_prefix(&hit_root.path) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            let rel = Path::new(&hit_root.label).join(roots::repo_rel_of_live(&rel_in_root));
            let kind = match name {
                "Write" => Kind::Write {
                    content: input
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
                "Edit" => Kind::Edit {
                    old: input
                        .get("old_string")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                    new: input
                        .get("new_string")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                    all: input
                        .get("replace_all")
                        .and_then(|c| c.as_bool())
                        .unwrap_or(false),
                },
                _ => continue,
            };
            ops.push(Op {
                ts: ts.clone(),
                session: session.clone(),
                cwd: cwd.clone(),
                rel,
                abs: fp,
                label: hit_root.label.clone(),
                kind,
                id: blk
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Ok(())
}

/// Summary per root for `init --dry-run`: how many replayable ops exist.
pub fn preview(cfg: &Config) -> Result<BTreeMap<String, (usize, String, String)>> {
    let scope = Scope::new(cfg);
    let mut out = BTreeMap::new();
    for root in &cfg.roots {
        let mut ops = Vec::new();
        let mut failed = HashSet::new();
        collect_root(&scope, root, &mut ops, &mut failed)?;
        ops.retain(|o| !failed.contains(&o.id));
        ops.sort_by(|a, b| a.ts.cmp(&b.ts));
        let first = ops
            .first()
            .map(|o| o.ts[..10.min(o.ts.len())].to_string())
            .unwrap_or_default();
        let last = ops
            .last()
            .map(|o| o.ts[..10.min(o.ts.len())].to_string())
            .unwrap_or_default();
        out.insert(root.label.clone(), (ops.len(), first, last));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(ts: &str, rel: &str, abs: &Path, kind: Kind) -> Op {
        Op {
            ts: ts.into(),
            session: "s".into(),
            cwd: "/c".into(),
            rel: PathBuf::from(rel),
            abs: abs.to_path_buf(),
            label: "claude".into(),
            kind,
            id: String::new(),
        }
    }

    #[test]
    fn backward_pass_recovers_append_only_history_from_the_live_file() {
        let dir = tempfile_dir();
        let live = dir.join("MEMORY.md");
        std::fs::write(&live, "- a\n- b\n- c\n").unwrap();
        let ops = vec![
            op(
                "2026-08-01T00:00:00Z",
                "claude/m/MEMORY.md",
                &live,
                Kind::Edit {
                    old: "- a\n".into(),
                    new: "- a\n- b\n".into(),
                    all: false,
                },
            ),
            op(
                "2026-08-02T00:00:00Z",
                "claude/m/MEMORY.md",
                &live,
                Kind::Edit {
                    old: "- b\n".into(),
                    new: "- b\n- c\n".into(),
                    all: false,
                },
            ),
        ];
        let after = reconstruct(&ops);
        assert_eq!(after[1].as_deref(), Some("- a\n- b\n- c\n"));
        assert_eq!(after[0].as_deref(), Some("- a\n- b\n"));
    }

    #[test]
    fn forward_pass_follows_writes_and_stops_at_drift() {
        let dir = tempfile_dir();
        let live = dir.join("t.md"); // does not exist: backward pass cannot help
        let ops = vec![
            op(
                "2026-08-01T00:00:00Z",
                "claude/t.md",
                &live,
                Kind::Write {
                    content: "v1\n".into(),
                },
            ),
            op(
                "2026-08-02T00:00:00Z",
                "claude/t.md",
                &live,
                Kind::Edit {
                    old: "v1".into(),
                    new: "v2".into(),
                    all: false,
                },
            ),
            op(
                "2026-08-03T00:00:00Z",
                "claude/t.md",
                &live,
                Kind::Edit {
                    old: "zzz".into(),
                    new: "v3".into(),
                    all: false,
                },
            ),
        ];
        let after = reconstruct(&ops);
        assert_eq!(after[0].as_deref(), Some("v1\n"));
        assert_eq!(after[1].as_deref(), Some("v2\n"));
        assert_eq!(after[2], None);
    }

    #[test]
    fn backward_pass_refuses_a_live_file_that_drifted_after_the_last_op() {
        let dir = tempfile_dir();
        let live = dir.join("d.md");
        std::fs::write(&live, "rewritten by bash\n").unwrap();
        let ops = vec![
            op(
                "2026-08-01T00:00:00Z",
                "claude/d.md",
                &live,
                Kind::Write {
                    content: "v1\n".into(),
                },
            ),
            op(
                "2026-08-02T00:00:00Z",
                "claude/d.md",
                &live,
                Kind::Edit {
                    old: "v1".into(),
                    new: "v2".into(),
                    all: false,
                },
            ),
        ];
        let after = reconstruct(&ops);
        assert_eq!(
            after[0].as_deref(),
            Some("v1\n"),
            "forward pass still valid"
        );
        assert_eq!(
            after[1].as_deref(),
            Some("v2\n"),
            "backward pass must not overwrite with drifted live content"
        );
    }

    fn tempfile_dir() -> PathBuf {
        // Unique per call: unit tests run in parallel threads of one process.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("memsaga-bf-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
