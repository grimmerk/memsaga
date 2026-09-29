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
    /// Entries of `relates_to` (a note about the repository), in order.
    pub relates_to: Vec<String>,
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
        let (status, source, relates_to) = frontmatter_fields(&text);
        out.push(Companion {
            rel,
            status,
            source,
            relates_to,
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// `status`, the first `resource`, and the `relates_to` list inside a
/// leading `---` block. A file without frontmatter yields `-` and nothing.
fn frontmatter_fields(text: &str) -> (String, String, Vec<String>) {
    let mut status = "-".to_string();
    let mut source = String::new();
    let mut relates_to = Vec::new();
    let mut in_relates = false;
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (status, source, relates_to);
    }
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        // `relates_to:` followed by `  - item` lines; any other key ends it.
        if in_relates {
            if let Some(v) = t.strip_prefix("- ") {
                relates_to.push(v.trim().to_string());
                continue;
            }
            in_relates = false;
        }
        if t == "relates_to:" {
            in_relates = true;
            continue;
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
    (status, source, relates_to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_status_and_first_source() {
        let text = "---\ntype: Translation\nsources:\n  - resource: ~/git/webapp/docs/a.md\n    commit: abc\n  - resource: other\nstatus: stable\n---\nbody\n";
        assert_eq!(
            frontmatter_fields(text),
            ("stable".into(), "~/git/webapp/docs/a.md".into(), vec![])
        );
        assert_eq!(
            frontmatter_fields("no frontmatter\n"),
            ("-".into(), String::new(), vec![])
        );
        assert_eq!(
            frontmatter_fields("---\nstatus: draft\nsources: []\n---\n"),
            ("draft".into(), String::new(), vec![])
        );
        let note = "---\ntype: Note\nrelates_to:\n  - docs/a.md\n  - acme/x#3\nstatus: stable\n---\nbody\n";
        assert_eq!(
            frontmatter_fields(note),
            (
                "stable".into(),
                String::new(),
                vec!["docs/a.md".to_string(), "acme/x#3".to_string()]
            )
        );
    }
}
