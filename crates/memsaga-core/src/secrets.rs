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
        // The older `A3T` form: one more character before the 16.
        id: "aws-access-key-id",
        prefixes: &["A3T"],
        body: upper_digit,
        min: 17,
        max: 17,
        contains: None,
    },
    Rule {
        id: "slack-token",
        prefixes: &[
            "xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-", "xoxo-", "xoxe-", "xapp-",
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
        // Classic tokens have 20 characters; routable ones (GitLab 17.x)
        // 27 to 300, then a dot and a short suffix the body stops at.
        prefixes: &["glpat-"],
        body: alnum_dash_underscore,
        min: 20,
        max: 300,
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

/// `-----BEGIN [WORDS ]PRIVATE KEY-----`, or `… PRIVATE KEY BLOCK-----`
/// (PGP), the words being upper-case (`RSA`, `OPENSSH`, `EC`, `ENCRYPTED`,
/// `PGP`), so prose such as `-----BEGIN ... PRIVATE KEY-----` is not one.
fn private_key_header(line: &str) -> Option<Found> {
    const HEAD: &str = "-----BEGIN ";
    const KEY: &str = "PRIVATE KEY";
    let start = line.find(HEAD)?;
    let rest = &line[start + HEAD.len()..];
    let end = rest.find(KEY)?;
    let words = &rest[..end];
    if !words.bytes().all(|b| {
        b.is_ascii_uppercase() || b.is_ascii_digit() || b == b' ' || b == b'_' || b == b'-'
    }) {
        return None;
    }
    let after = &rest[end + KEY.len()..];
    let tail = ["-----", " BLOCK-----"]
        .into_iter()
        .find(|t| after.starts_with(t))?;
    Some(Found {
        rule: "private-key",
        start,
        end: start + HEAD.len() + end + KEY.len() + tail.len(),
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
    text.lines()
        .any(|l| find(l).iter().any(|f| identity(f, l).is_some()))
}

/// Read a `git diff -U0` and return the files whose added lines hold a key
/// shape. A match whose text also appears on a removed line anywhere in the
/// diff is not new: it is already in `HEAD` (a moved line, a renamed file, an
/// edited line around an old key), and holding it would only block the rest
/// of that change. `allow` holds fingerprints and repository paths never to
/// report. Binary files have no lines in the diff and are skipped.
///
/// A private key spans lines, so the diff alone cannot judge it: its BEGIN
/// line may be unchanged (a key rotated in place), and armor headers or a
/// blank line may sit before the base64 (PGP, encrypted PEM). For a file
/// whose added lines include a BEGIN line or a base64 line, `content` gives
/// the staged file and `pem_blocks` finds the whole blocks in it.
pub fn scan_diff(
    diff: &str,
    allow: &[String],
    content: &dyn Fn(&str) -> Option<String>,
) -> Vec<Hit> {
    struct Added<'a> {
        path: String,
        line: usize,
        text: &'a str,
    }
    let mut added: Vec<Added> = Vec::new();
    let mut removed: Vec<&str> = Vec::new();
    let mut path = String::new();
    let (mut minus_left, mut plus_left, mut new_line) = (0usize, 0usize, 0usize);
    for l in diff.split('\n') {
        if minus_left > 0 || plus_left > 0 {
            if let Some(t) = l.strip_prefix('-') {
                minus_left = minus_left.saturating_sub(1);
                removed.push(t);
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
            // An unchanged line inside the hunk counts on both sides. `-U0`
            // and `--inter-hunk-context=0` leave none, but a hunk must not end
            // early if git prints one anyway: the lines after it would go
            // unread. An empty line is one too (`diff.suppressBlankEmpty`).
            if l.is_empty() || l.starts_with(' ') {
                minus_left = minus_left.saturating_sub(1);
                plus_left = plus_left.saturating_sub(1);
                new_line += 1;
                continue;
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
    for a in &added {
        if a.path.is_empty()
            || a.text.contains('\0')
            || a.text.contains(ALLOW_MARKER)
            || allow.iter().any(|x| x == &a.path)
            || hits.iter().any(|h| h.path == a.path)
        {
            continue;
        }
        for f in find(a.text) {
            // What identifies the key: the match itself, or for a private key
            // on one line (JSON's literal `\n`) its base64 body; a header
            // without one is left to the block scan below.
            let Some(matched) = identity(&f, a.text) else {
                continue;
            };
            let fp = fingerprint(matched);
            // Already in HEAD: the same text on a removed line (a moved line, a
            // renamed file, an edit around an old key) is not new.
            if removed.iter().any(|r| r.contains(matched)) || allow.iter().any(|x| x == &fp) {
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
    // Private keys over several lines, per candidate file.
    let mut candidates: Vec<&str> = added
        .iter()
        .filter(|a| !a.path.is_empty() && !allow.iter().any(|x| x == &a.path))
        .filter(|a| private_key_header(a.text).is_some() || base64_line(a.text).is_some())
        .map(|a| a.path.as_str())
        .collect();
    candidates.dedup();
    for path in candidates {
        if hits.iter().any(|h| h.path == path) {
            continue;
        }
        let new_lines: BTreeSet<usize> = added
            .iter()
            .filter(|a| a.path == path)
            .map(|a| a.line)
            .collect();
        let Some(text) = content(path) else {
            // The staged file cannot be read: hold it when a BEGIN line was
            // added, rather than guess.
            if let Some(a) = added
                .iter()
                .find(|a| a.path == path && private_key_header(a.text).is_some())
            {
                hits.push(Hit {
                    path: path.to_string(),
                    rule: "private-key".into(),
                    line: a.line,
                    fingerprint: fingerprint(a.text),
                });
            }
            continue;
        };
        for block in pem_blocks(&text) {
            if block.header.contains(ALLOW_MARKER) {
                continue;
            }
            // New when a body line was added and is not a moved line.
            let fresh = block
                .body
                .iter()
                .find(|(n, t)| new_lines.contains(n) && !removed.iter().any(|r| r.contains(*t)));
            if let Some((n, t)) = fresh {
                let fp = fingerprint(t);
                if allow.iter().any(|x| x == &fp) {
                    continue;
                }
                hits.push(Hit {
                    path: path.to_string(),
                    rule: "private-key".into(),
                    line: *n,
                    fingerprint: fp,
                });
                break;
            }
        }
    }
    hits
}

/// The base64 text of a line that is nothing but base64 (after indentation,
/// quotes and a trailing comma), at least 16 characters, with both upper and
/// lower case letters as a key body has (a hex hash has one case only). Only
/// picks the files to read: a key wrapped narrower than the usual 64 columns
/// must still make its file a candidate; `pem_blocks` then requires at least
/// 40 base64 characters over the whole block.
fn base64_line(line: &str) -> Option<&str> {
    let t = line.trim_matches([' ', '\t', '"', '\'', ',', '\r'].as_slice());
    let ok = t.len() >= 16
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        && t.bytes().any(|b| b.is_ascii_uppercase())
        && t.bytes().any(|b| b.is_ascii_lowercase());
    ok.then_some(t)
}

/// One private key block in a file: its BEGIN line and its base64 lines,
/// each with its 1-based line number.
struct Block<'a> {
    header: &'a str,
    body: Vec<(usize, &'a str)>,
}

/// Every `BEGIN … PRIVATE KEY` block in `text` whose base64 body has at least
/// 40 characters. Between the BEGIN line and the base64, armor headers
/// (`Version: …`, `Proc-Type: …`, `DEK-Info: …`) and blank lines are skipped;
/// the body runs until the END line or the first line that is not base64. A
/// BEGIN line with no body, such as a constant in code, is no block.
fn pem_blocks(text: &str) -> Vec<Block<'_>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if private_key_header(lines[i]).is_none() {
            i += 1;
            continue;
        }
        let header = lines[i];
        let mut j = i + 1;
        let mut skipped = 0;
        while j < lines.len() && skipped < 10 {
            let t = lines[j].trim();
            if t.is_empty() || (t.contains(": ") && !t.starts_with("-----")) {
                j += 1;
                skipped += 1;
            } else {
                break;
            }
        }
        let mut body = Vec::new();
        while j < lines.len() {
            let t = lines[j].trim_matches([' ', '\t', '"', '\'', ',', '\r'].as_slice());
            if t.starts_with("-----") || t.is_empty() {
                break;
            }
            if !t
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
            {
                break;
            }
            body.push((j + 1, t));
            j += 1;
        }
        if body.iter().map(|(_, t)| t.len()).sum::<usize>() >= 40 {
            out.push(Block { header, body });
        }
        i = j.max(i + 1);
    }
    out
}

/// The text that identifies the key `f` found in `line`: the match itself,
/// or, for a private key header, the base64 run that follows it on the same
/// line (after a literal `\n`, as JSON stores it). None for a header with no
/// body on its line: `pem_blocks` judges that one with the whole file.
fn identity<'a>(f: &Found, line: &'a str) -> Option<&'a str> {
    if f.rule != "private-key" {
        return Some(&line[f.start..f.end]);
    }
    let body = |s: &'a str| -> Option<&'a str> {
        let s = s.trim_start_matches([' ', '\t', '"', '\''].as_slice());
        let s = s
            .strip_prefix("\\r\\n")
            .or_else(|| s.strip_prefix("\\n"))
            .unwrap_or(s);
        let n = s
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'=')
            .count();
        (n >= 40).then(|| &s[..n])
    };
    body(&line[f.end..])
}

/// The path of a `+++ b/<path>` line; empty for `/dev/null` (a deletion).
/// A quoted path ends at its closing quote; an unquoted one at the tab git
/// adds after a name holding a space (`+++ b/my notes.md\t`).
fn diff_path(p: &str) -> String {
    let p = p.trim_end_matches('\r');
    if p == "/dev/null" {
        return String::new();
    }
    let unquoted = if let Some(inner) = p.strip_prefix('"') {
        let b = inner.as_bytes();
        let mut i = 0;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        unquote(&inner[..i.min(inner.len())])
    } else {
        p.split('\t').next().unwrap_or(p).to_string()
    };
    unquoted
        .strip_prefix("b/")
        .map(str::to_string)
        .unwrap_or(unquoted)
}

/// Git's C-style quoting of a path with special characters (core.quotepath
/// off leaves non-ASCII alone; control characters, quotes and backslashes
/// are still escaped, other control bytes as three octal digits).
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let e = b[i + 1];
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        if let Some(d) = octal {
            out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
            i += 4;
            continue;
        }
        out.push(match e {
            b't' => b'\t',
            b'n' => b'\n',
            b'r' => b'\r',
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'v' => 11,
            o => o,
        });
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        // The parser reads this exact shape, whatever the user's git config
        // says: no unchanged lines between merged hunks
        // (`diff.interHunkContext`), and `a/` / `b/` before every path
        // (`diff.mnemonicPrefix`, `diff.noprefix`).
        "--inter-hunk-context=0",
        "--src-prefix=a/",
        "--dst-prefix=b/",
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
    let content = |path: &str| -> Option<String> {
        let o = run(&["show", &format!(":{path}")]).ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let hits = scan_diff(&String::from_utf8_lossy(&out.stdout), allow, &content);
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
    // Fail closed: scan the index again and stop the commit if anything still
    // matches. This does not depend on the paths above being right (a name
    // that is not UTF-8 is mangled by the lossy decoding of the diff, so the
    // unstage could miss it), nor on exit codes (`git reset` exits 1 when
    // files stay modified).
    if !hits.is_empty() {
        let again = run(&diff_args(&base))?;
        if !again.status.success() {
            bail!("the secret check could not read the index again");
        }
        let left = scan_diff(&String::from_utf8_lossy(&again.stdout), allow, &content);
        if !left.is_empty() {
            let names: Vec<&str> = left.iter().map(|h| h.path.as_str()).collect();
            bail!(
                "a file that looks like it holds a secret is still staged ({})",
                names.join(", ")
            );
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

    fn none(_: &str) -> Option<String> {
        None
    }

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

    /// Keys at the lengths their issuers produce, as gitleaks' rules
    /// describe them (config/gitleaks.toml, read 2026-11-11), not only at
    /// this table's own minimums.
    #[test]
    fn finds_keys_at_their_real_lengths() {
        let b = |n: usize| B36.repeat(4)[..n].to_string();
        let cases = [
            (
                key("sk-ant-", "api03-") + &b(93) + "AA",
                "anthropic-api-key",
            ),
            (
                key("sk-ant-", "admin01-") + &b(93) + "AA",
                "anthropic-api-key",
            ),
            (
                key("sk-", "proj-") + &b(74) + "T3Blbk" + "FJ" + &b(74),
                "openai-api-key",
            ),
            (
                key("sk-", "") + &b(20) + "T3Blbk" + "FJ" + &b(20),
                "openai-api-key",
            ),
            (
                key("github", "_pat_") + &(b(22) + "_" + &b(59)),
                "github-fine-grained-token",
            ),
            (key("gh", "s_") + &b(36), "github-token"),
            (
                key("A3", "T") + "A" + "Q3EGRQ7ZJXJ4TM5P",
                "aws-access-key-id",
            ),
            (
                key("xox", "p-") + "1234567890-1234567890-1234567890-" + &b(32),
                "slack-token",
            ),
            (
                key("xa", "pp-") + "1-A0B1C2D3E4F-1234567890-" + &b(40),
                "slack-token",
            ),
            (key("glpat", "-") + &b(80) + ".01abcdefg", "gitlab-token"),
            (
                key("hf", "_") + "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEfGh",
                "huggingface-token",
            ),
            (key("rk_", "live_") + &b(99), "stripe-live-key"),
            (
                key("-----BEGIN PGP ", "PRIVATE KEY BLOCK-----"),
                "private-key",
            ),
            (key("-----BEGIN RSA ", "PRIVATE KEY-----"), "private-key"),
        ];
        for (k, rule) in cases {
            let line = format!("export TOKEN='{k}'");
            let f = find(&line);
            assert_eq!(f.len(), 1, "{rule}: {f:?}");
            assert_eq!(f[0].rule, rule);
            assert!(line[f[0].start..f[0].end].starts_with(&k[..8]), "{rule}");
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
    fn a_private_key_needs_its_body_and_is_known_by_it() {
        let head = key("-----BEGIN RSA ", "PRIVATE KEY-----");
        let end = key("-----END RSA ", "PRIVATE KEY-----");
        let body1 = "MIIEowIBAAKCAQEAu1SU1LfVLPHCozMxH2Mo4lgOEePzNm0tRgeLezV6ffAt0gunVTLw";
        let body2 = "MIIEpAIBAAKCAQEA7bq98fdV3Kq3Y9mR0nQ2sX1zW4vB8cH6jL0pT5uE2iO9aD3fG7kM";
        // `diff_with` puts the added lines at line 3: the staged file is two
        // lines, then `lines`.
        let file = |lines: &[&str]| {
            let text = format!("a\nb\n{}\n", lines.join("\n"));
            move |_: &str| Some(text.clone())
        };
        // A header alone (a constant in code, a strings dump): not a key.
        let d = diff_with("a.txt", &[], &[&head]);
        assert!(scan_diff(&d, &[], &file(&[&head])).is_empty());
        let q = format!("x = \"{head}\"");
        assert!(scan_diff(&diff_with("a.txt", &[], &[&q]), &[], &file(&[&q])).is_empty());
        // A base64 line with no BEGIN line around it: not a key.
        assert!(scan_diff(&diff_with("b.txt", &[], &[body1]), &[], &file(&[body1])).is_empty());
        // A PEM file.
        let pem_lines = [head.as_str(), body1, end.as_str()];
        let pem = scan_diff(&diff_with("k.pem", &[], &pem_lines), &[], &file(&pem_lines));
        assert_eq!(pem.len(), 1);
        assert_eq!(pem[0].line, 4);
        assert_eq!(pem[0].fingerprint, fingerprint(body1));
        // JSON keeps it on one line after a literal \n.
        let json = format!("\"key\": \"{head}\\n{body1}\\n\"");
        assert_eq!(
            scan_diff(&diff_with("s.json", &[], &[&json]), &[], &none).len(),
            1
        );
        // Replacing one key with another: same header, new body.
        let swap = diff_with("k.pem", &[&head, body1], &[&head, body2]);
        assert_eq!(scan_diff(&swap, &[], &file(&[&head, body2, &end])).len(), 1);
        // Rotated in place: only the body line changed, the BEGIN line is not
        // in the diff at all.
        let rotated = diff_with("k.pem", &[body1], &[body2]);
        let staged_text = format!("a\n{head}\n{body2}\n{end}\n");
        let staged = move |_: &str| Some(staged_text.clone());
        assert_eq!(scan_diff(&rotated, &[], &staged).len(), 1);
        // Rotated in place with narrow lines (32 columns): each line is short,
        // the block is not.
        let narrow = [&body2[..32], &body2[32..64]];
        let rotated_narrow = diff_with("k.pem", &[&body1[..32], &body1[32..64]], &narrow);
        let narrow_text = format!("a\n{head}\n{}\n{}\n{end}\n", narrow[0], narrow[1]);
        let narrow_file = move |_: &str| Some(narrow_text.clone());
        assert_eq!(scan_diff(&rotated_narrow, &[], &narrow_file).len(), 1);
        // PGP and encrypted PEM: armor headers and a blank line first.
        let pgp_head = key("-----BEGIN PGP ", "PRIVATE KEY BLOCK-----");
        let pgp = [pgp_head.as_str(), "Version: GnuPG v2", "", body1, "=Ab3d"];
        assert_eq!(
            scan_diff(&diff_with("k.asc", &[], &pgp), &[], &file(&pgp)).len(),
            1
        );
        let enc = [
            head.as_str(),
            "Proc-Type: 4,ENCRYPTED",
            "DEK-Info: AES-128-CBC,3F17F5316E2BAC89",
            "",
            body1,
        ];
        assert_eq!(
            scan_diff(&diff_with("k.pem", &[], &enc), &[], &file(&enc)).len(),
            1
        );
        // The same key moved or re-indented: not new.
        let moved = diff_with("k.pem", &[&head, body1], &[&format!(" {head}"), body1]);
        let moved_file = file(&[&format!(" {head}"), body1]);
        assert!(scan_diff(&moved, &[], &moved_file).is_empty());
        // The staged file cannot be read and a BEGIN line was added: held.
        assert_eq!(
            scan_diff(&diff_with("k.pem", &[], &[&head]), &[], &none).len(),
            1
        );
        // A title with a header alone is fine; with a body it is not.
        assert!(!text_has_secret(&head));
        assert!(text_has_secret(&format!("{head}\\n{body1}")));
    }

    #[test]
    fn unchanged_lines_inside_a_hunk_do_not_end_it() {
        let k = key("gh", "p_") + B36;
        let d = format!(
            "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1,3 +1,4 @@\n-x\n+y\n same\n\n+{k}\n"
        );
        let hits = scan_diff(&d, &[], &none);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 4);
    }

    #[test]
    fn header_paths_lose_git_s_trailing_tab_and_quotes() {
        assert_eq!(diff_path("b/my notes.md\t"), "my notes.md");
        assert_eq!(diff_path(r#""b/a\tb.md""#), "a\tb.md");
        assert_eq!(diff_path("/dev/null"), "");
    }

    #[test]
    fn quoted_paths_are_decoded() {
        assert_eq!(unquote(r"a\tb"), "a\tb");
        assert_eq!(unquote(r#"a\001b\\c\"d"#), "a\u{1}b\\c\"d");
    }

    /// A git runner that answers every diff with `diff` and every other
    /// command with success, as if the unstage had worked when it did not.
    fn fake_git(diff: String) -> impl Fn(&[&str]) -> Result<Output> {
        use std::os::unix::process::ExitStatusExt;
        move |args: &[&str]| {
            let stdout = if args.contains(&"diff") {
                diff.clone().into_bytes()
            } else {
                Vec::new()
            };
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn a_held_file_still_staged_after_the_unstage_stops_the_commit() {
        let k = key("gh", "p_") + B36;
        let run = fake_git(diff_with("a.md", &[], &[&format!("t={k}")]));
        let err = unstage_held(&run, &[]).unwrap_err().to_string();
        assert!(err.contains("still staged"), "{err}");
        assert!(!err.contains(&k));
        // Nothing held: no second scan, no error.
        let clean = fake_git(diff_with("a.md", &[], &["ok"]));
        assert!(unstage_held(&clean, &[]).unwrap().is_empty());
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
        let hits = scan_diff(&d, &[], &none);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "claude/CLAUDE.md");
        assert_eq!(hits[0].line, 4);
        assert!(!format!("{hits:?}").contains(&k));
        // Inline marker.
        let d2 = diff_with("a.md", &[], &[&format!("t={k} {ALLOW_MARKER}")]);
        assert!(scan_diff(&d2, &[], &none).is_empty());
        // Allowed by fingerprint, or by path.
        assert!(scan_diff(&d, &[fingerprint(&k)], &none).is_empty());
        assert!(scan_diff(&d, &["claude/CLAUDE.md".into()], &none).is_empty());
        // Already in HEAD: the same key on a removed line.
        let d3 = diff_with("a.md", &[&format!("old {k}")], &[&format!("new {k}")]);
        assert!(scan_diff(&d3, &[], &none).is_empty());
        // A `+++` inside a hunk is content, not a header.
        let d4 = diff_with("a.md", &[], &[&format!("++ {k}")]);
        assert_eq!(scan_diff(&d4, &[], &none)[0].path, "a.md");
    }
}
