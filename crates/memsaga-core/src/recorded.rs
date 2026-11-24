//! The recorded head, `refs/memsaga/recorded`: the commit up to which memsaga
//! has accounted for every change that reached the data repository other
//! than through the mirror. Design: docs/two-way-sync.md, "When HEAD moves
//! without a record".
//!
//! The guard lets the mirror overwrite a repository file that equals `HEAD`,
//! on the assumption that `HEAD` holds memsaga's own copies plus changes that
//! are already on the incoming list. A merge whose incoming record was never
//! written (memsaga stopped between the two, as on 2026-11-24), or a
//! `git pull` or `git commit` by hand in the data repository, breaks that
//! assumption: the next sweep copied stale live files over the newer
//! repository files and committed the revert. So every operation that relies
//! on it takes the sweep lock through `lock_reconciled`, which first records
//! such changes as incoming, exactly as a merge by memsaga would have.

use crate::config::Config;
use crate::git::{CatFile, Change, Git};
use crate::state::{self, Event, LockGuard, State};
use crate::sync::{self, Incoming, IncomingEntry};
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::time::Duration;

pub const RECORDED_REF: &str = "refs/memsaga/recorded";

/// The sweep lock, held with the recorded head equal to `HEAD`. The only way
/// to get one is `lock_reconciled`, and the guard needs one, so no sweep can
/// run on an unrecorded change of `HEAD`.
pub struct Locked {
    _lock: LockGuard,
    head: Option<String>,
}

impl Locked {
    /// `HEAD` (full sha) as reconciled; None before the first commit.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// After memsaga's own commit: record it, but only if it sits right on
    /// the recorded head. A commit someone else made in the same instant
    /// would be `HEAD` instead, and recording it would hide its change; then
    /// nothing is recorded and the next run lists what changed. True when
    /// recorded.
    pub fn advance_after_commit(&mut self, git: &Git, c: &crate::git::Committed) -> Result<bool> {
        if c.parents.first().map(String::as_str) != self.head.as_deref() {
            return Ok(false);
        }
        self.advance_to(git, &c.full)?;
        Ok(true)
    }

    /// memsaga itself moved `HEAD` to `new` and has listed every change up to
    /// it (a merge, once its incoming list is saved): record it.
    pub fn advance_to(&mut self, git: &Git, new: &str) -> Result<()> {
        if self.head.as_deref() != Some(new) {
            set_recorded(git, self.head.as_deref(), new)?;
            self.head = Some(new.to_string());
        }
        Ok(())
    }
}

/// Take the sweep lock, refuse while the data repository is half-way through
/// a git operation, and record any change of `HEAD` memsaga did not make or
/// did not finish recording.
pub fn lock_reconciled(cfg: &Config, state: &State, wait: Duration) -> Result<Locked> {
    let lock = state.lock(wait).context("sweep lock")?;
    let git = Git::new(&cfg.repo);
    check_not_busy(cfg)?;
    // The usual case, read from the files without a git process.
    let dir = cfg.repo.join(".git");
    if let Some(head) = read_head(&dir) {
        if read_ref(&dir, RECORDED_REF).as_deref() == Some(head.as_str()) {
            // Read once here, so an unreadable list stops every caller before
            // it changes anything (a merge would otherwise run first).
            sync::load_incoming(state)?;
            return Ok(Locked {
                _lock: lock,
                head: Some(head),
            });
        }
    }
    let Some(head) = git.commit_of("HEAD")? else {
        sync::load_incoming(state)?;
        return Ok(Locked {
            _lock: lock,
            head: None,
        });
    };
    match git.commit_of(RECORDED_REF)? {
        Some(r) if r == head => {}
        Some(r) => reconcile(cfg, state, &git, Some(&r), &r, &head)?,
        // First run of a version with the record (an upgrade, a fresh
        // clone): start from the last commit memsaga made or merged here,
        // when HEAD still contains it, so a change of HEAD made before the
        // upgrade is caught too.
        None => match bootstrap_start(state, &git, &head)? {
            Some(start) if start != head => reconcile(cfg, state, &git, None, &start, &head)?,
            _ => {
                set_recorded(&git, None, &head)?;
                state.log(&format!("recorded head started at {}", short(&head)));
            }
        },
    }
    sync::load_incoming(state)?;
    Ok(Locked {
        _lock: lock,
        head: Some(head),
    })
}

/// Has `HEAD` moved since memsaga last recorded it (or was it never
/// recorded)? No lock: a hook asks this on every prompt and on every Read or
/// Write of a mirrored file to decide whether to pull, so the refs are read
/// from their files (a few microseconds; two `git rev-parse` processes cost
/// about 23 ms, measured 2026-11-25) and git is asked only when a file says
/// nothing clear.
pub fn unrecorded_move(cfg: &Config) -> bool {
    let dir = cfg.repo.join(".git");
    if let (Some(head), recorded) = (read_head(&dir), read_ref(&dir, RECORDED_REF)) {
        return recorded.as_deref() != Some(head.as_str());
    }
    let git = Git::new(&cfg.repo);
    match git.commit_of("HEAD") {
        Ok(None) => false,
        Ok(Some(head)) => {
            git.commit_of(RECORDED_REF).ok().flatten().as_deref() != Some(head.as_str())
        }
        // Unknown: say yes, so the caller reconciles and meets the error.
        Err(_) => true,
    }
}

/// `HEAD`'s commit from the files, when it names a branch (or a commit)
/// that the files resolve. A reftable repository keeps no ref files (its
/// `HEAD` names `refs/heads/.invalid`), so this says None and git is asked.
fn read_head(dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let text = text.trim();
    match text.strip_prefix("ref: ") {
        Some(name) => read_ref(dir, name),
        None => is_id(text).then(|| text.to_string()),
    }
}

/// A ref's id from its loose file, else from `packed-refs`; None when
/// neither has it.
fn read_ref(dir: &std::path::Path, name: &str) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
        let id = text.trim();
        return is_id(id).then(|| id.to_string());
    }
    let packed = std::fs::read_to_string(dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|l| {
        let (id, r) = l.split_once(' ')?;
        (r == name && is_id(id)).then(|| id.to_string())
    })
}

fn is_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What `record_moves` found.
#[derive(Debug, Default)]
pub struct Moved {
    /// Every changed path with a live side.
    pub changed: Vec<String>,
    /// The paths it put on the incoming list.
    pub recorded: Vec<String>,
}

/// Put on `incoming` the files that changed between `from` and `to` and that
/// the live side does not have yet, with their version at `from` as base:
/// the one rule for a merge by memsaga and for a change of `HEAD` found
/// afterwards. When `to` does not contain `from` (history rewritten, a
/// reset), only what changed on `to`'s side of `side` (their merge base)
/// counts: what `from` had and `to` lacks without a change on `to`'s side
/// was dropped, not changed elsewhere, and the live files keep it (a reset
/// to an older commit takes nothing), while a deletion on `to`'s side (origin
/// deleted a file, then a `pull --rebase`) is taken like any other change.
///
/// Skipped: a path already on the list (its base is older and still right);
/// a gitlink (a nested repository: no file to write); a path whose live file
/// already equals `to`'s version; and a version equal to memsaga's own copy
/// not yet committed when it was recorded (a sweep stopped between its commit
/// and recording it).
#[allow(clippy::too_many_arguments)]
pub fn record_moves(
    cfg: &Config,
    state: &State,
    git: &Git,
    incoming: &mut Incoming,
    from: &str,
    side: &str,
    to: &str,
    label: &str,
) -> Result<Moved> {
    let mut out = Moved::default();
    let changes = git.changes(from, to)?;
    if changes.is_empty() {
        return Ok(out);
    }
    let rewritten = side != from;
    let theirs: Option<BTreeSet<String>> = if rewritten {
        // A rename on that side counts for both of its paths: the other diff
        // may list the old path as a plain deletion.
        Some(
            git.changes(side, to)?
                .into_iter()
                .flat_map(|c| std::iter::once(c.path).chain(c.moved_from.map(|(p, _)| p)))
                .collect(),
        )
    } else {
        None
    };
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let next = sync::scope_after_merge(cfg, &paths);
    let target_of = |p: &str| sync::live_target(cfg, p).or_else(|| sync::live_target(&next, p));
    // Live files that a path existing at `to` stands for: a path deleted
    // whose live file is among them moved within the repository (an older
    // layout, a memory directory renamed), and its live file stays.
    let kept_live: BTreeSet<std::path::PathBuf> = changes
        .iter()
        .filter(|c| c.new.is_some())
        .filter_map(|c| target_of(&c.path).map(|t| t.live().to_path_buf()))
        .collect();
    // For such a move, what the live file last agreed with is the version
    // at the old path: the base of the new path, so settle takes the move
    // as an edit of the live file (a rename by git's detection, or a
    // deletion and an addition it could not pair).
    let mut moved_base: std::collections::BTreeMap<std::path::PathBuf, Option<String>> =
        std::collections::BTreeMap::new();
    for c in &changes {
        let (from_path, blob) = match (&c.moved_from, &c.new) {
            (Some((p, b)), _) => (p.as_str(), b.clone()),
            (None, None) => (c.path.as_str(), c.old.clone()),
            _ => continue,
        };
        if let Some(t) = target_of(from_path) {
            if kept_live.contains(t.live()) {
                moved_base.insert(t.live().to_path_buf(), blob);
            }
        }
    }
    let uncommitted = sync::load_uncommitted(state);
    let mut cat: Option<CatFile> = None;
    // Whether each recorded path exists at `to`, as the diff said.
    let mut present: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for c in changes {
        if c.new_mode == "160000" || (c.new.is_none() && c.old_mode == "160000") {
            continue;
        }
        let Some(target) = target_of(&c.path) else {
            continue;
        };
        out.changed.push(c.path.clone());
        if incoming.contains_key(&c.path) {
            continue;
        }
        if theirs.as_ref().is_some_and(|t| !t.contains(&c.path)) {
            continue;
        }
        if c.new.is_none()
            && (kept_live.contains(target.live())
                || moved_elsewhere(cfg, git, &mut cat, to, &c.path, &target)?)
        {
            continue;
        }
        // memsaga's own copy, committed: only when the mode did not change
        // too (the copy's record holds content, not the executable bit).
        if c.new.is_some()
            && uncommitted.get(&c.path) == c.new.as_ref()
            && (c.old.is_none() || c.old_mode == c.new_mode)
        {
            continue;
        }
        if live_equals(cfg, git, &mut cat, &target, &c)? {
            continue;
        }
        let base = match &c.old {
            None => moved_base.get(target.live()).cloned().flatten(),
            old => old.clone(),
        };
        incoming.insert(
            c.path.clone(),
            IncomingEntry {
                base,
                from: label.to_string(),
            },
        );
        present.insert(c.path.clone(), c.new.is_some());
        out.recorded.push(c.path);
    }
    drop(cat);
    restore_own_copies(cfg, state, git, to, &out.recorded, &present)?;
    Ok(out)
}

/// Is `path`, deleted at `to`, a second copy of a live file whose own
/// repository path (as this machine names it) still exists at `to`? Then the
/// deletion removed a duplicate, not the live file.
fn moved_elsewhere(
    cfg: &Config,
    git: &Git,
    cat: &mut Option<CatFile>,
    to: &str,
    path: &str,
    target: &sync::Target,
) -> Result<bool> {
    let Ok(own) = sync::repo_rel_of_input(cfg, &target.live().to_string_lossy()) else {
        return Ok(false);
    };
    if own == path {
        return Ok(false);
    }
    let spec = format!("{to}:{own}");
    if spec.contains('\n') {
        // One request per line in a batch: asked on its own instead.
        return Ok(git.object_type(&spec)?.is_some());
    }
    if cat.is_none() {
        *cat = Some(CatFile::new(git)?);
    }
    let found = cat.as_mut().map(|c| c.read(&spec)).transpose()?.flatten();
    Ok(found.is_some())
}

/// Does the live file already hold `c`'s new version (both absent counts)?
fn live_equals(
    cfg: &Config,
    git: &Git,
    cat: &mut Option<CatFile>,
    target: &sync::Target,
    c: &Change,
) -> Result<bool> {
    let live = sync::mirror_form(cfg, target);
    let Some(id) = &c.new else {
        // Absent only when the file system says so: an error such as a
        // denied search leaves the deletion to be recorded.
        return Ok(live.is_none()
            && matches!(std::fs::symlink_metadata(target.live()), Err(e) if e.kind() == std::io::ErrorKind::NotFound));
    };
    // A link in the repository is recorded as a merge always recorded it.
    if c.new_mode == "120000" {
        return Ok(false);
    }
    let Some(live) = live else {
        return Ok(false);
    };
    // A mode-only change (`chmod +x` elsewhere) still has to reach the live
    // file: settle gives it the executable bit.
    let live_exec = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(target.live()).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if live_exec != (c.new_mode == "100755") {
        return Ok(false);
    }
    if cat.is_none() {
        *cat = Some(CatFile::new(git)?);
    }
    let blob = cat.as_mut().map(|c| c.read(id)).transpose()?.flatten();
    Ok(blob.as_deref() == Some(live.as_slice()))
}

/// A recorded path whose working-tree file is still memsaga's own uncommitted
/// copy (an older binary's sweep or preview wrote the stale live file there
/// after `HEAD` moved): put `to`'s version back, or settle would read the
/// stale copy as the repository side and drop the newer version.
fn restore_own_copies(
    cfg: &Config,
    state: &State,
    git: &Git,
    to: &str,
    recorded: &[String],
    present: &std::collections::BTreeMap<String, bool>,
) -> Result<()> {
    let mut record = sync::load_uncommitted(state);
    let own: Vec<&String> = recorded
        .iter()
        .filter(|p| record.contains_key(*p))
        .collect();
    if own.is_empty() {
        return Ok(());
    }
    let abs: Vec<std::path::PathBuf> = own.iter().map(|p| cfg.repo.join(p)).collect();
    let refs: Vec<&std::path::PathBuf> = abs.iter().collect();
    let now = sync::hash_many(git, &refs);
    for (p, blob) in own.into_iter().zip(now) {
        if blob.is_none() || record.get(p) != blob.as_ref() {
            continue; // changed since memsaga wrote it: an edit, left alone
        }
        let spec = format!(":(literal){p}");
        match present.get(p) {
            Some(true) => {
                git.run_ok(&["checkout", "-q", to, "--", &spec])?;
            }
            Some(false) => std::fs::remove_file(cfg.repo.join(p))?,
            None => continue,
        }
        record.remove(p);
    }
    sync::save_uncommitted(state, &record)
}

/// Record what changed between `from` and `head`, then move the recorded
/// head to `head`. `old_ref` is the ref's current value (None when it does
/// not exist yet).
fn reconcile(
    cfg: &Config,
    state: &State,
    git: &Git,
    old_ref: Option<&str>,
    from: &str,
    head: &str,
) -> Result<()> {
    // What follows may write the working tree (memsaga's own stale copies
    // put back): not over a conflict left half-way.
    check_no_conflicts(cfg, git)?;
    // A git error stops here, with the recorded head left where it was: a
    // wrong "unrelated history" would record HEAD without its changes.
    let ancestor = git.is_ancestor(from, head)?;
    let side = if ancestor {
        Some(from.to_string())
    } else {
        git.merge_base(from, head)?
    };
    // No common history (a switch to an unrelated branch): nothing says what
    // changed, and recording HEAD would let the next sweep copy the live
    // files over it. Stop, and say how to start from HEAD on purpose.
    let Some(side) = side else {
        bail!(
            "HEAD ({}) shares no history with the commit memsaga recorded ({}), so memsaga cannot tell what changed; check out the branch memsaga uses, or, to make the live files follow HEAD's files from now on, run `git -C {} update-ref {RECORDED_REF} HEAD` (the next sweep then copies the live files over any that differ)",
            short(head),
            short(from),
            cfg.repo.display()
        );
    };
    let mut incoming = sync::load_incoming(state)?;
    let moved = record_moves(
        cfg,
        state,
        git,
        &mut incoming,
        from,
        &side,
        head,
        &format!("HEAD moved to {}", short(head)),
    )?;
    if !moved.recorded.is_empty() {
        sync::save_incoming(state, &incoming)?;
    }
    set_recorded(git, old_ref, head)?;
    let how = if ancestor {
        ""
    } else {
        " (history rewritten or reset: only the changes on its side of the merge base were taken)"
    };
    let message = format!(
        "HEAD moved from {} to {} outside memsaga's record{how}; {} file(s) to bring to the live side",
        short(from),
        short(head),
        moved.recorded.len()
    );
    state.log(&message);
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "recovered".into(),
        event: "sync".into(),
        files: moved.recorded,
        sha: short(head).to_string(),
        message,
        ..Default::default()
    })
}

/// Where to start recording on the first run: the newest commit memsaga made,
/// merged or recorded here (from the event log) that `HEAD` still contains.
/// Newer ones `HEAD` does not contain were dropped by a reset, and a commit
/// made by hand on top of the reset is still found from an older one. Every
/// event is tried, against one `git rev-list` of `HEAD`'s history, once per
/// machine. An event log that cannot be read stops here: starting from
/// `HEAD` would hide whatever changed since memsaga's last commit.
fn bootstrap_start(state: &State, git: &Git, head: &str) -> Result<Option<String>> {
    let shas = state.event_shas(&["commit", "merge", "recovered"])?;
    if shas.is_empty() {
        return Ok(None);
    }
    let mut history: Vec<String> = git
        .run_ok(&["rev-list", head])?
        .lines()
        .map(str::to_string)
        .collect();
    history.sort();
    Ok(shas
        .iter()
        .find_map(|sha| commit_with_prefix(&history, sha)))
}

/// The one commit in sorted `history` whose id starts with `prefix` (an
/// event's short sha); None when none does, or more than one.
fn commit_with_prefix(history: &[String], prefix: &str) -> Option<String> {
    if prefix.len() < 4 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let start = history.partition_point(|h| h.as_str() < prefix);
    let mut hits = history[start..]
        .iter()
        .take_while(|h| h.starts_with(prefix));
    let first = hits.next()?;
    hits.next().is_none().then(|| first.clone())
}

/// Move the ref to `new`; with `old`, only if it still points there.
fn set_recorded(git: &Git, old: Option<&str>, new: &str) -> Result<()> {
    let mut args = vec!["update-ref", "-m", "memsaga: recorded", RECORDED_REF, new];
    if let Some(old) = old {
        args.push(old);
    }
    git.run_ok(&args).context("recording HEAD")?;
    Ok(())
}

/// Refuse while the data repository is half-way through a git operation:
/// its working tree then holds conflict markers or half-applied commits, and
/// a sweep would commit them or settle would write them to the live files.
/// Nothing is aborted for the user (see `blocker`).
fn check_not_busy(cfg: &Config) -> Result<()> {
    match blocker(cfg) {
        Some(reason) => bail!(reason),
        None => Ok(()),
    }
}

/// Why memsaga would change nothing in the data repository now, if it would
/// not: a git operation left half-way. Read-only, for `status` too.
///
/// A merge is never aborted for the user, not even one memsaga started and
/// was stopped in (its `.git/MERGE_MSG` then begins `sync: merge origin/`):
/// nothing can prove which process started a merge, and aborting someone
/// else's would throw away their conflict resolutions. memsaga's own merge
/// stops half-way only when it is killed in the moment between a conflict
/// and its own `git merge --abort`.
pub fn blocker(cfg: &Config) -> Option<String> {
    let dir = cfg.repo.join(".git");
    for (name, what, how) in [
        (
            "MERGE_HEAD",
            "a merge",
            "`git commit` or `git merge --abort`",
        ),
        (
            "rebase-merge",
            "a rebase",
            "`git rebase --continue` or `--abort`",
        ),
        (
            "rebase-apply",
            "a rebase or `git am`",
            "`git rebase --continue` or `--abort` for a rebase, `git am --continue` or `--abort` for `git am`",
        ),
        (
            "CHERRY_PICK_HEAD",
            "a cherry-pick",
            "`git cherry-pick --continue` or `--abort`",
        ),
        (
            "REVERT_HEAD",
            "a revert",
            "`git revert --continue` or `--abort`",
        ),
        (
            "sequencer",
            "a cherry-pick or revert of several commits",
            "`git cherry-pick --continue` or `--abort` for a cherry-pick, `git revert --continue` or `--abort` for a revert",
        ),
    ] {
        if dir.join(name).exists() {
            return Some(format!(
                "the data repository {} is in the middle of {what} (.git/{name} exists); memsaga changes nothing until it is finished or aborted ({how})",
                cfg.repo.display()
            ));
        }
    }
    None
}

/// Refuse while the index holds unresolved conflicts (a `git stash pop` by
/// hand that conflicted leaves no other trace). One git call, so only where
/// the conflict would do harm: before a sweep stages everything, and before
/// settle writes repository files to the live side.
pub fn check_no_conflicts(cfg: &Config, git: &Git) -> Result<()> {
    let unmerged = git.run_ok(&["ls-files", "-u", "-z"])?;
    if !unmerged.is_empty() {
        let first = unmerged
            .split('\0')
            .next()
            .and_then(|l| l.split_once('\t'))
            .map(|(_, p)| p.to_string())
            .unwrap_or_default();
        bail!(
            "the data repository {} has files with unresolved conflicts ({first}, …); memsaga changes nothing until they are resolved",
            cfg.repo.display()
        );
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::{read_head, read_ref, RECORDED_REF};
    use crate::git::Git;

    #[test]
    fn refs_read_from_files_match_git_loose_and_packed() {
        let dir = std::env::temp_dir().join(format!("memsaga-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = Git::new(&dir);
        // The files backend, whatever `init.defaultRefFormat` says (a
        // reftable repository is read through git instead, see `read_head`).
        let run = |args: &[&str]| {
            let mut a = vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultRefFormat=files",
            ];
            a.extend_from_slice(args);
            git.run_ok(&a).unwrap()
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("f"), "x\n").unwrap();
        run(&["add", "f"]);
        run(&["commit", "-q", "-m", "one"]);
        let head = git.commit_of("HEAD").unwrap().unwrap();
        run(&["update-ref", RECORDED_REF, &head]);
        let dot = dir.join(".git");
        assert_eq!(read_head(&dot).as_deref(), Some(head.as_str()));
        assert_eq!(read_ref(&dot, RECORDED_REF).as_deref(), Some(head.as_str()));
        // After `git gc` or `git pack-refs` the refs live in packed-refs.
        run(&["pack-refs", "--all", "--prune"]);
        assert!(!dot.join(RECORDED_REF).exists());
        assert_eq!(read_head(&dot).as_deref(), Some(head.as_str()));
        assert_eq!(read_ref(&dot, RECORDED_REF).as_deref(), Some(head.as_str()));
        // A loose ref written later wins over its packed copy.
        std::fs::write(dir.join("f"), "y\n").unwrap();
        run(&["commit", "-q", "-am", "two"]);
        let next = git.commit_of("HEAD").unwrap().unwrap();
        assert_eq!(read_head(&dot).as_deref(), Some(next.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
