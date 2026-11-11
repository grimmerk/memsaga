//! Holding back a file whose new lines look like a secret (grimmerk/memsaga#60).
//!
//! Before a commit, the lines the staged change ADDS are checked against a
//! short list of key shapes with a fixed prefix. A file with a match is taken
//! out of the commit; the other files commit as usual. The check runs only
//! when a sweep is about to commit, so the hooks' no-change path is untouched.
//!
//! The shapes are a hand-picked subset of gitleaks' rules
//! (<https://github.com/gitleaks/gitleaks>, `config/gitleaks.toml`, MIT
//! licence), kept to the ones a prefix, a character set and a length identify
//! with few false positives. Generic rules (entropy, `password=`, JWT) are left
//! out on purpose: measured on a real data repository of 6,447 files, JWTs
//! alone matched in 11 files of data dumps, and the strict shapes below in none.
//!
//! What a match reports is the rule, the line number and a fingerprint (a hash
//! of the matched text), never the text itself.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::process::Output;

/// Written on the same line, marks a match there as not a secret.
pub const ALLOW_MARKER: &str = "memsaga:allow-secret";

struct Rule {
    id: &'static str,
    prefixes: &'static [&'static str],
    /// Characters the key body may hold, after the prefix.
    body: fn(u8) -> bool,
    min: usize,
    max: usize,
    /// Text the body must contain (OpenAI keys carry `T3BlbkFJ`).
    contains: Option<&'static str>,
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn alnum_dash_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}
fn alnum_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}
fn alnum_dash(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}
fn letter(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

const RULES: &[Rule] = &[
    Rule {
        id: "anthropic-api-key",
        prefixes: &["sk-ant-"],
        body: alnum_dash_underscore,
        min: 32,
        max: 200,
        contains: None,
    },
    Rule {
        id: "openai-api-key",
        prefixes: &["sk-"],
        body: alnum_dash_underscore,
        min: 40,
        max: 200,
        contains: Some("T3BlbkFJ"),
    },
    Rule {
        id: "github-token",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        body: alnum,
        min: 36,
        max: 36,
        contains: None,
    },
    Rule {
        id: "github-fine-grained-token",
        prefixes: &["github_pat_"],
        body: alnum_underscore,
        min: 70,
        max: 100,
        contains: None,
    },
    Rule {
        id: "aws-access-key-id",
        prefixes: &["AKIA", "ASIA", "ABIA", "ACCA"],
        body: upper_digit,
        min: 16,
        max: 16,
        contains: None,
    },
    Rule {
        id: "slack-token",
        prefixes: &[
            "xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-", "xoxo-", "xoxe-",
        ],
        body: alnum_dash,
        min: 20,
        max: 300,
        contains: None,
    },
    Rule {
        id: "google-api-key",
        prefixes: &["AIza"],
        body: alnum_dash_underscore,
        min: 35,
        max: 35,
        contains: None,
    },
    Rule {
        id: "stripe-live-key",
        prefixes: &["sk_live_", "rk_live_"],
        body: alnum,
        min: 20,
        max: 120,
        contains: None,
    },
    Rule {
        id: "npm-token",
        prefixes: &["npm_"],
        body: alnum,
        min: 36,
        max: 36,
        contains: None,
    },
    Rule {
        id: "gitlab-token",
        prefixes: &["glpat-"],
        body: alnum_dash_underscore,
        min: 20,
        max: 60,
        contains: None,
    },
    Rule {
        id: "huggingface-token",
        prefixes: &["hf_"],
        body: letter,
        min: 34,
        max: 34,
        contains: None,
    },
];

/// One match in a line: the rule and the matched text's byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub rule: &'static str,
    pub start: usize,
    pub end: usize,
}

/// A placeholder rather than a key: AWS's documented example, or a body of
/// very few distinct characters (`xxxx…`, `0000…`).
fn placeholder(body: &[u8]) -> bool {
    if body.windows(7).any(|w| w == b"EXAMPLE") {
        return true;
    }
    let distinct: BTreeSet<u8> = body.iter().copied().collect();
    distinct.len() < 6
}

/// Every key shape in `line`.
pub fn find(line: &str) -> Vec<Found> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    if let Some(f) = private_key_header(line) {
        out.push(f);
    }
    let mut i = 0;
    while i < bytes.len() {
        // A key starts at a word boundary: `xsk-…` or `my_ghp_…` is not one.
        let boundary = i == 0 || !alnum_dash_underscore(bytes[i - 1]);
        let mut matched = None;
        if boundary {
            'rules: for rule in RULES {
                for p in rule.prefixes {
                    if !bytes[i..].starts_with(p.as_bytes()) {
                        continue;
                    }
                    let body_start = i + p.len();
                    let body_end = body_start
                        + bytes[body_start..]
                            .iter()
                            .take_while(|b| (rule.body)(**b))
                            .count();
                    let body = &bytes[body_start..body_end];
                    if body.len() < rule.min || body.len() > rule.max || placeholder(body) {
                        continue;
                    }
                    // A longer run of key-like characters than the shape
                    // allows is some other identifier.
                    if body_end < bytes.len()
                        && alnum_dash_underscore(bytes[body_end])
                        && rule.max == rule.min
                    {
                        continue;
                    }
                    if let Some(c) = rule.contains {
                        if !body.windows(c.len()).any(|w| w == c.as_bytes()) {
                            continue;
                        }
                    }
                    matched = Some(Found {
                        rule: rule.id,
                        start: i,
                        end: body_end,
                    });
                    break 'rules;
                }
            }
        }
        match matched {
            Some(f) => {
                i = f.end;
                out.push(f);
            }
            None => i += 1,
        }
    }
    out
}

/// `-----BEGIN [WORDS ]PRIVATE KEY-----`, the words being upper-case
/// (`RSA`, `OPENSSH`, `EC`, `ENCRYPTED`), so prose such as
/// `-----BEGIN ... PRIVATE KEY-----` is not one.
fn private_key_header(line: &str) -> Option<Found> {
    const HEAD: &str = "-----BEGIN ";
    const TAIL: &str = "PRIVATE KEY-----";
    let start = line.find(HEAD)?;
    let rest = &line[start + HEAD.len()..];
    let end = rest.find(TAIL)?;
    let words = &rest[..end];
    if !words
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b' ')
    {
        return None;
    }
    Some(Found {
        rule: "private-key",
        start,
        end: start + HEAD.len() + end + TAIL.len(),
    })
}

/// A short, stable hash of a matched text: FNV-1a, 64 bits, 12 hex digits.
/// Stable across versions and machines (unlike std's hasher), so a
/// fingerprint written into `allow_secrets` keeps working.
pub fn fingerprint(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", h >> 16)
}

/// A held file: which rule matched, on which line of the new file, and the
/// fingerprint of the matched text. The first match per file only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub rule: String,
    pub line: usize,
    pub fingerprint: String,
}

/// Does `text` (a session title bound for a commit message) hold a key
/// shape? The allow marker does not apply here: it is for a line in a file.
pub fn text_has_secret(text: &str) -> bool {
    text.lines().any(|l| !find(l).is_empty())
}

/// Read a `git diff -U0` and return the files whose added lines hold a key
/// shape. A match whose text also appears on a removed line anywhere in the
/// diff is not new: it is already in `HEAD` (a moved line, a renamed file, an
/// edited line around an old key), and holding it would only block the rest
/// of that change. `allow` holds fingerprints and repository paths never to
/// report. Binary files have no lines in the diff and are skipped.
pub fn scan_diff(diff: &str, allow: &[String]) -> Vec<Hit> {
    struct Added<'a> {
        path: String,
        line: usize,
        text: &'a str,
    }
    let mut added: Vec<Added> = Vec::new();
    let mut removed: BTreeSet<&str> = BTreeSet::new();
    let mut path = String::new();
    let (mut minus_left, mut plus_left, mut new_line) = (0usize, 0usize, 0usize);
    for l in diff.split('\n') {
        if minus_left > 0 || plus_left > 0 {
            if let Some(t) = l.strip_prefix('-') {
                minus_left = minus_left.saturating_sub(1);
                for f in find(t) {
                    removed.insert(&t[f.start..f.end]);
                }
                continue;
            }
            if let Some(t) = l.strip_prefix('+') {
                plus_left = plus_left.saturating_sub(1);
                added.push(Added {
                    path: path.clone(),
                    line: new_line,
                    text: t,
                });
                new_line += 1;
                continue;
            }
            if l.starts_with('\\') {
                continue; // "\ No newline at end of file"
            }
            // Anything else ends the hunk (a malformed count): fall through.
            minus_left = 0;
            plus_left = 0;
        }
        if let Some(p) = l.strip_prefix("+++ ") {
            path = diff_path(p);
        } else if let Some(h) = l.strip_prefix("@@ ") {
            if let Some((m, p, start)) = hunk_counts(h) {
                minus_left = m;
                plus_left = p;
                new_line = start;
            }
        }
    }
    let mut hits: Vec<Hit> = Vec::new();
    for a in added {
        if a.path.is_empty()
            || a.text.contains('\0')
            || a.text.contains(ALLOW_MARKER)
            || allow.iter().any(|x| x == &a.path)
            || hits.iter().any(|h| h.path == a.path)
        {
            continue;
        }
        for f in find(a.text) {
            let matched = &a.text[f.start..f.end];
            let fp = fingerprint(matched);
            if removed.contains(matched) || allow.iter().any(|x| x == &fp) {
                continue;
            }
            hits.push(Hit {
                path: a.path.clone(),
                rule: f.rule.to_string(),
                line: a.line,
                fingerprint: fp,
            });
            break;
        }
    }
    hits
}

/// The path of a `+++ b/<path>` line; empty for `/dev/null` (a deletion).
fn diff_path(p: &str) -> String {
    let p = p.trim_end_matches('\r');
    if p == "/dev/null" {
        return String::new();
    }
    let unquoted = if p.starts_with('"') && p.ends_with('"') && p.len() >= 2 {
        unquote(&p[1..p.len() - 1])
    } else {
        p.to_string()
    };
    unquoted
        .strip_prefix("b/")
        .map(str::to_string)
        .unwrap_or(unquoted)
}

/// Git's C-style quoting of a path with special characters (core.quotepath
/// off leaves non-ASCII alone; tabs, newlines, quotes and backslashes are
/// still escaped).
fn unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// `-a[,b] +c[,d] @@…` → (b, d, c); a missing count is 1.
fn hunk_counts(h: &str) -> Option<(usize, usize, usize)> {
    let mut parts = h.split_whitespace();
    let minus = parts.next()?.strip_prefix('-')?;
    let plus = parts.next()?.strip_prefix('+')?;
    let count = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (_, m) = count(minus)?;
    let (start, p) = count(plus)?;
    Some((m, p, start))
}

/// The arguments of the diff `scan_diff` reads: the index against `base`.
pub fn diff_args(base: &str) -> Vec<&str> {
    vec![
        "-c",
        "core.quotepath=off",
        "diff",
        "--cached",
        "-U0",
        "--no-renames",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        // A text file marked `-diff` in .gitattributes would otherwise show
        // as "Binary files differ" and go unchecked; lines of a real binary
        // file holding a NUL are skipped by `scan_diff`.
        "--text",
        base,
    ]
}

/// What the staged change is compared with: `HEAD`, or the empty tree in a
/// repository with no commit yet (every staged line is then added).
pub fn diff_base(run: &dyn Fn(&[&str]) -> Result<Output>) -> Result<String> {
    let head = run(&["rev-parse", "-q", "--verify", "HEAD^{commit}"])?;
    if head.status.success() {
        return Ok("HEAD".into());
    }
    let out = run(&["hash-object", "-t", "tree", "/dev/null"])?;
    if !out.status.success() {
        bail!("git hash-object failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Scan what `run`'s index holds against `HEAD` and take each held file out
/// of that index (back to its `HEAD` version, or removed when `HEAD` has none).
/// The working tree is not touched here; the caller decides what to do with
/// the held files' copies. `run` is a git runner bound to the repository and
/// to the index in use (the real one for a sweep, a temporary one for `diff`).
pub fn unstage_held(run: &dyn Fn(&[&str]) -> Result<Output>, allow: &[String]) -> Result<Vec<Hit>> {
    // `HEAD` first: one git process in the common case. Only a repository
    // with no commit yet needs the empty tree, found by a second call.
    let mut base = "HEAD".to_string();
    let mut out = run(&diff_args(&base))?;
    if !out.status.success() {
        base = diff_base(run)?;
        out = run(&diff_args(&base))?;
    }
    if !out.status.success() {
        bail!(
            "git diff for the secret check failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let hits = scan_diff(&String::from_utf8_lossy(&out.stdout), allow);
    for h in &hits {
        let spec = format!(":(literal){}", h.path);
        let r = if base == "HEAD" {
            run(&["reset", "-q", "HEAD", "--", &spec])?
        } else {
            run(&["rm", "-q", "--cached", "--ignore-unmatch", "--", &spec])?
        };
        if !r.status.success() && !r.stderr.is_empty() {
            bail!(
                "unstaging a held file failed: {}",
                String::from_utf8_lossy(&r.stderr).trim()
            );
        }
    }
    // Do not trust exit codes alone (`git reset` exits 1 when files stay
    // modified): check that no held path is still staged, or stop the commit.
    if !hits.is_empty() {
        let specs: Vec<String> = hits
            .iter()
            .map(|h| format!(":(literal){}", h.path))
            .collect();
        let mut args = vec!["diff", "--cached", "--name-only", base.as_str(), "--"];
        args.extend(specs.iter().map(String::as_str));
        let left = run(&args)?;
        if !left.status.success() || !left.stdout.is_empty() {
            bail!("a held file is still staged; not committing");
        }
    }
    Ok(hits)
}

/// The reporter line for a held set, or None when it is empty.
pub fn notice(held: &[Hit]) -> Option<String> {
    let first = held.first()?;
    let more = if held.len() > 1 {
        format!(" and {} more", held.len() - 1)
    } else {
        String::new()
    };
    Some(format!(
        "memsaga: held back {} (line {} looks like a {}, fingerprint {}){more}; not committed until the line is gone, see `memsaga status`",
        first.path, first.line, first.rule, first.fingerprint
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built by concatenation so this file holds no complete key shape.
    fn key(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }
    const B36: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8";

    #[test]
    fn finds_each_shape_and_reports_no_text() {
        let cases = [
            (
                key("sk-", "ant-api03-") + &B36.repeat(2),
                "anthropic-api-key",
            ),
            (key("gh", "p_") + B36, "github-token"),
            (key("AKI", "A") + "Q3EGRQ7ZJXJ4TM5P", "aws-access-key-id"),
            (key("AI", "za") + &B36[..35], "google-api-key"),
            (
                key("xox", "b-") + "1234567890-1234567890-" + &B36[..24],
                "slack-token",
            ),
            (key("sk_li", "ve_") + &B36[..24], "stripe-live-key"),
            (key("np", "m_") + B36, "npm-token"),
            (key("glp", "at-") + &B36[..20], "gitlab-token"),
            (
                key("sk-pr", "oj-") + &B36[..20] + "T3Blbk" + "FJ" + &B36[..20],
                "openai-api-key",
            ),
            (
                key("-----BEGIN OPENSSH ", "PRIVATE KEY-----"),
                "private-key",
            ),
        ];
        for (k, rule) in cases {
            let line = format!("token = \"{k}\"");
            let f = find(&line);
            assert_eq!(f.len(), 1, "{rule}: {f:?}");
            assert_eq!(f[0].rule, rule);
            assert_eq!(&line[f[0].start..f[0].end], k);
        }
    }

    #[test]
    fn prose_and_placeholders_are_not_keys() {
        for line in [
            "use an sk-ant- key from the console",
            "a key like sk-ant-...",
            "-----BEGIN ... PRIVATE KEY-----",
            &key("AKIA", "IOSFODNN7EXAMPLE"),
            &key("sk-ant-", &"x".repeat(40)),
            &(key("my", "ghp_") + B36),     // no word boundary
            &(key("gh", "p_") + B36 + "Z"), // too long for the shape
            &key("sk-", &B36.repeat(2)),    // `sk-` without OpenAI's marker
            "see https://example.com/xoxb-docs",
        ] {
            assert!(find(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn the_allow_marker_does_not_clear_a_title() {
        let k = key("gh", "p_") + B36;
        assert!(text_has_secret(&format!("{k} {ALLOW_MARKER}")));
        assert!(!text_has_secret("an ordinary title"));
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("abc").len(), 12);
    }

    fn diff_with(path: &str, minus: &[&str], plus: &[&str]) -> String {
        let mut d = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,{} +3,{} @@\n",
            minus.len(),
            plus.len()
        );
        for m in minus {
            d.push_str(&format!("-{m}\n"));
        }
        for p in plus {
            d.push_str(&format!("+{p}\n"));
        }
        d
    }

    #[test]
    fn scan_reports_path_line_and_skips_allowed_or_moved() {
        let k = key("gh", "p_") + B36;
        let d = diff_with("claude/CLAUDE.md", &[], &["ok", &format!("t={k}")]);
        let hits = scan_diff(&d, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "claude/CLAUDE.md");
        assert_eq!(hits[0].line, 4);
        assert!(!format!("{hits:?}").contains(&k));
        // Inline marker.
        let d2 = diff_with("a.md", &[], &[&format!("t={k} {ALLOW_MARKER}")]);
        assert!(scan_diff(&d2, &[]).is_empty());
        // Allowed by fingerprint, or by path.
        assert!(scan_diff(&d, &[fingerprint(&k)]).is_empty());
        assert!(scan_diff(&d, &["claude/CLAUDE.md".into()]).is_empty());
        // Already in HEAD: the same key on a removed line.
        let d3 = diff_with("a.md", &[&format!("old {k}")], &[&format!("new {k}")]);
        assert!(scan_diff(&d3, &[]).is_empty());
        // A `+++` inside a hunk is content, not a header.
        let d4 = diff_with("a.md", &[], &[&format!("++ {k}")]);
        assert_eq!(scan_diff(&d4, &[])[0].path, "a.md");
    }
}
