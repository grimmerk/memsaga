//! Claude Code names a project directory after the working directory it was
//! launched in, replacing every character that is not a letter or digit with
//! `-`: `/Users/me/git/webapp` -> `-Users-me-git-webapp`. The encoding loses
//! information (`/` and `.` both become `-`), so the reverse map is built from
//! evidence: each root's `history.jsonl` records the real `project` path of
//! every prompt ever typed. Measured on the author's machine: 18 of 19 memory
//! directories resolved, 0 collisions.

use crate::config::Root;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Encode a working directory the way Claude Code does. It replaces every
/// UTF-16 code unit, not every character (measured 2026-09-19: a character
/// outside the BMP, such as an emoji, becomes two dashes).
pub fn encode(path: &str) -> String {
    path.encode_utf16()
        .map(|u| match u8::try_from(u) {
            Ok(b) if b.is_ascii_alphanumeric() => b as char,
            _ => '-',
        })
        .collect()
}

/// slug -> set of real paths seen in `history.jsonl` (usually exactly one).
pub fn map_from_history(root: &Root) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let hist = root.path.join("history.jsonl");
    let text = match std::fs::read_to_string(&hist) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for line in text.lines() {
        // Cheap extraction without parsing the whole line as JSON.
        if let Some(p) = extract_project(line) {
            out.entry(encode(&p)).or_default().insert(p);
        }
    }
    out
}

fn extract_project(line: &str) -> Option<String> {
    let key = "\"project\":\"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    let p = &rest[..end];
    if p.is_empty() {
        None
    } else {
        Some(p.replace("\\/", "/"))
    }
}

/// Best single real path for a slug, if unambiguous.
pub fn resolve(map: &BTreeMap<String, BTreeSet<String>>, slug: &str) -> Option<String> {
    let set = map.get(slug)?;
    if set.len() == 1 {
        set.iter().next().cloned()
    } else {
        None
    }
}

/// Shorten a real path for display: `/Users/me/git/webapp` -> `~/git/webapp`.
pub fn display_path(real: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy().to_string();
        if let Some(rest) = real.strip_prefix(&h) {
            return format!("~{}", rest);
        }
    }
    real.to_string()
}

#[allow(dead_code)]
fn _unused(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_replaces_every_non_alphanumeric() {
        assert_eq!(encode("/Users/me/git/webapp"), "-Users-me-git-webapp");
        assert_eq!(
            encode("/Users/me/git/webapp.wiki"),
            "-Users-me-git-webapp-wiki"
        );
        assert_eq!(encode("/private/tmp/x y"), "-private-tmp-x-y");
        // One dash per UTF-16 code unit: a BMP character is one, an emoji two.
        assert_eq!(encode("/h/café"), "-h-caf-");
        assert_eq!(encode("/h/a🚀b"), "-h-a--b");
    }

    #[test]
    fn extract_project_reads_the_field() {
        let line =
            r#"{"display":"hi","pastedContents":{},"project":"/Users/me/git","timestamp":1}"#;
        assert_eq!(extract_project(line).as_deref(), Some("/Users/me/git"));
        assert_eq!(extract_project(r#"{"display":"x"}"#), None);
    }
}
