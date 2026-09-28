//! A thin wrapper over the system `git` binary. memsaga deliberately spawns git
//! instead of linking libgit2 so behaviour matches the user's own git
//! configuration (credential helpers, hooks) while overriding exactly the two
//! things a hook must never trigger: commit signing (a 1Password prompt would
//! hang a TTY-less hook) and interactive prompts.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Output};

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
        let out = self.run_ok(&["ls-tree", "-r", "--full-tree", rev])?;
        Ok(out
            .lines()
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

    /// Stage everything and report whether the index differs from HEAD.
    pub fn add_all_and_check(&self) -> Result<bool> {
        self.run_ok(&["add", "-A", "--", "."])?;
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
    pub fn commit(&self, message: &str, date: Option<&str>) -> Result<String> {
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
        self.head_short()
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
