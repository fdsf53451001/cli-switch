//! Portable custom-agent definitions and loss-aware native format adapters.
//!
//! The functions in this module are deliberately pure. Discovery, snapshots and
//! writes belong to the sync engine; adapters only turn bytes into a portable
//! definition and back.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::str::FromStr;
use toml_edit::{value, Array, DocumentMut, Item, Table, Value as TomlValue};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Primary,
    Subagent,
    #[default]
    Either,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelIntent {
    #[default]
    Inherit,
    Fast,
    Balanced,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityPolicy {
    Deny,
    Ask,
    Allow,
}

/// Portable permissions are deny-by-default when `capabilities` is non-empty.
/// An adapter must error rather than omit a restriction it cannot express.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentPermissions {
    #[serde(default)]
    pub capabilities: BTreeMap<String, CapabilityPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Stable, portable filename/key identifier.
    pub id: String,
    /// Human-facing agent name.
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default)]
    pub role: AgentRole,
    #[serde(default)]
    pub model: ModelIntent,
    #[serde(default)]
    pub permissions: AgentPermissions,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// Unknown native fields, keyed by adapter namespace (`claude`, `codex`, …).
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAgentFormat {
    Claude,
    Codex,
    OpenCode,
    Kiro,
    Copilot,
    Agy,
}

impl NativeAgentFormat {
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Kiro => "kiro",
            Self::Copilot => "copilot",
            Self::Agy => "agy",
        }
    }

    pub fn reserved_ids(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["default", "worker", "explorer"],
            Self::Claude => &[
                "general-purpose",
                "Explore",
                "Plan",
                "statusline-setup",
                "claude-code-guide",
            ],
            Self::OpenCode => &[
                "build",
                "plan",
                "general",
                "explore",
                "scout",
                "compaction",
                "title",
                "summary",
            ],
            Self::Kiro => &["kiro_default"],
            Self::Copilot => &["copilot", "code-review", "explore"],
            Self::Agy => &["default", "research", "browser", "self"],
        }
    }

    pub fn is_reserved(self, id: &str) -> bool {
        self.reserved_ids()
            .iter()
            .any(|v| v.eq_ignore_ascii_case(id))
    }

    pub fn parse(self, id: &str, source: &str) -> Result<AgentDefinition> {
        match self {
            Self::Claude => parse_markdown(self, id, source, MarkdownFlavor::Claude),
            Self::Codex => parse_codex(id, source),
            Self::OpenCode => parse_markdown(self, id, source, MarkdownFlavor::OpenCode),
            Self::Kiro => parse_kiro(id, source),
            Self::Agy => parse_agy(id, source),
            Self::Copilot => parse_markdown(self, id, source, MarkdownFlavor::Copilot),
        }
    }

    pub fn render(self, agent: &AgentDefinition) -> Result<String> {
        if self.is_reserved(&agent.id) {
            return Err(format!(
                "{} is a reserved {} agent id",
                agent.id,
                self.namespace()
            ));
        }
        if !agent.permissions.capabilities.is_empty()
            && !agent.extensions.contains_key(self.namespace())
        {
            return Err(format!(
                "cannot translate permissions into {} without proven native semantics",
                self.namespace()
            ));
        }
        match self {
            Self::Claude => render_markdown(self, agent, MarkdownFlavor::Claude),
            Self::Codex => render_codex(agent),
            Self::OpenCode => render_markdown(self, agent, MarkdownFlavor::OpenCode),
            Self::Kiro => render_kiro(agent),
            Self::Agy => render_agy(agent),
            Self::Copilot => render_markdown(self, agent, MarkdownFlavor::Copilot),
        }
    }
}

#[derive(Clone, Copy)]
enum MarkdownFlavor {
    Claude,
    OpenCode,
    Copilot,
}

fn parse_markdown(
    format: NativeAgentFormat,
    id: &str,
    source: &str,
    flavor: MarkdownFlavor,
) -> Result<AgentDefinition> {
    let (mut meta, body) = split_frontmatter(source)?;
    let name = take_string(&mut meta, "name").unwrap_or_else(|| id.to_string());
    let description = take_string(&mut meta, "description").unwrap_or_default();
    let role = match flavor {
        MarkdownFlavor::OpenCode => match take_string(&mut meta, "mode").as_deref() {
            Some("primary") => AgentRole::Primary,
            Some("subagent") => AgentRole::Subagent,
            _ => AgentRole::Either,
        },
        _ => parse_role(take_string(&mut meta, "role").as_deref()),
    };
    // Native model IDs are vendor-specific. Preserve them in the native
    // extension; do not pretend they are portable intents.
    let model = ModelIntent::Inherit;
    let skills = take_strings(&mut meta, "skills");
    let mcp_servers = take_strings(&mut meta, "mcpServers")
        .into_iter()
        .chain(take_strings(&mut meta, "mcp-servers"))
        .collect();
    let permissions = match flavor {
        MarkdownFlavor::Claude => {
            let mut p = AgentPermissions::default();
            for tool in take_strings(&mut meta, "tools") {
                p.capabilities.insert(tool, CapabilityPolicy::Allow);
            }
            for tool in take_strings(&mut meta, "disallowedTools") {
                p.capabilities.insert(tool, CapabilityPolicy::Deny);
            }
            p
        }
        MarkdownFlavor::OpenCode => parse_permission_value(meta.remove("permission")),
        MarkdownFlavor::Copilot => {
            let mut p = AgentPermissions::default();
            for tool in take_strings(&mut meta, "tools") {
                p.capabilities.insert(tool, CapabilityPolicy::Allow);
            }
            p
        }
    };
    let extensions = extension(format.namespace(), meta);
    Ok(AgentDefinition {
        id: id.to_string(),
        name,
        description,
        instructions: body.trim().to_string(),
        role,
        model,
        permissions,
        skills,
        mcp_servers,
        extensions,
    })
}

fn render_markdown(
    format: NativeAgentFormat,
    agent: &AgentDefinition,
    flavor: MarkdownFlavor,
) -> Result<String> {
    let mut meta = native_extension(agent, format.namespace());
    meta.insert("name".into(), Value::String(agent.id.clone()));
    meta.insert(
        "description".into(),
        Value::String(agent.description.clone()),
    );
    match flavor {
        MarkdownFlavor::OpenCode => {
            meta.insert(
                "mode".into(),
                Value::String(
                    match agent.role {
                        AgentRole::Primary => "primary",
                        AgentRole::Subagent => "subagent",
                        AgentRole::Either => "all",
                    }
                    .into(),
                ),
            );
            meta.insert("permission".into(), permission_value(&agent.permissions));
        }
        MarkdownFlavor::Claude => {
            meta.insert("role".into(), Value::String(role_name(agent.role).into()));
            let (allow, deny, ask) = partition_permissions(&agent.permissions);
            if !ask.is_empty() {
                return Err(
                    "Claude Markdown cannot express ask permissions without widening them".into(),
                );
            }
            insert_array(&mut meta, "tools", allow);
            insert_array(&mut meta, "disallowedTools", deny);
        }
        MarkdownFlavor::Copilot => {
            meta.insert("role".into(), Value::String(role_name(agent.role).into()));
            let (allow, deny, ask) = partition_permissions(&agent.permissions);
            if !deny.is_empty() || !ask.is_empty() {
                return Err("Copilot .agent.md cannot express deny/ask permissions safely".into());
            }
            insert_array(&mut meta, "tools", allow);
        }
    }
    if agent.model != ModelIntent::Inherit {
        return Err("abstract model intent requires an explicit CLI model mapping".into());
    }
    insert_array(&mut meta, "skills", agent.skills.clone());
    insert_array(&mut meta, "mcpServers", agent.mcp_servers.clone());
    Ok(format!(
        "---\n{}---\n\n{}\n",
        render_yaml_map(&meta),
        agent.instructions.trim()
    ))
}

fn parse_codex(id: &str, source: &str) -> Result<AgentDefinition> {
    let doc = DocumentMut::from_str(source).map_err(|e| e.to_string())?;
    let mut meta = Map::new();
    for (key, item) in doc.iter() {
        meta.insert(key.to_string(), toml_item_json(item));
    }
    let name = required_string(&mut meta, "name")?;
    let description = required_string(&mut meta, "description")?;
    let instructions = required_string(&mut meta, "developer_instructions")?;
    let model = ModelIntent::Inherit;
    // These are native config objects, not portable references. Preserve them
    // untouched in the Codex extension.
    let skills = Vec::new();
    let mcp_servers = Vec::new();
    let mut permissions = AgentPermissions::default();
    if let Some(sandbox) = take_string(&mut meta, "sandbox_mode") {
        permissions.capabilities.insert(
            "filesystem.write".into(),
            if sandbox == "read-only" {
                CapabilityPolicy::Deny
            } else {
                CapabilityPolicy::Allow
            },
        );
    }
    Ok(AgentDefinition {
        id: id.into(),
        name,
        description,
        instructions,
        role: AgentRole::Either,
        model,
        permissions,
        skills,
        mcp_servers,
        extensions: extension("codex", meta),
    })
}

fn render_codex(agent: &AgentDefinition) -> Result<String> {
    let mut meta = native_extension(agent, "codex");
    let mut doc = DocumentMut::new();
    for (key, val) in std::mem::take(&mut meta) {
        doc[&key] = json_toml_item(&val)?;
    }
    doc["name"] = value(&agent.id);
    doc["description"] = value(&agent.description);
    doc["developer_instructions"] = value(&agent.instructions);
    if agent.model != ModelIntent::Inherit {
        return Err("abstract model intent requires an explicit Codex model mapping".into());
    }
    let write = agent.permissions.capabilities.get("filesystem.write");
    if agent
        .permissions
        .capabilities
        .keys()
        .any(|k| k != "filesystem.write")
    {
        return Err("Codex TOML cannot safely express portable per-capability permissions".into());
    }
    match write {
        Some(CapabilityPolicy::Deny) => doc["sandbox_mode"] = value("read-only"),
        Some(CapabilityPolicy::Allow) => doc["sandbox_mode"] = value("workspace-write"),
        Some(CapabilityPolicy::Ask) => {
            return Err("Codex agent sandbox_mode cannot express ask permission".into())
        }
        None => {
            doc.remove("sandbox_mode");
        }
    }
    if !agent.skills.is_empty() {
        let mut arr = toml_edit::ArrayOfTables::new();
        for path in &agent.skills {
            let mut t = Table::new();
            t["path"] = value(path);
            t["enabled"] = value(true);
            arr.push(t);
        }
        let mut skills = Table::new();
        skills["config"] = Item::ArrayOfTables(arr);
        doc["skills"] = Item::Table(skills);
    }
    if !agent.mcp_servers.is_empty() {
        let mut servers = Table::new();
        for server in &agent.mcp_servers {
            servers[server] = Item::Table(Table::new());
        }
        doc["mcp_servers"] = Item::Table(servers);
    }
    Ok(doc.to_string())
}

fn json_object(source: &str) -> Result<Map<String, Value>> {
    serde_json::from_str::<Value>(source)
        .map_err(|e| e.to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "agent JSON must be an object".to_string())
}

fn parse_kiro(id: &str, source: &str) -> Result<AgentDefinition> {
    let mut meta = json_object(source)?;
    let name = take_string(&mut meta, "name").unwrap_or_else(|| id.into());
    let description = take_string(&mut meta, "description").unwrap_or_default();
    let instructions = take_string(&mut meta, "prompt").unwrap_or_default();
    let role = parse_role(take_string(&mut meta, "role").as_deref());
    let model = ModelIntent::Inherit;
    let skills = take_strings(&mut meta, "skills");
    // Kiro MCP entries carry full native configs. Keep them namespaced.
    let mcp_servers = Vec::new();
    let mut permissions = AgentPermissions::default();
    for tool in take_strings(&mut meta, "allowedTools") {
        permissions
            .capabilities
            .insert(tool, CapabilityPolicy::Allow);
    }
    Ok(AgentDefinition {
        id: id.into(),
        name,
        description,
        instructions,
        role,
        model,
        permissions,
        skills,
        mcp_servers,
        extensions: extension("kiro", meta),
    })
}

fn render_kiro(agent: &AgentDefinition) -> Result<String> {
    let mut obj = native_extension(agent, "kiro");
    obj.insert("name".into(), Value::String(agent.id.clone()));
    obj.insert(
        "description".into(),
        Value::String(agent.description.clone()),
    );
    obj.insert("prompt".into(), Value::String(agent.instructions.clone()));
    obj.insert("role".into(), Value::String(role_name(agent.role).into()));
    if agent.model != ModelIntent::Inherit {
        return Err("abstract model intent requires an explicit Kiro model mapping".into());
    }
    let (allow, deny, ask) = partition_permissions(&agent.permissions);
    if !deny.is_empty() || !ask.is_empty() {
        return Err("Kiro allowedTools cannot express deny/ask permissions safely".into());
    }
    insert_array(&mut obj, "allowedTools", allow.clone());
    insert_array(&mut obj, "tools", allow);
    obj.insert("skills".into(), strings_value(&agent.skills));
    if !agent.mcp_servers.is_empty() {
        obj.insert("mcpServers".into(), empty_object_refs(&agent.mcp_servers));
    }
    serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())
}

fn parse_agy(id: &str, source: &str) -> Result<AgentDefinition> {
    let mut meta = json_object(source)?;
    let name = take_string(&mut meta, "name").unwrap_or_else(|| id.into());
    let description = take_string(&mut meta, "description").unwrap_or_default();
    let role = parse_role(take_string(&mut meta, "role").as_deref());
    let model = ModelIntent::Inherit;
    let skills = take_strings(&mut meta, "skills");
    let mcp_servers = take_strings(&mut meta, "mcpServers");
    let mut instructions = String::new();
    let mut tools = Vec::new();
    if let Some(Value::Object(config)) = meta.get_mut("config") {
        if let Some(Value::Object(custom)) = config.get_mut("customAgent") {
            if let Some(Value::Array(sections)) = custom.remove("systemPromptSections") {
                instructions = sections
                    .iter()
                    .filter_map(|s| s.get("content").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let extras: Vec<Value> = sections
                    .into_iter()
                    .filter_map(|mut section| {
                        section.as_object_mut()?.remove("content");
                        (!section.as_object()?.is_empty()).then_some(section)
                    })
                    .collect();
                if !extras.is_empty() {
                    custom.insert("systemPromptSectionExtras".into(), Value::Array(extras));
                }
            }
            tools = match custom.remove("toolNames") {
                Some(Value::Array(values)) => values
                    .into_iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            };
        }
    }
    let permissions = AgentPermissions {
        capabilities: tools
            .into_iter()
            .map(|tool| (tool, CapabilityPolicy::Allow))
            .collect(),
    };
    Ok(AgentDefinition {
        id: id.into(),
        name,
        description,
        instructions,
        role,
        model,
        permissions,
        skills,
        mcp_servers,
        extensions: extension("agy", meta),
    })
}

fn render_agy(agent: &AgentDefinition) -> Result<String> {
    let (allow, deny, ask) = partition_permissions(&agent.permissions);
    if !deny.is_empty() || !ask.is_empty() {
        return Err("Agy toolNames cannot express deny/ask permissions safely".into());
    }
    let mut obj = native_extension(agent, "agy");
    obj.insert("name".into(), Value::String(agent.id.clone()));
    obj.insert(
        "description".into(),
        Value::String(agent.description.clone()),
    );
    obj.insert("role".into(), Value::String(role_name(agent.role).into()));
    if agent.model != ModelIntent::Inherit {
        return Err("abstract model intent requires an explicit agy model mapping".into());
    }
    obj.insert("skills".into(), strings_value(&agent.skills));
    obj.insert("mcpServers".into(), strings_value(&agent.mcp_servers));
    let config = object_entry(&mut obj, "config");
    let custom = object_entry(config, "customAgent");
    let mut sections = custom
        .remove("systemPromptSectionExtras")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    if sections.is_empty() {
        sections.push(Value::Object(Map::new()));
    }
    if !sections[0].is_object() {
        sections[0] = Value::Object(Map::new());
    }
    sections[0]
        .as_object_mut()
        .unwrap()
        .insert("content".into(), Value::String(agent.instructions.clone()));
    custom.insert("systemPromptSections".into(), Value::Array(sections));
    custom.insert("toolNames".into(), strings_value(&allow));
    serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())
}

fn split_frontmatter(source: &str) -> Result<(Map<String, Value>, &str)> {
    let normalized = source.strip_prefix('\u{feff}').unwrap_or(source);
    if !normalized.starts_with("---\n") {
        return Err("Markdown agent must begin with YAML frontmatter".into());
    }
    let rest = &normalized[4..];
    let end = rest
        .find("\n---")
        .ok_or_else(|| "unterminated YAML frontmatter".to_string())?;
    Ok((parse_simple_yaml(&rest[..end])?, &rest[end + 4..]))
}

/// Minimal frontmatter YAML parser for the scalar/list/map subset emitted here.
/// A future full YAML dependency can replace this without changing public APIs.
fn parse_simple_yaml(text: &str) -> Result<Map<String, Value>> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect();
    if lines.is_empty() {
        return Ok(Map::new());
    }
    let (value, consumed) = parse_yaml_block(&lines, 0, indentation(lines[0]))?;
    if consumed != lines.len() {
        return Err(format!(
            "unsupported YAML near `{}`",
            lines[consumed].trim()
        ));
    }
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "YAML frontmatter root must be a mapping".into())
}

fn parse_yaml_block(lines: &[&str], mut at: usize, indent: usize) -> Result<(Value, usize)> {
    if lines[at].trim_start().starts_with("- ") {
        let mut values = Vec::new();
        while at < lines.len()
            && indentation(lines[at]) == indent
            && lines[at].trim_start().starts_with("- ")
        {
            values.push(parse_yaml_value(lines[at].trim_start()[2..].trim()));
            at += 1;
        }
        return Ok((Value::Array(values), at));
    }

    let mut values = Map::new();
    while at < lines.len() && indentation(lines[at]) == indent {
        let line = lines[at].trim();
        let (key, raw_value) = line
            .split_once(':')
            .ok_or_else(|| format!("unsupported YAML near `{line}`"))?;
        let key = key.trim().trim_matches(['\'', '"']).to_string();
        let raw_value = raw_value.trim();
        at += 1;
        if matches!(raw_value, "|" | ">") {
            let mut body = Vec::new();
            while at < lines.len() && indentation(lines[at]) > indent {
                body.push(lines[at].trim());
                at += 1;
            }
            values.insert(
                key,
                Value::String(if raw_value == ">" {
                    body.join(" ")
                } else {
                    body.join("\n")
                }),
            );
        } else if raw_value.is_empty() && at < lines.len() && indentation(lines[at]) > indent {
            let child_indent = indentation(lines[at]);
            let (child, next) = parse_yaml_block(lines, at, child_indent)?;
            values.insert(key, child);
            at = next;
        } else {
            values.insert(
                key,
                if raw_value.is_empty() {
                    Value::Null
                } else {
                    parse_yaml_value(raw_value)
                },
            );
        }
    }
    Ok((Value::Object(values), at))
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn parse_yaml_value(raw: &str) -> Value {
    if let Ok(v) = serde_json::from_str(raw) {
        return v;
    }
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" | "~" => Value::Null,
        _ if raw.starts_with('[') && raw.ends_with(']') => Value::Array(
            raw[1..raw.len() - 1]
                .split(',')
                .filter_map(|s| {
                    let s = s.trim();
                    (!s.is_empty()).then(|| Value::String(unquote(s).into()))
                })
                .collect(),
        ),
        _ => Value::String(unquote(raw).into()),
    }
}

fn render_yaml_map(map: &Map<String, Value>) -> String {
    let mut out = String::new();
    let ordered: BTreeMap<_, _> = map.iter().collect();
    for (key, val) in ordered {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(&yaml_value(val));
        out.push('\n');
    }
    out
}

fn yaml_value(value: &Value) -> String {
    match value {
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
}

fn parse_role(value: Option<&str>) -> AgentRole {
    match value {
        Some("primary") => AgentRole::Primary,
        Some("subagent") => AgentRole::Subagent,
        _ => AgentRole::Either,
    }
}

fn role_name(value: AgentRole) -> &'static str {
    match value {
        AgentRole::Primary => "primary",
        AgentRole::Subagent => "subagent",
        AgentRole::Either => "either",
    }
}

fn take_string(map: &mut Map<String, Value>, key: &str) -> Option<String> {
    map.remove(key).and_then(|v| match v {
        Value::String(s) => Some(s),
        _ => None,
    })
}

fn required_string(map: &mut Map<String, Value>, key: &str) -> Result<String> {
    take_string(map, key)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required `{key}`"))
}

fn take_strings(map: &mut Map<String, Value>, key: &str) -> Vec<String> {
    let portable = matches!(map.get(key), Some(Value::Array(_)) | Some(Value::String(_)));
    if !portable {
        return Vec::new();
    }
    match map.remove(key) {
        Some(Value::Array(v)) => v
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(v)) => v
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn empty_object_refs(values: &[String]) -> Value {
    Value::Object(
        values
            .iter()
            .cloned()
            .map(|key| (key, Value::Object(Map::new())))
            .collect(),
    )
}

fn object_entry<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !matches!(map.get(key), Some(Value::Object(_))) {
        map.insert(key.into(), Value::Object(Map::new()));
    }
    map.get_mut(key).and_then(Value::as_object_mut).unwrap()
}

fn strings_value(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn insert_array(map: &mut Map<String, Value>, key: &str, values: Vec<String>) {
    if values.is_empty() {
        map.remove(key);
    } else {
        map.insert(key.into(), strings_value(&values));
    }
}

fn extension(namespace: &str, values: Map<String, Value>) -> BTreeMap<String, Value> {
    if values.is_empty() {
        BTreeMap::new()
    } else {
        BTreeMap::from([(namespace.into(), Value::Object(values))])
    }
}

fn native_extension(agent: &AgentDefinition, namespace: &str) -> Map<String, Value> {
    agent
        .extensions
        .get(namespace)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn parse_permission_value(value: Option<Value>) -> AgentPermissions {
    let mut result = AgentPermissions::default();
    if let Some(Value::Object(obj)) = value {
        flatten_permission_object("", &obj, &mut result.capabilities);
    }
    result
}

fn flatten_permission_object(
    prefix: &str,
    object: &Map<String, Value>,
    out: &mut BTreeMap<String, CapabilityPolicy>,
) {
    for (key, value) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(child) => flatten_permission_object(&path, child, out),
            Value::String(policy) => {
                out.insert(
                    path,
                    match policy.as_str() {
                        "allow" => CapabilityPolicy::Allow,
                        "ask" => CapabilityPolicy::Ask,
                        _ => CapabilityPolicy::Deny,
                    },
                );
            }
            // Unknown permission syntax must fail closed, never disappear into a
            // permissive native default.
            _ => {
                out.insert(path, CapabilityPolicy::Deny);
            }
        }
    }
}

fn permission_value(value: &AgentPermissions) -> Value {
    let mut root = Map::new();
    for (path, policy) in &value.capabilities {
        let segments: Vec<&str> = path.split('.').collect();
        let mut target = &mut root;
        for segment in &segments[..segments.len().saturating_sub(1)] {
            target = object_entry(target, segment);
        }
        if let Some(last) = segments.last() {
            target.insert(
                (*last).into(),
                Value::String(
                    match policy {
                        CapabilityPolicy::Deny => "deny",
                        CapabilityPolicy::Ask => "ask",
                        CapabilityPolicy::Allow => "allow",
                    }
                    .into(),
                ),
            );
        }
    }
    Value::Object(root)
}

fn partition_permissions(value: &AgentPermissions) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut allow = Vec::new();
    let mut deny = Vec::new();
    let mut ask = Vec::new();
    for (key, policy) in &value.capabilities {
        match policy {
            CapabilityPolicy::Allow => allow.push(key.clone()),
            CapabilityPolicy::Deny => deny.push(key.clone()),
            CapabilityPolicy::Ask => ask.push(key.clone()),
        }
    }
    (allow, deny, ask)
}

fn toml_item_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => toml_value_json(v),
        Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.into(), toml_item_json(v)))
                .collect(),
        ),
        Item::ArrayOfTables(a) => Value::Array(
            a.iter()
                .map(|t| {
                    Value::Object(
                        t.iter()
                            .map(|(k, v)| (k.into(), toml_item_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

fn toml_value_json(value: &TomlValue) -> Value {
    match value {
        TomlValue::String(v) => Value::String(v.value().to_string()),
        TomlValue::Integer(v) => Value::from(*v.value()),
        TomlValue::Float(v) => Value::from(*v.value()),
        TomlValue::Boolean(v) => Value::from(*v.value()),
        TomlValue::Datetime(v) => Value::String(v.value().to_string()),
        TomlValue::Array(v) => Value::Array(v.iter().map(toml_value_json).collect()),
        TomlValue::InlineTable(v) => Value::Object(
            v.iter()
                .map(|(k, v)| (k.into(), toml_value_json(v)))
                .collect(),
        ),
    }
}

fn json_toml_item(v: &Value) -> Result<Item> {
    Ok(match v {
        Value::Null => Item::None,
        Value::Bool(v) => value(*v),
        Value::Number(v) if v.is_i64() => value(v.as_i64().unwrap()),
        Value::Number(v) if v.is_f64() => value(v.as_f64().unwrap()),
        Value::Number(_) => return Err("TOML cannot represent unsigned integer".into()),
        Value::String(v) => value(v),
        Value::Array(values) => {
            let mut arr = Array::new();
            for val in values {
                let item = json_toml_item(val)?;
                let Item::Value(v) = item else {
                    return Err("nested TOML array/table extension is unsupported".into());
                };
                arr.push(v);
            }
            value(arr)
        }
        Value::Object(values) => {
            let mut table = Table::new();
            for (key, val) in values {
                table[key] = json_toml_item(val)?;
            }
            Item::Table(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AgentDefinition {
        AgentDefinition {
            id: "reviewer".into(),
            name: "Reviewer".into(),
            description: "Find bugs".into(),
            instructions: "Review carefully.".into(),
            role: AgentRole::Subagent,
            model: ModelIntent::Strong,
            permissions: AgentPermissions::default(),
            skills: vec!["security/SKILL.md".into()],
            mcp_servers: vec!["docs".into()],
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_round_trip_preserves_unknown_field() {
        let input = "---\nname: Reviewer\ndescription: Find bugs\nrole: subagent\nx-vendor: {\"flag\":true}\n---\nReview carefully.\n";
        let parsed = NativeAgentFormat::Claude.parse("reviewer", input).unwrap();
        assert_eq!(parsed.extensions["claude"]["x-vendor"]["flag"], true);
        let output = NativeAgentFormat::Claude.render(&parsed).unwrap();
        assert!(output.contains("x-vendor:"));
    }

    #[test]
    fn codex_requires_official_fields_and_preserves_unknown() {
        assert!(NativeAgentFormat::Codex.parse("x", "name='x'").is_err());
        let parsed = NativeAgentFormat::Codex.parse("reviewer", "name='Reviewer'\ndescription='Find bugs'\ndeveloper_instructions='Review'\nnickname_candidates=['Rex']\n").unwrap();
        assert_eq!(parsed.extensions["codex"]["nickname_candidates"][0], "Rex");
        let rendered = NativeAgentFormat::Codex.render(&parsed).unwrap();
        assert!(rendered.contains("nickname_candidates"));
    }

    #[test]
    fn kiro_preserves_unknown_and_uses_native_prompt_and_allowed_tools() {
        let input = r#"{"name":"R","description":"D","prompt":"Do it","allowedTools":["read"],"mcpServers":{"docs":{}},"nativeFlag":7}"#;
        let parsed = NativeAgentFormat::Kiro.parse("r", input).unwrap();
        assert_eq!(
            parsed.permissions.capabilities["read"],
            CapabilityPolicy::Allow
        );
        assert_eq!(parsed.extensions["kiro"]["nativeFlag"], 7);
        let rendered = NativeAgentFormat::Kiro.render(&parsed).unwrap();
        assert!(rendered.contains("\"prompt\""));
        let reparsed = NativeAgentFormat::Kiro.parse("r", &rendered).unwrap();
        assert_eq!(reparsed.permissions, parsed.permissions);
        assert!(reparsed.mcp_servers.is_empty());
        assert!(reparsed.extensions["kiro"]["mcpServers"]["docs"].is_object());
    }

    #[test]
    fn agy_reads_and_writes_nested_custom_agent_fields() {
        let input = r#"{"name":"R","description":"D","config":{"customAgent":{"systemPromptSections":[{"title":"base","content":"Do it"}],"toolNames":["read"],"nativeFlag":7}}}"#;
        let parsed = NativeAgentFormat::Agy.parse("r", input).unwrap();
        assert_eq!(parsed.instructions, "Do it");
        assert_eq!(
            parsed.permissions.capabilities["read"],
            CapabilityPolicy::Allow
        );
        let rendered = NativeAgentFormat::Agy.render(&parsed).unwrap();
        let native: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            native["config"]["customAgent"]["systemPromptSections"][0]["content"],
            "Do it"
        );
        assert_eq!(native["config"]["customAgent"]["nativeFlag"], 7);
    }

    #[test]
    fn render_never_silently_widens_permissions() {
        let mut agent = sample();
        agent
            .permissions
            .capabilities
            .insert("shell".into(), CapabilityPolicy::Ask);
        assert!(NativeAgentFormat::Claude.render(&agent).is_err());
        assert!(NativeAgentFormat::Copilot.render(&agent).is_err());
        assert!(NativeAgentFormat::Codex.render(&agent).is_err());
    }

    #[test]
    fn reserved_ids_are_rejected_case_insensitively() {
        let mut agent = sample();
        agent.id = "Worker".into();
        assert!(NativeAgentFormat::Codex.render(&agent).is_err());
    }

    #[test]
    fn every_format_renders_a_basic_agent() {
        let mut agent = sample();
        agent.model = ModelIntent::Inherit;
        agent.skills.clear();
        agent.mcp_servers.clear();
        for format in [
            NativeAgentFormat::Claude,
            NativeAgentFormat::Codex,
            NativeAgentFormat::OpenCode,
            NativeAgentFormat::Kiro,
            NativeAgentFormat::Copilot,
            NativeAgentFormat::Agy,
        ] {
            assert!(format.render(&agent).is_ok(), "{}", format.namespace());
        }
    }
}
