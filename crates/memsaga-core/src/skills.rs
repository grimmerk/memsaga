//! Skills shipped with the binary. `memsaga install --skills` writes them into a
//! root's `skills/` directory so Claude Code can find them; the source of
//! truth is `skills/<name>/SKILL.md` in this repository, compiled in at build
//! time so a single binary carries everything it needs.

use anyhow::{Context, Result};
use std::path::Path;

pub struct BundledSkill {
    pub name: &'static str,
    pub content: &'static str,
}

pub const BUNDLED: &[BundledSkill] = &[
    BundledSkill {
        name: "memsaga-history",
        content: include_str!("../skills/memsaga-history/SKILL.md"),
    },
    BundledSkill {
        name: "companion-docs",
        content: include_str!("../skills/companion-docs/SKILL.md"),
    },
    BundledSkill {
        name: "companions",
        content: include_str!("../skills/companions/SKILL.md"),
    },
];

pub fn find(name: &str) -> Option<&'static BundledSkill> {
    BUNDLED.iter().find(|s| s.name == name)
}

/// Write one bundled skill under `<root>/skills/<name>/SKILL.md`. Returns
/// true when the file was created or its content changed.
pub fn install_into(root: &Path, skill: &BundledSkill) -> Result<bool> {
    let dir = root.join("skills").join(skill.name);
    let path = dir.join("SKILL.md");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == skill.content {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Temp file + rename: Claude Code may be reading skills/ while we write.
    let tmp = dir.join(format!(".{}.memsaga-tmp", skill.name));
    std::fs::write(&tmp, skill.content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strict YAML rejects an unquoted value containing `: `; Claude Code's
    /// parser is lenient, GitHub's is not. Every bundled skill's frontmatter
    /// must be valid for both.
    #[test]
    fn bundled_frontmatter_is_strict_yaml() {
        for skill in BUNDLED {
            let mut lines = skill.content.lines();
            assert_eq!(lines.next(), Some("---"), "{}: no frontmatter", skill.name);
            let mut closed = false;
            for line in lines {
                if line == "---" {
                    closed = true;
                    break;
                }
                let (key, value) = line
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("{}: not a key: value line: {line}", skill.name));
                assert!(!key.contains(' '), "{}: bad key {key:?}", skill.name);
                let quoted = value.starts_with('"') && value.ends_with('"');
                assert!(
                    quoted || !value.contains(": "),
                    "{}: `{key}` contains `: ` and is not quoted",
                    skill.name
                );
            }
            assert!(closed, "{}: frontmatter never closed", skill.name);
        }
    }
}
