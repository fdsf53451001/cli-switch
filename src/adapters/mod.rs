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
        Cli::Claude => json_read(&text, "mcpServers", claude_parse),
        Cli::Opencode => json_read(&text, "mcp", opencode_parse),
        Cli::Kiro => json_read(&text, "mcpServers", kiro_parse),
        Cli::Antigravity => json_read(&text, "mcpServers", antigravity_parse),
        Cli::Copilot => json_read(&text, "mcpServers", copilot_parse),
    }
    .map_err(|e| util::ctx(&path, e))
}

/// Upsert `servers` and delete `remove` in the CLI's native config.
pub fn write_mcp(cli: Cli, servers: &McpMap, remove: &BTreeSet<String>) -> R<()> {
    let path = paths::mcp_config(cli);
    let existing = util::read_to_string_opt(&path)?;
    let out = match cli {
        Cli::Codex => codex_write(existing.as_deref(), servers, remove)?,
        Cli::Claude => json_write(
            existing.as_deref(),
            "mcpServers",
            servers,
            remove,
            claude_emit,
            false,
        )?,
        Cli::Opencode => json_write(
            existing.as_deref(),
            "mcp",
            servers,
            remove,
            opencode_emit,
            false,
        )?,
        Cli::Kiro => json_write(
            existing.as_deref(),
            "mcpServers",
            servers,
            remove,
            kiro_emit,
            true,
        )?,
        Cli::Antigravity => json_write(
            existing.as_deref(),
            "mcpServers",
            servers,
            remove,
            antigravity_emit,
            true,
        )?,
        Cli::Copilot => json_write(
            existing.as_deref(),
            "mcpServers",
            servers,
            remove,
            copilot_emit,
            true,
        )?,
    };
    util::write_atomic(&path, &out)
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
        *section = Value::Object(Map::new());
    }
    let section = section.as_object_mut().unwrap();

    for (name, srv) in servers {
        section.insert(name.clone(), emit(srv));
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
            o.insert("type".into(), Value::String("http".into()));
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
            o.insert("type".into(), Value::String("http".into()));
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

    for (name, s) in servers {
        let mut t = Table::new();
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
