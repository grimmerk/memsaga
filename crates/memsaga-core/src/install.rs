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
        let present = list.iter().any(|e| {
            e.get("hooks")
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()) == Some(command.as_str())
                    })
                })
                .unwrap_or(false)
        });
        if present {
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
