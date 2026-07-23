//! Reliability-first synchronization engine.
//!
//! Every source is compared with the last successful snapshot. A sync either
//! produces one deterministic plan and commits all of it, or writes nothing.

use crate::agents::{AgentDefinition, AgentRole, CapabilityPolicy, ModelIntent, NativeAgentFormat};
use crate::model::{Canonical, Cli, McpMap, McpServer};
use crate::util::{self, R};
use crate::{adapters, paths, store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBlob {
    #[serde(with = "util::base64_bytes")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub executable: bool,
}

pub type Tree = BTreeMap<String, FileBlob>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentSnapshot {
    #[serde(default)]
    pub mcp: McpMap,
    #[serde(default)]
    pub instructions: Option<Vec<u8>>,
    #[serde(default)]
    pub skills: BTreeMap<String, Tree>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentDefinition>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointSnapshot {
    #[serde(default)]
    pub mcp_initialized: bool,
    #[serde(default)]
    pub instructions_initialized: bool,
    #[serde(default)]
    pub skills_initialized: bool,
    #[serde(default)]
    pub agents_initialized: bool,
    #[serde(flatten)]
    pub content: ContentSnapshot,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default = "state_version")]
    pub version: u8,
    #[serde(default)]
    pub canonical: EndpointSnapshot,
    #[serde(default)]
    pub endpoints: BTreeMap<Cli, EndpointSnapshot>,
    #[serde(default)]
    pub last_transaction: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentScopeEndpoint {
    #[serde(default)]
    initialized: bool,
    #[serde(default)]
    agents: BTreeMap<String, AgentDefinition>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentScopeState {
    #[serde(default)]
    version: u8,
    #[serde(default)]
    canonical: AgentScopeEndpoint,
    #[serde(default)]
    endpoints: BTreeMap<Cli, AgentScopeEndpoint>,
}

fn state_version() -> u8 {
    2
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum UnitValue {
    Mcp(Option<McpServer>),
    Instructions(Option<Vec<u8>>),
    Skill(Option<Tree>),
    Agent(Option<AgentDefinition>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub source: String,
    pub value: UnitValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub scan_hash: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resolution {
    conflict_id: String,
    scan_hash: String,
    source: String,
    value: UnitValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "content", rename_all = "lowercase")]
enum Node {
    Absent,
    File(#[serde(with = "util::base64_bytes")] Vec<u8>),
    Dir(Tree),
    Symlink(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalEntry {
    path: PathBuf,
    before: Node,
    after_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    id: String,
    created_ms: u128,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone)]
struct Operation {
    path: PathBuf,
    after: Node,
    label: String,
}

pub struct Options {
    pub dry_run: bool,
    pub quiet: bool,
    pub prune: bool,
    pub allow_migration: bool,
}

pub struct Outcome {
    pub transaction: Option<String>,
    pub actions: Vec<String>,
    pub conflicts: Vec<ConflictRecord>,
    pub migration_required: bool,
}

pub fn last_transaction() -> R<Option<String>> {
    Ok(load_state()?.last_transaction)
}

/// Project agents have an independent snapshot so the same ID at global and
/// project scope is never merged. Writes still use the common transaction
/// journal, so `cli-switch rollback` covers them.
pub fn run_project_agents(active: &[Cli], opts: &Options) -> R<Outcome> {
    let state_path = paths::project_config_dir().join("agent-sync-state-v1.json");
    let state: AgentScopeState = match util::read_to_string_opt(&state_path)? {
        Some(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).map_err(|e| util::ctx(&state_path, e))?
        }
        _ => AgentScopeState::default(),
    };
    let canonical_now = read_canonical_agents(&paths::project_agents())?;
    let mut endpoint_now = BTreeMap::new();
    for &cli in active {
        let mut current = read_native_agents(cli, &paths::project_agents_dir(cli))?;
        inherit_other_agent_extensions(
            &mut current,
            state.endpoints.get(&cli).map(|s| &s.agents),
            agent_format(cli).namespace(),
        );
        endpoint_now.insert(cli, current);
    }
    let scan_hash = util::fingerprint(
        &serde_json::to_vec(&(&canonical_now, &endpoint_now)).map_err(|e| e.to_string())?,
    );
    let resolutions = load_resolutions()?;
    let mut desired = canonical_now.clone();
    let mut names = BTreeSet::new();
    names.extend(canonical_now.keys().cloned());
    names.extend(state.canonical.agents.keys().cloned());
    for agents in endpoint_now.values() {
        names.extend(agents.keys().cloned());
    }
    for endpoint in state.endpoints.values() {
        names.extend(endpoint.agents.keys().cloned());
    }
    let mut conflicts = Vec::new();
    for name in names {
        let sources = active
            .iter()
            .map(|cli| {
                let endpoint = state.endpoints.get(cli);
                (
                    cli.id().to_string(),
                    UnitValue::Agent(endpoint_now.get(cli).and_then(|m| m.get(&name)).cloned()),
                    UnitValue::Agent(endpoint.and_then(|e| e.agents.get(&name)).cloned()),
                    endpoint.map(|e| e.initialized).unwrap_or(false),
                )
            })
            .collect();
        match decide(
            ("project-agent", &name),
            UnitValue::Agent(canonical_now.get(&name).cloned()),
            UnitValue::Agent(state.canonical.agents.get(&name).cloned()),
            state.canonical.initialized,
            sources,
            &scan_hash,
            &resolutions,
        )? {
            Decision::Value(UnitValue::Agent(Some(agent))) => {
                desired.insert(name, agent);
            }
            Decision::Value(UnitValue::Agent(None)) => {
                desired.remove(&name);
            }
            Decision::Conflict(conflict) => conflicts.push(conflict),
            _ => unreachable!(),
        }
    }
    let project_skills = read_skills(&paths::project_root().join(".agents").join("skills"))?;
    let global_mcp = store::load_canonical()?.servers;
    validate_agents(&desired, &project_skills, &global_mcp)?;
    if !conflicts.is_empty() {
        if !opts.dry_run {
            save_conflicts(&conflicts)?;
        }
        return Ok(Outcome {
            transaction: None,
            actions: Vec::new(),
            conflicts,
            migration_required: false,
        });
    }

    let mut operations = Vec::new();
    for name in union_keys(&canonical_now, &desired) {
        operations.push(Operation {
            path: paths::project_agents().join(&name),
            after: desired
                .get(&name)
                .map(canonical_agent_tree)
                .transpose()?
                .map(Node::Dir)
                .unwrap_or(Node::Absent),
            label: format!("project canonical agent {name}"),
        });
    }
    for &cli in active {
        let current = endpoint_now.get(&cli).cloned().unwrap_or_default();
        let format = agent_format(cli);
        for name in union_keys(&current, &desired) {
            if format.is_reserved(&name) {
                continue;
            }
            operations.push(Operation {
                path: native_agent_path(cli, &paths::project_agents_dir(cli), &name),
                after: desired
                    .get(&name)
                    .map(|agent| format.render(agent).map(|s| Node::File(s.into_bytes())))
                    .transpose()?
                    .unwrap_or(Node::Absent),
                label: format!("project {} agent {name}", cli.id()),
            });
        }
    }
    let next_state = AgentScopeState {
        version: 1,
        canonical: AgentScopeEndpoint {
            initialized: true,
            agents: desired.clone(),
        },
        endpoints: active
            .iter()
            .map(|cli| {
                (
                    *cli,
                    AgentScopeEndpoint {
                        initialized: true,
                        agents: desired.clone(),
                    },
                )
            })
            .collect(),
    };
    operations.push(Operation {
        path: state_path,
        after: Node::File(serde_json::to_vec_pretty(&next_state).map_err(|e| e.to_string())?),
        label: "project agent state snapshot".into(),
    });
    let actions = operations
        .iter()
        .filter(|op| read_node(&op.path).ok().as_ref() != Some(&op.after))
        .map(|op| op.label.clone())
        .collect::<Vec<_>>();
    if opts.dry_run || actions.is_empty() {
        return Ok(Outcome {
            transaction: None,
            actions,
            conflicts: Vec::new(),
            migration_required: false,
        });
    }
    let id = transaction_id();
    apply_transaction_with_id(&operations, &id)?;
    clear_resolved_conflicts()?;
    retain_transactions(10)?;
    Ok(Outcome {
        transaction: Some(id),
        actions,
        conflicts: Vec::new(),
        migration_required: false,
    })
}

pub fn run(
    active: &[Cli],
    mcp: bool,
    instructions: bool,
    skills: bool,
    agents: bool,
    opts: &Options,
) -> R<Outcome> {
    let state = load_state()?;
    let mut canonical_now = read_canonical(agents)?;
    if !agents {
        canonical_now.agents = state.canonical.content.agents.clone();
    }
    let mut endpoint_now = BTreeMap::new();
    for &cli in active {
        let mut current = read_endpoint(cli, agents)?;
        if agents {
            inherit_other_agent_extensions(
                &mut current.agents,
                state.endpoints.get(&cli).map(|s| &s.content.agents),
                agent_format(cli).namespace(),
            );
        } else if let Some(previous) = state.endpoints.get(&cli) {
            current.agents = previous.content.agents.clone();
        }
        endpoint_now.insert(cli, current);
    }

    let legacy = detect_legacy(active, instructions, skills);
    if legacy && !opts.allow_migration {
        return Ok(Outcome {
            transaction: None,
            actions: vec!["legacy symlinks detected; run `cli-switch sync --migrate` to preview and confirm conversion to independent copies".into()],
            conflicts: Vec::new(),
            migration_required: true,
        });
    }

    let scan_hash = scan_fingerprint(&canonical_now, &endpoint_now)?;
    let resolutions = load_resolutions()?;
    let mut conflicts = Vec::new();
    let mut desired = canonical_now.clone();

    if mcp {
        let names = all_mcp_names(&state, &canonical_now, &endpoint_now);
        for name in names {
            let canon_current = canonical_now.mcp.get(&name).cloned();
            let canon_base = state.canonical.content.mcp.get(&name).cloned();
            let sources = active
                .iter()
                .map(|cli| {
                    let current = endpoint_now
                        .get(cli)
                        .and_then(|c| c.mcp.get(&name))
                        .cloned();
                    let base_state = state.endpoints.get(cli);
                    let base = base_state.and_then(|s| s.content.mcp.get(&name)).cloned();
                    (
                        *cli,
                        current,
                        base,
                        base_state.map(|s| s.mcp_initialized).unwrap_or(false),
                    )
                })
                .collect::<Vec<_>>();
            match decide(
                ("mcp", &name),
                UnitValue::Mcp(canon_current.clone()),
                UnitValue::Mcp(canon_base),
                state.canonical.mcp_initialized,
                sources
                    .into_iter()
                    .map(|(c, n, b, i)| (c.id().into(), UnitValue::Mcp(n), UnitValue::Mcp(b), i))
                    .collect(),
                &scan_hash,
                &resolutions,
            )? {
                Decision::Value(UnitValue::Mcp(Some(value))) => {
                    desired.mcp.insert(name, value);
                }
                Decision::Value(UnitValue::Mcp(None)) => {
                    if opts.prune {
                        desired.mcp.remove(&name);
                    }
                }
                Decision::Conflict(c) => conflicts.push(c),
                _ => unreachable!(),
            }
        }
    }

    if instructions {
        let sources = active
            .iter()
            .map(|cli| {
                let current = endpoint_now.get(cli).and_then(|c| c.instructions.clone());
                let base_state = state.endpoints.get(cli);
                let base = base_state.and_then(|s| s.content.instructions.clone());
                (
                    *cli,
                    current,
                    base,
                    base_state
                        .map(|s| s.instructions_initialized)
                        .unwrap_or(false),
                )
            })
            .collect::<Vec<_>>();
        match decide(
            ("instructions", "global"),
            UnitValue::Instructions(canonical_now.instructions.clone()),
            UnitValue::Instructions(state.canonical.content.instructions.clone()),
            state.canonical.instructions_initialized,
            sources
                .into_iter()
                .map(|(c, n, b, i)| {
                    (
                        c.id().into(),
                        UnitValue::Instructions(n),
                        UnitValue::Instructions(b),
                        i,
                    )
                })
                .collect(),
            &scan_hash,
            &resolutions,
        )? {
            Decision::Value(UnitValue::Instructions(v)) => desired.instructions = v,
            Decision::Conflict(c) => conflicts.push(c),
            _ => unreachable!(),
        }
    }

    if skills {
        let names = all_skill_names(&state, &canonical_now, &endpoint_now);
        for name in names {
            let sources = active
                .iter()
                .map(|cli| {
                    let current = endpoint_now
                        .get(cli)
                        .and_then(|c| c.skills.get(&name))
                        .cloned();
                    let base_state = state.endpoints.get(cli);
                    let base = base_state
                        .and_then(|s| s.content.skills.get(&name))
                        .cloned();
                    (
                        *cli,
                        current,
                        base,
                        base_state.map(|s| s.skills_initialized).unwrap_or(false),
                    )
                })
                .collect::<Vec<_>>();
            match decide(
                ("skill", &name),
                UnitValue::Skill(canonical_now.skills.get(&name).cloned()),
                UnitValue::Skill(state.canonical.content.skills.get(&name).cloned()),
                state.canonical.skills_initialized,
                sources
                    .into_iter()
                    .map(|(c, n, b, i)| {
                        (c.id().into(), UnitValue::Skill(n), UnitValue::Skill(b), i)
                    })
                    .collect(),
                &scan_hash,
                &resolutions,
            )? {
                Decision::Value(UnitValue::Skill(Some(tree))) => {
                    desired.skills.insert(name, tree);
                }
                Decision::Value(UnitValue::Skill(None)) => {
                    if opts.prune {
                        desired.skills.remove(&name);
                    }
                }
                Decision::Conflict(c) => conflicts.push(c),
                _ => unreachable!(),
            }
        }
    }

    if agents {
        let names = all_agent_names(&state, &canonical_now, &endpoint_now);
        for name in names {
            let sources = active
                .iter()
                .map(|cli| {
                    let current = endpoint_now
                        .get(cli)
                        .and_then(|c| c.agents.get(&name))
                        .cloned();
                    let base_state = state.endpoints.get(cli);
                    let base = base_state
                        .and_then(|s| s.content.agents.get(&name))
                        .cloned();
                    (
                        *cli,
                        current,
                        base,
                        base_state.map(|s| s.agents_initialized).unwrap_or(false),
                    )
                })
                .collect::<Vec<_>>();
            match decide(
                ("agent", &name),
                UnitValue::Agent(canonical_now.agents.get(&name).cloned()),
                UnitValue::Agent(state.canonical.content.agents.get(&name).cloned()),
                state.canonical.agents_initialized,
                sources
                    .into_iter()
                    .map(|(c, n, b, i)| {
                        (c.id().into(), UnitValue::Agent(n), UnitValue::Agent(b), i)
                    })
                    .collect(),
                &scan_hash,
                &resolutions,
            )? {
                Decision::Value(UnitValue::Agent(Some(agent))) => {
                    desired.agents.insert(name, agent);
                }
                // Agent deletion is intentionally snapshot-based, not --prune based.
                Decision::Value(UnitValue::Agent(None)) => {
                    desired.agents.remove(&name);
                }
                Decision::Conflict(c) => conflicts.push(c),
                _ => unreachable!(),
            }
        }
        validate_agents(&desired.agents, &desired.skills, &desired.mcp)?;
    }

    if !conflicts.is_empty() {
        if !opts.dry_run {
            save_conflicts(&conflicts)?;
        }
        return Ok(Outcome {
            transaction: None,
            actions: Vec::new(),
            conflicts,
            migration_required: false,
        });
    }

    let mut operations = build_operations(
        active,
        &canonical_now,
        &endpoint_now,
        &desired,
        mcp,
        instructions,
        skills,
        agents,
    )?;
    let mut next_state = state;
    next_state.version = 2;
    next_state.canonical = EndpointSnapshot {
        mcp_initialized: next_state.canonical.mcp_initialized || mcp,
        instructions_initialized: next_state.canonical.instructions_initialized || instructions,
        skills_initialized: next_state.canonical.skills_initialized || skills,
        agents_initialized: next_state.canonical.agents_initialized || agents,
        content: merged_snapshot(&canonical_now, &desired, mcp, instructions, skills, agents),
    };
    for &cli in active {
        let current = endpoint_now.get(&cli).cloned().unwrap_or_default();
        let previous = next_state.endpoints.get(&cli).cloned().unwrap_or_default();
        next_state.endpoints.insert(
            cli,
            EndpointSnapshot {
                mcp_initialized: previous.mcp_initialized || mcp,
                instructions_initialized: previous.instructions_initialized || instructions,
                skills_initialized: previous.skills_initialized || skills,
                agents_initialized: previous.agents_initialized || agents,
                content: merged_snapshot(&current, &desired, mcp, instructions, skills, agents),
            },
        );
    }

    let state_without_new_transaction =
        Node::File(serde_json::to_vec_pretty(&next_state).map_err(|e| e.to_string())?);
    let state_changed = read_node(&paths::state_v2())? != state_without_new_transaction;
    let mut actions = operations
        .iter()
        .filter(|op| read_node(&op.path).ok().as_ref() != Some(&op.after))
        .map(|op| op.label.clone())
        .collect::<Vec<_>>();
    if state_changed {
        actions.push("state snapshot".into());
    }
    if opts.dry_run || actions.is_empty() {
        return Ok(Outcome {
            transaction: None,
            actions,
            conflicts: Vec::new(),
            migration_required: false,
        });
    }

    let id = transaction_id();
    next_state.last_transaction = Some(id.clone());
    let state_bytes = serde_json::to_vec_pretty(&next_state).map_err(|e| e.to_string())?;
    operations.push(Operation {
        path: paths::state_v2(),
        after: Node::File(state_bytes),
        label: "state snapshot".into(),
    });
    apply_transaction_with_id(&operations, &id)?;
    clear_resolved_conflicts()?;
    retain_transactions(10)?;
    if !opts.quiet {
        // Caller prints the detailed action list.
    }
    Ok(Outcome {
        transaction: Some(id),
        actions,
        conflicts: Vec::new(),
        migration_required: false,
    })
}

fn merged_snapshot(
    current: &ContentSnapshot,
    desired: &ContentSnapshot,
    mcp: bool,
    instructions: bool,
    skills: bool,
    agents: bool,
) -> ContentSnapshot {
    ContentSnapshot {
        mcp: if mcp {
            desired.mcp.clone()
        } else {
            current.mcp.clone()
        },
        instructions: if instructions {
            desired.instructions.clone()
        } else {
            current.instructions.clone()
        },
        skills: if skills {
            desired.skills.clone()
        } else {
            current.skills.clone()
        },
        agents: if agents {
            desired.agents.clone()
        } else {
            current.agents.clone()
        },
    }
}

enum Decision {
    Value(UnitValue),
    Conflict(ConflictRecord),
}

fn decide(
    unit: (&str, &str),
    canonical_now: UnitValue,
    canonical_base: UnitValue,
    canonical_initialized: bool,
    endpoints: Vec<(String, UnitValue, UnitValue, bool)>,
    scan_hash: &str,
    resolutions: &[Resolution],
) -> R<Decision> {
    let (kind, name) = unit;
    let mut changed = Vec::new();
    if (canonical_initialized && canonical_now != canonical_base)
        || (!canonical_initialized && value_present(&canonical_now))
    {
        changed.push(Candidate {
            source: "canonical".into(),
            value: canonical_now.clone(),
        });
    }
    for (source, now, base, initialized) in endpoints {
        if (initialized && now != base) || (!initialized && value_present(&now)) {
            changed.push(Candidate { source, value: now });
        }
    }
    merge_compatible_agent_candidates(&mut changed);
    dedup_candidates(&mut changed);
    if changed.is_empty() {
        return Ok(Decision::Value(canonical_now));
    }
    if changed.iter().all(|c| c.value == changed[0].value) {
        return Ok(Decision::Value(changed[0].value.clone()));
    }
    let raw = serde_json::to_vec(&(kind, name, scan_hash, &changed)).map_err(|e| e.to_string())?;
    let id = format!("{}-{}-{}", kind, sanitize(name), util::fingerprint(&raw));
    if let Some(resolution) = resolutions
        .iter()
        .find(|r| r.conflict_id == id && r.scan_hash == scan_hash)
    {
        return Ok(Decision::Value(resolution.value.clone()));
    }
    Ok(Decision::Conflict(ConflictRecord {
        id,
        kind: kind.into(),
        name: name.into(),
        scan_hash: scan_hash.into(),
        candidates: changed,
    }))
}

fn value_present(value: &UnitValue) -> bool {
    match value {
        UnitValue::Mcp(v) => v.is_some(),
        UnitValue::Instructions(v) => v.as_ref().map(|b| !is_placeholder(b)).unwrap_or(false),
        UnitValue::Skill(v) => v.is_some(),
        UnitValue::Agent(v) => v.is_some(),
    }
}

fn dedup_candidates(values: &mut Vec<Candidate>) {
    let mut unique: Vec<Candidate> = Vec::new();
    for candidate in values.drain(..) {
        if let Some(existing) = unique.iter_mut().find(|c| c.value == candidate.value) {
            existing.source.push(',');
            existing.source.push_str(&candidate.source);
        } else {
            unique.push(candidate);
        }
    }
    *values = unique;
}

/// Equivalent portable definitions from different CLIs are one change even
/// though each carries a different namespaced native extension. Merge those
/// namespaces; conflicting values in the same namespace remain a conflict.
fn merge_compatible_agent_candidates(values: &mut Vec<Candidate>) {
    let mut merged: Vec<Candidate> = Vec::new();
    for candidate in values.drain(..) {
        let UnitValue::Agent(Some(agent)) = &candidate.value else {
            merged.push(candidate);
            continue;
        };
        let mut core = agent.clone();
        core.extensions.clear();
        let compatible = merged.iter().position(|existing| {
            let UnitValue::Agent(Some(other)) = &existing.value else {
                return false;
            };
            let mut other_core = other.clone();
            other_core.extensions.clear();
            if core != other_core {
                return false;
            }
            agent.extensions.iter().all(|(namespace, value)| {
                other
                    .extensions
                    .get(namespace)
                    .map(|existing| existing == value)
                    .unwrap_or(true)
            })
        });
        if let Some(index) = compatible {
            let existing = &mut merged[index];
            existing.source.push(',');
            existing.source.push_str(&candidate.source);
            if let UnitValue::Agent(Some(target)) = &mut existing.value {
                for (namespace, value) in &agent.extensions {
                    target
                        .extensions
                        .entry(namespace.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        } else {
            merged.push(candidate);
        }
    }
    *values = merged;
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn read_canonical(include_agents: bool) -> R<ContentSnapshot> {
    let canonical = store::load_canonical()?;
    Ok(ContentSnapshot {
        mcp: canonical.servers,
        instructions: fs::read(paths::store_instructions()).ok(),
        skills: read_skills(&paths::store_skills())?,
        agents: if include_agents {
            read_canonical_agents(&paths::store_agents())?
        } else {
            BTreeMap::new()
        },
    })
}

fn read_endpoint(cli: Cli, include_agents: bool) -> R<ContentSnapshot> {
    Ok(ContentSnapshot {
        mcp: adapters::read_mcp(cli)?,
        instructions: fs::read(paths::instructions_file(cli)).ok(),
        skills: read_skills(&paths::skills_dir(cli))?,
        agents: if include_agents {
            read_native_agents(cli, &paths::agents_dir(cli))?
        } else {
            BTreeMap::new()
        },
    })
}

fn agent_format(cli: Cli) -> NativeAgentFormat {
    match cli {
        Cli::Claude => NativeAgentFormat::Claude,
        Cli::Codex => NativeAgentFormat::Codex,
        Cli::Opencode => NativeAgentFormat::OpenCode,
        Cli::Kiro => NativeAgentFormat::Kiro,
        Cli::Antigravity => NativeAgentFormat::Agy,
        Cli::Copilot => NativeAgentFormat::Copilot,
    }
}

fn native_agent_path(cli: Cli, root: &Path, id: &str) -> PathBuf {
    match cli {
        Cli::Claude | Cli::Opencode => root.join(format!("{id}.md")),
        Cli::Codex => root.join(format!("{id}.toml")),
        Cli::Kiro => root.join(format!("{id}.json")),
        Cli::Copilot => root.join(format!("{id}.agent.md")),
        Cli::Antigravity => root.join(id).join("agent.json"),
    }
}

fn read_native_agents(cli: Cli, root: &Path) -> R<BTreeMap<String, AgentDefinition>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(out);
    };
    let format = agent_format(cli);
    for entry in entries {
        let entry = entry.map_err(|e| util::ctx(root, e))?;
        let path = entry.path();
        let id = if cli == Cli::Antigravity {
            if !path.is_dir() || !path.join("agent.json").is_file() {
                continue;
            }
            entry.file_name().to_string_lossy().to_string()
        } else {
            if !path.is_file() {
                continue;
            }
            let file = entry.file_name().to_string_lossy().to_string();
            match cli {
                Cli::Claude | Cli::Opencode => file.strip_suffix(".md"),
                Cli::Codex => file.strip_suffix(".toml"),
                Cli::Kiro => file.strip_suffix(".json"),
                Cli::Copilot => file.strip_suffix(".agent.md"),
                Cli::Antigravity => unreachable!(),
            }
            .map(str::to_string)
            .unwrap_or_default()
        };
        if id.is_empty() || id.starts_with('.') || format.is_reserved(&id) {
            continue;
        }
        let file = if cli == Cli::Antigravity {
            path.join("agent.json")
        } else {
            path
        };
        let source = fs::read_to_string(&file).map_err(|e| util::ctx(&file, e))?;
        let mut agent = format
            .parse(&id, &source)
            .map_err(|e| format!("invalid {} agent {}: {e}", cli.id(), file.display()))?;
        let identity = match cli {
            Cli::Claude | Cli::Codex => agent.name.clone(),
            _ => id.clone(),
        };
        if !valid_agent_id(&identity) {
            return Err(format!(
                "invalid {} agent identity `{identity}` in {}",
                cli.id(),
                file.display()
            ));
        }
        if cli == Cli::Antigravity && agent.name != id {
            return Err(format!(
                "agy agent directory `{id}` does not match JSON name `{}`",
                agent.name
            ));
        }
        if format.is_reserved(&identity) {
            continue;
        }
        agent.id = identity.clone();
        if out.insert(identity.clone(), agent).is_some() {
            return Err(format!(
                "duplicate {} agent identity `{identity}`",
                cli.id()
            ));
        }
    }
    Ok(out)
}

fn inherit_other_agent_extensions(
    current: &mut BTreeMap<String, AgentDefinition>,
    baseline: Option<&BTreeMap<String, AgentDefinition>>,
    own_namespace: &str,
) {
    let Some(baseline) = baseline else { return };
    for (id, agent) in current {
        let Some(previous) = baseline.get(id) else {
            continue;
        };
        for (namespace, value) in &previous.extensions {
            if namespace != own_namespace {
                agent
                    .extensions
                    .entry(namespace.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
}

fn read_canonical_agents(root: &Path) -> R<BTreeMap<String, AgentDefinition>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(out);
    };
    for entry in entries {
        let entry = entry.map_err(|e| util::ctx(root, e))?;
        let dir = entry.path();
        if !dir.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let meta_path = dir.join("agent.toml");
        let prompt_path = dir.join("prompt.md");
        if !meta_path.is_file() || !prompt_path.is_file() {
            return Err(format!(
                "canonical agent {id} must contain agent.toml and prompt.md"
            ));
        }
        let text = fs::read_to_string(&meta_path).map_err(|e| util::ctx(&meta_path, e))?;
        let doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| util::ctx(&meta_path, e))?;
        let string = |key: &str| doc.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let list = |key: &str| {
            doc.get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut permissions = crate::agents::AgentPermissions::default();
        if let Some(table) = doc.get("permissions").and_then(|v| v.as_table_like()) {
            for (key, value) in table.iter() {
                let policy = match value.as_str() {
                    Some("allow") => CapabilityPolicy::Allow,
                    Some("ask") => CapabilityPolicy::Ask,
                    Some("deny") => CapabilityPolicy::Deny,
                    _ => {
                        return Err(format!(
                            "invalid permission {key} in {}",
                            meta_path.display()
                        ))
                    }
                };
                permissions.capabilities.insert(key.into(), policy);
            }
        }
        let mut extensions = BTreeMap::new();
        let ext_dir = dir.join("extensions");
        if let Ok(exts) = fs::read_dir(&ext_dir) {
            for ext in exts {
                let ext = ext.map_err(|e| util::ctx(&ext_dir, e))?;
                let path = ext.path();
                let Some(namespace) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let value =
                    serde_json::from_slice(&fs::read(&path).map_err(|e| util::ctx(&path, e))?)
                        .map_err(|e| util::ctx(&path, e))?;
                extensions.insert(namespace.into(), value);
            }
        }
        out.insert(
            id.clone(),
            AgentDefinition {
                id: string("id").unwrap_or(id),
                name: string("name")
                    .ok_or_else(|| format!("missing name in {}", meta_path.display()))?,
                description: string("description").unwrap_or_default(),
                instructions: fs::read_to_string(&prompt_path)
                    .map_err(|e| util::ctx(&prompt_path, e))?,
                role: match string("role").as_deref() {
                    Some("primary") => AgentRole::Primary,
                    Some("subagent") => AgentRole::Subagent,
                    _ => AgentRole::Either,
                },
                model: match string("model").as_deref() {
                    Some("fast") => ModelIntent::Fast,
                    Some("balanced") => ModelIntent::Balanced,
                    Some("strong") => ModelIntent::Strong,
                    _ => ModelIntent::Inherit,
                },
                permissions,
                skills: list("skills"),
                mcp_servers: list("mcp_servers"),
                extensions,
            },
        );
    }
    Ok(out)
}

fn canonical_agent_tree(agent: &AgentDefinition) -> R<Tree> {
    let quoted = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
    let array = |values: &[String]| {
        values
            .iter()
            .map(|v| quoted(v))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let role = match agent.role {
        AgentRole::Primary => "primary",
        AgentRole::Subagent => "subagent",
        AgentRole::Either => "either",
    };
    let model = match agent.model {
        ModelIntent::Inherit => "inherit",
        ModelIntent::Fast => "fast",
        ModelIntent::Balanced => "balanced",
        ModelIntent::Strong => "strong",
    };
    let mut meta = format!(
        "id = {}\nname = {}\ndescription = {}\nrole = {}\nmodel = {}\nskills = [{}]\nmcp_servers = [{}]\n",
        quoted(&agent.id), quoted(&agent.name), quoted(&agent.description), quoted(role), quoted(model),
        array(&agent.skills), array(&agent.mcp_servers)
    );
    if !agent.permissions.capabilities.is_empty() {
        meta.push_str("\n[permissions]\n");
        for (key, policy) in &agent.permissions.capabilities {
            let policy = match policy {
                CapabilityPolicy::Deny => "deny",
                CapabilityPolicy::Ask => "ask",
                CapabilityPolicy::Allow => "allow",
            };
            meta.push_str(&format!("{} = {}\n", quoted(key), quoted(policy)));
        }
    }
    let mut tree = Tree::new();
    tree.insert(
        "agent.toml".into(),
        FileBlob {
            data: meta.into_bytes(),
            executable: false,
        },
    );
    tree.insert(
        "prompt.md".into(),
        FileBlob {
            data: agent.instructions.as_bytes().to_vec(),
            executable: false,
        },
    );
    for (namespace, value) in &agent.extensions {
        tree.insert(
            format!("extensions/{namespace}.json"),
            FileBlob {
                data: serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
                executable: false,
            },
        );
    }
    Ok(tree)
}

fn validate_agents(
    agents: &BTreeMap<String, AgentDefinition>,
    skills: &BTreeMap<String, Tree>,
    mcp: &McpMap,
) -> R<()> {
    for (id, agent) in agents {
        if id != &agent.id || !valid_agent_id(id) {
            return Err(format!("invalid canonical agent id `{id}`"));
        }
        for skill in &agent.skills {
            if !skills.contains_key(skill) {
                return Err(format!(
                    "agent `{id}` references missing canonical skill `{skill}`"
                ));
            }
        }
        for server in &agent.mcp_servers {
            if !mcp.contains_key(server) {
                return Err(format!(
                    "agent `{id}` references missing canonical MCP server `{server}`"
                ));
            }
        }
    }
    Ok(())
}

fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn read_skills(root: &Path) -> R<BTreeMap<String, Tree>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(out);
    };
    for entry in entries {
        let entry = entry.map_err(|e| util::ctx(root, e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || !entry.path().is_dir()
            || !entry.path().join("SKILL.md").exists()
        {
            continue;
        }
        out.insert(name, read_tree(&entry.path())?);
    }
    Ok(out)
}

fn read_tree(root: &Path) -> R<Tree> {
    fn insert_file(root: &Path, path: &Path, source: &Path, out: &mut Tree) -> R<()> {
        let rel = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(source).map_err(|e| util::ctx(source, e))?;
            meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        out.insert(
            rel,
            FileBlob {
                data: fs::read(source).map_err(|e| util::ctx(source, e))?,
                executable,
            },
        );
        Ok(())
    }

    fn walk(root: &Path, canonical_root: &Path, dir: &Path, out: &mut Tree) -> R<()> {
        let mut entries = fs::read_dir(dir)
            .map_err(|e| util::ctx(dir, e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| util::ctx(dir, e))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            // Skills are sometimes authored inside a git checkout. `.git`
            // holds no skill content but can be arbitrarily large (packed
            // objects, history) and would otherwise be vendored into the
            // canonical store and every CLI endpoint's snapshot.
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| util::ctx(&path, e))?;
            if meta.file_type().is_symlink() {
                let target = fs::canonicalize(&path).map_err(|e| util::ctx(&path, e))?;
                if !target.starts_with(canonical_root) {
                    return Err(format!(
                        "nested symlink escapes skill directory: {} -> {}",
                        path.display(),
                        target.display()
                    ));
                }
                if !target.is_file() {
                    return Err(format!(
                        "nested directory symlink is not supported in skill {}",
                        path.display()
                    ));
                }
                // Materialize internal file links as regular files. This keeps
                // skills portable across CLIs and avoids recreating link cycles.
                insert_file(root, &path, &target, out)?;
            } else if meta.is_dir() {
                walk(root, canonical_root, &path, out)?;
            } else if meta.is_file() {
                insert_file(root, &path, &path, out)?;
            }
        }
        Ok(())
    }
    let mut out = Tree::new();
    let canonical_root = fs::canonicalize(root).map_err(|e| util::ctx(root, e))?;
    walk(root, &canonical_root, root, &mut out)?;
    Ok(out)
}

fn all_mcp_names(
    state: &SyncState,
    canonical: &ContentSnapshot,
    endpoints: &BTreeMap<Cli, ContentSnapshot>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    names.extend(canonical.mcp.keys().cloned());
    names.extend(state.canonical.content.mcp.keys().cloned());
    for v in endpoints.values() {
        names.extend(v.mcp.keys().cloned());
    }
    for v in state.endpoints.values() {
        names.extend(v.content.mcp.keys().cloned());
    }
    names
}

fn all_skill_names(
    state: &SyncState,
    canonical: &ContentSnapshot,
    endpoints: &BTreeMap<Cli, ContentSnapshot>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    names.extend(canonical.skills.keys().cloned());
    names.extend(state.canonical.content.skills.keys().cloned());
    for v in endpoints.values() {
        names.extend(v.skills.keys().cloned());
    }
    for v in state.endpoints.values() {
        names.extend(v.content.skills.keys().cloned());
    }
    names
}

fn all_agent_names(
    state: &SyncState,
    canonical: &ContentSnapshot,
    endpoints: &BTreeMap<Cli, ContentSnapshot>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    names.extend(canonical.agents.keys().cloned());
    names.extend(state.canonical.content.agents.keys().cloned());
    for value in endpoints.values() {
        names.extend(value.agents.keys().cloned());
    }
    for value in state.endpoints.values() {
        names.extend(value.content.agents.keys().cloned());
    }
    names
}

#[allow(clippy::too_many_arguments)]
fn build_operations(
    active: &[Cli],
    canonical_now: &ContentSnapshot,
    endpoint_now: &BTreeMap<Cli, ContentSnapshot>,
    desired: &ContentSnapshot,
    mcp: bool,
    instructions: bool,
    skills: bool,
    agents: bool,
) -> R<Vec<Operation>> {
    let mut ops = Vec::new();
    if mcp {
        let canonical = Canonical {
            servers: desired.mcp.clone(),
        };
        ops.push(Operation {
            path: paths::store_mcp(),
            after: Node::File(serde_json::to_vec_pretty(&canonical).map_err(|e| e.to_string())?),
            label: "canonical MCP".into(),
        });
    }
    if instructions {
        ops.push(Operation {
            path: paths::store_instructions(),
            after: desired
                .instructions
                .clone()
                .map(Node::File)
                .unwrap_or(Node::Absent),
            label: "canonical instructions".into(),
        });
    }
    if skills {
        for name in union_keys(&canonical_now.skills, &desired.skills) {
            ops.push(Operation {
                path: paths::store_skills().join(&name),
                after: desired
                    .skills
                    .get(&name)
                    .cloned()
                    .map(Node::Dir)
                    .unwrap_or(Node::Absent),
                label: format!("canonical skill {name}"),
            });
        }
    }
    if agents {
        for name in union_keys(&canonical_now.agents, &desired.agents) {
            ops.push(Operation {
                path: paths::store_agents().join(&name),
                after: desired
                    .agents
                    .get(&name)
                    .map(canonical_agent_tree)
                    .transpose()?
                    .map(Node::Dir)
                    .unwrap_or(Node::Absent),
                label: format!("canonical agent {name}"),
            });
        }
    }
    for &cli in active {
        if mcp {
            let existing = util::read_to_string_opt(&paths::mcp_config(cli))?;
            let existing_names: BTreeSet<String> = endpoint_now
                .get(&cli)
                .map(|c| c.mcp.keys().cloned().collect())
                .unwrap_or_default();
            let desired_names: BTreeSet<String> = desired.mcp.keys().cloned().collect();
            let remove = existing_names.difference(&desired_names).cloned().collect();
            let rendered = adapters::render_mcp(cli, existing.as_deref(), &desired.mcp, &remove)?;
            ops.push(Operation {
                path: paths::mcp_config(cli),
                after: Node::File(rendered.into_bytes()),
                label: format!("{} MCP", cli.id()),
            });
        }
        if instructions {
            ops.push(Operation {
                path: paths::instructions_file(cli),
                after: desired
                    .instructions
                    .clone()
                    .map(Node::File)
                    .unwrap_or(Node::Absent),
                label: format!("{} instructions", cli.id()),
            });
        }
        if skills {
            let current = endpoint_now
                .get(&cli)
                .map(|c| &c.skills)
                .cloned()
                .unwrap_or_default();
            for name in union_keys(&current, &desired.skills) {
                if paths::skills_reserved(cli).contains(&name.as_str()) {
                    continue;
                }
                ops.push(Operation {
                    path: paths::skills_dir(cli).join(&name),
                    after: desired
                        .skills
                        .get(&name)
                        .cloned()
                        .map(Node::Dir)
                        .unwrap_or(Node::Absent),
                    label: format!("{} skill {}", cli.id(), name),
                });
            }
        }
        if agents {
            let current = endpoint_now
                .get(&cli)
                .map(|c| &c.agents)
                .cloned()
                .unwrap_or_default();
            let format = agent_format(cli);
            for name in union_keys(&current, &desired.agents) {
                if format.is_reserved(&name) {
                    continue;
                }
                let path = native_agent_path(cli, &paths::agents_dir(cli), &name);
                let after = match desired.agents.get(&name) {
                    Some(agent) => {
                        let rendered = format.render(agent)?.into_bytes();
                        Node::File(rendered)
                    }
                    None => Node::Absent,
                };
                ops.push(Operation {
                    path,
                    after,
                    label: format!("{} agent {}", cli.id(), name),
                });
            }
        }
    }
    Ok(ops)
}

fn union_keys<T>(a: &BTreeMap<String, T>, b: &BTreeMap<String, T>) -> BTreeSet<String> {
    a.keys().chain(b.keys()).cloned().collect()
}

fn load_state() -> R<SyncState> {
    match util::read_to_string_opt(&paths::state_v2())? {
        Some(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).map_err(|e| util::ctx(&paths::state_v2(), e))
        }
        _ => Ok(SyncState {
            version: 2,
            ..SyncState::default()
        }),
    }
}

fn scan_fingerprint(
    canonical: &ContentSnapshot,
    endpoints: &BTreeMap<Cli, ContentSnapshot>,
) -> R<String> {
    let bytes = serde_json::to_vec(&(canonical, endpoints)).map_err(|e| e.to_string())?;
    Ok(util::fingerprint(&bytes))
}

fn conflict_path(id: &str) -> PathBuf {
    paths::pending_conflicts().join(format!("{id}.json"))
}
fn resolution_path(id: &str) -> PathBuf {
    paths::pending_conflicts().join(format!("{id}.resolution.json"))
}

fn save_conflicts(conflicts: &[ConflictRecord]) -> R<()> {
    util::ensure_private_dir(&paths::pending_conflicts())?;
    let keep = conflicts
        .iter()
        .map(|c| c.id.as_str())
        .collect::<BTreeSet<_>>();
    if let Ok(entries) = fs::read_dir(paths::pending_conflicts()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".resolution.json") {
                continue;
            }
            let id = name.strip_suffix(".json").unwrap_or(&name);
            if !keep.contains(id) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    for conflict in conflicts {
        util::write_private(
            &conflict_path(&conflict.id),
            &serde_json::to_vec_pretty(conflict).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(())
}

fn load_resolutions() -> R<Vec<Resolution>> {
    let Ok(entries) = fs::read_dir(paths::pending_conflicts()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .ends_with(".resolution.json")
        {
            continue;
        }
        let bytes = fs::read(entry.path()).map_err(|e| util::ctx(&entry.path(), e))?;
        out.push(serde_json::from_slice(&bytes).map_err(|e| util::ctx(&entry.path(), e))?);
    }
    Ok(out)
}

pub fn list_conflicts() -> R<Vec<ConflictRecord>> {
    let Ok(entries) = fs::read_dir(paths::pending_conflicts()) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<ConflictRecord> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.ends_with(".resolution.json") {
            continue;
        }
        let bytes = fs::read(entry.path()).map_err(|e| util::ctx(&entry.path(), e))?;
        out.push(serde_json::from_slice(&bytes).map_err(|e| util::ctx(&entry.path(), e))?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

pub fn public_conflict(record: &ConflictRecord) -> Value {
    let candidates = record.candidates.iter().map(|candidate| {
        let summary = match &candidate.value {
            UnitValue::Mcp(None) | UnitValue::Instructions(None) | UnitValue::Skill(None) | UnitValue::Agent(None) => json!({"deleted": true}),
            UnitValue::Mcp(Some(server)) => json!({
                "transport": server.transport,
                "protocol_hint": server.protocol_hint,
                "command": server.command,
                "args": server.args,
                "env": server.env.keys().map(|k| (k.clone(), "***")).collect::<BTreeMap<_,_>>(),
                "url": server.url,
                "headers": server.headers.keys().map(|k| (k.clone(), "***")).collect::<BTreeMap<_,_>>(),
                "enabled": server.enabled,
            }),
            UnitValue::Instructions(Some(bytes)) => json!({"bytes": bytes.len(), "hash": util::fingerprint(bytes), "preview": String::from_utf8_lossy(bytes).chars().take(500).collect::<String>()}),
            UnitValue::Skill(Some(tree)) => json!({"files": tree.keys().collect::<Vec<_>>(), "hash": tree_hash(tree)}),
            UnitValue::Agent(Some(agent)) => json!({
                "name": agent.name,
                "description": agent.description,
                "role": agent.role,
                "model": agent.model,
                "skills": agent.skills,
                "mcp_servers": agent.mcp_servers,
                "prompt_hash": util::fingerprint(agent.instructions.as_bytes()),
                "prompt_preview": agent.instructions.chars().take(500).collect::<String>(),
            }),
        };
        json!({"source": candidate.source, "summary": summary})
    }).collect::<Vec<_>>();
    json!({"id": record.id, "kind": record.kind, "name": record.name, "candidates": candidates})
}

pub fn resolve_conflict(id: &str, source: &str) -> R<()> {
    let path = conflict_path(id);
    let bytes = fs::read(&path).map_err(|e| util::ctx(&path, e))?;
    let record: ConflictRecord = serde_json::from_slice(&bytes).map_err(|e| util::ctx(&path, e))?;
    let candidate = record
        .candidates
        .iter()
        .find(|c| c.source == source || c.source.split(',').any(|s| s == source))
        .ok_or_else(|| format!("source '{source}' is not a candidate for conflict {id}"))?;
    let resolution = Resolution {
        conflict_id: record.id,
        scan_hash: record.scan_hash,
        source: source.into(),
        value: candidate.value.clone(),
    };
    util::write_private(
        &resolution_path(id),
        &serde_json::to_vec_pretty(&resolution).map_err(|e| e.to_string())?,
    )
}

fn clear_resolved_conflicts() -> R<()> {
    let Ok(entries) = fs::read_dir(paths::pending_conflicts()) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let _ = fs::remove_file(entry.path());
    }
    Ok(())
}

fn tree_hash(tree: &Tree) -> String {
    util::fingerprint(&serde_json::to_vec(tree).unwrap_or_default())
}

fn detect_legacy(active: &[Cli], instructions: bool, skills: bool) -> bool {
    active.iter().any(|&cli| {
        (instructions
            && fs::symlink_metadata(paths::instructions_file(cli))
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false))
            || (skills
                && fs::read_dir(paths::skills_dir(cli))
                    .map(|rd| {
                        rd.flatten().any(|e| {
                            fs::symlink_metadata(e.path())
                                .map(|m| m.file_type().is_symlink())
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false))
    })
}

fn is_placeholder(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).contains("single source of truth synced to every CLI")
}

fn read_node(path: &Path) -> R<Node> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Node::Absent),
        Err(e) => return Err(util::ctx(path, e)),
    };
    if meta.file_type().is_symlink() {
        return fs::read_link(path)
            .map(Node::Symlink)
            .map_err(|e| util::ctx(path, e));
    }
    if meta.is_file() {
        return fs::read(path)
            .map(Node::File)
            .map_err(|e| util::ctx(path, e));
    }
    if meta.is_dir() {
        return read_tree(path).map(Node::Dir);
    }
    Err(format!("unsupported filesystem object: {}", path.display()))
}

fn remove_node(path: &Path) -> R<()> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path).map_err(|e| util::ctx(path, e))
    } else if meta.is_dir() {
        fs::remove_dir_all(path).map_err(|e| util::ctx(path, e))
    } else {
        Err(format!("unsupported filesystem object: {}", path.display()))
    }
}

fn write_node(path: &Path, node: &Node, token: &str) -> R<()> {
    match node {
        Node::Absent => remove_node(path),
        Node::File(bytes) => {
            if !matches!(read_node(path)?, Node::Absent | Node::File(_)) {
                remove_node(path)?;
            }
            if path.starts_with(paths::store_root()) {
                util::write_private(path, bytes)
            } else {
                util::write_atomic_bytes(path, bytes)
            }
        }
        Node::Dir(tree) => {
            util::ensure_parent(path)?;
            let stage = path.with_extension(format!("cli-switch-stage-{token}"));
            remove_node(&stage)?;
            util::ensure_dir(&stage)?;
            for (rel, blob) in tree {
                let target = stage.join(rel);
                util::write_atomic_bytes(&target, &blob.data)?;
                #[cfg(unix)]
                if blob.executable {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                        .map_err(|e| util::ctx(&target, e))?;
                }
            }
            remove_node(path)?;
            fs::rename(&stage, path).map_err(|e| util::ctx(path, e))
        }
        Node::Symlink(target) => {
            remove_node(path)?;
            util::ensure_parent(path)?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, path).map_err(|e| util::ctx(path, e))
            }
            #[cfg(windows)]
            {
                if target.is_dir() {
                    std::os::windows::fs::symlink_dir(target, path).map_err(|e| util::ctx(path, e))
                } else {
                    std::os::windows::fs::symlink_file(target, path).map_err(|e| util::ctx(path, e))
                }
            }
        }
    }
}

fn node_hash(node: &Node) -> String {
    util::fingerprint(&serde_json::to_vec(node).unwrap_or_default())
}

/// A transaction that fails partway leaves its journal directory behind
/// unless we clean it up here: callers only prune old transactions
/// (`retain_transactions`) after a successful apply, so an error returned
/// from this function would otherwise leak the journal it just wrote —
/// forever, since the transaction can never succeed on retry with the same id.
fn apply_transaction_with_id(operations: &[Operation], id: &str) -> R<()> {
    let tx_dir = paths::transactions().join(id);
    let result = apply_transaction_inner(operations, id, &tx_dir);
    if result.is_err() {
        let _ = fs::remove_dir_all(&tx_dir);
    }
    result
}

fn apply_transaction_inner(operations: &[Operation], id: &str, tx_dir: &Path) -> R<()> {
    let mut entries = Vec::new();
    for op in operations {
        entries.push(JournalEntry {
            path: op.path.clone(),
            before: read_node(&op.path)?,
            after_hash: node_hash(&op.after),
        });
    }
    let journal = Journal {
        id: id.to_string(),
        created_ms: util::now_millis(),
        entries,
    };
    let journal_path = tx_dir.join("journal.json");
    util::ensure_private_dir(tx_dir)?;
    util::write_private(
        &journal_path,
        &serde_json::to_vec_pretty(&journal).map_err(|e| e.to_string())?,
    )?;
    let fail_after = if cfg!(debug_assertions) {
        std::env::var("CLI_SWITCH_TEST_FAIL_AFTER")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
    } else {
        None
    };
    for (index, op) in operations.iter().enumerate() {
        if read_node(&op.path)? == op.after {
            continue;
        }
        let result = if fail_after == Some(index) {
            Err(format!("injected failure before operation {index}"))
        } else {
            write_node(&op.path, &op.after, &format!("{id}-{index}"))
        };
        if let Err(error) = result {
            for entry in journal.entries.iter().take(index + 1).rev() {
                let _ = write_node(&entry.path, &entry.before, &format!("rollback-{id}"));
            }
            return Err(format!(
                "transaction {id} failed and was rolled back: {error}"
            ));
        }
    }
    Ok(())
}

pub fn rollback(id: &str) -> R<String> {
    let path = paths::transactions().join(id).join("journal.json");
    let bytes = fs::read(&path).map_err(|e| util::ctx(&path, e))?;
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|e| util::ctx(&path, e))?;
    for entry in &journal.entries {
        let current = read_node(&entry.path)?;
        if node_hash(&current) != entry.after_hash {
            return Err(format!(
                "refusing rollback: {} changed after transaction {}",
                entry.path.display(),
                id
            ));
        }
    }
    let new_id = transaction_id();
    let mut ops = journal
        .entries
        .iter()
        .map(|entry| Operation {
            path: entry.path.clone(),
            after: entry.before.clone(),
            label: format!("restore {}", entry.path.display()),
        })
        .collect::<Vec<_>>();
    if let Some(state_op) = ops.iter_mut().find(|op| op.path == paths::state_v2()) {
        if let Node::File(bytes) = &state_op.after {
            if let Ok(mut state) = serde_json::from_slice::<SyncState>(bytes) {
                state.last_transaction = Some(new_id.clone());
                state_op.after =
                    Node::File(serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?);
            }
        }
    }
    apply_transaction_with_id(&ops, &new_id)?;
    retain_transactions(10)?;
    Ok(new_id)
}

fn retain_transactions(keep: usize) -> R<()> {
    let Ok(entries) = fs::read_dir(paths::transactions()) else {
        return Ok(());
    };
    let mut dirs = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect::<Vec<_>>();
    dirs.sort_by_key(|e| e.file_name());
    let remove_count = dirs.len().saturating_sub(keep);
    for entry in dirs.into_iter().take(remove_count) {
        fs::remove_dir_all(entry.path()).map_err(|e| util::ctx(&entry.path(), e))?;
    }
    Ok(())
}

fn transaction_id() -> String {
    format!("tx-{}-{}", util::now_millis(), std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn skill_tree_materializes_internal_file_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "cli-switch-symlink-test-{}-{}",
            std::process::id(),
            util::now_millis()
        ));
        fs::create_dir_all(root.join("plugins/demo")).unwrap();
        fs::write(root.join("SKILL.md"), b"# demo\n").unwrap();
        symlink("../../SKILL.md", root.join("plugins/demo/SKILL.md")).unwrap();

        let tree = read_tree(&root).unwrap();
        assert_eq!(tree["SKILL.md"].data, b"# demo\n");
        assert_eq!(tree["plugins/demo/SKILL.md"].data, b"# demo\n");
        assert!(fs::symlink_metadata(root.join("plugins/demo/SKILL.md"))
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn skill_tree_rejects_symlinks_that_escape_the_skill() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "cli-switch-escape-test-{}-{}",
            std::process::id(),
            util::now_millis()
        ));
        let root = base.join("skill");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("SKILL.md"), b"# demo\n").unwrap();
        fs::write(base.join("secret"), b"outside\n").unwrap();
        symlink("../secret", root.join("outside")).unwrap();

        let error = read_tree(&root).unwrap_err();
        assert!(error.contains("escapes skill directory"), "{error}");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn skill_tree_excludes_nested_git_directory() {
        let root = std::env::temp_dir().join(format!(
            "cli-switch-git-skip-test-{}-{}",
            std::process::id(),
            util::now_millis()
        ));
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join(".git/objects/pack-file"), vec![0u8; 1024]).unwrap();
        fs::write(root.join("SKILL.md"), b"# demo\n").unwrap();

        let tree = read_tree(&root).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree["SKILL.md"].data, b"# demo\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn simultaneous_different_values_conflict() {
        let a = UnitValue::Instructions(Some(b"a".to_vec()));
        let b = UnitValue::Instructions(Some(b"b".to_vec()));
        let result = decide(
            ("instructions", "global"),
            a.clone(),
            a.clone(),
            true,
            vec![
                (
                    "claude".into(),
                    a.clone(),
                    UnitValue::Instructions(None),
                    false,
                ),
                ("codex".into(), b, UnitValue::Instructions(None), false),
            ],
            "scan",
            &[],
        )
        .unwrap();
        assert!(matches!(result, Decision::Conflict(_)));
    }

    #[test]
    fn identical_values_are_merged_without_conflict() {
        let value = UnitValue::Instructions(Some(b"same".to_vec()));
        let result = decide(
            ("instructions", "global"),
            UnitValue::Instructions(None),
            UnitValue::Instructions(None),
            false,
            vec![
                (
                    "claude".into(),
                    value.clone(),
                    UnitValue::Instructions(None),
                    false,
                ),
                (
                    "codex".into(),
                    value.clone(),
                    UnitValue::Instructions(None),
                    false,
                ),
            ],
            "scan",
            &[],
        )
        .unwrap();
        assert!(matches!(result, Decision::Value(v) if v == value));
    }

    #[test]
    fn public_mcp_conflict_redacts_secrets() {
        let mut server = McpServer::http("https://example.test");
        server
            .headers
            .insert("Authorization".into(), "super-secret".into());
        let record = ConflictRecord {
            id: "x".into(),
            kind: "mcp".into(),
            name: "server".into(),
            scan_hash: "s".into(),
            candidates: vec![Candidate {
                source: "claude".into(),
                value: UnitValue::Mcp(Some(server)),
            }],
        };
        let text = public_conflict(&record).to_string();
        assert!(!text.contains("super-secret"));
        assert!(text.contains("***"));
    }
}
