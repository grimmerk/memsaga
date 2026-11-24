//! A thin wrapper over the system `git` binary. memsaga deliberately spawns git
//! instead of linking libgit2 so behaviour matches the user's own git
//! configuration (credential helpers, hooks) while overriding exactly the two
//! things a hook must never trigger: commit signing (a 1Password prompt would
//! hang a TTY-less hook) and interactive prompts.

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `git add --renormalize` argument lists for `paths` (files inside `repo`),
/// at most 200 paths each. Each path is a literal pathspec built from the raw
/// OS bytes, so a name holding `*`, `[` or invalid UTF-8 means exactly that
/// file. Shared by the sweep and the `diff` preview.
pub fn renormalize_args(repo: &Path, paths: &[PathBuf]) -> Vec<Vec<OsString>> {
    let specs: Vec<OsString> = paths
        .iter()
        .filter_map(|p| p.strip_prefix(repo).ok())
        .map(|rel| {
            let mut s = OsString::from(":(literal)");
            s.push(rel.as_os_str());
            s
        })
        .collect();
    specs
        .chunks(200)
        .map(|chunk| {
            let mut args: Vec<OsString> = ["add", "--renormalize", "--"]
                .iter()
                .map(OsString::from)
                .collect();
            args.extend(chunk.iter().cloned());
            args
        })
        .collect()
}

pub struct Git<'a> {
    pub repo: &'a Path,
}

impl<'a> Git<'a> {
    pub fn new(repo: &'a Path) -> Git<'a> {
        Git { repo }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(self.repo);
        c.arg("-c").arg("commit.gpgsign=false");
        c.arg("-c").arg("core.quotepath=off");
        c.env("GIT_TERMINAL_PROMPT", "0");
        c.env_remove("GIT_DIR");
        c.env_remove("GIT_WORK_TREE");
        c.env_remove("GIT_INDEX_FILE");
        c
    }

    pub fn run(&self, args: &[&str]) -> Result<Output> {
        let out = self
            .cmd()
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        Ok(out)
    }

    /// `run` with arguments that may not be UTF-8 (raw path bytes).
    pub fn run_os(&self, args: &[&std::ffi::OsStr]) -> Result<Output> {
        self.cmd().args(args).output().context("running git")
    }

    /// `run` with `input` written to git's stdin. The input is written from
    /// another thread, so a large output cannot fill its pipe and stall both.
    pub fn run_with_input(&self, args: &[&str], input: Vec<u8>) -> Result<Output> {
        use std::io::Write;
        let mut child = self
            .cmd()
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        let stdin = child.stdin.take();
        let writer = std::thread::spawn(move || {
            if let Some(mut s) = stdin {
                let _ = s.write_all(&input);
            }
        });
        let out = child.wait_with_output().context("waiting for git")?;
        let _ = writer.join();
        Ok(out)
    }

    pub fn run_ok(&self, args: &[&str]) -> Result<String> {
        let out = self.run(args)?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Every entry of the tree at `rev`: (mode, type, path).
    pub fn ls_tree(&self, rev: &str) -> Result<Vec<(String, String, String)>> {
        // -z: raw paths, NUL-separated, so a quoted or escaped name is not
        // mistaken for the file name.
        let out = self.run_ok(&["ls-tree", "-r", "-z", "--full-tree", rev])?;
        Ok(out
            .split('\0')
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                let (meta, path) = l.split_once('\t')?;
                let mut parts = meta.split_whitespace();
                let mode = parts.next()?.to_string();
                let kind = parts.next()?.to_string();
                Some((mode, kind, path.to_string()))
            })
            .collect())
    }

    pub fn is_repo(&self) -> bool {
        self.repo.join(".git").is_dir()
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(self.repo)?;
        self.run_ok(&["init", "-q"])?;
        Ok(())
    }

    /// Re-read these files' contents into the index whatever their stat
    /// data says (see `mirror::Stats::restat`). Paths inside the repo.
    pub fn add_renormalize(&self, paths: &[PathBuf]) -> Result<()> {
        for args in renormalize_args(self.repo, paths) {
            let out = self
                .cmd()
                .args(&args)
                .output()
                .context("running git add --renormalize")?;
            if !out.status.success() {
                bail!(
                    "git add --renormalize failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
        }
        Ok(())
    }

    /// Stage everything and report whether the index differs from HEAD.
    pub fn add_all_and_check(&self) -> Result<bool> {
        self.run_ok(&["add", "-A", "--", "."])?;
        self.has_staged_changes()
    }

    /// Whether the index still differs from `HEAD` (or holds anything, in a
    /// repository with no commit yet).
    pub fn has_staged_changes(&self) -> Result<bool> {
        let out = self.run(&["diff", "--cached", "--quiet"])?;
        // exit 1 = differences, 0 = none; anything else is an error (e.g. no HEAD yet)
        match out.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => {
                // No HEAD yet (fresh repo): anything staged counts as a change.
                let staged = self.run_ok(&["diff", "--cached", "--name-only"])?;
                Ok(!staged.trim().is_empty())
            }
        }
    }

    pub fn staged_name_status(&self) -> Result<String> {
        self.run_ok(&["diff", "--cached", "--name-status"])
    }

    pub fn staged_shortstat(&self) -> Result<String> {
        Ok(self
            .run_ok(&["diff", "--cached", "--shortstat"])?
            .trim()
            .to_string())
    }

    /// Commit the index. `date` (RFC 3339) backdates author and committer.
    /// Returns what `HEAD` is right after: full id, short id and parents (one
    /// git call). A commit made by someone else in the same instant would be
    /// `HEAD` instead, so a caller that records the commit as its own checks
    /// the parents first.
    pub fn commit(&self, message: &str, date: Option<&str>) -> Result<Committed> {
        let mut c = self.cmd();
        c.args(["commit", "-q", "-F", "-"]);
        if let Some(d) = date {
            c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        c.stdin(std::process::Stdio::piped());
        let mut child = c.spawn().context("spawning git commit")?;
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().context("git commit stdin")?;
            stdin.write_all(message.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "git commit failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let ids = self.run_ok(&["log", "-1", "--format=%H %h %P", "HEAD"])?;
        let mut parts = ids.split_whitespace().map(str::to_string);
        match (parts.next(), parts.next()) {
            (Some(full), Some(short)) => Ok(Committed {
                full,
                short,
                parents: parts.collect(),
            }),
            _ => bail!("unexpected git log output: {ids}"),
        }
    }

    /// `blob` / `tree` / `commit` for `<rev>:<path>`, or None when it does not exist.
    pub fn object_type(&self, spec: &str) -> Result<Option<String>> {
        let out = self.run(&["cat-file", "-t", spec])?;
        if out.status.success() {
            Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ))
        } else {
            Ok(None)
        }
    }

    /// Run `git` with a throwaway copy of the index, so staging done for an
    /// inspection (`diff`) never changes what the user has staged.
    pub fn run_with_temp_index(&self, args: &[&str]) -> Result<Output> {
        let index = self.repo.join(".git").join("index");
        let tmp = self
            .repo
            .join(".git")
            .join(format!("memsaga-index-{}", std::process::id()));
        if index.exists() {
            std::fs::copy(&index, &tmp)?;
        }
        let out = self
            .cmd()
            .env("GIT_INDEX_FILE", &tmp)
            .args(args)
            .output()
            .with_context(|| format!("running git {} (temp index)", args.join(" ")));
        let _ = std::fs::remove_file(&tmp);
        out
    }

    pub fn head_short(&self) -> Result<String> {
        Ok(self
            .run_ok(&["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string())
    }

    pub fn has_head(&self) -> bool {
        self.run(&["rev-parse", "--verify", "-q", "HEAD"])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Porcelain status of the working tree (untracked included).
    pub fn status_porcelain(&self) -> Result<String> {
        self.run_ok(&["status", "--porcelain", "--untracked-files=all"])
    }

    pub fn has_remote(&self, name: &str) -> bool {
        self.run(&["remote", "get-url", name])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Commits on HEAD that `origin`'s tracking branch lacks; 0 when unknown.
    pub fn ahead_of(&self, remote: &str) -> usize {
        let branch = self
            .run_ok(&["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let range = format!("{remote}/{branch}..HEAD");
        self.run(&["rev-list", "--count", &range])
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn push(&self, remote: &str) -> Result<()> {
        let branch = self
            .run_ok(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string();
        let out = self.run(&["push", "-q", "-u", remote, &branch])?;
        if !out.status.success() {
            bail!(
                "git push failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Full sha of `rev` as a commit; None when it does not resolve to one
    /// (no commit yet, a missing ref); an error when git fails otherwise, so
    /// a transient failure is never read as "no commit".
    pub fn commit_of(&self, rev: &str) -> Result<Option<String>> {
        let spec = format!("{rev}^{{commit}}");
        let out = self.run(&["rev-parse", "--verify", "-q", &spec])?;
        match out.status.code() {
            Some(0) => Ok(
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                    .filter(|s| !s.is_empty()),
            ),
            Some(1) => Ok(None),
            _ => bail!(
                "git rev-parse {spec} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    /// Is `a` an ancestor of `b` (or equal to it)? An error when git cannot
    /// tell (an object it cannot read), never a "no": a wrong "no" would be
    /// taken as unrelated history.
    pub fn is_ancestor(&self, a: &str, b: &str) -> Result<bool> {
        let out = self.run(&["merge-base", "--is-ancestor", a, b])?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!(
                "git merge-base --is-ancestor {a} {b} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    /// The merge base of two commits; None when they share no history, an
    /// error when git cannot tell.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>> {
        let out = self.run(&["merge-base", a, b])?;
        match out.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )),
            Some(1) if out.stdout.is_empty() => Ok(None),
            _ => bail!(
                "git merge-base {a} {b} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    /// The paths that differ between two commits, with the blob on each
    /// side (None where the path does not exist), from one `git diff --raw`.
    /// A rename (found by git's own detection, asked for explicitly so the
    /// user's `diff.renames` does not decide) is one entry for the new path,
    /// added, naming the old path in `moved_from`: the old path is not listed
    /// as deleted. Another machine moving a memory directory to the name it
    /// uses is a rename, and reading it as a deletion would delete this
    /// machine's live file (the merges before 0.1.6 listed only new paths
    /// too, through `git diff --name-only`'s rename detection).
    pub fn changes(&self, from: &str, to: &str) -> Result<Vec<Change>> {
        let out = self.run_ok(&[
            "diff",
            "--raw",
            "-z",
            "--find-renames",
            "--no-abbrev",
            "--no-ext-diff",
            from,
            to,
        ])?;
        // `:<old mode> <new mode> <old id> <new id> <status>` NUL `<path>` NUL,
        // and for a rename (`R<score>`) NUL `<old path>` NUL `<new path>` NUL.
        let mut fields = out.split('\0');
        let mut list = Vec::new();
        while let Some(meta) = fields.next() {
            let Some(meta) = meta.strip_prefix(':') else {
                continue;
            };
            let Some(path) = fields.next() else { break };
            let parts: Vec<&str> = meta.split(' ').collect();
            if parts.len() < 5 {
                bail!("unexpected git diff --raw line: {meta}");
            }
            let id = |mode: &str, id: &str| {
                (mode != "000000" && id.bytes().any(|b| b != b'0')).then(|| id.to_string())
            };
            if parts[4].starts_with('R') || parts[4].starts_with('C') {
                let Some(new_path) = fields.next() else { break };
                list.push(Change {
                    path: new_path.to_string(),
                    old: None,
                    new: id(parts[1], parts[3]),
                    old_mode: "000000".to_string(),
                    new_mode: parts[1].to_string(),
                    moved_from: Some((path.to_string(), id(parts[0], parts[2]))),
                });
                continue;
            }
            list.push(Change {
                path: path.to_string(),
                old: id(parts[0], parts[2]),
                new: id(parts[1], parts[3]),
                old_mode: parts[0].to_string(),
                new_mode: parts[1].to_string(),
                moved_from: None,
            });
        }
        Ok(list)
    }

    /// `git log` lines for the status view.
    pub fn recent(&self, n: usize) -> Result<String> {
        let n = n.to_string();
        self.run_ok(&[
            "log",
            "-n",
            &n,
            "--date=format:%Y-%m-%d %H:%M",
            "--format=%h  %ad  %s",
        ])
    }
}

/// What `Git::commit` made.
#[derive(Debug, Clone)]
pub struct Committed {
    pub full: String,
    pub short: String,
    pub parents: Vec<String>,
}

/// One path that differs between two commits (`Git::changes`).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    /// Blob (or link, or gitlink) id before; None when the path was added.
    pub old: Option<String>,
    /// Id after; None when the path was deleted.
    pub new: Option<String>,
    pub old_mode: String,
    pub new_mode: String,
    /// For a rename: the path it moved from (listed nowhere else) and that
    /// path's blob before.
    pub moved_from: Option<(String, Option<String>)>,
}

/// Reads many objects through one `git cat-file --batch` process, one at a
/// time, so a catch-up of thousands of files costs one process and holds one
/// file in memory at a time.
pub struct CatFile {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl CatFile {
    pub fn new(git: &Git) -> Result<CatFile> {
        let mut child = git
            .cmd()
            .args(["cat-file", "--batch"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("running git cat-file --batch")?;
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().context("cat-file stdout")?);
        Ok(CatFile {
            child,
            stdin,
            stdout,
        })
    }

    /// The content of object `id`, or None when it does not exist.
    pub fn read(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
        use std::io::{BufRead, Read, Write};
        // One request per line: a name holding a newline would be read as
        // two, and every later answer would belong to the wrong request.
        if id.contains('\n') {
            bail!("an object name with a newline cannot be read in a batch: {id:?}");
        }
        let stdin = self.stdin.as_mut().context("cat-file stdin")?;
        writeln!(stdin, "{id}")?;
        stdin.flush()?;
        let mut header = String::new();
        if self.stdout.read_line(&mut header)? == 0 {
            // The process ended without answering: not the same as "missing".
            bail!("git cat-file --batch stopped before answering {id}");
        }
        let header = header.trim_end();
        if header == format!("{id} missing") {
            return Ok(None);
        }
        let fields: Vec<&str> = header.split(' ').collect();
        let size: usize = match fields.as_slice() {
            [_, _, size] => size.parse().ok(),
            _ => None,
        }
        .with_context(|| format!("unexpected git cat-file answer for {id}: {header}"))?;
        let mut buf = vec![0u8; size + 1]; // content, then a newline
        self.stdout.read_exact(&mut buf)?;
        buf.pop();
        Ok(Some(buf))
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn renormalize_paths_are_literal_raw_bytes() {
        let repo = Path::new("/r");
        let odd = std::ffi::OsStr::from_bytes(b"/r/claude/x[1]\xff.md");
        let args = renormalize_args(repo, &[PathBuf::from(odd), PathBuf::from("/elsewhere/y")]);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].len(), 4, "outside paths are dropped: {args:?}");
        assert_eq!(
            args[0][3].as_bytes(),
            b":(literal)claude/x[1]\xff.md".as_slice()
        );
    }
}
