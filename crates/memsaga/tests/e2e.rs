//! Behaviour tests: run the real `memsaga` binary against fake Claude config
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
    write(
        &claude.join("projects/-h-git-a/memory/MEMORY.md"),
        "- idx\n",
    );
    write(&claude.join("projects/-h-git-a/memory/one.md"), "fact\n");
    write(
        &claude.join("history.jsonl"),
        "{\"display\":\"x\",\"project\":\"/h/git/a\",\"timestamp\":1}\n",
    );
    fs::create_dir_all(claude.join("projects/-h-git-b/memory")).unwrap(); // empty: skipped
                                                                          // noise that must never be mirrored
    write(&claude.join("settings.json"), "{\"a\":1}\n");
    write(
        &claude.join("projects/-h-git-a/sess.jsonl"),
        "{\"transcript\":true}\n",
    );
    write(&claude.join("file-history/s1/h@v1"), "big\n");
    write(&claude.join("plugins/p"), "plugin\n");
    write(&claude.join(".claude.json"), "{\"oauth\":\"SECRET\"}\n");
    write(&claude.join("hooks/x.sh"), "hook\n");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(claude.join("hooks/x.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
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
            "repo = \"{}\"\ngranularity = \"{granularity}\"\nreporter = true\n{}{extra_toml}\n[[roots]]\npath = \"{}\"\nlabel = \"claude\"\n[[roots]]\npath = \"{}\"\nlabel = \"claude-work\"\n",
            repo.display(),
            if extra_toml.contains("push_min_interval_secs") {
                ""
            } else {
                "push_min_interval_secs = 0\n"
            },
            claude.display(),
            work.display()
        ),
    );
    Fx {
        _tmp: tmp,
        claude,
        work,
        repo,
        cfg,
    }
}

fn cmd(fx: &Fx) -> Command {
    let mut c = Command::cargo_bin("memsaga").unwrap();
    c.env("MEMSAGA_CONFIG", &fx.cfg)
        .env("CLAUDE_CONFIG_DIR", &fx.claude)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    c
}

fn run(fx: &Fx, args: &[&str]) -> String {
    let out = cmd(fx).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "memsaga {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn hook(fx: &Fx, json: &str) -> String {
    let out = cmd(fx).arg("hook").write_stdin(json).output().unwrap();
    assert!(
        out.status.success(),
        "hook exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn git(fx: &Fx, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn commits(fx: &Fx) -> usize {
    git(fx, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

fn head_files(fx: &Fx) -> Vec<String> {
    git(fx, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .map(|s| s.to_string())
        .collect()
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
        "claude/memory/-h-git-a/MEMORY.md",
        "claude/memory/-h-git-a/one.md",
        "claude-work/memory/-h-git-a/w.md",
        "claude-work/CLAUDE.md",
        "claude-work/skills",
        "claude/hooks/x.sh",
        "index.md",
        "paths.json",
        ".gitattributes",
    ] {
        assert!(
            files.iter().any(|f| f == want),
            "missing {want} in {files:?}"
        );
    }
    for never in [
        "settings.json",
        "sess.jsonl",
        "file-history",
        "plugins/",
        ".claude.json",
        "-h-git-b",
    ] {
        assert!(
            !files.iter().any(|f| f.contains(never)),
            "noise {never} leaked: {files:?}"
        );
    }
    // symlinks are kept as symlinks and rewritten relative to the repo
    let link = fs::read_link(fx.repo.join("claude-work/CLAUDE.md")).unwrap();
    assert_eq!(link, PathBuf::from("../claude/CLAUDE.md"));
    let link = fs::read_link(fx.repo.join("claude-work/skills")).unwrap();
    assert_eq!(link, PathBuf::from("../claude/skills"));
    let paths: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fx.repo.join("paths.json")).unwrap()).unwrap();
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
    assert!(!fx.repo.join(".git/memsaga/dirty").exists());
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
    for want in [
        "PostToolUse: claude:memory:-h-git-a",
        "Event: PostToolUse",
        "Account: claude",
        "Session: 1e82bad5-aaaa",
        "Cwd: /h/git/b",
        "Agent: Explore",
    ] {
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
    assert!(fx.repo.join(".git/memsaga/dirty").exists());
    hook(&fx, &stop("s1"));
    assert_eq!(commits(&fx), 2, "exactly one commit at Stop");
    let files = git(&fx, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        files.contains("-h-git-a/a.md") && files.contains("-h-git-a/b.md"),
        "{files}"
    );
    assert!(!fx.repo.join(".git/memsaga/dirty").exists());
}

#[test]
fn bash_edits_and_deletions_are_caught_by_stop() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    // an edit no hook saw (as a Bash heredoc would do)
    fs::write(
        fx.claude.join("projects/-h-git-a/memory/one.md"),
        "fact, revised\n",
    )
    .unwrap();
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), 2);
    // a deletion
    fs::remove_file(fx.claude.join("projects/-h-git-a/memory/one.md")).unwrap();
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), 3);
    assert!(!head_files(&fx)
        .iter()
        .any(|f| f.ends_with("-h-git-a/one.md")));
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
        let bin = assert_cmd::cargo::cargo_bin("memsaga");
        let mut child = std::process::Command::new(bin)
            .arg("hook")
            .env("MEMSAGA_CONFIG", &fx.cfg)
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
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stop(&format!("par-{i}")).as_bytes())
            .unwrap();
        children.push(child);
    }
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let added = commits(&fx) - before;
    assert!((1..=8).contains(&added), "commits added: {added}");
    let files = head_files(&fx);
    for i in 0..8 {
        assert!(
            files
                .iter()
                .any(|f| f.ends_with(&format!("-h-git-a/c{i}.md"))),
            "c{i}.md missing"
        );
    }
    let log = fs::read_to_string(fx.repo.join(".git/memsaga/memsaga.log")).unwrap_or_default();
    assert!(!log.contains("error"), "errors logged:\n{log}");
}

#[test]
fn excludes_apply_to_names_and_projects() {
    let fx = fixture("turn", "exclude_projects = [\"-h-git-scratch*\"]\n");
    write(&fx.claude.join("CLAUDE.md.codev-bak-2026-07-10"), "old\n");
    write(&fx.claude.join("skills/foo/SKILL.md.codev-bak-x"), "old\n");
    write(&fx.claude.join("skills/.DS_Store"), "x");
    write(
        &fx.claude.join("projects/-h-git-scratch-1/memory/s.md"),
        "scratch\n",
    );
    run(&fx, &["init", "--no-backfill"]);
    let files = head_files(&fx);
    assert!(!files.iter().any(|f| f.contains("codev-bak")), "{files:?}");
    assert!(!files.iter().any(|f| f.contains(".DS_Store")), "{files:?}");
    assert!(
        !files.iter().any(|f| f.contains("-h-git-scratch-1")),
        "{files:?}"
    );
}

#[test]
fn exclude_paths_prune_subtrees_and_outside_symlinks_become_absolute() {
    let fx = fixture("turn", "");
    // Claude Code's synced skills live under skills/synced: excluded by default.
    write(&fx.claude.join("skills/synced/docx/schema.xsd"), "<xsd/>");
    write(&fx.claude.join("skills/synced/docx/SKILL.md"), "vendor");
    // a skill that is a relative symlink out of the tree (~/.agents/skills/...)
    let agents = fx
        .claude
        .parent()
        .unwrap()
        .join(".agents/skills/agent-browser");
    write(&agents.join("SKILL.md"), "outside\n");
    std::os::unix::fs::symlink(
        "../../.agents/skills/agent-browser",
        fx.claude.join("skills/agent-browser"),
    )
    .unwrap();
    // and one that stays inside the tree
    std::os::unix::fs::symlink("foo", fx.claude.join("skills/foo-alias")).unwrap();
    run(&fx, &["init", "--no-backfill"]);
    let files = head_files(&fx);
    assert!(
        !files.iter().any(|f| f.contains("skills/synced")),
        "{files:?}"
    );
    let outside = fs::read_link(fx.repo.join("claude/skills/agent-browser")).unwrap();
    assert!(
        outside.is_absolute() && outside.ends_with(".agents/skills/agent-browser"),
        "{outside:?}"
    );
    assert!(
        fx.repo
            .join("claude/skills/agent-browser/SKILL.md")
            .exists(),
        "absolute link must resolve on this machine"
    );
    let inside = fs::read_link(fx.repo.join("claude/skills/foo-alias")).unwrap();
    assert_eq!(inside, PathBuf::from("foo"));
    // and the hook fast path treats the excluded subtree as out of scope
    fs::write(
        fx.claude.join("skills/synced/docx/SKILL.md"),
        "vendor update",
    )
    .unwrap();
    hook(
        &fx,
        &post_tool_use(&fx.claude.join("skills/synced/docx/SKILL.md"), "s9"),
    );
    assert!(
        !fx.repo.join(".git/memsaga/dirty").exists(),
        "excluded path must not mark dirty"
    );
}

#[test]
fn user_owned_paths_in_the_repo_are_committed_and_kept() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    write(
        &fx.repo.join("companion/webapp/docs/x.zh-tw.md"),
        "translation\n",
    );
    write(&fx.repo.join("manual/note.md"), "hand-written\n");
    hook(&fx, &stop("s3"));
    assert_eq!(commits(&fx), 2);
    let files = head_files(&fx);
    assert!(files
        .iter()
        .any(|f| f == "companion/webapp/docs/x.zh-tw.md"));
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
        walk(p)
            .into_iter()
            .map(|f| {
                let m = fs::symlink_metadata(&f).unwrap();
                (f, m.len())
            })
            .collect()
    };
    let before = (snapshot(&fx.claude), snapshot(&fx.work));
    run(&fx, &["init", "--no-backfill"]);
    // a hand edit inside the tool-owned subtree is overwritten by the next sweep
    fs::write(fx.repo.join("claude/CLAUDE.md"), "# hand edited in repo\n").unwrap();
    run(&fx, &["sweep"]);
    assert_eq!(
        fs::read_to_string(fx.repo.join("claude/CLAUDE.md")).unwrap(),
        "# global\n"
    );
    hook(&fx, &stop("s4"));
    assert_eq!(
        (snapshot(&fx.claude), snapshot(&fx.work)),
        before,
        "a root changed"
    );
    assert_eq!(
        fs::read_to_string(fx.claude.join("CLAUDE.md")).unwrap(),
        "# global\n"
    );
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
    assert!(
        first.contains("systemMessage") && first.contains("memsaga:"),
        "{first}"
    );
    let again = run(&fx, &["report"]);
    assert!(again.trim().is_empty(), "nothing new: {again:?}");
    fs::write(
        fx.claude.join("projects/-h-git-a/memory/one.md"),
        "changed fact\n",
    )
    .unwrap();
    hook(&fx, &stop("s5"));
    let third = run(&fx, &["report"]);
    assert!(third.contains("1 file committed"), "{third}");
}

#[test]
fn install_and_uninstall_preserve_foreign_hooks() {
    let fx = fixture("turn", "");
    let settings = fx.claude.join("settings.json");
    fs::write(&settings, r#"{"model":"x","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/other/status.sh","timeout":5}]}]}}"#).unwrap();
    let out = run(&fx, &["install", "--bin", "/usr/local/bin/memsaga"]);
    assert!(out.contains("hooks registered"), "{out}");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["model"], "x");
    let stop_cmds: Vec<String> = v["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| {
            e["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["command"].as_str().unwrap().to_string())
        })
        .collect();
    assert!(
        stop_cmds.contains(&"/other/status.sh".to_string()),
        "foreign hook lost: {stop_cmds:?}"
    );
    assert!(stop_cmds.contains(&"/usr/local/bin/memsaga hook".to_string()));
    assert!(stop_cmds.contains(&"/usr/local/bin/memsaga report".to_string()));
    assert_eq!(
        v["hooks"]["PostToolUse"][0]["matcher"],
        "Write|Edit|MultiEdit|NotebookEdit"
    );
    assert_eq!(v["hooks"]["PostToolUse"][0]["hooks"][0]["async"], true);
    assert!(v["hooks"]["SessionEnd"].is_array());
    assert!(fx.claude.join("settings.json.memsaga-bak").exists());
    let out2 = run(&fx, &["install", "--bin", "/usr/local/bin/memsaga"]);
    assert!(out2.contains("already registered"), "{out2}");
    let out3 = run(&fx, &["uninstall"]);
    assert!(out3.contains("hooks removed"), "{out3}");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let stop_cmds: Vec<String> = v["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| {
            e["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["command"].as_str().unwrap().to_string())
        })
        .collect();
    assert_eq!(stop_cmds, vec!["/other/status.sh".to_string()]);
    assert!(v["hooks"].get("PostToolUse").is_none());
    // the second root, which had no hooks, must also be clean after uninstall
    let w: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fx.work.join("settings.json")).unwrap()).unwrap();
    assert!(
        w.get("hooks")
            .map(|h| h.as_object().map(|o| o.is_empty()).unwrap_or(true))
            .unwrap_or(true),
        "{w}"
    );
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
    write(
        &fx.claude.join("projects/-h-git-a/0000-session.jsonl"),
        &transcript,
    );
    let out = run(&fx, &["init"]);
    assert!(out.contains("backfill: 2 ops -> 2 commits"), "{out}");
    let dates = git(&fx, &["log", "--format=%ad", "--date=short"]);
    assert!(
        dates.contains("2026-08-01") && dates.contains("2026-08-02"),
        "{dates}"
    );
    let shas: Vec<String> = git(&fx, &["log", "--format=%H", "--reverse"])
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(shas.len(), 3, "2 backfill + 1 import");
    let v1 = git(
        &fx,
        &[
            "show",
            &format!("{}:claude/memory/-h-git-a/one.md", shas[0]),
        ],
    );
    let v2 = git(
        &fx,
        &[
            "show",
            &format!("{}:claude/memory/-h-git-a/one.md", shas[1]),
        ],
    );
    assert_eq!(v1, "v1 fact\n");
    assert_eq!(v2, "v2 fact\n");
    // the import commit brings the file to its real current content
    assert_eq!(
        fs::read_to_string(fx.repo.join("claude/memory/-h-git-a/one.md")).unwrap(),
        "fact\n"
    );
    let msg = git(&fx, &["log", "--format=%B", "-1", &shas[0]]);
    assert!(
        msg.contains("Event: backfill") && msg.contains("Session: s-old"),
        "{msg}"
    );
}

#[test]
fn status_reports_recent_events() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let out = run(&fx, &["status"]);
    assert!(
        out.contains("head:") && out.contains("commit") && out.contains("import"),
        "{out}"
    );
}

#[test]
fn config_change_triggers_a_sweep_and_the_config_mirrors_itself() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let files = head_files(&fx);
    assert!(
        files.iter().any(|f| f == "memsaga/config.toml"),
        "{files:?}"
    );
    let before = commits(&fx);
    // A Stop with nothing new is a no-op.
    hook(&fx, &stop("s0"));
    assert_eq!(commits(&fx), before);
    // Widen the scope to settings.json (its mtime is older than the marker).
    let extra = fx.claude.parent().unwrap().join("extra.json");
    write(&extra, "{\"extra\":true}\n");
    let toml = fs::read_to_string(&fx.cfg).unwrap();
    write(
        &fx.cfg,
        &format!(
            "files = [\"CLAUDE.md\", \"settings.json\"]\nextra_files = [\"{}\"]\n{toml}",
            extra.display()
        ),
    );
    hook(&fx, &stop("s1"));
    assert_eq!(commits(&fx), before + 1, "config change must sweep");
    let files = head_files(&fx);
    for want in [
        "claude/settings.json",
        "claude-work/settings.json",
        "memsaga/config.toml",
    ] {
        assert!(files.iter().any(|f| f == want), "missing {want}: {files:?}");
    }
    assert!(
        files
            .iter()
            .any(|f| f.starts_with("extra/") && f.ends_with("/extra.json")),
        "{files:?}"
    );
    // The extra file is watched by mtime like everything else.
    let before = commits(&fx);
    write(&extra, "{\"extra\":2}\n");
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), before + 1);
    let out = run(&fx, &["status"]);
    assert!(
        out.contains("root claude: 1 memory dir(s)")
            && out.contains("root claude-work: 1 memory dir(s)"),
        "{out}"
    );
}

#[test]
fn throttled_push_is_deferred_not_dropped() {
    let fx = fixture("turn", "push_min_interval_secs = 4\n");
    run(&fx, &["init", "--no-backfill"]);
    let origin = fx.repo.parent().unwrap().join("origin.git");
    assert!(std::process::Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&origin)
        .status()
        .unwrap()
        .success());
    git(&fx, &["remote", "add", "origin", origin.to_str().unwrap()]);
    run(&fx, &["push"]); // sets last-push = now
    let branch = git(&fx, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_string();
    let origin_head = || {
        let out = std::process::Command::new("git")
            .arg(format!("--git-dir={}", origin.display()))
            .args(["rev-parse", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let before = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(origin_head(), before);

    // A commit inside the window: not pushed by this Stop, but scheduled.
    write(
        &fx.claude.join("projects/-h-git-a/memory/late.md"),
        "late\n",
    );
    hook(&fx, &stop("s1"));
    let head = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(head, before, "the Stop must have committed");
    assert_eq!(origin_head(), before, "throttled: not pushed yet");
    let marker = fx.repo.join(".git/memsaga/push-pending");
    assert!(marker.exists(), "a waiter must have been scheduled");

    // No further hook call: the detached waiter pushes on its own, records
    // the event, and removes the marker (in that order, so wait for the last).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(!marker.exists(), "deferred push never finished");
    assert_eq!(origin_head(), head, "deferred push did not reach origin");
    let events = fs::read_to_string(fx.repo.join(".git/memsaga/events.jsonl")).unwrap();
    assert!(events.contains("\"push-deferred\""), "{events}");
    assert!(
        events.contains("\"kind\":\"push\",\"event\":\"deferred\""),
        "{events}"
    );
}

#[test]
fn settings_volatile_keys_are_stripped_from_the_mirror() {
    let fx = fixture("turn", "files = [\"CLAUDE.md\", \"settings.json\"]\n");
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"opus\",\"a\":1,\"effortLevel\":\"high\"}\n",
    );
    run(&fx, &["init", "--no-backfill"]);
    let mirrored = fs::read_to_string(fx.repo.join("claude/settings.json")).unwrap();
    assert!(
        !mirrored.contains("model") && !mirrored.contains("effortLevel"),
        "{mirrored}"
    );
    assert!(mirrored.contains("\"a\": 1"), "{mirrored}");
    // Toggling only the volatile keys is not a commit.
    let before = commits(&fx);
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"sonnet\",\"a\":1,\"effortLevel\":\"low\"}\n",
    );
    hook(&fx, &stop("s1"));
    assert_eq!(
        commits(&fx),
        before,
        "a model/effort toggle must not commit"
    );
    // A real change still is.
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"sonnet\",\"a\":2,\"effortLevel\":\"low\"}\n",
    );
    hook(&fx, &stop("s2"));
    assert_eq!(commits(&fx), before + 1);
}

#[test]
fn restore_keeps_the_live_volatile_settings_keys() {
    let fx = fixture("turn", "files = [\"CLAUDE.md\", \"settings.json\"]\n");
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"opus\",\"a\":1}\n",
    );
    run(&fx, &["init", "--no-backfill"]);
    let rev = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"sonnet\",\"a\":2}\n",
    );
    hook(&fx, &stop("s1"));
    // Single-file and whole-tree restore both bring `a` back and keep `model`.
    run(
        &fx,
        &["restore", "claude/settings.json", "--at", &rev, "--yes"],
    );
    let live: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fx.claude.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(live["a"], 1);
    assert_eq!(live["model"], "sonnet");
    write(
        &fx.claude.join("settings.json"),
        "{\"model\":\"haiku\",\"a\":3}\n",
    );
    hook(&fx, &stop("s2"));
    run(&fx, &["restore", "--all", "--at", &rev, "--yes"]);
    let live: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fx.claude.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(live["a"], 1);
    assert_eq!(live["model"], "haiku");
}

#[test]
fn restore_all_on_a_new_machine_skips_a_missing_root_and_needs_a_clone() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let rev = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    // A second machine: the main account exists but is empty, the work account does not.
    fs::remove_dir_all(&fx.work).unwrap();
    fs::remove_dir_all(&fx.claude).unwrap();
    fs::create_dir_all(&fx.claude).unwrap();
    let out = run(&fx, &["restore", "--all", "--at", &rev, "--yes"]);
    assert!(
        out.contains("root claude-work") && out.contains("does not exist on this machine"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(fx.claude.join("CLAUDE.md")).unwrap(),
        "# global\n"
    );
    assert!(fx.claude.join("projects/-h-git-a/memory/one.md").exists());
    assert!(
        !fx.work.exists(),
        "a restore never creates a config directory"
    );
    // A sweep while the work root is absent keeps its recorded slug map.
    let before_paths = fs::read_to_string(fx.repo.join("paths.json")).unwrap();
    run(&fx, &["sweep", "--event", "partial"]);
    assert_eq!(
        fs::read_to_string(fx.repo.join("paths.json")).unwrap(),
        before_paths
    );
    // The work account signs in later: its links come back, the hook is
    // executable again, and the first sweep finds nothing to commit even
    // though this machine has no prompt history to resolve slugs from.
    fs::create_dir_all(&fx.work).unwrap();
    let out = run(&fx, &["restore", "--all", "--at", &rev, "--yes"]);
    assert!(out.contains("link"), "{out}");
    assert_eq!(
        fs::read_link(fx.work.join("CLAUDE.md")).unwrap(),
        fx.claude.join("CLAUDE.md")
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fx.claude.join("hooks/x.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "hook not executable: {mode:o}");
    }
    let before = commits(&fx);
    run(&fx, &["sweep", "--event", "verify"]);
    assert_eq!(
        commits(&fx),
        before,
        "a faithful restore leaves nothing to commit: {}",
        git(&fx, &["show", "--stat", "HEAD"])
    );
    // No clone at all: a clear message instead of a raw git error.
    let moved = fx.repo.with_file_name("repo-moved");
    fs::rename(&fx.repo, &moved).unwrap();
    let err = cmd(&fx)
        .args(["restore", "--all", "--at", "HEAD"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&err.stderr);
    assert!(
        !err.status.success() && stderr.contains("clone the data repository"),
        "{stderr}"
    );
}

#[test]
fn restore_all_puts_every_root_back_to_a_revision_without_deleting() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let rev = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    // Change two files, add a third, then commit the new state.
    write(&fx.claude.join("CLAUDE.md"), "# changed\n");
    write(
        &fx.work.join("projects/-h-git-a/memory/w.md"),
        "work changed\n",
    );
    write(&fx.claude.join("projects/-h-git-a/memory/new.md"), "new\n");
    hook(&fx, &stop("s1"));
    // Declining leaves everything as it is.
    let declined = cmd(&fx)
        .args(["restore", "--all", "--at", &rev])
        .write_stdin("n\n")
        .output()
        .unwrap();
    let declined_out = String::from_utf8_lossy(&declined.stdout);
    assert!(
        declined_out.contains("not restored") && declined_out.contains("-# changed"),
        "{declined_out}"
    );
    assert_eq!(
        fs::read_to_string(fx.claude.join("CLAUDE.md")).unwrap(),
        "# changed\n"
    );
    // A deleted memory directory is recreated on the way back.
    fs::remove_dir_all(fx.work.join("projects/-h-git-a/memory")).unwrap();
    hook(&fx, &stop("s2"));
    let out = run(&fx, &["restore", "--all", "--at", &rev, "--yes"]);
    assert!(out.contains("2 file(s) differ"), "{out}");
    assert!(out.contains("symlink(s) skipped"), "{out}");
    assert_eq!(
        fs::read_to_string(fx.claude.join("CLAUDE.md")).unwrap(),
        "# global\n"
    );
    assert_eq!(
        fs::read_to_string(fx.work.join("projects/-h-git-a/memory/w.md")).unwrap(),
        "work fact\n"
    );
    // Never deletes: the file added after `rev` survives.
    assert!(fx.claude.join("projects/-h-git-a/memory/new.md").exists());
    // Idempotent.
    let out = run(&fx, &["restore", "--all", "--at", &rev, "--yes"]);
    assert!(out.contains("nothing to restore"), "{out}");
    // Both forms of misuse are refused.
    assert!(!cmd(&fx)
        .args(["restore"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!cmd(&fx)
        .args(["restore", "--all", "claude/CLAUDE.md"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn companions_subcommand_lists_status_and_source() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let out = run(&fx, &["companions"]);
    assert!(out.starts_with("no companions under"), "{out}");
    write(
        &fx.repo.join("companion/webapp/docs/a.zh-tw.md"),
        "---\ntype: Translation\nsources:\n  - resource: ~/git/webapp/docs/a.md\n    commit: abc\nstatus: stable\n---\n# 中文\n",
    );
    write(
        &fx.repo.join("companion/webapp/docs/new.zh-tw.md"),
        "---\nstatus: draft\nsources: []\n---\n# 草稿\n",
    );
    write(&fx.repo.join("companion/other/x.zh-tw.md"), "plain\n");
    write(
        &fx.repo.join("companion/webapp/_notes/eval.md"),
        "---\ntype: Note\nrelates_to:\n  - docs/a.md\n  - acme/webapp#7\nstatus: stable\n---\n# eval\n",
    );
    let out = run(&fx, &["companions"]);
    assert!(out.contains("stable  webapp/docs/a.zh-tw.md"), "{out}");
    assert!(out.contains("<- ~/git/webapp/docs/a.md"), "{out}");
    assert!(out.contains("draft   webapp/docs/new.zh-tw.md"), "{out}");
    assert!(out.contains("(no source yet)"), "{out}");
    assert!(out.contains("-       other/x.zh-tw.md"), "{out}");
    assert!(
        out.contains("webapp/_notes/eval.md") && out.contains("~ docs/a.md, acme/webapp#7"),
        "{out}"
    );
    let out = run(&fx, &["companions", "webapp"]);
    assert!(
        out.contains("webapp/docs/a.zh-tw.md") && !out.contains("other/"),
        "{out}"
    );
    let out = run(&fx, &["companions", "nope"]);
    assert!(out.starts_with("no companions for nope"), "{out}");
}

#[test]
fn companions_are_listed_in_the_index() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    write(
        &fx.repo.join("companion/webapp/docs/a.zh-tw.md"),
        "# 中文\n",
    );
    write(&fx.repo.join("companion/webapp/docs/b (1).md"), "x\n");
    hook(&fx, &stop("s1"));
    let idx = fs::read_to_string(fx.repo.join("index.md")).unwrap();
    assert!(
        idx.contains("[webapp/docs/a.zh-tw.md](companion/webapp/docs/a.zh-tw.md)"),
        "{idx}"
    );
    assert!(
        idx.contains("[webapp/docs/b (1).md](companion/webapp/docs/b%20%281%29.md)"),
        "{idx}"
    );
}

#[test]
fn diff_shows_what_the_next_sweep_would_commit() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let clean = run(&fx, &["diff"]);
    assert!(clean.contains("no changes"), "{clean}");
    fs::write(
        fx.claude.join("projects/-h-git-a/memory/one.md"),
        "fact, now longer\n",
    )
    .unwrap();
    let out = run(&fx, &["diff", "--stat"]);
    assert!(out.contains("-h-git-a/one.md"), "{out}");
    assert!(
        !out.contains("+fact, now longer"),
        "stat must not include the patch: {out}"
    );
    let full = run(&fx, &["diff"]);
    assert!(full.contains("+fact, now longer"), "{full}");
    // diff must not commit, and must not touch what the user staged in the real index
    assert_eq!(commits(&fx), 1);
    write(&fx.repo.join("manual/staged.md"), "mine\n");
    git(&fx, &["add", "manual/staged.md"]);
    run(&fx, &["diff"]);
    let staged = git(&fx, &["diff", "--cached", "--name-only"]);
    assert_eq!(
        staged.trim(),
        "manual/staged.md",
        "real index changed: {staged}"
    );
}

#[test]
fn restore_writes_an_old_version_back_only_when_confirmed() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    let live = fx.claude.join("projects/-h-git-a/memory/one.md");
    fs::write(&live, "fact v2\n").unwrap();
    hook(&fx, &stop("s1"));
    assert_eq!(commits(&fx), 2);
    // Declined: nothing changes.
    let out = cmd(&fx)
        .args(["restore", "claude/memory/-h-git-a/one.md", "--at", "HEAD~1"])
        .write_stdin("n\n")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("-fact v2") && text.contains("+fact") && text.contains("not restored"),
        "{text}"
    );
    assert_eq!(fs::read_to_string(&live).unwrap(), "fact v2\n");
    // Confirmed with --yes: the live file gets the old content and the turn is dirty.
    let out = run(
        &fx,
        &[
            "restore",
            "claude/memory/-h-git-a/one.md",
            "--at",
            "HEAD~1",
            "--yes",
        ],
    );
    assert!(out.contains("restored"), "{out}");
    assert_eq!(fs::read_to_string(&live).unwrap(), "fact\n");
    assert!(fx.repo.join(".git/memsaga/dirty").exists());
    hook(&fx, &stop("s1"));
    assert_eq!(
        commits(&fx),
        3,
        "the restored state is committed at the next Stop"
    );
    // A live path works too, and a deleted file can be brought back.
    fs::remove_file(&live).unwrap();
    hook(&fx, &stop("s1"));
    let out = run(
        &fx,
        &["restore", live.to_str().unwrap(), "--at", "HEAD~1", "--yes"],
    );
    assert!(out.contains("restored"), "{out}");
    assert_eq!(fs::read_to_string(&live).unwrap(), "fact\n");
    // A directory is not restorable.
    let dir_out = cmd(&fx)
        .args(["restore", "claude/memory/-h-git-a", "--yes"])
        .output()
        .unwrap();
    assert!(
        !dir_out.status.success()
            && String::from_utf8_lossy(&dir_out.stderr).contains("not a file")
    );
    // A live path that resolves outside every root (a skill dir symlinked away) is refused.
    let outside = fx.claude.parent().unwrap().join("elsewhere/foo");
    write(&outside.join("SKILL.md"), "outside\n");
    fs::remove_dir_all(fx.claude.join("skills/foo")).unwrap();
    std::os::unix::fs::symlink(&outside, fx.claude.join("skills/foo")).unwrap();
    let esc = cmd(&fx)
        .args(["restore", "claude/skills/foo/SKILL.md", "--yes"])
        .output()
        .unwrap();
    assert!(
        !esc.status.success(),
        "must refuse writing through an escaping symlink"
    );
    assert!(String::from_utf8_lossy(&esc.stderr).contains("outside every mirrored root"));
    assert_eq!(
        fs::read_to_string(outside.join("SKILL.md")).unwrap(),
        "outside\n",
        "nothing written outside"
    );
    // Out-of-scope paths are refused before anything is written.
    let out = cmd(&fx)
        .args(["restore", "claude/settings.json", "--yes"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not inside"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn install_skills_writes_bundled_skills_without_touching_hooks() {
    let fx = fixture("turn", "");
    let settings_before = fs::read_to_string(fx.claude.join("settings.json")).unwrap();
    let out = run(
        &fx,
        &[
            "install",
            "--root",
            fx.claude.to_str().unwrap(),
            "--skills",
            "--no-hooks",
        ],
    );
    assert!(
        out.contains("skills/memsaga-history: written")
            && out.contains("skills/companion-docs: written")
            && out.contains("skills/companions: written"),
        "{out}"
    );
    assert!(fx.claude.join("skills/memsaga-history/SKILL.md").exists());
    assert!(
        fs::read_to_string(fx.claude.join("skills/companion-docs/SKILL.md"))
            .unwrap()
            .starts_with("---\nname: companion-docs")
    );
    assert_eq!(
        fs::read_to_string(fx.claude.join("settings.json")).unwrap(),
        settings_before,
        "--no-hooks must not touch settings"
    );
    let again = run(
        &fx,
        &[
            "install",
            "--root",
            fx.claude.to_str().unwrap(),
            "--skill",
            "memsaga-history",
            "--no-hooks",
        ],
    );
    assert!(
        again.contains("memsaga-history: up to date") && !again.contains("companion-docs"),
        "{again}"
    );
    let bad = cmd(&fx)
        .args(["install", "--skill", "nope", "--no-hooks"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}

#[test]
fn legacy_projects_layout_is_renamed_and_old_revisions_stay_restorable() {
    let fx = fixture("turn", "");
    run(&fx, &["init", "--no-backfill"]);
    // Rewrite HEAD into the layout used before 2026-09-18.
    fs::create_dir_all(fx.repo.join("claude/projects/-h-git-a")).unwrap();
    git(
        &fx,
        &[
            "mv",
            "claude/memory/-h-git-a",
            "claude/projects/-h-git-a/memory",
        ],
    );
    git(&fx, &["commit", "-q", "-m", "legacy layout"]);
    let legacy = git(&fx, &["rev-parse", "HEAD"]).trim().to_string();
    let before = commits(&fx);

    // A plain Stop moves it back even though nothing in the live root changed.
    hook(&fx, &stop("s0"));
    assert_eq!(commits(&fx), before + 1);
    let subject = git(&fx, &["log", "-1", "--format=%s"]);
    assert!(
        subject.contains("claude:memory:-h-git-a") && !subject.contains("claude:projects"),
        "{subject}"
    );
    let files = head_files(&fx);
    assert!(
        files.iter().any(|f| f == "claude/memory/-h-git-a/one.md"),
        "{files:?}"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("claude/projects/")),
        "{files:?}"
    );
    assert!(!fx.repo.join("claude/projects").exists());
    // git sees the move as a rename, so history follows the file across it.
    let followed = git(
        &fx,
        &[
            "log",
            "--follow",
            "--format=%h",
            "--",
            "claude/memory/-h-git-a/one.md",
        ],
    );
    assert!(followed.lines().count() >= 2, "{followed}");

    // A revision from before the move is still readable through the new path.
    let live = fx.claude.join("projects/-h-git-a/memory/one.md");
    write(&live, "changed after the move\n");
    hook(&fx, &stop("s1"));
    let out = run(
        &fx,
        &[
            "restore",
            "claude/memory/-h-git-a/one.md",
            "--at",
            &legacy,
            "--yes",
        ],
    );
    assert!(out.contains("restored"), "{out}");
    assert_eq!(fs::read_to_string(&live).unwrap(), "fact\n");
    // The old spelling is still accepted as input.
    write(&live, "changed again\n");
    hook(&fx, &stop("s2"));
    let out = run(
        &fx,
        &[
            "restore",
            "claude/projects/-h-git-a/memory/one.md",
            "--at",
            &legacy,
            "--yes",
        ],
    );
    assert!(out.contains("restored"), "{out}");
    assert_eq!(fs::read_to_string(&live).unwrap(), "fact\n");
}

#[test]
fn dirs_may_not_name_memory_or_projects() {
    let fx = fixture("turn", "dirs = [\"skills\", \"memory\"]\n");
    let out = cmd(&fx).arg("config").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("must not contain \"memory\""),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
