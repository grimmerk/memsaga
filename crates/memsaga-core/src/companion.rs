//! Companions: translations or annotated copies of documents that live in
//! other repositories, kept under `<repo>/companion/<repo name>/<path>.<lang>.md`
//! (the `companion-docs` skill defines the convention). This module only
//! lists them; memsaga never writes a companion itself.

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Companion {
    /// Path relative to `<repo>/companion/`, e.g. `webapp/docs/a.zh-tw.md`.
    pub rel: String,
    /// `status` from the frontmatter, or `-` when absent.
    pub status: String,
    /// First `resource:` under `sources` in the frontmatter, or empty.
    pub source: String,
}

/// Every companion file, sorted by path. `filter` keeps only those whose
/// first path component (the source repository's name) equals it.
pub fn list(repo: &Path, filter: Option<&str>) -> Vec<Companion> {
    let dir = repo.join("companion");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for item in walkdir::WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !item.file_type().is_file() {
            continue;
        }
        let rel = match item.path().strip_prefix(&dir) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if let Some(f) = filter {
            if rel.split('/').next() != Some(f) {
                continue;
            }
        }
        let text = std::fs::read_to_string(item.path()).unwrap_or_default();
        let (status, source) = frontmatter_fields(&text);
        out.push(Companion {
            rel,
            status,
            source,
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// `status` and the first `resource` inside a leading `---` block. A file
/// without frontmatter yields `-` and an empty source.
fn frontmatter_fields(text: &str) -> (String, String) {
    let mut status = "-".to_string();
    let mut source = String::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (status, source);
    }
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if let Some(v) = t.strip_prefix("status:") {
            status = v.trim().to_string();
        } else if source.is_empty() {
            if let Some(v) = t
                .strip_prefix("- resource:")
                .or_else(|| t.strip_prefix("resource:"))
            {
                source = v.trim().to_string();
            }
        }
    }
    (status, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_status_and_first_source() {
        let text = "---\ntype: Translation\nsources:\n  - resource: ~/git/webapp/docs/a.md\n    commit: abc\n  - resource: other\nstatus: stable\n---\nbody\n";
        assert_eq!(
            frontmatter_fields(text),
            ("stable".into(), "~/git/webapp/docs/a.md".into())
        );
        assert_eq!(
            frontmatter_fields("no frontmatter\n"),
            ("-".into(), String::new())
        );
        assert_eq!(
            frontmatter_fields("---\nstatus: draft\nsources: []\n---\n"),
            ("draft".into(), String::new())
        );
    }
}
