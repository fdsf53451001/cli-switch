//! Adapters: convert each CLI's native MCP config <-> the neutral model.
//!
//! Reads parse a CLI's on-disk file into an `McpMap`. Writes upsert the given
//! servers and delete the given names, touching ONLY the MCP section so that
//! unrelated config (auth, `[projects]`, `[tui]`, theme, …) is preserved.

use crate::model::{Cli, McpMap, McpServer, Transport};
use crate::paths;
use crate::util::{self, R};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Is this CLI present on the machine? (config dir / file exists)
pub fn installed(cli: Cli) -> bool {
    let h = paths::home();
    let probe = match cli {
        Cli::Claude => h.join(".claude.json"),
        Cli::Codex => h.join(".codex"),
        Cli::Opencode => h.join(".config").join("opencode"),
        Cli::Kiro => h.join(".kiro"),
        Cli::Antigravity => {
            return command_exists("agy") || h.join(".gemini").join("antigravity-cli").exists();
        }
        Cli::Copilot => {
            return command_exists("copilot") || h.join(".copilot").exists();
        }
    };
    probe.exists()
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let plain = dir.join(name);
                #[cfg(windows)]
                {
                    plain.exists() || dir.join(format!("{name}.exe")).exists()
                }
                #[cfg(not(windows))]
                {
                    plain.exists()
                }
            })
        })
        .unwrap_or(false)
}

/// Read a CLI's MCP servers into the neutral model. Missing file -> empty map.
pub fn read_mcp(cli: Cli) -> R<McpMap> {
    let path = paths::mcp_config(cli);
    let Some(text) = util::read_to_string_opt(&path)? else {
        return Ok(McpMap::new());
    };
    if text.trim().is_empty() {
        return Ok(McpMap::new());
    }
    match cli {
        Cli::Codex => codex_read(&text),
        Cli::Claude => json_read_checked(&text, "mcpServers", claude_parse),
        Cli::Opencode => json_read_checked(&text, "mcp", opencode_parse),
        Cli::Kiro => json_read_checked(&text, "mcpServers", kiro_parse),
        Cli::Antigravity => json_read_checked(&text, "mcpServers", antigravity_parse),
        Cli::Copilot => json_read_checked(&text, "mcpServers", copilot_parse),
    }
    .map_err(|e| util::ctx(&path, e))
}

/// Render a native MCP config without touching disk. This is the planning seam
/// used by the transactional synchronizer.
pub fn render_mcp(
    cli: Cli,
    existing: Option<&str>,
    servers: &McpMap,
    remove: &BTreeSet<String>,
) -> R<String> {
    for (name, server) in servers {
        validate_server(cli, name, server)?;
    }
    match cli {
        Cli::Codex => codex_write(existing, servers, remove),
        Cli::Claude => json_write(existing, "mcpServers", servers, remove, claude_emit, false),
        Cli::Opencode => json_write(existing, "mcp", servers, remove, opencode_emit, false),
        Cli::Kiro => json_write(existing, "mcpServers", servers, remove, kiro_emit, true),
        Cli::Antigravity => json_write(
            existing,
            "mcpServers",
            servers,
            remove,
            antigravity_emit,
            true,
        ),
        Cli::Copilot => json_write(existing, "mcpServers", servers, remove, copilot_emit, true),
    }
}

fn validate_server(cli: Cli, name: &str, server: &McpServer) -> R<()> {
    match server.transport {
        Transport::Stdio if server.command.as_deref().map(str::is_empty).unwrap_or(true) => {
            return Err(format!(
                "MCP server '{name}' has stdio transport but no command"
            ));
        }
        Transport::Http if server.url.as_deref().map(str::is_empty).unwrap_or(true) => {
            return Err(format!("MCP server '{name}' has HTTP transport but no URL"));
        }
        _ => {}
    }
    if !server.enabled && matches!(cli, Cli::Claude | Cli::Copilot) {
        return Err(format!(
            "{} cannot represent disabled MCP server '{name}'; refusing a lossy sync",
            cli.id()
        ));
    }
    if let Some(protocol) = &server.protocol_hint {
        if !matches!(cli, Cli::Claude | Cli::Copilot) {
            return Err(format!(
                "{} cannot preserve MCP protocol '{protocol}' for server '{name}'; refusing a lossy sync",
                cli.id()
            ));
        }
    }
    Ok(())
}

// ───────────────────────── JSON plumbing ─────────────────────────

type ParseFn = fn(&Value) -> Option<McpServer>;
type EmitFn = fn(&McpServer) -> Value;

/// Read servers out of a JSON file at top-level `key`.
fn json_read(text: &str, key: &str, parse: ParseFn) -> Result<McpMap, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = McpMap::new();
    if let Some(obj) = root.get(key).and_then(|v| v.as_object()) {
        for (name, entry) in obj {
            if let Some(srv) = parse(entry) {
                out.insert(name.clone(), srv);
            }
        }
    }
    Ok(out)
}

fn json_read_checked(text: &str, key: &str, parse: ParseFn) -> Result<McpMap, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(section) = root.get(key) {
        let object = section
            .as_object()
            .ok_or_else(|| format!("{key} is not an object"))?;
        for (name, entry) in object {
            let Some(fields) = entry.as_object() else {
                return Err(format!("MCP server '{name}' is not an object"));
            };
            for map_key in ["env", "environment", "headers"] {
                if let Some(value) = fields.get(map_key) {
                    let values = value.as_object().ok_or_else(|| {
                        format!("MCP server '{name}' field '{map_key}' is not an object")
                    })?;
                    if values.values().any(|value| !value.is_string()) {
                        return Err(format!(
                            "MCP server '{name}' field '{map_key}' contains a non-string value"
                        ));
                    }
                }
            }
            for array_key in ["args"] {
                if let Some(value) = fields.get(array_key) {
                    let values = value.as_array().ok_or_else(|| {
                        format!("MCP server '{name}' field '{array_key}' is not an array")
                    })?;
                    if values.iter().any(|value| !value.is_string()) {
                        return Err(format!(
                            "MCP server '{name}' field '{array_key}' contains a non-string value"
                        ));
                    }
                }
            }
            if let Some(value) = fields.get("command") {
                let valid = value.is_string()
                    || value
                        .as_array()
                        .map(|items| items.iter().all(Value::is_string))
                        .unwrap_or(false);
                if !valid {
                    return Err(format!(
                        "MCP server '{name}' field 'command' has an unsupported shape"
                    ));
                }
            }
        }
    }
    json_read(text, key, parse)
}

/// Upsert/remove servers under top-level `key`, preserving every other key.
/// `wrap_only` controls files dedicated to MCP (kiro/antigravity): if the file
/// is empty/new we still produce a minimal `{ "<key>": {...} }`.
fn json_write(
    existing: Option<&str>,
    key: &str,
    servers: &McpMap,
    remove: &BTreeSet<String>,
    emit: EmitFn,
    _wrap_only: bool,
) -> Result<String, String> {
    let mut root: Value = match existing {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| e.to_string())?,
        _ => Value::Object(Map::new()),
    };
    if !root.is_object() {
        return Err("top-level JSON is not an object".into());
    }
    let obj = root.as_object_mut().unwrap();
    let section = obj
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !section.is_object() {
        return Err(format!("{key} is not an object"));
    }
    let section = section.as_object_mut().unwrap();

    const MANAGED: &[&str] = &[
        "type",
        "command",
        "args",
        "env",
        "environment",
        "url",
        "serverUrl",
        "headers",
        "enabled",
        "disabled",
    ];
    for (name, srv) in servers {
        let emitted = emit(srv);
        let mut merged = section
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in MANAGED {
            merged.remove(*key);
        }
        if let Some(fields) = emitted.as_object() {
            for (key, value) in fields {
                merged.insert(key.clone(), value.clone());
            }
        }
        section.insert(name.clone(), Value::Object(merged));
    }
    for name in remove {
        section.remove(name);
    }

    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

// JSON value builders ---------------------------------------------------------

fn str_map_to_value(m: &std::collections::BTreeMap<String, String>) -> Value {
    let mut o = Map::new();
    for (k, v) in m {
        o.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(o)
}

fn str_vec_to_value(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

fn value_to_str_map(v: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(s) = val.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

fn value_to_str_vec(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

// ───────────────────────── Claude ─────────────────────────
// stdio: {"type":"stdio","command","args","env"}  http: {"type":"http","url","headers"}

fn claude_parse(e: &Value) -> Option<McpServer> {
    let ty = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let is_http = e.get("url").is_some() || matches!(ty, "http" | "sse" | "streamable-http" | "ws");
    if is_http {
        let url = e.get("url")?.as_str()?.to_string();
        let mut s = McpServer::http(url);
        if !ty.is_empty() && ty != "http" {
            s.protocol_hint = Some(ty.to_string());
        }
        s.headers = value_to_str_map(e.get("headers"));
        Some(s)
    } else {
        let cmd = e.get("command")?.as_str()?.to_string();
        let mut s = McpServer::stdio(cmd, value_to_str_vec(e.get("args")));
        s.env = value_to_str_map(e.get("env"));
        Some(s)
    }
}

fn claude_emit(s: &McpServer) -> Value {
    let mut o = Map::new();
    match s.transport {
        Transport::Stdio => {
            o.insert("type".into(), Value::String("stdio".into()));
            if let Some(c) = &s.command {
                o.insert("command".into(), Value::String(c.clone()));
            }
            if !s.args.is_empty() {
                o.insert("args".into(), str_vec_to_value(&s.args));
            }
            if !s.env.is_empty() {
                o.insert("env".into(), str_map_to_value(&s.env));
            }
        }
        Transport::Http => {
            o.insert(
                "type".into(),
                Value::String(s.protocol_hint.clone().unwrap_or_else(|| "http".into())),
            );
            if let Some(u) = &s.url {
                o.insert("url".into(), Value::String(u.clone()));
            }
            if !s.headers.is_empty() {
                o.insert("headers".into(), str_map_to_value(&s.headers));
            }
        }
    }
    Value::Object(o)
}

// ───────────────────────── opencode ─────────────────────────
// local: {"type":"local","command":[cmd,...args],"environment","enabled"}
// remote:{"type":"remote","url","headers","enabled"}

fn opencode_parse(e: &Value) -> Option<McpServer> {
    let ty = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = e.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    if ty == "remote" || e.get("url").is_some() {
        let url = e.get("url")?.as_str()?.to_string();
        let mut s = McpServer::http(url);
        s.headers = value_to_str_map(e.get("headers"));
        s.enabled = enabled;
        Some(s)
    } else {
        let cmd_arr = value_to_str_vec(e.get("command"));
        let (cmd, args) = cmd_arr.split_first()?;
        let mut s = McpServer::stdio(cmd.clone(), args.to_vec());
        s.env = value_to_str_map(e.get("environment"));
        s.enabled = enabled;
        Some(s)
    }
}

fn opencode_emit(s: &McpServer) -> Value {
    let mut o = Map::new();
    match s.transport {
        Transport::Stdio => {
            o.insert("type".into(), Value::String("local".into()));
            let mut cmd = Vec::new();
            if let Some(c) = &s.command {
                cmd.push(c.clone());
            }
            cmd.extend(s.args.iter().cloned());
            o.insert("command".into(), str_vec_to_value(&cmd));
            if !s.env.is_empty() {
                o.insert("environment".into(), str_map_to_value(&s.env));
            }
        }
        Transport::Http => {
            o.insert("type".into(), Value::String("remote".into()));
            if let Some(u) = &s.url {
                o.insert("url".into(), Value::String(u.clone()));
            }
            if !s.headers.is_empty() {
                o.insert("headers".into(), str_map_to_value(&s.headers));
            }
        }
    }
    o.insert("enabled".into(), Value::Bool(s.enabled));
    Value::Object(o)
}

// ───────────────────────── Kiro ─────────────────────────
// local: {"command","args","env","disabled"}  remote:{"url","headers","disabled"}

fn kiro_parse(e: &Value) -> Option<McpServer> {
    let disabled = e.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
    if e.get("url").is_some() {
        let url = e.get("url")?.as_str()?.to_string();
        let mut s = McpServer::http(url);
        s.headers = value_to_str_map(e.get("headers"));
        s.enabled = !disabled;
        Some(s)
    } else {
        let cmd = e.get("command")?.as_str()?.to_string();
        let mut s = McpServer::stdio(cmd, value_to_str_vec(e.get("args")));
        s.env = value_to_str_map(e.get("env"));
        s.enabled = !disabled;
        Some(s)
    }
}

fn kiro_emit(s: &McpServer) -> Value {
    let mut o = Map::new();
    match s.transport {
        Transport::Stdio => {
            if let Some(c) = &s.command {
                o.insert("command".into(), Value::String(c.clone()));
            }
            if !s.args.is_empty() {
                o.insert("args".into(), str_vec_to_value(&s.args));
            }
            if !s.env.is_empty() {
                o.insert("env".into(), str_map_to_value(&s.env));
            }
        }
        Transport::Http => {
            if let Some(u) = &s.url {
                o.insert("url".into(), Value::String(u.clone()));
            }
            if !s.headers.is_empty() {
                o.insert("headers".into(), str_map_to_value(&s.headers));
            }
        }
    }
    if !s.enabled {
        o.insert("disabled".into(), Value::Bool(true));
    }
    Value::Object(o)
}

// ───────────────────────── Antigravity ─────────────────────────
// additionalProperties:false! Only emit allowed keys.
// local: {"command","args","env","disabled"}  remote:{"serverUrl","headers","disabled"}

fn antigravity_parse(e: &Value) -> Option<McpServer> {
    let disabled = e.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
    if e.get("serverUrl").is_some() {
        let url = e.get("serverUrl")?.as_str()?.to_string();
        let mut s = McpServer::http(url);
        s.headers = value_to_str_map(e.get("headers"));
        s.enabled = !disabled;
        Some(s)
    } else {
        let cmd = e.get("command")?.as_str()?.to_string();
        let mut s = McpServer::stdio(cmd, value_to_str_vec(e.get("args")));
        s.env = value_to_str_map(e.get("env"));
        s.enabled = !disabled;
        Some(s)
    }
}

fn antigravity_emit(s: &McpServer) -> Value {
    let mut o = Map::new();
    match s.transport {
        Transport::Stdio => {
            if let Some(c) = &s.command {
                o.insert("command".into(), Value::String(c.clone()));
            }
            if !s.args.is_empty() {
                o.insert("args".into(), str_vec_to_value(&s.args));
            }
            if !s.env.is_empty() {
                o.insert("env".into(), str_map_to_value(&s.env));
            }
        }
        Transport::Http => {
            if let Some(u) = &s.url {
                o.insert("serverUrl".into(), Value::String(u.clone()));
            }
            if !s.headers.is_empty() {
                o.insert("headers".into(), str_map_to_value(&s.headers));
            }
        }
    }
    if !s.enabled {
        o.insert("disabled".into(), Value::Bool(true));
    }
    Value::Object(o)
}

// ───────────────────────── Copilot ─────────────────────────
// Dedicated MCP file (~/.copilot/mcp-config.json), Claude-shaped `mcpServers`.
// local: {"type":"local","command","args","env"}  http:{"type":"http","url","headers"}
// (Copilot also accepts a `tools` filter per server; we leave it unset = all tools.)

fn copilot_parse(e: &Value) -> Option<McpServer> {
    let ty = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let is_http = e.get("url").is_some() || matches!(ty, "http" | "sse" | "streamable-http" | "ws");
    if is_http {
        let url = e.get("url")?.as_str()?.to_string();
        let mut s = McpServer::http(url);
        if !ty.is_empty() && ty != "http" {
            s.protocol_hint = Some(ty.to_string());
        }
        s.headers = value_to_str_map(e.get("headers"));
        Some(s)
    } else {
        let cmd = e.get("command")?.as_str()?.to_string();
        let mut s = McpServer::stdio(cmd, value_to_str_vec(e.get("args")));
        s.env = value_to_str_map(e.get("env"));
        Some(s)
    }
}

fn copilot_emit(s: &McpServer) -> Value {
    let mut o = Map::new();
    match s.transport {
        Transport::Stdio => {
            o.insert("type".into(), Value::String("local".into()));
            if let Some(c) = &s.command {
                o.insert("command".into(), Value::String(c.clone()));
            }
            if !s.args.is_empty() {
                o.insert("args".into(), str_vec_to_value(&s.args));
            }
            if !s.env.is_empty() {
                o.insert("env".into(), str_map_to_value(&s.env));
            }
        }
        Transport::Http => {
            o.insert(
                "type".into(),
                Value::String(s.protocol_hint.clone().unwrap_or_else(|| "http".into())),
            );
            if let Some(u) = &s.url {
                o.insert("url".into(), Value::String(u.clone()));
            }
            if !s.headers.is_empty() {
                o.insert("headers".into(), str_map_to_value(&s.headers));
            }
        }
    }
    Value::Object(o)
}

// ───────────────────────── Codex (TOML) ─────────────────────────

fn codex_read(text: &str) -> Result<McpMap, String> {
    use toml_edit::DocumentMut;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut out = McpMap::new();
    let Some(servers) = doc.get("mcp_servers").and_then(|i| i.as_table()) else {
        return Ok(out);
    };
    for (name, item) in servers.iter() {
        let Some(t) = item.as_table_like() else {
            continue;
        };
        let enabled = t.get("enabled").and_then(|i| i.as_bool()).unwrap_or(true);
        if let Some(url) = t.get("url").and_then(|i| i.as_str()) {
            let mut s = McpServer::http(url.to_string());
            s.headers = toml_table_to_map(t.get("http_headers"));
            s.enabled = enabled;
            out.insert(name.to_string(), s);
        } else if let Some(cmd) = t.get("command").and_then(|i| i.as_str()) {
            let args = t
                .get("args")
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut s = McpServer::stdio(cmd.to_string(), args);
            s.env = toml_table_to_map(t.get("env"));
            s.enabled = enabled;
            out.insert(name.to_string(), s);
        }
    }
    Ok(out)
}

fn toml_table_to_map(item: Option<&toml_edit::Item>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(t) = item.and_then(|i| i.as_table_like()) {
        for (k, v) in t.iter() {
            if let Some(s) = v.as_str() {
                out.insert(k.to_string(), s.to_string());
            }
        }
    }
    out
}

fn codex_write(
    existing: Option<&str>,
    servers: &McpMap,
    remove: &BTreeSet<String>,
) -> Result<String, String> {
    use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table, Value as TVal};

    let mut doc: DocumentMut = match existing {
        Some(t) if !t.trim().is_empty() => {
            t.parse().map_err(|e: toml_edit::TomlError| e.to_string())?
        }
        _ => DocumentMut::new(),
    };

    if !doc.contains_key("mcp_servers") {
        doc.insert("mcp_servers", Item::Table(Table::new()));
    }
    let root = doc["mcp_servers"]
        .as_table_mut()
        .ok_or("mcp_servers is not a table")?;
    root.set_implicit(true);

    let inline_map = |m: &std::collections::BTreeMap<String, String>| -> InlineTable {
        let mut it = InlineTable::new();
        for (k, v) in m {
            it.insert(k, TVal::from(v.clone()));
        }
        it
    };

    const MANAGED: &[&str] = &["command", "args", "env", "url", "http_headers", "enabled"];
    for (name, s) in servers {
        let mut t = root
            .get(name)
            .and_then(Item::as_table)
            .cloned()
            .unwrap_or_else(Table::new);
        for key in MANAGED {
            t.remove(key);
        }
        match s.transport {
            Transport::Stdio => {
                if let Some(c) = &s.command {
                    t.insert("command", value(c.clone()));
                }
                if !s.args.is_empty() {
                    let mut a = Array::new();
                    for arg in &s.args {
                        a.push(arg.clone());
                    }
                    t.insert("args", value(a));
                }
                if !s.env.is_empty() {
                    t.insert("env", Item::Value(TVal::InlineTable(inline_map(&s.env))));
                }
            }
            Transport::Http => {
                if let Some(u) = &s.url {
                    t.insert("url", value(u.clone()));
                }
                if !s.headers.is_empty() {
                    t.insert(
                        "http_headers",
                        Item::Value(TVal::InlineTable(inline_map(&s.headers))),
                    );
                }
            }
        }
        if !s.enabled {
            t.insert("enabled", value(false));
        }
        root.insert(name, Item::Table(t));
    }

    for name in remove {
        root.remove(name);
    }

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_map() -> McpMap {
        let mut map = McpMap::new();
        let mut local = McpServer::stdio("npx", vec!["-y".into(), "server".into()]);
        local.env.insert("TOKEN".into(), "secret".into());
        map.insert("local".into(), local);
        let mut remote = McpServer::http("https://example.test/mcp");
        remote
            .headers
            .insert("Authorization".into(), "Bearer secret".into());
        map.insert("remote".into(), remote);
        map
    }

    fn parse_rendered(cli: Cli, text: &str) -> McpMap {
        match cli {
            Cli::Codex => codex_read(text).unwrap(),
            Cli::Claude => json_read(text, "mcpServers", claude_parse).unwrap(),
            Cli::Opencode => json_read(text, "mcp", opencode_parse).unwrap(),
            Cli::Kiro => json_read(text, "mcpServers", kiro_parse).unwrap(),
            Cli::Antigravity => json_read(text, "mcpServers", antigravity_parse).unwrap(),
            Cli::Copilot => json_read(text, "mcpServers", copilot_parse).unwrap(),
        }
    }

    #[test]
    fn all_adapters_round_trip_representable_fields() {
        for cli in Cli::ALL {
            let map = sample_map();
            let rendered = render_mcp(cli, None, &map, &BTreeSet::new()).unwrap();
            assert_eq!(parse_rendered(cli, &rendered), map, "{}", cli.id());
        }
    }

    #[test]
    fn json_adapters_preserve_unknown_server_fields() {
        let cases = [
            (Cli::Claude, "mcpServers"),
            (Cli::Opencode, "mcp"),
            (Cli::Kiro, "mcpServers"),
            (Cli::Antigravity, "mcpServers"),
            (Cli::Copilot, "mcpServers"),
        ];
        for (cli, key) in cases {
            let existing = format!(
                r#"{{"unrelated":true,"{key}":{{"local":{{"command":"old","tools":["x"],"oauth":{{"mode":"device"}}}}}}}}"#
            );
            let rendered =
                render_mcp(cli, Some(&existing), &sample_map(), &BTreeSet::new()).unwrap();
            let root: Value = serde_json::from_str(&rendered).unwrap();
            let server = &root[key]["local"];
            assert_eq!(server["tools"], serde_json::json!(["x"]), "{}", cli.id());
            assert_eq!(server["oauth"]["mode"], "device", "{}", cli.id());
            assert_eq!(root["unrelated"], true, "{}", cli.id());
        }
    }

    #[test]
    fn codex_preserves_unknown_server_fields_and_comments() {
        let existing = "# keep me\n[mcp_servers.local]\ncommand = \"old\"\ntimeout_sec = 30\n";
        let rendered =
            render_mcp(Cli::Codex, Some(existing), &sample_map(), &BTreeSet::new()).unwrap();
        assert!(rendered.contains("# keep me"));
        assert!(rendered.contains("timeout_sec = 30"));
        assert_eq!(codex_read(&rendered).unwrap(), sample_map());
    }

    #[test]
    fn rejects_invalid_and_lossy_servers() {
        let mut missing_command = McpMap::new();
        let mut invalid = McpServer::stdio("", Vec::new());
        invalid.command = None;
        missing_command.insert("bad".into(), invalid);
        assert!(render_mcp(Cli::Codex, None, &missing_command, &BTreeSet::new()).is_err());

        let mut disabled = sample_map();
        disabled.get_mut("local").unwrap().enabled = false;
        assert!(render_mcp(Cli::Claude, None, &disabled, &BTreeSet::new())
            .unwrap_err()
            .contains("cannot represent disabled"));
        assert!(render_mcp(Cli::Copilot, None, &disabled, &BTreeSet::new()).is_err());
        assert!(render_mcp(Cli::Codex, None, &disabled, &BTreeSet::new()).is_ok());

        let specialized = json_read(
            r#"{"mcpServers":{"stream":{"type":"sse","url":"https://example.test/sse"}}}"#,
            "mcpServers",
            claude_parse,
        )
        .unwrap();
        assert_eq!(specialized["stream"].protocol_hint.as_deref(), Some("sse"));
        assert!(
            render_mcp(Cli::Claude, None, &specialized, &BTreeSet::new())
                .unwrap()
                .contains("\"sse\"")
        );
        assert!(
            render_mcp(Cli::Opencode, None, &specialized, &BTreeSet::new())
                .unwrap_err()
                .contains("cannot preserve MCP protocol")
        );
    }

    #[test]
    fn removal_only_removes_named_entries() {
        let existing =
            r#"{"mcpServers":{"remove":{"command":"x"},"keep":{"command":"y","tools":["z"]}}}"#;
        let mut remove = BTreeSet::new();
        remove.insert("remove".into());
        let rendered = render_mcp(Cli::Copilot, Some(existing), &McpMap::new(), &remove).unwrap();
        let root: Value = serde_json::from_str(&rendered).unwrap();
        assert!(root["mcpServers"].get("remove").is_none());
        assert_eq!(
            root["mcpServers"]["keep"]["tools"],
            serde_json::json!(["z"])
        );
    }

    #[test]
    fn malformed_native_fields_and_sections_fail_closed() {
        let invalid_env = r#"{"mcpServers":{"bad":{"command":"x","env":{"PORT":1234}}}}"#;
        assert!(json_read_checked(invalid_env, "mcpServers", claude_parse)
            .unwrap_err()
            .contains("non-string"));
        let invalid_section = r#"{"mcpServers":[]}"#;
        assert!(json_read_checked(invalid_section, "mcpServers", claude_parse).is_err());
        assert!(render_mcp(
            Cli::Claude,
            Some(invalid_section),
            &sample_map(),
            &BTreeSet::new()
        )
        .is_err());
    }
}
