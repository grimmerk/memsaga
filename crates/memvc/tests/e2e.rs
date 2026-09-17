//! Behaviour tests: run the real `memvc` binary against fake Claude config
//! directories in a temp dir, feed it hook JSON on stdin, and inspect the git
//! repo it produces. Nothing here looks at implementation details; the same
//! cases can be run against any implementation that keeps the CLI contract.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::TempDir;

struct Fx {
    _tmp: TempDir,
    claude: PathBuf,
    work: PathBuf,
    repo: PathBuf,
    cfg: PathBuf,
}

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn fixture(granularity: &str, extra_toml: &str) -> Fx {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let claude = home.join(".claude");
    let work = home.join(".claude-work");
    let repo = tmp.path().join("repo");
    // main account: allowlisted content
    write(&claude.join("CLAUDE.md"), "# global\n");
    write(&claude.join("skills/foo/SKILL.md"), "skill\n");
    write(&claude.join("projects/-h-git-a/memory/MEMORY.md"), "- idx\n");
    write(&claude.join("projects/-h-git-a/memory/one.md"), "fact\n");
    write(&claude.join("history.jsonl"), "{\"display\":\"x\",\"project\":\"/h/git/a\",\"timestamp\":1}\n");
    fs::create_dir_all(claude.join("projects/-h-git-b/memory")).unwrap(); // empty: skipped
    // noise that must never be mirrored
    write(&claude.join("settings.json"), "{\"a\":1}\n");
    write(&claude.join("projects/-h-git-a/sess.jsonl"), "{\"transcript\":true}\n");
    write(&claude.join("file-history/s1/h@v1"), "big\n");
    write(&claude.join("plugins/p"), "plugin\n");
    write(&claude.join(".claude.json"), "{\"oauth\":\"SECRET\"}\n");
    write(&claude.join("hooks/x.sh"), "hook\n");
    // second account: links to the anchor, own memory
    fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink(claude.join("CLAUDE.md"), work.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink(claude.join("skills"), work.join("skills")).unwrap();
    write(&work.join("settings.json"), "{\"w\":1}\n");
    write(&work.join("projects/-h-git-a/memory/w.md"), "work fact\n");
    let cfg = tmp.path().join("config.toml");
    write(
        &cfg,
        &format!(
            "repo = \"{}\"\ngranularity = \"{granularity}\"\nreporter = true\npush_min_interval_secs = 0\n{extra_toml}\n[[roots]]\npath = \"{}\"\nlabel = \"claude\"\n[[roots]]\npath = \"{}\"\nlabel = \"claude-work\"\n",
            repo.display(),
            claude.display(),
            work.display()
        ),
    );
    Fx { _tmp: tmp, claude, work, repo, cfg }
}

fn cmd(fx: &Fx) -> Command {
    let mut c = Command::cargo_bin("memvc").unwrap();
    c.env("MEMVC_CONFIG", &fx.cfg)
        .env("CLAUDE_CONFIG_DIR", &fx.claude)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    c
}

fn run(fx: &Fx, args: &[&str]) -> String {
    let out = cmd(fx).args(args).output().unwrap();
    assert!(out.status.success(), "memvc {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn hook(fx: &Fx, json: &str) -> String {
    let out = cmd(fx).arg("hook").write_stdin(json).output().unwrap();
    assert!(out.status.success(), "hook exited non-zero: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn git(fx: &Fx, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(&fx.repo).args(args).output().unwrap();
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn commits(fx: &Fx) -> usize {
    git(fx, &["rev-list", "--count", "HEAD"]).trim().parse().unwrap()
}

fn head_files(fx: &Fx) -> Vec<String> {
    git(fx, &["ls-tree", "-r", "--name-only", "HEAD"]).lines().map(|s| s.to_string()).collect()
}

fn stop(session: &str) -> String {
    format!("{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session}\",\"cwd\":\"/h/git/a\"}}")
}

fn post_tool_use(path: &Path, session: &str) -> String {
    format!(
        "{{\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Write\",\"tool_input\":{{\"file_path\":\"{}\"}},\"session_id\":\"{session}\",\"cwd\":\"/h/git/b\",\"agent_type\":\"Explore\"}}",
        path.display()
    )
}

#[test]
fn init_imports_allowlist_and_excludes_noise() {
    let fx = fixture("turn", "");
    let out = run(&fx, &["init", "--no-backfill"]);
    assert!(out.contains("import: committed"), "{out}");
    let files = head_files(&fx);
    for want in [
        "claude/CLAUDE.md",
        "claude/skills/foo/SKILL.md",
        "claude/projects/-h-git-a/memory/MEMORY.md",
        "claude/projects/-h-git-a/memory/one.md",
        "claude-work/projects/-h-git-a/memory/w.md",
        "claude-work/CLAUDE.md",
        "claude-work/skills",
        "index.md",
        "paths.json",
        ".gitattributes",
    ] {
        assert!(files.iter().any(|f| f == want), "missing {want} in {files:?}");
    }
    for never in ["settings.json", "sess.jsonl", "file-history", "plugins/", ".claude.json", "hooks/x.sh", "-h-git-b"] {
        assert!(!files.iter().any(|f| f.contains(never)), "noise {never} leaked: {files:?}");
    }
    // symlinks are kept as symlinks and rewritten relative to the repo
    let link = fs::read_link(fx.repo.join("claude-work/CLAUDE.md")).unwrap();
    assert_eq!(link, PathBuf::from("../claude/CLAUDE.md"));
    let link = fs::read_link(fx.repo.join("claude-work/skills")).unwrap();
    assert_eq!(link, PathBuf::from("../claude/skills"));
    let paths: serde_json::Value = serde_json::from_str(&fs::read_to_string(fx.repo.join("paths.json")).unwrap()).unwrap();
    assert_eq!(paths["claude"]["-h-git-a"], "/h/git/a");
    assert_eq!(commits(&fx), 1);
}

#[test]
fn second_sweep_is_a_noop() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let out = run(&fx, &["sweep"]);
    assert!(out.contains("no changes"), "{out}");
    assert_eq!(commits(&fx), 1);
}

#[test]
fn out_of_scope_edit_is_ignored() {
    let fx = fixture("edit", "");
    run(&fx, &["init", "--no-backfill"]);
    fs::write(fx.claude.join("CLAUDE.md"), "# global\nmore\n").unwrap(); // dirty, but the hook is told about another file
    hook(&fx, &post_tool_use(Path::new("/h/git/a/src/foo.ts"), "s1"));
    assert_eq!(commits(&fx), 1);
    assert!(!fx.repo.join(".git/memvc/dirty").exists());
}

#[test]
fn edit_granularity_commits_immediately_with_attribution() {
    let fx = fixture("edit", "");
    run(&fx, &["init", "--no-backfill"]);
    let f = fx.claude.join("projects/-h-git-a/memory/two.md");
    fs::write(&f, "second fact\n").unwrap();
    hook(&fx, &post_tool_use(&f, "1e82bad5-aaaa"));
    assert_eq!(commits(&fx), 2);
    let msg = git(&fx, &["log", "-1", "--format=%B"]);
    for want in ["PostToolUse: claude:memory:-h-git-a", "Event: PostToolUse", "Account: claude", "Session: 1e82bad5-aaaa", "Cwd: /h/git/b", "Agent: Explore"] {
        assert!(msg.contains(want), "missing {want:?} in:\n{msg}");
    }
}

#[test]
fn turn_granularity_batches_a_turn_into_one_commit() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let a = fx.claude.join("projects/-h-git-a/memory/a.md");
    let b = fx.claude.join("projects/-h-git-a/memory/b.md");
    fs::write(&a, "aaa\n").unwrap();
    hook(&fx, &post_tool_use(&a, "s1"));
    fs::write(&b, "bbbb\n").unwrap();
    hook(&fx, &post_tool_use(&b, "s1"));
    assert_eq!(commits(&fx), 1, "no commit before Stop");
    assert!(fx.repo.join(".git/memvc/dirty").exists());
    hook(&fx, &stop("s1"));
    assert_eq!(commits(&fx), 2, "exactly one commit at Stop");
    let files = git(&fx, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(files.contains("memory/a.md") && files.contains("memory/b.md"), "{files}");
    assert!(!fx.repo.join(".git/memvc/dirty").exists());
}

#[test]
fn bash_edits_and_deletions_are_caught_by_stop() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    // an edit no hook saw (as a Bash heredoc would do)
    fs::write(fx.claude.join("projects/-h-git-a/memory/one.md"), "fact, revised\n").unwrap();
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), 2);
    // a deletion
    fs::remove_file(fx.claude.join("projects/-h-git-a/memory/one.md")).unwrap();
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), 3);
    assert!(!head_files(&fx).iter().any(|f| f.ends_with("memory/one.md")));
    let ns = git(&fx, &["show", "--name-status", "--format=", "HEAD"]);
    assert!(ns.starts_with('D'), "{ns}");
}

#[test]
fn concurrent_stop_hooks_lose_nothing() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let before = commits(&fx);
    let mut children = Vec::new();
    for i in 0..8 {
        let f = fx.claude.join(format!("projects/-h-git-a/memory/c{i}.md"));
        fs::write(&f, format!("fact {i}\n")).unwrap();
        let bin = assert_cmd::cargo::cargo_bin("memvc");
        let mut child = std::process::Command::new(bin)
            .arg("hook")
            .env("MEMVC_CONFIG", &fx.cfg)
            .env("CLAUDE_CONFIG_DIR", &fx.claude)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stop(&format!("par-{i}")).as_bytes()).unwrap();
        children.push(child);
    }
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let added = commits(&fx) - before;
    assert!((1..=8).contains(&added), "commits added: {added}");
    let files = head_files(&fx);
    for i in 0..8 {
        assert!(files.iter().any(|f| f.ends_with(&format!("memory/c{i}.md"))), "c{i}.md missing");
    }
    let log = fs::read_to_string(fx.repo.join(".git/memvc/memvc.log")).unwrap_or_default();
    assert!(!log.contains("error"), "errors logged:\n{log}");
}

#[test]
fn excludes_apply_to_names_and_projects() {
    let fx = fixture("turn", "exclude_projects = [\"-h-git-scratch*\"]\n");
    write(&fx.claude.join("CLAUDE.md.codev-bak-2026-07-10"), "old\n");
    write(&fx.claude.join("skills/foo/SKILL.md.codev-bak-x"), "old\n");
    write(&fx.claude.join("skills/.DS_Store"), "x");
    write(&fx.claude.join("projects/-h-git-scratch-1/memory/s.md"), "scratch\n");
    run(&fx, &["init", "--no-backfill"]);
    let files = head_files(&fx);
    assert!(!files.iter().any(|f| f.contains("codev-bak")), "{files:?}");
    assert!(!files.iter().any(|f| f.contains(".DS_Store")), "{files:?}");
    assert!(!files.iter().any(|f| f.contains("-h-git-scratch-1")), "{files:?}");
}

#[test]
fn exclude_paths_prune_subtrees_and_outside_symlinks_become_absolute() {
    let fx = fixture("turn", "");
    // Claude Code's synced skills live under skills/synced: excluded by default.
    write(&fx.claude.join("skills/synced/docx/schema.xsd"), "<xsd/>");
    write(&fx.claude.join("skills/synced/docx/SKILL.md"), "vendor");
    // a skill that is a relative symlink out of the tree (~/.agents/skills/...)
    let agents = fx.claude.parent().unwrap().join(".agents/skills/agent-browser");
    write(&agents.join("SKILL.md"), "outside\n");
    std::os::unix::fs::symlink("../../.agents/skills/agent-browser", fx.claude.join("skills/agent-browser")).unwrap();
    // and one that stays inside the tree
    std::os::unix::fs::symlink("foo", fx.claude.join("skills/foo-alias")).unwrap();
    run(&fx, &["init", "--no-backfill"]);
    let files = head_files(&fx);
    assert!(!files.iter().any(|f| f.contains("skills/synced")), "{files:?}");
    let outside = fs::read_link(fx.repo.join("claude/skills/agent-browser")).unwrap();
    assert!(outside.is_absolute() && outside.ends_with(".agents/skills/agent-browser"), "{outside:?}");
    assert!(fx.repo.join("claude/skills/agent-browser/SKILL.md").exists(), "absolute link must resolve on this machine");
    let inside = fs::read_link(fx.repo.join("claude/skills/foo-alias")).unwrap();
    assert_eq!(inside, PathBuf::from("foo"));
    // and the hook fast path treats the excluded subtree as out of scope
    fs::write(fx.claude.join("skills/synced/docx/SKILL.md"), "vendor update").unwrap();
    hook(&fx, &post_tool_use(&fx.claude.join("skills/synced/docx/SKILL.md"), "s9"));
    assert!(!fx.repo.join(".git/memvc/dirty").exists(), "excluded path must not mark dirty");
}

#[test]
fn user_owned_paths_in_the_repo_are_committed_and_kept() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    write(&fx.repo.join("companion/webapp/docs/x.zh-tw.md"), "translation\n");
    write(&fx.repo.join("manual/note.md"), "hand-written\n");
    hook(&fx, &stop("s3"));
    assert_eq!(commits(&fx), 2);
    let files = head_files(&fx);
    assert!(files.iter().any(|f| f == "companion/webapp/docs/x.zh-tw.md"));
    assert!(files.iter().any(|f| f == "manual/note.md"));
    // a later sweep must not delete them
    run(&fx, &["sweep"]);
    assert!(fx.repo.join("companion/webapp/docs/x.zh-tw.md").exists());
    assert!(fx.repo.join("manual/note.md").exists());
}

#[test]
fn mirror_is_one_way_and_never_touches_roots() {
    let fx = fixture("turn", "");
    let snapshot = |p: &Path| -> Vec<(PathBuf, u64)> {
        walk(p).into_iter().map(|f| { let m = fs::symlink_metadata(&f).unwrap(); (f, m.len()) }).collect()
    };
    let before = (snapshot(&fx.claude), snapshot(&fx.work));
    run(&fx, &["init", "--no-backfill"]);
    // a hand edit inside the tool-owned subtree is overwritten by the next sweep
    fs::write(fx.repo.join("claude/CLAUDE.md"), "# hand edited in repo\n").unwrap();
    run(&fx, &["sweep"]);
    assert_eq!(fs::read_to_string(fx.repo.join("claude/CLAUDE.md")).unwrap(), "# global\n");
    hook(&fx, &stop("s4"));
    assert_eq!((snapshot(&fx.claude), snapshot(&fx.work)), before, "a root changed");
    assert_eq!(fs::read_to_string(fx.claude.join("CLAUDE.md")).unwrap(), "# global\n");
}

fn walk(p: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    fn rec(p: &Path, v: &mut Vec<PathBuf>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            v.push(path.clone());
            if e.file_type().unwrap().is_dir() {
                rec(&path, v);
            }
        }
    }
    rec(p, &mut v);
    v.sort();
    v
}

#[test]
fn reporter_prints_once_per_batch_of_commits() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let first = run(&fx, &["report"]); // consumes the import commit event
    assert!(first.contains("systemMessage") && first.contains("memvc:"), "{first}");
    let again = run(&fx, &["report"]);
    assert!(again.trim().is_empty(), "nothing new: {again:?}");
    fs::write(fx.claude.join("projects/-h-git-a/memory/one.md"), "changed fact\n").unwrap();
    hook(&fx, &stop("s5"));
    let third = run(&fx, &["report"]);
    assert!(third.contains("1 file committed"), "{third}");
}

#[test]
fn install_and_uninstall_preserve_foreign_hooks() {
    let fx = fixture("turn", "");
    let settings = fx.claude.join("settings.json");
    fs::write(&settings, r#"{"model":"x","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/other/status.sh","timeout":5}]}]}}"#).unwrap();
    let out = run(&fx, &["install", "--bin", "/usr/local/bin/memvc"]);
    assert!(out.contains("hooks registered"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["model"], "x");
    let stop_cmds: Vec<String> = v["hooks"]["Stop"].as_array().unwrap().iter()
        .flat_map(|e| e["hooks"].as_array().unwrap().iter().map(|h| h["command"].as_str().unwrap().to_string()))
        .collect();
    assert!(stop_cmds.contains(&"/other/status.sh".to_string()), "foreign hook lost: {stop_cmds:?}");
    assert!(stop_cmds.contains(&"/usr/local/bin/memvc hook".to_string()));
    assert!(stop_cmds.contains(&"/usr/local/bin/memvc report".to_string()));
    assert_eq!(v["hooks"]["PostToolUse"][0]["matcher"], "Write|Edit|MultiEdit|NotebookEdit");
    assert_eq!(v["hooks"]["PostToolUse"][0]["hooks"][0]["async"], true);
    assert!(v["hooks"]["SessionEnd"].is_array());
    assert!(fx.claude.join("settings.json.memvc-bak").exists());
    let out2 = run(&fx, &["install", "--bin", "/usr/local/bin/memvc"]);
    assert!(out2.contains("already registered"), "{out2}");
    let out3 = run(&fx, &["uninstall"]);
    assert!(out3.contains("hooks removed"), "{out3}");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let stop_cmds: Vec<String> = v["hooks"]["Stop"].as_array().unwrap().iter()
        .flat_map(|e| e["hooks"].as_array().unwrap().iter().map(|h| h["command"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(stop_cmds, vec!["/other/status.sh".to_string()]);
    assert!(v["hooks"].get("PostToolUse").is_none());
    // the second root, which had no hooks, must also be clean after uninstall
    let w: serde_json::Value = serde_json::from_str(&fs::read_to_string(fx.work.join("settings.json")).unwrap()).unwrap();
    assert!(w.get("hooks").map(|h| h.as_object().map(|o| o.is_empty()).unwrap_or(true)).unwrap_or(true), "{w}");
}

#[test]
fn backfill_replays_transcripts_into_dated_commits() {
    let fx = fixture("turn", "");
    let mem = fx.claude.join("projects/-h-git-a/memory/one.md");
    // Current live content is "fact\n" (from the fixture). History says it was v1, then edited to v2,
    // then a failed Write that must be ignored.
    let transcript = format!(
        concat!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-08-01T10:00:00.000Z\",\"sessionId\":\"s-old\",\"cwd\":\"/h/git/a\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Write\",\"input\":{{\"file_path\":\"{p}\",\"content\":\"v1 fact\\n\"}}}}]}}}}\n",
            "{{\"type\":\"user\",\"timestamp\":\"2026-08-01T10:00:01.000Z\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}}]}}}}\n",
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-08-02T11:00:00.000Z\",\"sessionId\":\"s-old\",\"cwd\":\"/h/git/a\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"Edit\",\"input\":{{\"file_path\":\"{p}\",\"old_string\":\"v1\",\"new_string\":\"v2\"}}}}]}}}}\n",
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-08-03T12:00:00.000Z\",\"sessionId\":\"s-old\",\"cwd\":\"/h/git/a\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t3\",\"name\":\"Write\",\"input\":{{\"file_path\":\"{p}\",\"content\":\"should not land\\n\"}}}}]}}}}\n",
            "{{\"type\":\"user\",\"timestamp\":\"2026-08-03T12:00:01.000Z\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t3\",\"is_error\":true,\"content\":\"denied\"}}]}}}}\n",
        ),
        p = mem.display()
    );
    write(&fx.claude.join("projects/-h-git-a/0000-session.jsonl"), &transcript);
    let out = run(&fx, &["init"]);
    assert!(out.contains("backfill: 2 ops -> 2 commits"), "{out}");
    let dates = git(&fx, &["log", "--format=%ad", "--date=short"]);
    assert!(dates.contains("2026-08-01") && dates.contains("2026-08-02"), "{dates}");
    let shas: Vec<String> = git(&fx, &["log", "--format=%H", "--reverse"]).lines().map(|s| s.to_string()).collect();
    assert_eq!(shas.len(), 3, "2 backfill + 1 import");
    let v1 = git(&fx, &["show", &format!("{}:claude/projects/-h-git-a/memory/one.md", shas[0])]);
    let v2 = git(&fx, &["show", &format!("{}:claude/projects/-h-git-a/memory/one.md", shas[1])]);
    assert_eq!(v1, "v1 fact\n");
    assert_eq!(v2, "v2 fact\n");
    // the import commit brings the file to its real current content
    assert_eq!(fs::read_to_string(fx.repo.join("claude/projects/-h-git-a/memory/one.md")).unwrap(), "fact\n");
    let msg = git(&fx, &["log", "--format=%B", "-1", &shas[0]]);
    assert!(msg.contains("Event: backfill") && msg.contains("Session: s-old"), "{msg}");
}

#[test]
fn status_reports_recent_events() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let out = run(&fx, &["status"]);
    assert!(out.contains("head:") && out.contains("commit") && out.contains("import"), "{out}");
}
