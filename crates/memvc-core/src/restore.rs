//! `restore`: copy one file from the repo's history back into a live config
//! directory. This is the only place memvc ever writes into a root, and it
//! does so only for a path inside the allowlist, only after showing the diff,
//! and only with explicit confirmation (`--yes` or an interactive "y").
//!
//! Also the path mapping both directions: `claude/projects/<slug>/memory/x.md`
//! in the repo is `<root of label "claude">/projects/<slug>/memory/x.md` live.

use crate::config::{expand_tilde, Config};
use crate::git::Git;
use crate::roots::Scope;
use crate::state::{self, Event, State};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A file named either way, resolved to both of its locations.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    /// Path inside the repo, e.g. `claude/CLAUDE.md`.
    pub repo_rel: PathBuf,
    /// Absolute live path.
    pub live: PathBuf,
}

/// Accepts a repo-relative path (`claude/...`), an absolute live path, or a
/// `~/...` live path. Refuses anything outside the mirrored scope.
pub fn locate(cfg: &Config, input: &str) -> Result<Located> {
    let scope = Scope::new(cfg);
    let p = expand_tilde(input);
    if p.is_absolute() {
        let root = scope.contains(&p).with_context(|| {
            format!(
                "{} is not inside any mirrored root or allowlist",
                p.display()
            )
        })?;
        let rel = p.strip_prefix(&root.path)?;
        return Ok(Located {
            repo_rel: Path::new(&root.label).join(rel),
            live: p,
        });
    }
    let mut comps = p.components();
    let label = comps
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default();
    let root = cfg
        .roots
        .iter()
        .find(|r| r.label == label)
        .with_context(|| format!("no mirrored root is labelled {label:?} (path {input})"))?;
    let rest: PathBuf = comps.collect();
    let live = root.path.join(&rest);
    if scope.contains(&live).is_none() {
        bail!(
            "{} is not inside the allowlist of root {}",
            live.display(),
            root.label
        );
    }
    Ok(Located { repo_rel: p, live })
}

/// Content of `repo_rel` at `rev`, or None when it did not exist there.
pub fn content_at(cfg: &Config, repo_rel: &Path, rev: &str) -> Result<Option<Vec<u8>>> {
    let git = Git::new(&cfg.repo);
    let spec = format!("{rev}:{}", repo_rel.to_string_lossy());
    let out = git.run(&["show", &spec])?;
    if out.status.success() {
        Ok(Some(out.stdout))
    } else {
        Ok(None)
    }
}

/// A unified diff between the live file (or nothing) and `candidate`.
pub fn diff_against_live(cfg: &Config, live: &Path, candidate: &[u8]) -> Result<String> {
    let state = State::new(cfg.state_dir());
    state.ensure()?;
    let tmp = state.marker("restore-candidate");
    std::fs::write(&tmp, candidate)?;
    let live_arg = if live.exists() {
        live.to_string_lossy().to_string()
    } else {
        "/dev/null".to_string()
    };
    let git = Git::new(&cfg.repo);
    // --no-index exits 1 when the files differ; that is not an error here.
    let out = git.run(&[
        "diff",
        "--no-index",
        "--color=never",
        &live_arg,
        &tmp.to_string_lossy(),
    ])?;
    let _ = std::fs::remove_file(&tmp);
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    // Make the header readable: the temp path is an implementation detail.
    text = text.replace(
        &tmp.to_string_lossy().to_string(),
        &format!("{} (restored)", live.display()),
    );
    Ok(text)
}

/// Write `candidate` to the live path and record the event. The caller has
/// already shown the diff and obtained confirmation.
pub fn write_live(cfg: &Config, loc: &Located, rev: &str, candidate: &[u8]) -> Result<()> {
    if let Some(parent) = loc.live.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&loc.live, candidate)
        .with_context(|| format!("writing {}", loc.live.display()))?;
    let state = State::new(cfg.state_dir());
    // The live file now differs from the mirror: make sure the next Stop commits it.
    state.write_dirty(&format!(
        "{} restore {} {}",
        state::now_rfc3339(),
        rev,
        loc.live.display()
    ))?;
    state.append_event(&Event {
        ts: state::now_rfc3339(),
        kind: "restore".into(),
        event: "manual".into(),
        files: vec![loc.repo_rel.to_string_lossy().to_string()],
        sha: rev.to_string(),
        message: format!("restored {} from {rev}", loc.live.display()),
        ..Default::default()
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Root;

    fn cfg() -> Config {
        Config {
            roots: vec![
                Root {
                    path: PathBuf::from("/h/.claude"),
                    label: "claude".into(),
                },
                Root {
                    path: PathBuf::from("/h/.claude-work"),
                    label: "claude-work".into(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn locate_maps_both_directions_and_refuses_out_of_scope() {
        let c = cfg();
        let a = locate(&c, "claude/projects/-h-git-a/memory/x.md").unwrap();
        assert_eq!(
            a.live,
            PathBuf::from("/h/.claude/projects/-h-git-a/memory/x.md")
        );
        let b = locate(&c, "/h/.claude-work/CLAUDE.md").unwrap();
        assert_eq!(b.repo_rel, PathBuf::from("claude-work/CLAUDE.md"));
        assert!(
            locate(&c, "claude/settings.json").is_err(),
            "settings.json is not mirrored"
        );
        assert!(locate(&c, "/h/git/a/src/x.ts").is_err());
        assert!(locate(&c, "nope/CLAUDE.md").is_err());
    }
}
