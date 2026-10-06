//! Embeds the git commit the binary is built from, so `memsaga --version`
//! tells a branch build from a `main` build. Outside a git checkout (a
//! packaged source tree) the version is printed without it.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let version = env!("CARGO_PKG_VERSION");
    let full = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(sha) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            format!("{version} ({sha}{})", if dirty { "-dirty" } else { "" })
        }
        None => version.to_string(),
    };
    println!("cargo:rustc-env=MEMSAGA_VERSION={full}");
    // Rebuild when HEAD moves (a commit or a checkout), the index changes, or
    // a source file changes (the `-dirty` mark depends on the working tree;
    // naming any path here turns off Cargo's own rerun-on-any-change).
    for dir in ["src", "../memsaga-core/src", "../../skills"] {
        println!("cargo:rerun-if-changed={dir}");
    }
    // Paths through `--git-path`, which resolves each to where git keeps it:
    // in a linked worktree HEAD and the index are the worktree's own, while
    // branch refs live in the common git directory.
    let mut watch = vec!["HEAD".to_string(), "index".to_string()];
    watch.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for name in watch {
        if let Some(p) = git(&["rev-parse", "--path-format=absolute", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}
