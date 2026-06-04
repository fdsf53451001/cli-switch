//! `<store>/config.toml` — which CLIs and which features to sync.

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
    if util::read_to_string_opt(&paths::store_config())?.is_none() {
        return Ok(disabled_global());
    }
    load_from(&paths::store_config(), Scope::Global)
}

pub fn load_project() -> R<Option<Config>> {
    let path = paths::project_config();
    if util::read_to_string_opt(&path)?.is_none() {
        return Ok(None);
    }
    load_from(&path, Scope::Project).map(Some)
}

pub fn project_joined() -> bool {
    paths::project_config().exists()
}

fn load_from(path: &std::path::Path, default_scope: Scope) -> R<Config> {
    let Some(text) = util::read_to_string_opt(path)? else {
        return Ok(default_for_scope(default_scope));
    };
    let doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| util::ctx(path, e))?;

    let mut cfg = default_for_scope(default_scope);
    if let Some(scope) = doc
        .get("scope")
        .and_then(|i| i.as_str())
        .and_then(Scope::from_id)
    {
        // Backward-compatible read for old config files. The caller's path
        // decides whether this is global or project config.
        if path == paths::store_config() && scope == Scope::Global {
            cfg.scope = Scope::Global;
        }
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
        cfg.mcp = b("mcp", cfg.mcp);
        cfg.skills = b("skills", cfg.skills);
        cfg.instructions = b("instructions", cfg.instructions);
    }
    Ok(cfg)
}

fn default_for_scope(scope: Scope) -> Config {
    Config {
        scope,
        mcp: scope == Scope::Global,
        ..Config::default()
    }
}

fn disabled_global() -> Config {
    Config {
        scope: Scope::Global,
        clis: Vec::new(),
        mcp: true,
        skills: true,
        instructions: true,
    }
}

pub fn save(cfg: &Config) -> R<()> {
    save_to(&paths::store_config(), cfg)
}

pub fn save_project(cfg: &Config) -> R<()> {
    save_to(&paths::project_config(), cfg)
}

pub fn remove_project() -> R<()> {
    let path = paths::project_config();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(util::ctx(&path, e)),
    }
}

fn save_to(path: &std::path::Path, cfg: &Config) -> R<()> {
    let clis = cfg
        .clis
        .iter()
        .map(|c| format!("\"{}\"", c.id()))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        r#"# cli-switch configuration.
# scope is kept for readability; global and project config live in different files.
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
    util::write_atomic(path, &body)
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
