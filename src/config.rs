//! `<store>/config.toml` — which CLIs and which features to sync. Missing file
//! means "all installed CLIs, all features".

use crate::adapters;
use crate::model::Cli;
use crate::paths;
use crate::util::{self, R};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

impl Scope {
    pub fn id(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }

    pub fn from_id(s: &str) -> Option<Scope> {
        match s {
            "global" => Some(Scope::Global),
            "project" | "current" | "cwd" => Some(Scope::Project),
            _ => None,
        }
    }
}

pub struct Config {
    pub scope: Scope,
    pub clis: Vec<Cli>,
    pub mcp: bool,
    pub skills: bool,
    pub instructions: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            scope: Scope::Global,
            clis: Cli::ALL.to_vec(),
            mcp: true,
            skills: true,
            instructions: true,
        }
    }
}

pub fn load() -> R<Config> {
    let Some(text) = util::read_to_string_opt(&paths::store_config())? else {
        return Ok(Config::default());
    };
    let doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| util::ctx(&paths::store_config(), e))?;

    let mut cfg = Config::default();
    if let Some(scope) = doc
        .get("scope")
        .and_then(|i| i.as_str())
        .and_then(Scope::from_id)
    {
        cfg.scope = scope;
    }
    if let Some(arr) = doc.get("clis").and_then(|i| i.as_array()) {
        cfg.clis = arr
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(Cli::from_id)
            .collect();
    }
    if let Some(f) = doc.get("features").and_then(|i| i.as_table_like()) {
        let b = |k: &str, d: bool| f.get(k).and_then(|i| i.as_bool()).unwrap_or(d);
        cfg.mcp = b("mcp", true);
        cfg.skills = b("skills", true);
        cfg.instructions = b("instructions", true);
    }
    Ok(cfg)
}

pub fn save(cfg: &Config) -> R<()> {
    let clis = cfg
        .clis
        .iter()
        .map(|c| format!("\"{}\"", c.id()))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        r#"# agent-sync configuration.
# scope = "global" syncs ~/.config/agent-sync to global CLI config.
# scope = "project" syncs the current directory's AGENTS.md/.agents to project-local CLI files.
scope = "{}"

# Which CLIs to sync (remove any you don't want touched):
clis = [{}]

[features]
mcp = {}
skills = {}
instructions = {}
"#,
        cfg.scope.id(),
        clis,
        cfg.mcp,
        cfg.skills,
        cfg.instructions
    );
    util::write_atomic(&paths::store_config(), &body)
}

/// Write a default config.toml documenting the options.
pub fn write_default() -> R<()> {
    save(&Config::default())
}

/// The CLIs we will actually act on: configured AND installed.
pub fn active_clis(cfg: &Config) -> Vec<Cli> {
    cfg.clis
        .iter()
        .copied()
        .filter(|&c| adapters::installed(c))
        .collect()
}
