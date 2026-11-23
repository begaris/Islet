//! Installs and removes the Islet hooks in Claude Code's `settings.json` and
//! the `notify` line in Codex's `config.toml`.
//!
//! The existing file is backed up first and only Islet's own entries are
//! touched: everything else in the file is written back as it was. A file that
//! is not valid JSON is left alone.

use crate::json::{self, Value};
use std::path::{Path, PathBuf};

/// Hook events to register and whether Claude Code should wait for the hook.
/// Only the permission request needs an answer; the rest just report status.
const EVENTS: [(&str, bool); 8] = [
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", false),
    ("PermissionRequest", true),
    ("Notification", false),
    ("Stop", false),
    ("SubagentStop", false),
    ("SessionEnd", false),
];

/// Seconds Claude Code waits for an Allow / Deny click before showing its own prompt.
pub const PERMISSION_TIMEOUT: u32 = 130;
const MARKER: &str = "islet-hook";

pub fn claude_settings() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude").join("settings.json"))
}

pub fn codex_config() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".codex").join("config.toml"))
}

/// `islet-hook.exe` next to the running executable.
pub fn hook_exe() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.with_file_name("islet-hook.exe");
    path.is_file().then_some(path)
}

fn is_ours(group: &Value) -> bool {
    let Some(Value::Arr(handlers)) = group.get("hooks") else { return false };
    handlers.iter().any(|h| h.text("command").contains(MARKER))
}

/// Removes Islet's matcher groups from every event; returns how many were removed.
fn strip(root: &mut Value) -> usize {
    let mut removed = 0;
    let Some(Value::Obj(events)) = root.entry("hooks", Value::Obj(Vec::new())) else { return 0 };
    for (_, groups) in events.iter_mut() {
        if let Value::Arr(groups) = groups {
            let before = groups.len();
            groups.retain(|g| !is_ours(g));
            removed += before - groups.len();
        }
    }
    events.retain(|(_, groups)| !matches!(groups, Value::Arr(g) if g.is_empty()));
    let empty = events.is_empty();
    if empty {
        if let Value::Obj(items) = root {
            items.retain(|(k, _)| k != "hooks");
        }
    }
    removed
}

fn add(root: &mut Value, exe: &Path) -> Option<()> {
    let command = exe.to_string_lossy().into_owned();
    let Value::Obj(events) = root.entry("hooks", Value::Obj(Vec::new()))? else { return None };
    for (event, wait) in EVENTS {
        // Exec form (`command` + `args`): no shell, so the Windows path needs no quoting.
        let mut handler = vec![("type".to_string(), Value::Str("command".into())), ("command".to_string(), Value::Str(command.clone())), ("args".to_string(), Value::Arr(Vec::new()))];
        if wait {
            handler.push(("timeout".to_string(), Value::Num(PERMISSION_TIMEOUT.to_string())));
        } else {
            handler.push(("timeout".to_string(), Value::Num("5".into())));
            handler.push(("async".to_string(), Value::Bool(true)));
        }
        let group = Value::Obj(vec![("hooks".to_string(), Value::Arr(vec![Value::Obj(handler)]))]);
        if !events.iter().any(|(k, _)| k == event) {
            events.push((event.to_string(), Value::Arr(Vec::new())));
        }
        match events.iter_mut().find(|(k, _)| k == event).map(|(_, v)| v) {
            Some(Value::Arr(groups)) => groups.push(group),
            _ => return None, // an event that is not an array: not a file we understand
        }
    }
    Some(())
}

fn backup(path: &Path) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = format!("{}.islet-backup-{stamp}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let target = path.with_file_name(name);
    std::fs::copy(path, &target).map_err(|e| format!("could not back up {}: {e}", path.display()))?;
    Ok(target)
}

/// Writes through a temporary file so a crash never leaves a half-written file.
fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("islet-tmp");
    std::fs::write(&temp, text).map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

fn load(settings: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(settings) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => match json::parse(text.trim_start_matches('\u{feff}')) {
            Some(root @ Value::Obj(_)) => Ok(Some(root)),
            _ => Err(format!("{} is not valid JSON; nothing was changed", settings.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", settings.display())),
    }
}

/// Adds (or refreshes) Islet's hooks in Claude Code's settings file.
pub fn install_claude(settings: &Path, exe: &Path) -> Result<String, String> {
    let existing = load(settings)?;
    let mut note = String::from("created");
    if existing.is_some() {
        note = format!("backup: {}", backup(settings)?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    let mut root = existing.unwrap_or(Value::Obj(Vec::new()));
    strip(&mut root);
    add(&mut root, exe).ok_or_else(|| format!("{} has an unexpected \"hooks\" layout; nothing was changed", settings.display()))?;
    write(settings, &root.pretty())?;
    Ok(note)
}

/// Removes Islet's hooks again. Returns how many entries were removed.
pub fn uninstall_claude(settings: &Path) -> Result<usize, String> {
    let Some(mut root) = load(settings)? else { return Ok(0) };
    let had_hooks = root.get("hooks").is_some();
    let removed = strip(&mut root);
    if removed == 0 {
        // `strip` may have added and removed an empty "hooks" object; don't rewrite for that.
        let _ = had_hooks;
        return Ok(0);
    }
    backup(settings)?;
    write(settings, &root.pretty())?;
    Ok(removed)
}

/// Adds `notify = ["...islet-hook.exe", "--codex"]` to Codex's config, if Codex
/// is set up on this PC and has no `notify` of its own.
pub fn install_codex(config: &Path, exe: &Path) -> Result<String, String> {
    let Ok(text) = std::fs::read_to_string(config) else { return Ok("Codex not found".into()) };
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains(MARKER)).collect();
    if kept.iter().any(|l| l.trim_start().starts_with("notify") && l.contains('=')) {
        return Err("Codex already has a notify program; add islet-hook.exe --codex to it by hand".into());
    }
    backup(config)?;
    // TOML basic strings need backslashes doubled. A top-level key must come before any table.
    let line = format!("notify = [\"{}\", \"--codex\"]", exe.to_string_lossy().replace('\\', "\\\\"));
    let mut out = vec![line.as_str()];
    out.extend(kept);
    write(config, &(out.join("\n") + "\n"))?;
    Ok("Codex notify added".into())
}

pub fn uninstall_codex(config: &Path) -> Result<bool, String> {
    let Ok(text) = std::fs::read_to_string(config) else { return Ok(false) };
    if !text.lines().any(|l| l.contains(MARKER)) {
        return Ok(false);
    }
    backup(config)?;
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains(MARKER)).collect();
    write(config, &(kept.join("\n") + "\n"))?;
    Ok(true)
}

/// Installs or removes everything (Claude Code, and Codex if present) and
/// returns a one-line summary. `settings` / `codex` override the default
/// file locations (tests).
pub fn apply(install: bool, settings: Option<PathBuf>, codex: Option<PathBuf>) -> Result<String, String> {
    let settings = settings.or_else(claude_settings).ok_or("no user profile folder")?;
    let codex = codex.or_else(codex_config);
    if install {
        let exe = hook_exe().ok_or("islet-hook.exe is missing next to islet.exe")?;
        let note = install_claude(&settings, &exe)?;
        Ok(match codex.map(|c| install_codex(&c, &exe)) {
            Some(Ok(c)) if c != "Codex not found" => format!("{note} \u{00B7} {c}"),
            Some(Err(e)) => format!("{note} \u{00B7} {e}"),
            _ => note,
        })
    } else {
        let removed = uninstall_claude(&settings)?;
        let codex_removed = codex.map(|c| uninstall_codex(&c)).transpose()?.unwrap_or(false);
        Ok(format!("{removed} hook entries removed{}", if codex_removed { " \u{00B7} Codex notify removed" } else { "" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("islet-hooks-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const EXISTING: &str = "{\n  \"model\": \"opus\",\n  \"permissions\": {\n    \"allow\": [\n      \"Bash(ls:*)\"\n    ]\n  },\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"my-own-script.cmd\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";

    #[test]
    fn install_keeps_everything_else_and_uninstall_restores_it() {
        let dir = temp("merge");
        let settings = dir.join("settings.json");
        std::fs::write(&settings, EXISTING).unwrap();
        let exe = Path::new(r"C:\Program Files\Islet\islet-hook.exe");

        install_claude(&settings, exe).unwrap();
        install_claude(&settings, exe).unwrap(); // idempotent
        let root = json::parse(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(root.text("model"), "opus");
        let hooks = root.get("hooks").unwrap();
        // The user's own Stop hook is still there, next to exactly one of ours.
        let Some(Value::Arr(stop)) = hooks.get("Stop") else { panic!() };
        assert_eq!(stop.len(), 2);
        assert_eq!(stop.iter().filter(|g| is_ours(g)).count(), 1);
        let Some(Value::Arr(permission)) = hooks.get("PermissionRequest") else { panic!() };
        let Some(Value::Arr(handlers)) = permission[0].get("hooks") else { panic!() };
        assert_eq!(handlers[0].text("command"), r"C:\Program Files\Islet\islet-hook.exe");
        assert_eq!(handlers[0].get("timeout"), Some(&Value::Num("130".into())));
        assert_eq!(handlers[0].get("async"), None);
        let Some(Value::Arr(pre)) = hooks.get("PreToolUse") else { panic!() };
        assert_eq!(pre[0].get("hooks").and_then(|h| if let Value::Arr(a) = h { a[0].get("async") } else { None }), Some(&Value::Bool(true)));
        // Backups were taken.
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("islet-backup")));

        assert_eq!(uninstall_claude(&settings).unwrap(), EVENTS.len());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), EXISTING);
        assert_eq!(uninstall_claude(&settings).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_broken_files() {
        let dir = temp("edge");
        let settings = dir.join("settings.json");
        let exe = Path::new(r"C:\x\islet-hook.exe");
        assert_eq!(install_claude(&settings, exe).unwrap(), "created");
        assert_eq!(uninstall_claude(&settings).unwrap(), EVENTS.len());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}\n");

        std::fs::write(&settings, "{ not json").unwrap();
        assert!(install_claude(&settings, exe).is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_notify_line() {
        let dir = temp("codex");
        let config = dir.join("config.toml");
        let exe = Path::new(r"C:\x\islet-hook.exe");
        assert_eq!(install_codex(&config, exe).unwrap(), "Codex not found");
        std::fs::write(&config, "model = \"gpt\"\n\n[tools]\nweb = true\n").unwrap();
        install_codex(&config, exe).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.starts_with("notify = [\"C:\\\\x\\\\islet-hook.exe\", \"--codex\"]\nmodel = \"gpt\"\n"));
        assert!(uninstall_codex(&config).unwrap());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "model = \"gpt\"\n\n[tools]\nweb = true\n");

        std::fs::write(&config, "notify = [\"other\"]\n").unwrap();
        assert!(install_codex(&config, exe).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
