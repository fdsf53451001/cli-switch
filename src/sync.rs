//! Orchestration: run one full sync pass (MCP merge + skills/instructions links).

use crate::config::{self, Config};
use crate::merge::{self, CliState};
use crate::model::{Canonical, Cli};
use crate::util::{mtime_secs, R};
use crate::{adapters, links, paths, project, store};
use std::collections::BTreeSet;

pub struct Options {
    pub prune: bool,
    pub quiet: bool,
    pub dry_run: bool,
}

pub fn run(opts: &Options) -> R<()> {
    if !opts.dry_run {
        store::ensure_scaffold()?;
    }

    // Prevent concurrent syncs (multiple CLIs launching at once would race on
    // the shared canonical store and each CLI's config file).
    let _lock = if opts.dry_run {
        None
    } else {
        let lock_path = paths::store_root().join(".lock");
        match crate::util::acquire_lock(&lock_path, 120)? {
            Some(l) => Some(l),
            None => {
                if !opts.quiet {
                    println!("Another cli-switch run is in progress — skipping.");
                }
                return Ok(());
            }
        }
    };

    let cfg = config::load()?;
    let mut log = Logger { quiet: opts.quiet };

    let active = config::active_clis(&cfg);

    if active.is_empty() {
        log.info("No configured CLI is installed — nothing to sync.");
    } else {
        log.info(&format!(
            "Syncing global config for {} CLI(s): {}",
            active.len(),
            active.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
        ));

        if cfg.mcp {
            sync_mcp(&cfg, &active, opts, &mut log)?;
        }
        if cfg.instructions {
            if opts.dry_run {
                log.info(&format!(
                    "  instructions: dry-run — would sync links for {} CLI(s)",
                    active.len()
                ));
            } else {
                let out = links::sync_instructions(&active)?;
                report_links("instructions", &out, &mut log);
            }
        }
        if cfg.skills {
            if opts.dry_run {
                log.info(&format!(
                    "  skills: dry-run — would sync links for {} CLI(s)",
                    active.len()
                ));
            } else {
                let out = links::sync_skills(&active)?;
                report_links("skills", &out, &mut log);
            }
        }
    }

    if let Some(project_cfg) = config::load_project()? {
        sync_project(&project_cfg, opts, &mut log)?;
    }

    log.info("Done.");
    Ok(())
}

fn sync_project(cfg: &Config, opts: &Options, log: &mut Logger) -> R<()> {
    if cfg.clis.is_empty() {
        log.info("No configured CLI is selected — nothing to sync.");
        return Ok(());
    }
    log.info(&format!(
        "Syncing current directory for {} CLI(s): {}",
        cfg.clis.len(),
        cfg.clis
            .iter()
            .map(|c| c.id())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let out = project::sync(
        &cfg.clis,
        &project::Options {
            instructions: cfg.instructions,
            skills: cfg.skills,
            dry_run: opts.dry_run,
        },
    )?;

    for action in out.actions {
        log.info(&format!("  project: {action}"));
    }
    for note in out.notes {
        log.debug(&format!("  project: {note}"));
    }
    for conflict in out.conflicts {
        log.warn(&format!("  project: {conflict}"));
    }
    Ok(())
}

fn sync_mcp(cfg: &Config, active: &[Cli], opts: &Options, log: &mut Logger) -> R<()> {
    let canonical: Canonical = store::load_canonical()?;

    let states: Vec<CliState> = active
        .iter()
        .map(|&cli| -> R<CliState> {
            Ok(CliState {
                cli,
                native: adapters::read_mcp(cli)?,
                snapshot: store::load_snapshot(cli)?,
                mtime: mtime_secs(&paths::mcp_config(cli)),
                has_snapshot: store::has_snapshot(cli),
                enabled: true,
            })
        })
        .collect::<R<Vec<_>>>()?;

    // Include any CLI configured but not active so its snapshot isn't used.
    let _ = cfg;

    let result = merge::merge(&canonical.servers, &states, opts.prune);

    // Report what the merge decided.
    for a in &result.adopted {
        let verb = match a.kind {
            merge::ChangeKind::Added => "added",
            merge::ChangeKind::Modified => "updated",
        };
        log.info(&format!(
            "  mcp: '{}' {} (from {})",
            a.name,
            verb,
            a.cli.id()
        ));
    }
    for c in &result.conflicts {
        let losers = c
            .losers
            .iter()
            .map(|c| c.id())
            .collect::<Vec<_>>()
            .join(", ");
        log.warn(&format!(
            "  mcp: conflict on '{}' — kept {}'s version (newer), ignored: {}",
            c.name,
            c.winner.id(),
            losers
        ));
    }
    for name in &result.deletions {
        log.info(&format!("  mcp: '{}' pruned (gone from all CLIs)", name));
    }
    for name in &result.stale {
        log.warn(&format!(
            "  mcp: '{}' is gone from all CLIs — run with --prune to remove it",
            name
        ));
    }

    // Quarantine: orphaned servers stay in canonical but are NOT pushed back to
    // the CLIs (otherwise removing a server everywhere would just resurrect it).
    let stale_set: BTreeSet<String> = result.stale.iter().cloned().collect();
    let push_map: crate::model::McpMap = result
        .canonical
        .iter()
        .filter(|(name, _)| !stale_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let n_servers = push_map.len();
    // Remove pruned names AND any quarantined orphan from the CLIs.
    let mut removal: BTreeSet<String> = result.deletions.iter().cloned().collect();
    removal.extend(stale_set.iter().cloned());

    if opts.dry_run {
        log.info(&format!(
            "  mcp: dry-run — would write {} server(s) to {} CLI(s)",
            n_servers,
            active.len()
        ));
        return Ok(());
    }

    // Persist canonical, then push to every CLI and refresh snapshots.
    let new_canonical = Canonical {
        servers: result.canonical.clone(),
    };
    backup_then(active, log)?;
    store::save_canonical(&new_canonical)?;

    for &cli in active {
        adapters::write_mcp(cli, &push_map, &removal)?;
        // Snapshot = round-tripped read-back, so the next diff is clean even when
        // a CLI can't represent a field (e.g. Claude drops `enabled`).
        let readback = adapters::read_mcp(cli)?;
        store::save_snapshot(cli, &readback)?;
    }
    log.info(&format!(
        "  mcp: {} server(s) synced to {} CLI(s)",
        n_servers,
        active.len()
    ));
    Ok(())
}

fn backup_then(active: &[Cli], log: &mut Logger) -> R<()> {
    let dir = paths::store_backups();
    for &cli in active {
        let p = paths::mcp_config(cli);
        if let Some(b) = crate::util::backup_file(&p, &dir)? {
            log.debug(&format!("  backup: {} -> {}", p.display(), b.display()));
        }
    }
    Ok(())
}

fn report_links(kind: &str, out: &links::LinkOutcome, log: &mut Logger) {
    for a in &out.adopted {
        log.info(&format!("  {}: {}", kind, a));
    }
    for l in &out.linked {
        log.debug(&format!("  {}: {}", kind, l));
    }
    for c in &out.conflicts {
        log.warn(&format!("  {}: {}", kind, c));
    }
    if out.adopted.is_empty() && out.linked.is_empty() && out.conflicts.is_empty() {
        log.debug(&format!("  {}: already in sync", kind));
    }
}

pub struct Logger {
    pub quiet: bool,
}
impl Logger {
    pub fn info(&mut self, s: &str) {
        if !self.quiet {
            println!("{s}");
        }
    }
    pub fn debug(&mut self, s: &str) {
        if !self.quiet {
            println!("{s}");
        }
    }
    pub fn warn(&mut self, s: &str) {
        eprintln!("{s}");
    }
}
