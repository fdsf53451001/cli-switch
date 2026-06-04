//! Three-way merge for MCP servers.
//!
//! For each CLI: base = last-sync snapshot, theirs = current native config,
//! ours = canonical. A server counts as "changed in CLI X" when its native form
//! differs from X's snapshot. Changes flow into canonical; ties between CLIs are
//! broken by config-file mtime (newest wins). Deletions are conservative: a
//! server is dropped only when it's gone from EVERY CLI and was synced before —
//! and even then only when `prune` is set.

use crate::model::{Cli, McpMap, McpServer};
use std::collections::{BTreeMap, BTreeSet};

pub struct CliState {
    pub cli: Cli,
    pub native: McpMap,
    pub snapshot: McpMap,
    pub mtime: u64,
    pub has_snapshot: bool,
    pub enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
}

pub struct Adopt {
    pub name: String,
    pub cli: Cli,
    pub kind: ChangeKind,
}

pub struct Conflict {
    pub name: String,
    pub winner: Cli,
    pub losers: Vec<Cli>,
}

pub struct MergeResult {
    pub canonical: McpMap,
    pub adopted: Vec<Adopt>,
    pub conflicts: Vec<Conflict>,
    /// Names removed from canonical (only populated when `prune`).
    pub deletions: Vec<String>,
    /// Names gone everywhere but kept because `prune` was off (reporting only).
    pub stale: Vec<String>,
}

pub fn merge(canonical: &McpMap, states: &[CliState], prune: bool) -> MergeResult {
    // 1. Gather per-name changes across all enabled CLIs.
    let mut changes: BTreeMap<String, Vec<(Cli, McpServer, u64)>> = BTreeMap::new();
    for st in states.iter().filter(|s| s.enabled) {
        for (name, srv) in &st.native {
            let changed = match st.snapshot.get(name) {
                Some(prev) => prev != srv,
                None => true,
            };
            if changed {
                changes
                    .entry(name.clone())
                    .or_default()
                    .push((st.cli, srv.clone(), st.mtime));
            }
        }
    }

    let mut new_canon = canonical.clone();
    let mut adopted = Vec::new();
    let mut conflicts = Vec::new();

    for (name, mut list) in changes.clone() {
        list.sort_by_key(|(_, _, m)| *m);
        let (wcli, wsrv, _) = list.last().cloned().unwrap();

        let kind = if canonical.contains_key(&name) {
            ChangeKind::Modified
        } else {
            ChangeKind::Added
        };

        // Conflict iff two CLIs changed it to materially different definitions.
        let losers: Vec<Cli> = list
            .iter()
            .filter(|(c, s, _)| *c != wcli && *s != wsrv)
            .map(|(c, _, _)| *c)
            .collect();

        new_canon.insert(name.clone(), wsrv);
        adopted.push(Adopt {
            name: name.clone(),
            cli: wcli,
            kind,
        });
        if !losers.is_empty() {
            conflicts.push(Conflict {
                name,
                winner: wcli,
                losers,
            });
        }
    }

    // 2. Orphan detection (stable across runs, snapshot-membership-independent).
    //
    // After any sync we push every canonical server into every CLI, so once
    // synced a canonical server is present in all natives. Therefore a server
    // that is in canonical but in NO active native — given we've synced before —
    // must have been removed from every CLI after being pushed: a real orphan.
    // This keeps detecting (and warning) every run until prune resolves it, and
    // never depends on a server still living in a snapshot.
    let synced_before = states.iter().any(|s| s.enabled && s.has_snapshot);

    let live: BTreeSet<String> = states
        .iter()
        .filter(|s| s.enabled)
        .flat_map(|s| s.native.keys().cloned())
        .collect();

    let mut deletions = Vec::new();
    let mut stale = Vec::new();
    if synced_before {
        let orphans: Vec<String> = new_canon
            .keys()
            .filter(|name| !live.contains(*name) && !changes.contains_key(*name))
            .cloned()
            .collect();
        for name in orphans {
            if prune {
                new_canon.remove(&name);
                deletions.push(name);
            } else {
                // Quarantined: kept in canonical but NOT pushed back to the CLIs
                // (the orchestrator drops `stale` from the push set), so the user
                // can still `--prune` it later or recover it from mcp.json.
                stale.push(name);
            }
        }
    }

    MergeResult {
        canonical: new_canon,
        adopted,
        conflicts,
        deletions,
        stale,
    }
}
