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
    /// Versions left out because a new line looked like a secret
    /// (grimmerk/memsaga#60): a key pasted once and removed long ago would
    /// otherwise enter history through the replay.
    pub held: usize,
    /// Versions left out because the file waits on the incoming list: the
    /// newer repository version is delivered to the live side instead.
    pub waiting: usize,
    /// Versions left out because they are larger than `max_file_mb`.
    pub too_large: usize,
    /// Versions left out because their file is inside a nested repository.
    pub nested: usize,
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

    // Commits move HEAD: under the sweep lock, with the recorded head moved
    // after each one. Otherwise a commit of old versions here would later
    // read as a change of HEAD from elsewhere, and its old versions would be
    // brought to the live files.
    let mut locked =
        crate::recorded::lock_reconciled(cfg, &state, std::time::Duration::from_secs(60))?;
    // The replay writes files and stages everything: a conflicted file would
    // be overwritten, and its markers staged.
    crate::recorded::check_no_conflicts(cfg, &git)?;
    // A change in the clone that no sweep has seen (a hand edit, a file added
    // there) is not on the incoming list yet: the replay would write an old
    // version over it, or commit it under a backfill commit. A sweep first
    // records it and commits it.
    check_clean(cfg, &git, &state)?;
    // A file waiting for the live side holds the newer version: a replayed
    // old one over it would be what sync then writes live. Left out, by live
    // file, so an entry kept under an older name of its directory counts.
    let waiting: HashSet<PathBuf> = crate::sync::load_incoming(&state)?
        .keys()
        .filter_map(|k| crate::sync::live_target(cfg, k).map(|t| t.live().to_path_buf()))
        .collect();
    let mut live_of: HashMap<PathBuf, Option<crate::sync::Target>> = HashMap::new();
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
        // What this group wrote: the only files a held version may be put
        // back for. Anything else staged (the user's own uncommitted file in
        // an existing repository) is left exactly as it is.
        let mut written_now: HashSet<PathBuf> = HashSet::new();
        for (rel, content) in versions {
            if latest.get(&rel) == Some(&content) {
                continue;
            }
            // Each path's live file resolved once: `live_target` builds the
            // scope's matchers on every call.
            let target = live_of
                .entry(rel.clone())
                .or_insert_with(|| crate::sync::live_target(cfg, &rel.to_string_lossy()))
                .clone();
            if target.as_ref().is_some_and(|t| waiting.contains(t.live())) {
                report.waiting += 1;
                continue;
            }
            // Inside a git repository of its own: not mirrored, so not
            // replayed either (invariant 8).
            if target
                .as_ref()
                .is_some_and(|t| crate::recorded::in_nested_live(cfg, t))
            {
                report.nested += 1;
                continue;
            }
            // Over `max_file_mb`, as the mirror leaves such a file out: one
            // version in history would stop every push (GitHub's 100 MB).
            if cfg.max_file_mb > 0 && content.len() as u64 > cfg.max_file_mb.saturating_mul(1 << 20)
            {
                report.too_large += 1;
                continue;
            }
            let dst = cfg.repo.join(&rel);
            // Counted before the write: one that fails half-way is put back too.
            written_now.insert(rel.clone());
            let wrote_it = dst
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&dst, &content));
            if let Err(e) = wrote_it {
                let e = anyhow::Error::from(e).context(format!("writing {}", dst.display()));
                return Err(with_put_back(cfg, &git, &written_now, e));
            }
            latest.insert(rel, content);
            wrote = true;
        }
        // From here to the commit, a failure puts the files this group wrote
        // back to HEAD: an old version left in the clone without a record
        // would read as a hand edit, and reach the live file.
        let finished = (|| -> Result<()> {
            // memsaga's own copies until committed: stopped before the commit,
            // the next sweep overwrites them with the live files instead of
            // taking them for hand edits.
            let own: Vec<PathBuf> = written_now.iter().map(|r| cfg.repo.join(r)).collect();
            crate::sync::record_uncommitted(cfg, &state, &own)?;
            let mut changed = wrote && git.add_all_and_check()?;
            if changed && cfg.scan_secrets {
                let run = |a: &[&str]| git.run(a);
                let held = crate::secrets::unstage_held(&run, &cfg.allow_secrets)?;
                // Every file here is a version the replay wrote: put back the
                // last committed one, and remember that as the latest version.
                for h in &held {
                    let rel = PathBuf::from(&h.path);
                    if !written_now.contains(&rel) {
                        continue;
                    }
                    let dst = cfg.repo.join(&rel);
                    let spec = format!(":(literal){}", h.path);
                    if git.object_type(&format!("HEAD:{}", h.path))?.is_some() {
                        git.run_ok(&["checkout", "-q", "HEAD", "--", &spec])?;
                        match std::fs::read_to_string(&dst) {
                            Ok(c) => {
                                latest.insert(rel, c);
                            }
                            Err(_) => {
                                latest.remove(&rel);
                            }
                        }
                    } else {
                        std::fs::remove_file(&dst)?;
                        latest.remove(&rel);
                    }
                }
                report.held += held.len();
                if !held.is_empty() {
                    changed = git.has_staged_changes()?;
                }
            }
            if changed {
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
                let msg = crate::sweep::commit_message(
                    cfg,
                    &attrib,
                    "",
                    &files,
                    &name_status,
                    &shortstat,
                );
                let made = git.commit(&msg, Some(&last.ts))?;
                if locked.advance_after_commit(&git, &made)? {
                    crate::sync::clear_uncommitted(&state);
                }
                report.commits += 1;
            }
            Ok(())
        })();
        if let Err(e) = finished {
            return Err(with_put_back(cfg, &git, &written_now, e));
        }
    }

    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "backfill".into(),
        event: "init".into(),
        message: format!(
            "ops={} commits={} held={} waiting={} too_large={} nested={} known={} unknown={} dropped={} range={}..{}",
            report.ops,
            report.commits,
            report.held,
            report.waiting,
            report.too_large,
            report.nested,
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

/// Refuse while the clone holds a change to a file with a live side that is
/// neither memsaga's own uncommitted copy nor on the incoming list. Files
/// with no live side (`manual/`, `companion/`) are never replayed over.
fn check_clean(cfg: &Config, git: &Git, state: &State) -> Result<()> {
    let own = crate::sync::load_uncommitted(state);
    // memsaga's own copy only while it still holds what memsaga wrote: an
    // edit made over a preview's copy is a hand edit.
    let is_own = |path: &str| -> Result<bool> {
        let Some(blob) = own.get(path) else {
            return Ok(false);
        };
        let now = git.run_ok(&["hash-object", "--", path]);
        if !now.is_ok_and(|h| h.trim() == blob) {
            return Ok(false);
        }
        // And nothing else staged: the index holds HEAD's version or the copy.
        let staged = git.run(&["rev-parse", "-q", "--verify", &format!(":{path}")])?;
        let staged = String::from_utf8_lossy(&staged.stdout).trim().to_string();
        let head = git.run(&["rev-parse", "-q", "--verify", &format!("HEAD:{path}")])?;
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        // No staged version: fine only when HEAD has none either (else a
        // deletion was staged by hand).
        Ok(if staged.is_empty() {
            head.is_empty()
        } else {
            staged == *blob || staged == head
        })
    };
    let incoming = crate::sync::load_incoming(state)?;
    let out = git.run_ok(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        let (code, path) = f.split_at(3.min(f.len()));
        let mut paths = vec![path];
        if code.starts_with('R') || code.starts_with('C') {
            // The old path of a rename: a mirrored file moved out by hand.
            paths.extend(fields.next());
        }
        for path in paths {
            if crate::mirror::is_temp_path(Path::new(path)) {
                continue; // memsaga's own leftover: never staged
            }
            if incoming.contains_key(path)
                || crate::sync::live_target(cfg, path).is_none()
                || is_own(path)?
            {
                continue;
            }
            anyhow::bail!(
                "the data repository {} has changes no sweep has recorded yet ({path}, …); run `memsaga sweep` first, which commits the files as they are in the working tree. A version staged by hand that differs from its file would be replaced by that sweep (it stages the working tree): commit it, or unstage it, first. A file the secret check holds back has to be committed or removed by hand",
                cfg.repo.display()
            );
        }
    }
    Ok(())
}

/// `e`, after putting the group's files back to HEAD; a failure there is
/// added to the error.
fn with_put_back(
    cfg: &Config,
    git: &Git,
    paths: &HashSet<PathBuf>,
    e: anyhow::Error,
) -> anyhow::Error {
    match put_back(cfg, git, paths) {
        Ok(()) => e,
        Err(undo) => e.context(format!(
            "and putting this group's files back to HEAD failed too: {undo:#}"
        )),
    }
}

/// Put `paths` (repository-relative) back to HEAD, index and working tree;
/// one that HEAD lacks is removed. Every path is tried; the first failure
/// is returned.
fn put_back(cfg: &Config, git: &Git, paths: &HashSet<PathBuf>) -> Result<()> {
    let mut first: Option<anyhow::Error> = None;
    for rel in paths {
        let rel = rel.to_string_lossy();
        let spec = format!(":(literal){rel}");
        let one = (|| -> Result<()> {
            if git.object_type(&format!("HEAD:{rel}"))?.is_some() {
                git.run_ok(&["checkout", "-q", "HEAD", "--", &spec])?;
            } else {
                git.run_ok(&["rm", "-q", "--cached", "--ignore-unmatch", "--", &spec])?;
                match std::fs::remove_file(cfg.repo.join(rel.as_ref())) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
            Ok(())
        })();
        if let Err(e) = one {
            first.get_or_insert(e.context(format!("putting back {rel}")));
        }
    }
    first.map_or(Ok(()), Err)
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
