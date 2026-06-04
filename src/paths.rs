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
pub fn store_config() -> PathBuf {
    store_root().join("config.toml")
}
pub fn store_state_dir() -> PathBuf {
    store_root().join("state")
}
pub fn store_snapshot(cli: Cli) -> PathBuf {
    store_state_dir().join(format!("{}.snapshot.json", cli.id()))
}
pub fn store_backups() -> PathBuf {
    store_root().join("backups")
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
