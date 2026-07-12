//! Reliability-first synchronization engine.
//!
//! Every source is compared with the last successful snapshot. A sync either
//! produces one deterministic plan and commits all of it, or writes nothing.

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointSnapshot {
    #[serde(default)]
    pub mcp_initialized: bool,
    #[serde(default)]
    pub instructions_initialized: bool,
    #[serde(default)]
    pub skills_initialized: bool,
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

fn state_version() -> u8 {
    2
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum UnitValue {
    Mcp(Option<McpServer>),
    Instructions(Option<Vec<u8>>),
    Skill(Option<Tree>),
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
    File(Vec<u8>),
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

pub fn run(
    active: &[Cli],
    mcp: bool,
    instructions: bool,
    skills: bool,
    opts: &Options,
) -> R<Outcome> {
    let state = load_state()?;
    let canonical_now = read_canonical()?;
    let mut endpoint_now = BTreeMap::new();
    for &cli in active {
        endpoint_now.insert(cli, read_endpoint(cli)?);
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
    )?;
    let mut next_state = state;
    next_state.version = 2;
    next_state.canonical = EndpointSnapshot {
        mcp_initialized: next_state.canonical.mcp_initialized || mcp,
        instructions_initialized: next_state.canonical.instructions_initialized || instructions,
        skills_initialized: next_state.canonical.skills_initialized || skills,
        content: merged_snapshot(&canonical_now, &desired, mcp, instructions, skills),
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
                content: merged_snapshot(&current, &desired, mcp, instructions, skills),
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

fn read_canonical() -> R<ContentSnapshot> {
    let canonical = store::load_canonical()?;
    Ok(ContentSnapshot {
        mcp: canonical.servers,
        instructions: fs::read(paths::store_instructions()).ok(),
        skills: read_skills(&paths::store_skills())?,
    })
}

fn read_endpoint(cli: Cli) -> R<ContentSnapshot> {
    Ok(ContentSnapshot {
        mcp: adapters::read_mcp(cli)?,
        instructions: fs::read(paths::instructions_file(cli)).ok(),
        skills: read_skills(&paths::skills_dir(cli))?,
    })
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
    fn walk(root: &Path, dir: &Path, out: &mut Tree) -> R<()> {
        let mut entries = fs::read_dir(dir)
            .map_err(|e| util::ctx(dir, e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| util::ctx(dir, e))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| util::ctx(&path, e))?;
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "nested symlink is not supported in skill {}",
                    path.display()
                ));
            }
            if meta.is_dir() {
                walk(root, &path, out)?;
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                out.insert(
                    rel,
                    FileBlob {
                        data: fs::read(&path).map_err(|e| util::ctx(&path, e))?,
                        executable,
                    },
                );
            }
        }
        Ok(())
    }
    let mut out = Tree::new();
    walk(root, root, &mut out)?;
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

fn build_operations(
    active: &[Cli],
    canonical_now: &ContentSnapshot,
    endpoint_now: &BTreeMap<Cli, ContentSnapshot>,
    desired: &ContentSnapshot,
    mcp: bool,
    instructions: bool,
    skills: bool,
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
            UnitValue::Mcp(None) | UnitValue::Instructions(None) | UnitValue::Skill(None) => json!({"deleted": true}),
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

fn apply_transaction_with_id(operations: &[Operation], id: &str) -> R<()> {
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
    let journal_path = paths::transactions().join(id).join("journal.json");
    util::ensure_private_dir(journal_path.parent().unwrap())?;
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
