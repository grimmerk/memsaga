//! memvc CLI. `hook` and `report` are what Claude Code calls; the rest is for
//! humans. `hook` and `report` always exit 0: a hook that fails must never
//! become a failed tool call in someone's session.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use memvc_core::config::{expand_tilde, Config};
use memvc_core::git::Git;
use memvc_core::hook::{self, Action, HookInput};
use memvc_core::roots::{Entry, Scope};
use memvc_core::state::State;
use memvc_core::sweep::{self, Attribution};
use memvc_core::{backfill, install, restore, skills, slug};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "memvc",
    version,
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
        /// Repo-relative path (`claude/CLAUDE.md`) or the live path.
        path: String,
        /// Revision to restore from (default HEAD).
        #[arg(long, default_value = "HEAD")]
        at: String,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Last sweeps, last push, repo state.
    Status {
        #[arg(short, default_value_t = 10)]
        n: usize,
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
    /// Remove memvc hooks from each root's settings.json.
    Uninstall {
        #[arg(long)]
        root: Vec<PathBuf>,
    },
    /// Push to origin now.
    Push,
    /// Replay transcripts into dated commits (normally part of init).
    Backfill {
        #[arg(long, default_value_t = 120)]
        group_window: i64,
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
            eprintln!("memvc: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<()> {
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
        } => cmd_init(no_backfill, dry_run, group_window),
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
        Cmd::Restore { path, at, yes } => cmd_restore(&path, &at, yes),
        Cmd::Status { n } => cmd_status(n),
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
                None => std::env::current_exe()?.to_string_lossy().to_string(),
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
        Cmd::Push => {
            let cfg = Config::load()?;
            let git = Git::new(&cfg.repo);
            if !git.has_remote("origin") {
                anyhow::bail!("no remote named origin in {}", cfg.repo.display());
            }
            git.push("origin")?;
            State::new(cfg.state_dir()).touch("last-push")?;
            println!("pushed");
            Ok(())
        }
        Cmd::Backfill { group_window } => {
            let cfg = Config::load()?;
            let r = backfill::backfill(&cfg, group_window)?;
            println!("backfill: {} ops, {} commits, {} versions known / {} unknown, {} failed calls dropped, {} .. {}", r.ops, r.commits, r.known_versions, r.unknown_versions, r.failed_dropped, r.first_ts, r.last_ts);
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
                memvc_core::config::config_path().display()
            );
            Ok(())
        }
    }
}

fn cmd_init(no_backfill: bool, dry_run: bool, group_window: i64) -> Result<()> {
    let cfg = Config::load()?;
    let scope = Scope::new(&cfg);
    println!("repo: {}", cfg.repo.display());
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
    // A .gitattributes so MEMORY.md indexes merge line-by-line across machines.
    let ga = cfg.repo.join(".gitattributes");
    if !ga.exists() {
        std::fs::write(&ga, "**/MEMORY.md merge=union\n")?;
    }
    if !no_backfill && !git.has_head() {
        let r = backfill::backfill(&cfg, group_window).context("backfill")?;
        println!("backfill: {} ops -> {} commits ({} versions known, {} unknown, {} failed calls dropped)", r.ops, r.commits, r.known_versions, r.unknown_versions, r.failed_dropped);
    } else if !no_backfill {
        println!("repo already has commits; skipping backfill (run `memvc backfill` explicitly to append)");
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

fn cmd_restore(path: &str, at: &str, yes: bool) -> Result<()> {
    let cfg = Config::load()?;
    let loc = restore::locate(&cfg, path)?;
    let candidate = restore::content_at(&cfg, &loc.repo_rel, at)?
        .with_context(|| format!("{} does not exist at {at}", loc.repo_rel.display()))?;
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

fn cmd_status(n: usize) -> Result<()> {
    let cfg = Config::load()?;
    let git = Git::new(&cfg.repo);
    let state = State::new(cfg.state_dir());
    println!("repo: {}", cfg.repo.display());
    if !git.is_repo() {
        println!("not initialised (run memvc init)");
        return Ok(());
    }
    println!(
        "head: {}",
        git.head_short().unwrap_or_else(|_| "none".into())
    );
    let last_sweep = state
        .mtime("last-sweep")
        .map(fmt_time)
        .unwrap_or_else(|| "never".into());
    println!("last sweep: {last_sweep}");
    if git.has_remote("origin") {
        let last_push = state
            .mtime("last-push")
            .map(fmt_time)
            .unwrap_or_else(|| "never".into());
        println!(
            "origin: ahead by {} commit(s); last push {}",
            git.ahead_of("origin"),
            last_push
        );
    } else {
        println!("origin: none configured");
    }
    println!(
        "dirty marker: {}",
        if state.exists("dirty") {
            "set (sweep pending at next Stop)"
        } else {
            "clear"
        }
    );
    let events = state.read_events();
    println!("recent events:");
    for ev in events.iter().rev().take(n) {
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

fn fmt_time(t: std::time::SystemTime) -> String {
    chrono_fmt(t)
}

fn chrono_fmt(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ago = std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{} ({}s ago)", secs, ago)
}

fn count_files(dir: &std::path::Path) -> usize {
    walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
}
