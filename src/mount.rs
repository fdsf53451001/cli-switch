//! Install auto-sync triggers so each CLI syncs at startup.
//!
//! - Claude Code: native SessionStart hook in settings.json (reliable).
//! - Codex: hooks.json SessionStart entry (mechanism verified; schema may need
//!   `codex /hooks` approval — marked experimental).
//! - opencode: a global plugin that runs sync on session start (experimental).
//! - Kiro / Antigravity: no startup hook exists — we generate a shell-init file
//!   with wrapper functions so terminal launches sync first.

use crate::model::Cli;
use crate::paths;
use crate::util::{self, R};
use serde_json::{json, Map, Value};

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
            Cli::Kiro | Cli::Antigravity => {} // covered by shell-init below
        }
    }

    if clis.contains(&Cli::Kiro) || clis.contains(&Cli::Antigravity) {
        lines.push(write_shell_init(&exe)?);
    }

    Ok(MountReport { lines })
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

// ───────────────────────── shell-init for GUI CLIs ─────────────────────────

fn write_shell_init(exe: &str) -> R<String> {
    let path = paths::store_root().join("shell-init.sh");
    let script = format!(
        r#"# cli-switch shell init — source this from ~/.zshrc or ~/.bashrc:
#   source "{path}"
# Wraps Kiro/Antigravity terminal launches so config syncs first.
# (GUI/Dock launches bypass the shell; use a periodic sync for those.)
__cli_switch_run() {{ command "{exe}" sync --quiet >/dev/null 2>&1 || true; }}
kiro()        {{ __cli_switch_run; command kiro "$@"; }}
agy()         {{ __cli_switch_run; command agy "$@"; }}
antigravity() {{ __cli_switch_run; command antigravity "$@"; }}
"#,
        path = path.display(),
        exe = exe
    );
    util::write_atomic(&path, &script)?;
    Ok(format!(
        "kiro/antigravity: no native hook — wrappers written to {}\n         add to your shell rc:  source \"{}\"",
        path.display(),
        path.display()
    ))
}
