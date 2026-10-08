//! Registering memsaga's hooks in a root's `settings.json`, read-merge-write:
//! every other key and every foreign hook entry is preserved; only entries
//! whose command mentions `memsaga` are ours. A backup `settings.json.memsaga-bak`
//! is written before the first change. `uninstall` removes exactly our entries.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

/// (event, matcher, command suffix, async). The synchronous ones are two-way
/// sync's step 3 (docs/two-way-sync.md §7): they must finish before the
/// session reads the file or the prompt, so they cannot be async.
fn wanted(bin: &str, reporter: bool) -> Vec<(&'static str, Option<&'static str>, String, bool)> {
    let mut v = vec![
        (
            "PostToolUse",
            Some("Write|Edit|MultiEdit|NotebookEdit"),
            format!("{bin} hook"),
            true,
        ),
        ("Stop", None, format!("{bin} hook"), true),
        ("SessionEnd", None, format!("{bin} hook"), true),
        ("SessionStart", None, format!("{bin} hook"), false),
        ("UserPromptSubmit", None, format!("{bin} hook"), false),
        (
            "PreToolUse",
            Some("Read|Write|Edit|MultiEdit"),
            format!("{bin} hook"),
            false,
        ),
    ];
    if reporter {
        v.push(("Stop", None, format!("{bin} report"), false));
    }
    v
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("memsaga"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// The command path to register for `bin`: `$HOME/...` when it lies under the
/// home directory, so a `settings.json` restored or synced to a machine with
/// another home still finds the binary (Claude Code runs hook commands through
/// a shell, which expands `$HOME`; checked on 2.1.291). Anything else as given.
/// Quoted when the home or the path holds a character the shell would split
/// or interpret (a space, a quote, `$`).
pub fn portable_bin(bin: &str) -> String {
    portable_bin_in(bin, dirs::home_dir().as_deref())
}

fn portable_bin_in(bin: &str, home: Option<&Path>) -> String {
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@%,:".contains(c))
    };
    if let Some(home) = home {
        let home = home.to_string_lossy();
        if let Some(rest) = bin.strip_prefix(&format!("{home}/")) {
            if plain(&home) && plain(rest) {
                return format!("$HOME/{rest}");
            }
            // Double quotes keep `$HOME` expanding and its value one word.
            let mut quoted = String::new();
            for c in rest.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    quoted.push('\\');
                }
                quoted.push(c);
            }
            return format!("\"$HOME/{quoted}\"");
        }
    }
    if plain(bin) {
        bin.to_string()
    } else {
        format!("'{}'", bin.replace('\'', r"'\''"))
    }
}

/// `memsaga hook` or `memsaga report`: which of memsaga's commands an entry runs.
fn command_kind(command: &str) -> Option<&'static str> {
    if !command.contains("memsaga") {
        return None;
    }
    if command.ends_with(" hook") {
        Some("hook")
    } else if command.ends_with(" report") {
        Some("report")
    } else {
        None
    }
}

/// Returns true when the file changed.
pub fn install(settings: &Path, bin: &str, reporter: bool) -> Result<bool> {
    let mut root = read_settings(settings)?;
    let hooks = root
        .as_object_mut()
        .context("settings.json is not an object")?
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().context("hooks is not an object")?;
    let mut changed = false;
    for (event, matcher, command, is_async) in wanted(bin, reporter) {
        let list = hooks.entry(event).or_insert_with(|| Value::Array(vec![]));
        let list = list.as_array_mut().context("hook list is not an array")?;
        // The same memsaga command, possibly under another path (an absolute
        // one from an older install, or a moved binary): the first is updated
        // in place and any further copy removed, so the hook runs once.
        let kind = command_kind(&command);
        let same = |old: &str| old == command || (kind.is_some() && command_kind(old) == kind);
        let mut found = false;
        let mut emptied = Vec::new();
        for (i, entry) in list.iter_mut().enumerate() {
            let Some(hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            let before = hooks.len();
            hooks.retain_mut(|h| {
                let old = h
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default();
                if !same(old) {
                    return true;
                }
                if found {
                    return false;
                }
                found = true;
                if old != command {
                    h["command"] = json!(command);
                    changed = true;
                }
                true
            });
            if hooks.len() < before {
                changed = true;
                if hooks.is_empty() {
                    emptied.push(i);
                }
            }
        }
        for i in emptied.into_iter().rev() {
            list.remove(i);
        }
        if found {
            continue;
        }
        let mut hook = json!({ "type": "command", "command": command, "timeout": 60 });
        if is_async {
            hook["async"] = json!(true);
        } else if command.ends_with(" report") {
            hook["timeout"] = json!(5);
        } else {
            // A pull may fetch over the network; on a timeout Claude Code
            // carries on without the hook, so the session never hangs.
            hook["timeout"] = json!(30);
        }
        let mut entry = json!({ "hooks": [hook] });
        if let Some(m) = matcher {
            entry["matcher"] = json!(m);
        }
        list.push(entry);
        changed = true;
    }
    if changed {
        write_settings(settings, &root)?;
    }
    Ok(changed)
}

pub fn uninstall(settings: &Path) -> Result<bool> {
    let mut root = read_settings(settings)?;
    let mut changed = false;
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for ev in events {
            if let Some(list) = hooks.get_mut(&ev).and_then(|l| l.as_array_mut()) {
                let before = list.len();
                list.retain(|e| !is_ours(e));
                if list.len() != before {
                    changed = true;
                }
                if list.is_empty() {
                    hooks.remove(&ev);
                }
            }
        }
    }
    if changed {
        write_settings(settings, &root)?;
    }
    Ok(changed)
}

fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_settings(path: &Path, value: &Value) -> Result<()> {
    if path.exists() {
        let bak = path.with_file_name(format!(
            "{}.memsaga-bak",
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
        if !bak.exists() {
            std::fs::copy(path, &bak)?;
        }
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let text = serde_json::to_string_pretty(value)? + "\n";
    // Write to a temp file and rename so a concurrent reader never sees a torn file.
    let tmp = path.with_extension("json.memsaga-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::portable_bin;

    #[test]
    fn portable_bin_uses_home_only_under_home() {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(
            portable_bin(&format!("{home}/.cargo/bin/memsaga")),
            "$HOME/.cargo/bin/memsaga"
        );
        assert_eq!(
            portable_bin("/usr/local/bin/memsaga"),
            "/usr/local/bin/memsaga"
        );
        assert_eq!(
            portable_bin(&format!("{home}-other/memsaga")),
            format!("{home}-other/memsaga")
        );
    }

    #[test]
    fn portable_bin_stays_one_word_in_the_shell() {
        use super::portable_bin_in;
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            portable_bin_in("/opt/my tools/memsaga", None),
            "'/opt/my tools/memsaga'"
        );
        // A home with a space: `$HOME` must expand inside quotes.
        let home = std::env::temp_dir().join(format!("memsaga home {}", std::process::id()));
        let exe = home.join("bin").join("memsaga");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "#!/bin/sh\necho \"ran $1\"\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let word = portable_bin_in(&exe.to_string_lossy(), Some(&home));
        assert_eq!(word, "\"$HOME/bin/memsaga\"");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{word} hook"))
            .env("HOME", &home)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ran hook\n");
    }
}
