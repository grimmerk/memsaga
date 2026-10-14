//! memsaga CLI. `hook` and `report` are what Claude Code calls; the rest is for
//! humans. `hook` and `report` always exit 0: a hook that fails must never
//! become a failed tool call in someone's session.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use memsaga_core::config::{
    expand_tilde, is_data_repo, normalize, Config, RepoOrigin, GITATTRIBUTES_LINE,
};
use memsaga_core::git::Git;
use memsaga_core::hook::{self, Action, HookInput};
use memsaga_core::roots::{Entry, Scope};
use memsaga_core::state::State;
use memsaga_core::sweep::{self, Attribution};
use memsaga_core::{backfill, install, restore, skills, slug};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "memsaga",
    version = env!("MEMSAGA_VERSION"),
    about = "Version control for Claude Code memory and config"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Claude Code hook entry point: reads the hook JSON on stdin. Always exits 0.
    Hook,
    /// Synchronous Stop hook: prints a one-line notice for commits since the last report.
    Report,
    /// Mirror every root and commit if anything changed.
    Sweep {
        /// Event name recorded in the commit (default: manual).
        #[arg(long, default_value = "manual")]
        event: String,
    },
    /// Create the repo, replay history from transcripts, import the current state.
    Init {
        /// Skip the transcript replay.
        #[arg(long)]
        no_backfill: bool,
        /// Only print what would be imported.
        #[arg(long)]
        dry_run: bool,
        /// Seconds within which ops of one session merge into one backfill commit.
        #[arg(long, default_value_t = 120)]
        group_window: i64,
        /// Where to create the data repository. Without it, the config file's
        /// `repo`, else `memories` in the first of ~/git, ~/code, ~/src,
        /// ~/Projects, ~/dev that exists, else ~/memories. The choice is
        /// recorded in the config file.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Refresh the mirror and show what the next sweep would commit.
    Diff {
        /// Only a `--stat` summary instead of the full diff.
        #[arg(long)]
        stat: bool,
    },
    /// Copy one file from the repo's history back into its live location.
    /// The only command that writes into a config directory.
    Restore {
        /// Repo-relative path (`claude/CLAUDE.md`) or the live path. Omit with --all.
        path: Option<String>,
        /// Revision to restore from (default HEAD).
        #[arg(long, default_value = "HEAD")]
        at: String,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Restore every file of every root as it was at --at. Overwrites
        /// differing files, never deletes, skips symlinks.
        #[arg(long)]
        all: bool,
    },
    /// Last sweeps, last push, repo state.
    Status {
        #[arg(short, default_value_t = 10)]
        n: usize,
        /// Print one JSON object (for scripts, CodeV, a Claude Code mod).
        #[arg(long)]
        json: bool,
    },
    /// Recent commits.
    Log {
        #[arg(short, default_value_t = 20)]
        n: usize,
    },
    /// Register hooks in each root's settings.json (all configured roots by default),
    /// and optionally install the bundled skills.
    Install {
        /// Only this root path (repeatable).
        #[arg(long)]
        root: Vec<PathBuf>,
        /// Command to register (default: this executable's absolute path).
        #[arg(long)]
        bin: Option<String>,
        /// Also write every bundled skill into each root's skills/ directory.
        #[arg(long)]
        skills: bool,
        /// Write only this bundled skill (repeatable); implies --skills.
        #[arg(long)]
        skill: Vec<String>,
        /// Do not touch settings.json (use with --skills / --skill).
        #[arg(long)]
        no_hooks: bool,
    },
    /// Remove memsaga hooks from each root's settings.json.
    Uninstall {
        #[arg(long)]
        root: Vec<PathBuf>,
    },
    /// Both directions: commit live changes, bring in origin's commits and
    /// repository-side edits (written to the live files when only the
    /// repository changed, merged when both did), commit again, push.
    Sync {
        /// Settle one waiting file by picking a side: `--take live <path>`
        /// keeps the live file, `--take repo <path>` writes the repository one.
        #[arg(long, num_args = 2, value_names = ["live|repo", "PATH"])]
        take: Vec<String>,
    },
    /// Inbound only: bring origin's commits and repository-side edits to the
    /// live files. No commit, no push. Used by the memsaga Claude Code mod.
    Pull {
        /// Only ask whether origin moved (throttled across sessions); print JSON.
        #[arg(long)]
        check: bool,
        /// A file about to be read or written: note a hand edit of its
        /// repository copy first; with --check, say whether it is affected.
        #[arg(long)]
        path: Option<String>,
        /// Seconds an earlier --check answer stays valid.
        #[arg(long, default_value_t = 60)]
        max_age: u64,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Push to origin now.
    Push {
        /// Wait this many seconds first, then push silently (used internally
        /// for the throttled push; runs detached from the hook that started it).
        #[arg(long)]
        after: Option<u64>,
    },
    /// Replay transcripts into dated commits (normally part of init).
    Backfill {
        #[arg(long, default_value_t = 120)]
        group_window: i64,
    },
    /// List companion documents under <repo>/companion/, optionally for one source repository.
    Companions {
        /// Source repository name (the first path component under companion/).
        repo: Option<String>,
    },
    /// Print the slug -> real path map.
    Paths,
    /// Print the effective configuration.
    Config,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("memsaga: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<()> {
    // Every command but these works on an existing data repository. Without
    // one (before `init`, or a `repo` naming a folder that is not one) they
    // stop here, so nothing is ever written into a folder `init` has not made
    // a data repository; hooks stop silently, since a hook never fails a turn.
    let needs_repo = !matches!(
        cli.cmd,
        Cmd::Init { .. } | Cmd::Install { .. } | Cmd::Uninstall { .. } | Cmd::Config | Cmd::Paths
    );
    if needs_repo {
        let quiet = matches!(cli.cmd, Cmd::Hook | Cmd::Report);
        match Config::load() {
            Ok(cfg) if is_data_repo(&cfg.repo) => {}
            Ok(_) | Err(_) if quiet => return Ok(()),
            Ok(cfg) => anyhow::bail!(
                "no memsaga data repository at {}: on a new machine, clone the data repository there (README, \"Setting up another machine\"); otherwise run `memsaga init` (`memsaga config` shows the settings)",
                cfg.repo.display()
            ),
            Err(e) => return Err(e),
        }
    }
    match cli.cmd {
        Cmd::Hook => {
            // Never fail a hook. Log instead.
            let cfg = match Config::load() {
                Ok(c) => c,
                Err(_) => return Ok(()),
            };
            let mut text = String::new();
            let _ = std::io::stdin().read_to_string(&mut text);
            let input = HookInput::parse(&text);
            let state = State::new(cfg.state_dir());
            match hook::handle(&cfg, &input) {
                Ok(Action::Swept(o)) if o.committed => state.log(&format!(
                    "{}: committed {} ({} files, {} ms)",
                    input.hook_event_name,
                    o.sha,
                    o.files.len(),
                    o.duration_ms
                )),
                Ok(Action::Deny(reason)) => println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": reason,
                        }
                    })
                ),
                Ok(Action::Notice { event, text }) => println!(
                    "{}",
                    serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": event,
                            "additionalContext": text,
                        }
                    })
                ),
                Ok(_) => {}
                Err(e) => state.log(&format!("{}: error: {e:#}", input.hook_event_name)),
            }
            Ok(())
        }
        Cmd::Report => {
            if let Ok(cfg) = Config::load() {
                if let Some(msg) = hook::report(&cfg) {
                    println!("{}", serde_json::json!({ "systemMessage": msg }));
                }
            }
            Ok(())
        }
        Cmd::Sweep { event } => {
            let cfg = Config::load()?;
            let attrib = Attribution {
                event,
                account: cfg
                    .current_root()
                    .map(|r| r.label.clone())
                    .unwrap_or_default(),
                ..Default::default()
            };
            let o = sweep::sweep(&cfg, &attrib)?;
            if o.committed {
                println!(
                    "committed {} ({}; copied {}, deleted {}, {} ms)",
                    o.sha, o.shortstat, o.copied, o.deleted, o.duration_ms
                );
            } else {
                println!(
                    "no changes (copied {}, deleted {}, {} ms)",
                    o.copied, o.deleted, o.duration_ms
                );
            }
            Ok(())
        }
        Cmd::Init {
            no_backfill,
            dry_run,
            group_window,
            repo,
        } => cmd_init(no_backfill, dry_run, group_window, repo.as_deref()),
        Cmd::Diff { stat } => {
            let cfg = Config::load()?;
            let (status, diff) = sweep::pending(&cfg, stat)?;
            if status.trim().is_empty() {
                println!("no changes since the last commit");
            } else {
                print!("{status}");
                if !diff.trim().is_empty() {
                    println!();
                    print!("{diff}");
                }
            }
            Ok(())
        }
        Cmd::Restore { path, at, yes, all } => match (all, path) {
            (true, None) => cmd_restore_all(&at, yes),
            (false, Some(p)) => cmd_restore(&p, &at, yes),
            (true, Some(_)) => anyhow::bail!("give either a path or --all, not both"),
            (false, None) => anyhow::bail!("a path is required (or --all)"),
        },
        Cmd::Status { n, json } => cmd_status(n, json),
        Cmd::Sync { take } => cmd_sync(&take),
        Cmd::Pull {
            check,
            path,
            max_age,
            json,
        } => cmd_pull(check, path.as_deref(), max_age, json),
        Cmd::Log { n } => {
            let cfg = Config::load()?;
            print!("{}", Git::new(&cfg.repo).recent(n)?);
            Ok(())
        }
        Cmd::Install {
            root,
            bin,
            skills: all_skills,
            skill,
            no_hooks,
        } => {
            let cfg = Config::load()?;
            let bin = match bin {
                Some(b) => b,
                None => install::portable_bin(&std::env::current_exe()?.to_string_lossy()),
            };
            let roots: Vec<PathBuf> = if root.is_empty() {
                cfg.roots.iter().map(|r| r.path.clone()).collect()
            } else {
                root.iter()
                    .map(|p| expand_tilde(&p.to_string_lossy()))
                    .collect()
            };
            let wanted: Vec<&skills::BundledSkill> = if !skill.is_empty() {
                skill
                    .iter()
                    .map(|n| skills::find(n).with_context(|| format!("no bundled skill named {n}")))
                    .collect::<Result<Vec<_>>>()?
            } else if all_skills {
                skills::BUNDLED.iter().collect()
            } else {
                Vec::new()
            };
            for r in roots {
                if !no_hooks {
                    let settings = r.join("settings.json");
                    let changed = install::install(&settings, &bin, cfg.reporter)?;
                    println!(
                        "{}: {}",
                        settings.display(),
                        if changed {
                            "hooks registered"
                        } else {
                            "already registered"
                        }
                    );
                }
                for sk in &wanted {
                    let changed = skills::install_into(&r, sk)?;
                    println!(
                        "{}/skills/{}: {}",
                        r.display(),
                        sk.name,
                        if changed { "written" } else { "up to date" }
                    );
                }
            }
            Ok(())
        }
        Cmd::Uninstall { root } => {
            let cfg = Config::load()?;
            let roots: Vec<PathBuf> = if root.is_empty() {
                cfg.roots.iter().map(|r| r.path.clone()).collect()
            } else {
                root.iter()
                    .map(|p| expand_tilde(&p.to_string_lossy()))
                    .collect()
            };
            for r in roots {
                let settings = r.join("settings.json");
                let changed = install::uninstall(&settings)?;
                println!(
                    "{}: {}",
                    settings.display(),
                    if changed {
                        "hooks removed"
                    } else {
                        "nothing to remove"
                    }
                );
            }
            Ok(())
        }
        Cmd::Push { after } => {
            let cfg = Config::load()?;
            if let Some(secs) = after {
                // Detached waiter: never fail loudly, the state dir has the log.
                if let Err(e) = hook::deferred_push(&cfg, secs) {
                    State::new(cfg.state_dir()).log(&format!("deferred push: error: {e:#}"));
                }
                return Ok(());
            }
            let git = Git::new(&cfg.repo);
            if !git.has_remote("origin") {
                anyhow::bail!("no remote named origin in {}", cfg.repo.display());
            }
            let attrib = memsaga_core::sweep::Attribution {
                event: "manual".into(),
                ..Default::default()
            };
            if hook::push_now(&cfg, &State::new(cfg.state_dir()), &attrib)? {
                println!("pushed");
            } else {
                anyhow::bail!(
                    "push failed; see {}",
                    cfg.state_dir().join("memsaga.log").display()
                );
            }
            Ok(())
        }
        Cmd::Backfill { group_window } => {
            let cfg = Config::load()?;
            let r = backfill::backfill(&cfg, group_window)?;
            println!("backfill: {} ops, {} commits, {} versions known / {} unknown, {} failed calls dropped, {} .. {}", r.ops, r.commits, r.known_versions, r.unknown_versions, r.failed_dropped, r.first_ts, r.last_ts);
            Ok(())
        }
        Cmd::Companions { repo } => {
            let cfg = Config::load()?;
            let list = memsaga_core::companion::list(&cfg.repo, repo.as_deref());
            if list.is_empty() {
                println!(
                    "no companions{} under {} (the companion-docs skill explains the convention)",
                    repo.map(|r| format!(" for {r}")).unwrap_or_default(),
                    cfg.repo.join("companion").display()
                );
                return Ok(());
            }
            let width = list.iter().map(|c| c.rel.len()).max().unwrap_or(0);
            for c in &list {
                let source = if !c.source.is_empty() {
                    format!("<- {}", c.source)
                } else if !c.relates_to.is_empty() {
                    format!("~ {}", c.relates_to.join(", "))
                } else {
                    "(no source yet)".to_string()
                };
                println!("{:<7} {:<width$}  {}", c.status, c.rel, source);
            }
            Ok(())
        }
        Cmd::Paths => {
            let cfg = Config::load()?;
            for root in &cfg.roots {
                let map = slug::map_from_history(root);
                println!("[{}] {}", root.label, root.path.display());
                for (s, paths) in map {
                    println!(
                        "  {s}  ->  {}",
                        paths.into_iter().collect::<Vec<_>>().join(" | ")
                    );
                }
            }
            Ok(())
        }
        Cmd::Config => {
            let cfg = Config::load()?;
            println!("{}", toml::to_string_pretty(&cfg)?);
            println!(
                "# config file: {}",
                memsaga_core::config::config_path().display()
            );
            Ok(())
        }
    }
}

fn cmd_init(
    no_backfill: bool,
    dry_run: bool,
    group_window: i64,
    repo_flag: Option<&str>,
) -> Result<()> {
    let mut cfg = Config::load()?;
    let cfg_path = memsaga_core::config::config_path();
    if let Some(r) = repo_flag {
        let p = normalize(&expand_tilde(r));
        // Checked against the file itself, so MEMSAGA_REPO cannot hide a conflict.
        if let Some(file_repo) = cfg.file_repo.as_ref().filter(|f| **f != p) {
            anyhow::bail!(
                "{} already sets repo = {}; change it there instead of passing --repo",
                cfg_path.display(),
                file_repo.display()
            );
        }
        cfg.repo = p;
    }
    // Never take over someone else's folder. A new path, an empty folder or a
    // data repository is fine. An existing git repository is fine only when
    // you named it (the config file, --repo or MEMSAGA_REPO), for example a
    // clone of an empty GitHub repository; never when the search picked it.
    // A folder with other files that is not a git repository never is.
    let named = repo_flag.is_some() || cfg.repo_origin != RepoOrigin::Detected;
    let is_empty_dir =
        |p: &std::path::Path| std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_none());
    let elsewhere = if cfg.file_repo.is_some() {
        format!("change `repo` in {}", cfg_path.display())
    } else {
        "pass --repo <path> to choose another place".to_string()
    };
    if cfg.repo.exists() && !is_data_repo(&cfg.repo) && !is_empty_dir(&cfg.repo) {
        if !cfg.repo.join(".git").exists() {
            anyhow::bail!(
                "{} holds other files and is not a git repository, so it cannot become the data repository; {elsewhere}",
                cfg.repo.display()
            );
        }
        if !named {
            anyhow::bail!(
                "{} is a git repository memsaga did not create; pass --repo {} to use it anyway, or --repo <path> to choose another place",
                cfg.repo.display(),
                cfg.repo.display()
            );
        }
        println!(
            "using the existing git repository {} as the data repository",
            cfg.repo.display()
        );
    }
    // Record a location the config file does not set yet, when --repo or the
    // search chose it (MEMSAGA_REPO alone is a one-off run and is not recorded).
    let record =
        cfg.file_repo.is_none() && (repo_flag.is_some() || cfg.repo_origin == RepoOrigin::Detected);
    let from = match (repo_flag, cfg.repo_origin) {
        (Some(_), RepoOrigin::Env) => "from --repo, instead of MEMSAGA_REPO".to_string(),
        (_, RepoOrigin::File) => "set in the config file".to_string(),
        (_, RepoOrigin::Env) => "set by MEMSAGA_REPO".to_string(),
        (Some(_), RepoOrigin::Detected) => "from --repo".to_string(),
        (None, RepoOrigin::Detected) if is_data_repo(&cfg.repo) => {
            "an existing memsaga repository; pass --repo to choose another place".to_string()
        }
        (None, RepoOrigin::Detected) => {
            "chosen by default: memories in the first of ~/git, ~/code, ~/src, ~/Projects, ~/dev that exists, else ~/memories; pass --repo to choose another place".to_string()
        }
    };
    println!("repo: {} ({from})", cfg.repo.display());
    if record {
        let verb = if dry_run { "would record" } else { "records" };
        println!("init {verb} it as `repo` in {}", cfg_path.display());
    }
    let scope = Scope::new(&cfg);
    let mut total_files = 0;
    for root in &cfg.roots {
        println!("root {} ({})", root.label, root.path.display());
        let map = slug::map_from_history(root);
        for e in scope.entries(root) {
            let src = e.source(root);
            match &e {
                Entry::File { rel } => println!("  {rel}"),
                Entry::Dir { rel } => {
                    let n = count_files(&src);
                    total_files += n;
                    println!("  {rel}/  ({n} files)");
                }
                Entry::Memory { slug: s } => {
                    let n = count_files(&src);
                    total_files += n;
                    let real = slug::resolve(&map, s)
                        .map(|p| slug::display_path(&p))
                        .unwrap_or_else(|| "(unresolved)".into());
                    println!("  memory {real:<48} {n:>3} files   {s}");
                }
            }
        }
    }
    println!("total: {total_files} files in memory/skills, plus top-level files");
    if !no_backfill {
        for (label, (n, first, last)) in backfill::preview(&cfg)? {
            println!("backfill {label}: {n} replayable Write/Edit ops, {first} .. {last}");
        }
    }
    if dry_run {
        return Ok(());
    }
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        git.init()?;
        println!("initialised {}", cfg.repo.display());
    }
    // Mark it as memsaga's at once, so an init interrupted from here on can be
    // run again (the check above accepts a marked repository).
    std::fs::create_dir_all(cfg.state_dir())?;
    if record {
        // The config file is also a two-way-synced live file: write it under
        // the sweep lock, like every other live write.
        let _lock = State::new(cfg.state_dir()).lock(std::time::Duration::from_secs(20))?;
        memsaga_core::config::record_repo(&cfg.repo)?;
        println!("recorded repo in {}", cfg_path.display());
    }
    // A .gitattributes so MEMORY.md indexes merge line-by-line across machines.
    let ga = cfg.repo.join(".gitattributes");
    if !ga.exists() {
        std::fs::write(&ga, format!("{GITATTRIBUTES_LINE}\n"))?;
    }
    if !no_backfill && !git.has_head() {
        let r = backfill::backfill(&cfg, group_window).context("backfill")?;
        println!("backfill: {} ops -> {} commits ({} versions known, {} unknown, {} failed calls dropped)", r.ops, r.commits, r.known_versions, r.unknown_versions, r.failed_dropped);
    } else if !no_backfill {
        println!("repo already has commits; skipping backfill (run `memsaga backfill` explicitly to append)");
    }
    let attrib = Attribution {
        event: "import".into(),
        ..Default::default()
    };
    let o = sweep::sweep(&cfg, &attrib)?;
    if o.committed {
        println!("import: committed {} ({})", o.sha, o.shortstat);
    } else {
        println!("import: nothing new to commit");
    }
    Ok(())
}

fn cmd_restore_all(at: &str, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let plan = restore::plan_all(&cfg, at)?;
    for (label, n) in &plan.missing_roots {
        let path = cfg
            .roots
            .iter()
            .find(|r| &r.label == label)
            .map(|r| r.path.display().to_string())
            .unwrap_or_default();
        println!(
            "root {label} ({path}) does not exist on this machine: {n} file(s) skipped; create it (sign in to that account) and run restore again to include them"
        );
    }
    println!(
        "{} file(s) differ from {at}, {} identical, {} symlink(s) skipped, {} out of scope skipped; nothing is deleted",
        plan.changed.len(),
        plan.identical,
        plan.symlinks,
        plan.skipped
    );
    for l in &plan.links {
        println!("  link  {}  ->  {}", l.live.display(), l.target.display());
    }
    if plan.changed.is_empty() && plan.links.is_empty() {
        println!("nothing to restore");
        return Ok(());
    }
    for f in &plan.changed {
        println!(
            "  {}  ->  {}",
            f.loc.repo_rel.display(),
            f.loc.live.display()
        );
    }
    if !yes {
        // Show what would change before asking, like the single-file restore.
        for f in &plan.changed {
            print!(
                "{}",
                restore::diff_against_live(&cfg, &f.loc.live, &f.content)?
            );
        }
        eprint!(
            "Overwrite these {} live file(s) and create {} link(s)? [y/N] ",
            plan.changed.len(),
            plan.links.len()
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("not restored");
            return Ok(());
        }
    }
    for f in &plan.changed {
        restore::verify_destination(&cfg, &f.loc.live)?;
    }
    let written = restore::write_live_batch(&cfg, &plan.changed, &plan.links, at)?;
    println!("restored {written} file(s) from {at}; the next Stop commits the live state");
    Ok(())
}

fn cmd_restore(path: &str, at: &str, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let loc = restore::locate(&cfg, path)?;
    restore::verify_destination(&cfg, &loc.live)?;
    let candidate = restore::content_at(&cfg, &loc.repo_rel, at)?
        .with_context(|| format!("{} does not exist at {at}", loc.repo_rel.display()))?;
    let candidate = restore::with_live_ignored_keys(&cfg, &loc, candidate);
    let current = std::fs::read(&loc.live).ok();
    if current.as_deref() == Some(candidate.as_slice()) {
        println!("{} already matches {at}; nothing to do", loc.live.display());
        return Ok(());
    }
    let diff = restore::diff_against_live(&cfg, &loc.live, &candidate)?;
    print!("{diff}");
    if !yes {
        eprint!(
            "Write this version of {} to {}? [y/N] ",
            loc.repo_rel.display(),
            loc.live.display()
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("not restored");
            return Ok(());
        }
    }
    restore::write_live(&cfg, &loc, at, &candidate)?;
    println!(
        "restored {} from {at} ({} bytes); the next Stop commits the live state",
        loc.live.display(),
        candidate.len()
    );
    Ok(())
}

fn print_settled(s: &memsaga_core::sync::Settled) {
    let line = |label: &str, files: &[String]| {
        if !files.is_empty() {
            println!("{label}: {}", files.join(", "));
        }
    };
    line("written to the live files", &s.written);
    line("merged (both sides changed different lines)", &s.merged);
    line("deleted (deleted in the repository)", &s.deleted);
    for (f, why) in &s.skipped {
        println!("skipped {f}: {why}");
    }
    if !s.conflicts.is_empty() {
        println!(
            "conflict (both sides changed the same lines): {}",
            s.conflicts.join(", ")
        );
        println!("  edit either copy until they match and run `memsaga sync` again, or `memsaga sync --take live|repo <path>`");
    }
}

fn cmd_sync(take: &[String]) -> Result<()> {
    use memsaga_core::sync;
    let cfg = Config::load()?;
    let git = Git::new(&cfg.repo);
    if !git.is_repo() {
        anyhow::bail!(
            "{} is not a git repository; run `memsaga init` first",
            cfg.repo.display()
        );
    }
    let take = match take {
        [] => None,
        [side, path] => Some((
            match side.as_str() {
                "live" => sync::Take::Live,
                "repo" => sync::Take::Repo,
                other => anyhow::bail!("--take wants `live` or `repo`, not {other:?}"),
            },
            path.as_str(),
        )),
        _ => anyhow::bail!("--take wants two values: live|repo and a path"),
    };
    let attrib = Attribution {
        event: "sync".into(),
        account: cfg
            .current_root()
            .map(|r| r.label.clone())
            .unwrap_or_default(),
        ..Default::default()
    };
    // 1. Commit what changed live, so the merge base matches the live files.
    let first = sweep::sweep(&cfg, &attrib)?;
    if first.committed {
        println!("committed live changes {}", first.sha);
    }
    // 2. Origin's commits.
    let mut blocked = None;
    match sync::integrate_remote(&cfg) {
        Ok(i) if i.merged => println!("merged origin ({} file(s) with a live side)", i.files.len()),
        Ok(_) => {}
        Err(e) => blocked = Some(e),
    }
    // 3. Repository-side edits to the live files.
    let settled = sync::settle(&cfg, take)?;
    print_settled(&settled);
    // 4. Commit merged results; 5. push.
    let second = sweep::sweep(&cfg, &attrib)?;
    if second.committed {
        println!("committed {}", second.sha);
    }
    if let Some(e) = blocked {
        anyhow::bail!("{e:#}");
    }
    let mut conflicts = settled.conflicts.len();
    if git.has_remote("origin") {
        let state = State::new(cfg.state_dir());
        if hook::push_now(&cfg, &state, &attrib)? {
            println!("pushed");
        } else {
            anyhow::bail!(
                "push failed: {}",
                state.read_marker("push-error").unwrap_or_default()
            );
        }
        // The push may have merged a commit that reached origin meanwhile:
        // bring it to the live files too, and push the result.
        if !sync::load_incoming(&state).is_empty() {
            let late = sync::settle(&cfg, None)?;
            print_settled(&late);
            conflicts += late.conflicts.len();
            if sweep::sweep(&cfg, &attrib)?.committed {
                if !hook::push_now(&cfg, &state, &attrib)? {
                    anyhow::bail!(
                        "push failed: {}",
                        state.read_marker("push-error").unwrap_or_default()
                    );
                }
                println!("pushed");
            }
        }
    }
    // The push is safe with conflicts left (they stay as they are on both
    // sides); exit 1 says a person has to settle them.
    if conflicts > 0 {
        anyhow::bail!("{conflicts} conflict(s) left; see above");
    }
    Ok(())
}

fn cmd_pull(check: bool, path: Option<&str>, max_age: u64, json: bool) -> Result<()> {
    use memsaga_core::sync;
    let cfg = Config::load()?;
    let rel = path.map(|p| sync::repo_rel_of_input(&cfg, p)).transpose()?;
    if check {
        // A hand edit of this file's repository copy counts as "affected"
        // too, as it does for a pull without --check.
        if let Some(r) = &rel {
            sync::note_hand_edit(&cfg, r)?;
        }
        let c = sync::check_remote(&cfg, std::time::Duration::from_secs(max_age))?;
        let state = State::new(cfg.state_dir());
        let incoming = sync::load_incoming(&state);
        let affected = rel
            .as_ref()
            .map(|r| incoming.contains_key(r) || c.changed.iter().any(|(f, _)| f == r));
        let pending: Vec<(String, String)> = incoming
            .keys()
            .filter_map(|k| {
                sync::live_target(&cfg, k).map(|t| (k.clone(), t.live().display().to_string()))
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "moved": c.moved,
                "changed": c.changed,
                "incoming": pending,
                "affected": affected,
                "checked_at": c.checked_at,
                "via": c.via,
            })
        );
        return Ok(());
    }
    if let Some(r) = &rel {
        sync::note_hand_edit(&cfg, r)?;
    }
    let mut merge_error = None;
    let merged = match sync::integrate_remote(&cfg) {
        Ok(i) => i.files,
        Err(e) => {
            merge_error = Some(format!("{e:#}"));
            Vec::new()
        }
    };
    let settled = sync::settle(&cfg, None)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "merged_from_origin": merged,
                "settled": settled,
                "merge_error": merge_error,
            })
        );
    } else {
        print_settled(&settled);
        if let Some(e) = merge_error {
            println!("origin not merged: {e}");
        }
    }
    Ok(())
}

fn cmd_status(n: usize, json: bool) -> Result<()> {
    let cfg = Config::load()?;
    let st = memsaga_core::status::collect(&cfg, n);
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(());
    }
    if st.config.missing {
        if let Some(w) = memsaga_core::status::config_missing_warning(&cfg) {
            println!("{w}");
        }
    }
    println!("repo: {}", st.repo.display());
    if !st.initialised {
        println!("not initialised (run memsaga init)");
        return Ok(());
    }
    println!(
        "head: {}",
        if st.head.is_empty() { "none" } else { &st.head }
    );
    println!("last sweep: {}", fmt_stamp(&st.last_sweep));
    match &st.origin {
        Some(o) => {
            println!(
                "origin: ahead by {} commit(s); last push {}",
                o.ahead,
                fmt_stamp(&o.last_push)
            );
            if o.push_failing {
                println!("origin: the last push failed: {}", o.push_error);
            }
            if let Some(at) = &o.push_pending {
                println!("origin: deferred push scheduled for {at}");
            }
        }
        None => println!("origin: none configured"),
    }
    println!(
        "dirty marker: {}",
        if st.dirty {
            "set (sweep pending at next Stop)"
        } else {
            "clear"
        }
    );
    for r in &st.roots {
        println!(
            "root {}: {} memory dir(s) ({}){}",
            r.label,
            r.memory_dirs,
            r.path.display(),
            if r.exists { "" } else { " (missing)" }
        );
    }
    if !st.incoming.is_empty() {
        println!(
            "waiting for `memsaga sync` ({} file(s) newer in the repository):",
            st.incoming.len()
        );
        for (f, e) in &st.incoming {
            println!("  {f}  ({})", e.from);
        }
    }
    println!("recent events:");
    for ev in &st.events {
        println!(
            "  {}  {:<12} {:<12} {:<8} {} {}",
            &ev.ts[..19.min(ev.ts.len())],
            ev.kind,
            ev.event,
            ev.account,
            ev.sha,
            if ev.files.is_empty() {
                ev.message.clone()
            } else {
                format!("{} files", ev.files.len())
            }
        );
    }
    Ok(())
}

fn fmt_stamp(s: &Option<memsaga_core::status::Stamp>) -> String {
    match s {
        Some(s) => format!("{} ({}s ago)", s.at, s.age_secs),
        None => "never".into(),
    }
}

fn count_files(dir: &std::path::Path) -> usize {
    walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
}
