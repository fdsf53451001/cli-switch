//! Install auto-sync triggers so each CLI syncs at startup.
//!
//! - Claude Code: native SessionStart hook in settings.json (reliable).
//! - Codex: hooks.json SessionStart entry (mechanism verified; schema may need
//!   `codex /hooks` approval — marked experimental).
//! - opencode: a global plugin that runs sync on session start (experimental).
//! - Antigravity CLI (`agy`): native lifecycle hook in ~/.gemini/config/hooks.json.
//! - Kiro: no startup hook exists — we generate a shell-init wrapper.

use crate::model::Cli;
use crate::paths;
use crate::util::{self, R};
use serde_json::{json, Map, Value};
use std::fs;

pub struct MountReport {
    pub lines: Vec<String>,
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "cli-switch".to_string())
}

pub fn mount(clis: &[Cli]) -> R<MountReport> {
    let exe = exe_path();
    let mut lines = Vec::new();

    for &cli in clis {
        match cli {
            Cli::Claude => lines.push(install_claude_hook(&exe)?),
            Cli::Codex => lines.push(install_codex_hook(&exe)?),
            Cli::Opencode => lines.push(install_opencode_plugin(&exe)?),
            Cli::Antigravity => lines.push(install_antigravity_hook(&exe)?),
            Cli::Copilot => lines.push(install_copilot_hook(&exe)?),
            Cli::Kiro => {} // covered by shell-init below
        }
    }

    if clis.contains(&Cli::Kiro) {
        lines.push(write_shell_init(&exe)?);
    }

    Ok(MountReport { lines })
}

pub fn unmount(clis: &[Cli]) -> R<MountReport> {
    let mut lines = Vec::new();

    for &cli in clis {
        match cli {
            Cli::Claude => lines.push(remove_hook_group(
                &paths::claude_settings(),
                "SessionStart",
                "claude",
            )?),
            Cli::Codex => lines.push(remove_hook_group(
                &paths::codex_hooks(),
                "SessionStart",
                "codex",
            )?),
            Cli::Opencode => lines.push(remove_generated_file(
                &paths::opencode_plugin(),
                "opencode plugin",
            )?),
            Cli::Antigravity => lines.push(remove_antigravity_hook()?),
            Cli::Copilot => lines.push(remove_generated_file(
                &paths::copilot_hook(),
                "copilot hook",
            )?),
            Cli::Kiro => lines.push(remove_generated_file(
                &paths::shell_init(),
                "kiro shell init",
            )?),
        }
    }

    Ok(MountReport { lines })
}

fn remove_hook_group(path: &std::path::Path, event: &str, label: &str) -> R<String> {
    let Some(text) = util::read_to_string_opt(path)? else {
        return Ok(format!("{label}: no hook file"));
    };
    if text.trim().is_empty() {
        return Ok(format!("{label}: hook file is empty"));
    }

    let mut root: Value = serde_json::from_str(&text).map_err(|e| util::ctx(path, e))?;
    let Some(groups) = root
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut(event))
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(format!("{label}: no cli-switch hook"));
    };

    let before = groups.len();
    groups.retain(|g| !group_mentions(g, "cli-switch") && !group_mentions(g, "agent-sync"));
    if groups.len() == before {
        return Ok(format!("{label}: no cli-switch hook"));
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(path, &out)?;
    Ok(format!("{label}: hook removed -> {}", path.display()))
}

fn remove_generated_file(path: &std::path::Path, label: &str) -> R<String> {
    let Some(text) = util::read_to_string_opt(path)? else {
        return Ok(format!("{label}: no file"));
    };
    if !text.contains("cli-switch") && !text.contains("__cli_switch_run") {
        return Ok(format!(
            "{label}: custom file left unchanged -> {}",
            path.display()
        ));
    }
    fs::remove_file(path).map_err(|e| util::ctx(path, e))?;
    Ok(format!("{label}: removed -> {}", path.display()))
}

fn remove_antigravity_hook() -> R<String> {
    let path = paths::antigravity_hooks();
    let Some(text) = util::read_to_string_opt(&path)? else {
        return Ok("antigravity: no hook file".to_string());
    };
    if text.trim().is_empty() {
        return Ok("antigravity: hook file is empty".to_string());
    }

    let mut root: Value = serde_json::from_str(&text).map_err(|e| util::ctx(&path, e))?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(format!(
            "antigravity: custom hook file left unchanged -> {}",
            path.display()
        ));
    };
    let before = obj.len();
    obj.retain(|name, val| {
        !name.contains("cli-switch")
            && !name.contains("agent-sync")
            && !value_mentions(val, "cli-switch")
            && !value_mentions(val, "agent-sync")
    });
    if obj.len() == before {
        return Ok("antigravity: no cli-switch hook".to_string());
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(&path, &out)?;
    Ok(format!("antigravity: hook removed -> {}", path.display()))
}

// ───────────────────────── Claude ─────────────────────────

fn install_claude_hook(exe: &str) -> R<String> {
    let path = paths::claude_settings();
    let mut root: Value = match util::read_to_string_opt(&path)? {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| util::ctx(&path, e))?
        }
        _ => Value::Object(Map::new()),
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "settings.json is not an object".to_string())?;

    let hooks_item = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks_item.is_object() {
        *hooks_item = Value::Object(Map::new());
    }
    let hooks = hooks_item.as_object_mut().unwrap();

    // Drop any prior cli-switch SessionStart group, then add a fresh one.
    let mut groups: Vec<Value> = hooks
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    groups.retain(|g| !group_mentions(g, "cli-switch") && !group_mentions(g, "agent-sync"));
    groups.push(json!({
        "matcher": "startup|resume",
        "hooks": [{
            "type": "command",
            "command": exe,
            "args": ["sync", "--quiet"],
            "timeout": 30
        }]
    }));
    hooks.insert("SessionStart".into(), Value::Array(groups));

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(&path, &out)?;
    Ok(format!(
        "claude: SessionStart hook installed -> {}",
        path.display()
    ))
}

fn group_mentions(group: &Value, needle: &str) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(needle))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

// ───────────────────────── Codex (experimental) ─────────────────────────

fn install_codex_hook(exe: &str) -> R<String> {
    let path = paths::codex_hooks();
    let mut root: Value = match util::read_to_string_opt(&path)? {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| util::ctx(&path, e))?
        }
        _ => json!({ "hooks": {} }),
    };
    let obj = root.as_object_mut().ok_or("hooks.json is not an object")?;
    let hooks_item = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks_item.is_object() {
        *hooks_item = Value::Object(Map::new());
    }
    let hooks = hooks_item.as_object_mut().unwrap();
    let mut groups: Vec<Value> = hooks
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    groups.retain(|g| !group_mentions(g, "cli-switch") && !group_mentions(g, "agent-sync"));
    groups.push(json!({
        "matcher": "startup|resume",
        "hooks": [{
            "type": "command",
            "command": format!("{exe} sync --quiet"),
            "statusMessage": "Syncing CLI config"
        }]
    }));
    hooks.insert("SessionStart".into(), Value::Array(groups));

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(&path, &out)?;
    Ok(format!(
        "codex: SessionStart hook written -> {} (run `codex /hooks` to approve; experimental)",
        path.display()
    ))
}

// ───────────────────────── opencode (experimental) ─────────────────────────

fn install_opencode_plugin(exe: &str) -> R<String> {
    let path = paths::opencode_plugin();
    let js = format!(
        r#"// cli-switch: auto-sync MCP/skills/instructions on opencode startup.
// Generated by `cli-switch mount`. Experimental: opencode plugin event names
// may change between versions; adjust the matched event if sync doesn't fire.
export default async ({{ $ }}) => {{
  let done = false;
  const run = async () => {{
    if (done) return; done = true;
    try {{ await $`{exe} sync --quiet`; }} catch (e) {{ /* ignore */ }}
  }};
  return {{
    event: async ({{ event }}) => {{
      const t = event && event.type ? event.type : "";
      if (t.startsWith("session") || t.startsWith("server")) await run();
    }},
  }};
}}
"#
    );
    util::write_atomic(&path, &js)?;
    Ok(format!(
        "opencode: startup plugin written -> {} (experimental)",
        path.display()
    ))
}

// ───────────────────────── Antigravity CLI ─────────────────────────

fn install_antigravity_hook(exe: &str) -> R<String> {
    let path = paths::antigravity_hooks();
    let mut root: Value = match util::read_to_string_opt(&path)? {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| util::ctx(&path, e))?
        }
        _ => Value::Object(Map::new()),
    };
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().unwrap();
    obj.retain(|name, val| {
        !name.contains("cli-switch")
            && !name.contains("agent-sync")
            && !value_mentions(val, "cli-switch")
            && !value_mentions(val, "agent-sync")
    });

    let command = format!(
        "sh -c '{} sync --quiet >/dev/null 2>&1 || true; printf \"{{}}\"'",
        shell_double_quote(exe)
    );
    obj.insert(
        "cli-switch-sync".into(),
        json!({
            "PreInvocation": [{
                "type": "command",
                "command": command,
                "timeout": 30
            }]
        }),
    );

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(&path, &out)?;
    Ok(format!(
        "antigravity: PreInvocation hook written -> {}",
        path.display()
    ))
}

// ───────────────────────── Copilot ─────────────────────────
// Dedicated user-level hooks file at ~/.copilot/hooks/cli-switch.json.

fn install_copilot_hook(exe: &str) -> R<String> {
    let path = paths::copilot_hook();
    let bash = format!("{} sync --quiet", shell_double_quote(exe));
    let pwsh = format!("& {} sync --quiet", shell_double_quote(exe));
    let root = json!({
        "version": 1,
        "hooks": {
            "sessionStart": [{
                "type": "command",
                "bash": bash,
                "powershell": pwsh,
                "timeoutSec": 30
            }]
        }
    });
    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    util::write_atomic(&path, &out)?;
    Ok(format!(
        "copilot: sessionStart hook written -> {}",
        path.display()
    ))
}

fn value_mentions(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(s) => s.contains(needle),
        Value::Array(arr) => arr.iter().any(|v| value_mentions(v, needle)),
        Value::Object(obj) => obj
            .iter()
            .any(|(k, v)| k.contains(needle) || value_mentions(v, needle)),
        _ => false,
    }
}

fn shell_double_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

// ───────────────────────── shell-init for CLIs without hooks ─────────────────────────

fn write_shell_init(exe: &str) -> R<String> {
    let path = paths::shell_init();
    let script = format!(
        r#"# cli-switch shell init — source this from ~/.zshrc or ~/.bashrc:
#   source "{path}"
# Wraps Kiro terminal launches so config syncs first.
__cli_switch_run() {{ command "{exe}" sync --quiet >/dev/null 2>&1 || true; }}
kiro()        {{ __cli_switch_run; command kiro "$@"; }}
"#,
        path = path.display(),
        exe = exe
    );
    util::write_atomic(&path, &script)?;
    Ok(format!(
        "kiro: no native hook — wrapper written to {}\n         add to your shell rc:  source \"{}\"",
        path.display(),
        path.display()
    ))
}
