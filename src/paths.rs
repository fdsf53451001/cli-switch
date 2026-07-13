//! Per-OS path resolution. All the verified, CLI-specific locations live here
//! so the rest of the code never hard-codes a path.

use crate::model::Cli;
use std::path::PathBuf;

/// The user's home directory, cross-platform.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    // Windows
    if let Some(up) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(up);
    }
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut p = PathBuf::from(drive);
        p.push(path);
        return p;
    }
    PathBuf::from(".")
}

/// Canonical store root: `~/.config/cli-switch` (override with CLI_SWITCH_HOME).
pub fn store_root() -> PathBuf {
    if let Some(custom) = std::env::var_os("CLI_SWITCH_HOME") {
        return PathBuf::from(custom);
    }
    home().join(".config").join("cli-switch")
}

pub fn store_mcp() -> PathBuf {
    store_root().join("mcp.json")
}
pub fn store_skills() -> PathBuf {
    store_root().join("skills")
}
pub fn store_instructions() -> PathBuf {
    store_root().join("AGENTS.md")
}
/// Canonical global custom-agent bundles.
pub fn store_agents() -> PathBuf {
    store_root().join("agents")
}
pub fn store_config() -> PathBuf {
    store_root().join("config.toml")
}
pub fn setup_config() -> PathBuf {
    store_root().join("setup.toml")
}
pub fn project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
pub fn project_config_dir() -> PathBuf {
    project_root().join(".cli-switch")
}
pub fn project_config() -> PathBuf {
    project_config_dir().join("config.toml")
}
/// Canonical project custom-agent bundles.
pub fn project_agents() -> PathBuf {
    project_config_dir().join("agents")
}
pub fn store_state_dir() -> PathBuf {
    store_root().join("state")
}
pub fn state_v2() -> PathBuf {
    store_state_dir().join("sync-state-v2.json")
}
pub fn pending_conflicts() -> PathBuf {
    store_state_dir().join("conflicts")
}
pub fn transactions() -> PathBuf {
    store_state_dir().join("transactions")
}
pub fn shell_init() -> PathBuf {
    store_root().join("shell-init.sh")
}

/// Path to the file holding a CLI's MCP servers (the file we read/merge/write).
pub fn mcp_config(cli: Cli) -> PathBuf {
    let h = home();
    match cli {
        Cli::Claude => h.join(".claude.json"),
        Cli::Codex => h.join(".codex").join("config.toml"),
        Cli::Opencode => h.join(".config").join("opencode").join("opencode.json"),
        Cli::Kiro => h.join(".kiro").join("settings").join("mcp.json"),
        Cli::Antigravity => h
            .join(".gemini")
            .join("antigravity-cli")
            .join("mcp_config.json"),
        Cli::Copilot => h.join(".copilot").join("mcp-config.json"),
    }
}

/// The directory a CLI scans for SKILL.md folders (global scope).
pub fn skills_dir(cli: Cli) -> PathBuf {
    let h = home();
    match cli {
        Cli::Claude => h.join(".claude").join("skills"),
        Cli::Codex => h.join(".codex").join("skills"),
        Cli::Opencode => h.join(".config").join("opencode").join("skills"),
        Cli::Kiro => h.join(".kiro").join("skills"),
        Cli::Antigravity => h.join(".gemini").join("antigravity-cli").join("skills"),
        Cli::Copilot => h.join(".copilot").join("skills"),
    }
}

/// The global directory a CLI scans for custom agent definitions.
///
/// Antigravity calls this directory `{appDataDir}/agents`. Unlike its project
/// path, the global path is not documented publicly; we infer appDataDir as
/// `~/.gemini/antigravity-cli` from the current binary/config layout and keep
/// that assumption isolated in this function.
pub fn agents_dir(cli: Cli) -> PathBuf {
    let h = home();
    match cli {
        Cli::Claude => h.join(".claude").join("agents"),
        Cli::Codex => h.join(".codex").join("agents"),
        Cli::Opencode => {
            let base = h.join(".config").join("opencode");
            let current = base.join("agents");
            let legacy = base.join("agent");
            // OpenCode 1.17.x still loads the old singular directory. Keep an
            // existing installation in place; new setups use the documented
            // plural path.
            if current.exists() || !legacy.exists() {
                current
            } else {
                legacy
            }
        }
        Cli::Kiro => h.join(".kiro").join("agents"),
        Cli::Antigravity => h.join(".gemini").join("antigravity-cli").join("agents"),
        Cli::Copilot => h.join(".copilot").join("agents"),
    }
}

/// The project directory a CLI scans for custom agent definitions.
pub fn project_agents_dir(cli: Cli) -> PathBuf {
    let root = project_root();
    match cli {
        Cli::Claude => root.join(".claude").join("agents"),
        Cli::Codex => root.join(".codex").join("agents"),
        Cli::Opencode => {
            let base = root.join(".opencode");
            let current = base.join("agents");
            let legacy = base.join("agent");
            if current.exists() || !legacy.exists() {
                current
            } else {
                legacy
            }
        }
        Cli::Kiro => root.join(".kiro").join("agents"),
        Cli::Antigravity => root.join(".agents").join("agents"),
        Cli::Copilot => root.join(".github").join("agents"),
    }
}

/// Subdirectory names inside a CLI's skills dir that we must never touch
/// (built-in/system skills owned by the CLI itself).
pub fn skills_reserved(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::Codex => &[".system"],
        _ => &[],
    }
}

/// The global instructions file a CLI reads as its system prompt.
pub fn instructions_file(cli: Cli) -> PathBuf {
    let h = home();
    match cli {
        Cli::Claude => h.join(".claude").join("CLAUDE.md"),
        Cli::Codex => h.join(".codex").join("AGENTS.md"),
        Cli::Opencode => h.join(".config").join("opencode").join("AGENTS.md"),
        Cli::Kiro => h.join(".kiro").join("steering").join("AGENTS.md"),
        Cli::Antigravity => h.join(".gemini").join("GEMINI.md"),
        Cli::Copilot => h.join(".copilot").join("copilot-instructions.md"),
    }
}

/// Claude settings.json — where we install the SessionStart hook.
pub fn claude_settings() -> PathBuf {
    home().join(".claude").join("settings.json")
}

/// Codex hooks.json — where we install the SessionStart hook.
pub fn codex_hooks() -> PathBuf {
    home().join(".codex").join("hooks.json")
}

/// opencode global plugin file we drop to trigger sync at startup.
pub fn opencode_plugin() -> PathBuf {
    home()
        .join(".config")
        .join("opencode")
        .join("plugin")
        .join("cli-switch.js")
}
pub fn antigravity_hooks() -> PathBuf {
    home().join(".gemini").join("config").join("hooks.json")
}

/// Kiro CLI settings file — holds `chat.defaultAgent`.
pub fn kiro_settings() -> PathBuf {
    home().join(".kiro").join("settings").join("cli.json")
}

/// A named global Kiro agent config file (`~/.kiro/agents/<name>.json`),
/// where the CLI looks up that agent's `hooks`.
pub fn kiro_agent_config(name: &str) -> PathBuf {
    home()
        .join(".kiro")
        .join("agents")
        .join(format!("{name}.json"))
}

/// Copilot CLI user-level hooks file we drop to trigger sync at startup.
pub fn copilot_hook() -> PathBuf {
    home()
        .join(".copilot")
        .join("hooks")
        .join("cli-switch.json")
}
