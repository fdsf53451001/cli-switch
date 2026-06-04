//! Neutral, CLI-agnostic data model. Every adapter converts its native config
//! format to/from these types, so the merge engine only ever sees one shape.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How an MCP server is reached. We collapse every CLI's transport vocabulary
/// (stdio/local, http/sse/remote/ws) into these two buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

/// One MCP server in neutral form. `BTreeMap` everywhere so equality is
/// order-independent — that's what lets us diff a server against its snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub transport: Transport,

    // --- stdio fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    // --- http fields ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,

    // --- common ---
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl McpServer {
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        McpServer {
            transport: Transport::Stdio,
            command: Some(command.into()),
            args,
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
        }
    }

    pub fn http(url: impl Into<String>) -> Self {
        McpServer {
            transport: Transport::Http,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(url.into()),
            headers: BTreeMap::new(),
            enabled: true,
        }
    }
}

/// name -> server. The canonical store and every adapter speak this.
pub type McpMap = BTreeMap<String, McpServer>;

/// The canonical store's on-disk shape (`<store>/mcp.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Canonical {
    #[serde(default)]
    pub servers: McpMap,
}

/// Which CLIs are known. Order here is the order sync visits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cli {
    Claude,
    Codex,
    Opencode,
    Kiro,
    Antigravity,
    Copilot,
}

impl Cli {
    pub const ALL: [Cli; 6] = [
        Cli::Claude,
        Cli::Codex,
        Cli::Opencode,
        Cli::Kiro,
        Cli::Antigravity,
        Cli::Copilot,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
            Cli::Opencode => "opencode",
            Cli::Kiro => "kiro",
            Cli::Antigravity => "antigravity",
            Cli::Copilot => "copilot",
        }
    }

    pub fn from_id(s: &str) -> Option<Cli> {
        Cli::ALL.into_iter().find(|c| c.id() == s)
    }
}
