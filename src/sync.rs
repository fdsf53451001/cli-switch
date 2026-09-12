//! Orchestration for the reliability-first global sync and project mappings.

use crate::config::{self, Config};
use crate::health;
use crate::model::Cli;
use crate::util::{self, R};
use crate::{engine, paths, project};
use std::io::{self, IsTerminal, Write};

pub struct Options {
    pub prune: bool,
    pub quiet: bool,
    pub dry_run: bool,
    pub migrate: bool,
}

/// What one sync attempt actually did, in the shape `status` needs to be able
/// to tell the truth about it later.
#[derive(Default)]
struct Report {
    applied: usize,
    transaction: Option<String>,
    conflicts: usize,
    skipped: Vec<engine::Skipped>,
    blocked: Option<String>,
    notes: Vec<health::Note>,
}

impl Report {
    fn absorb(&mut self, outcome: &engine::Outcome) {
        self.applied += outcome.actions.len();
        self.conflicts += outcome.conflicts.len();
        self.skipped.extend(outcome.skipped.iter().cloned());
        if outcome.transaction.is_some() {
            self.transaction = outcome.transaction.clone();
        }
    }

    fn result(&self) -> health::SyncResult {
        if self.blocked.is_some() {
            health::SyncResult::Blocked
        } else if self.conflicts > 0 {
            health::SyncResult::Conflicts
        } else if !self.skipped.is_empty() {
            health::SyncResult::Degraded
        } else {
            health::SyncResult::Ok
        }
    }

    fn record(&self) -> health::LastSync {
        health::LastSync {
            finished_ms: util::now_millis(),
            result: self.result(),
            error: self.blocked.clone(),
            transaction: self.transaction.clone(),
            conflicts: self.conflicts,
            applied: self.applied,
            skipped: self
                .skipped
                .iter()
                .map(|skipped| health::Note {
                    feature: skipped.feature.id().to_string(),
                    unit: skipped.unit.clone(),
                    reason: skipped.reason.clone(),
                })
                .chain(self.notes.iter().cloned())
                .collect(),
        }
    }
}

pub fn run(opts: &Options) -> R<()> {
    // Hold the lock through scaffold creation and the final health write.
    let _lock = if opts.dry_run {
        None
    } else {
        match util::acquire_lock(&paths::store_root().join(".lock"), 120)? {
            Some(lock) => Some(lock),
            None => {
                if !opts.quiet {
                    println!("Another cli-switch run is in progress — skipping.");
                }
                return Ok(());
            }
        }
    };
    let mut report = Report::default();
    let outcome = run_inner(opts, &mut report);
    if !opts.dry_run {
        let mut record = report.record();
        if let Err(error) = &outcome {
            record.result = health::SyncResult::Failed;
            record.error = Some(error.clone());
        }
        if let Err(error) = health::record(&record) {
            return Err(format!(
                "{}could not record sync health: {error}",
                outcome
                    .as_ref()
                    .err()
                    .map(|e| format!("{e}; "))
                    .unwrap_or_default()
            ));
        }
    }
    outcome?;
    finish(report.blocked, report.conflicts, opts)
}

/// Exit semantics, unchanged from before per-feature isolation: a migration
/// gate only fails the run when nobody could have answered the prompt, and
/// unresolved conflicts always fail with the dedicated status.
fn finish(blocked: Option<String>, conflicts: usize, opts: &Options) -> R<()> {
    if let Some(message) = blocked {
        if opts.quiet {
            return Err(message);
        }
    }
    if conflicts > 0 {
        return Err("unresolved conflicts".into());
    }
    Ok(())
}

fn run_inner(opts: &Options, report: &mut Report) -> R<()> {
    if !opts.dry_run {
        crate::store::ensure_scaffold()?;
    }
    let cfg = config::load()?;
    let active = config::active_clis(&cfg);
    if active.is_empty() {
        if !opts.quiet {
            println!("No configured CLI is installed — nothing to sync.");
        }
    } else {
        run_global(&cfg, &active, opts, report)?;
    }

    if let Some(project_cfg) = config::load_project()? {
        sync_project(&project_cfg, opts, report)?;
    }
    Ok(())
}

fn run_global(cfg: &Config, active: &[Cli], opts: &Options, report: &mut Report) -> R<()> {
    let mut outcome = engine::run(
        active,
        cfg.mcp,
        cfg.instructions,
        cfg.skills,
        cfg.agents,
        &engine::Options {
            dry_run: opts.dry_run,
            prune: opts.prune,
            allow_migration: opts.migrate,
        },
    )?;

    if outcome.migration_required {
        let message = outcome
            .migration
            .clone()
            .unwrap_or_else(|| "legacy symlinks detected".to_string());
        if opts.quiet {
            report.absorb(&outcome);
            report.blocked = Some(message);
            return Ok(());
        }
        println!("Migration required:\n  {message}");
        // The gate is fail-closed, but everything else found in the same pass
        // is reported now instead of one blocker per run.
        report_findings(&outcome, opts);
        if !opts.dry_run
            && io::stdin().is_terminal()
            && confirm("Convert these symlinks to independent copies now? [y/N] ")?
        {
            outcome = engine::run(
                active,
                cfg.mcp,
                cfg.instructions,
                cfg.skills,
                cfg.agents,
                &engine::Options {
                    dry_run: false,
                    prune: opts.prune,
                    allow_migration: true,
                },
            )?;
        } else {
            println!("No files were changed.");
            report.absorb(&outcome);
            report.blocked = Some(message);
            return Ok(());
        }
    }

    report_findings(&outcome, opts);
    if !opts.quiet {
        if !outcome.actions.is_empty() {
            let verb = if opts.dry_run {
                "would update"
            } else {
                "updated"
            };
            println!("Sync plan: {} item(s) {verb}", outcome.actions.len());
            for action in &outcome.actions {
                println!("  - {action}");
            }
            if let Some(id) = &outcome.transaction {
                println!("Transaction: {id}");
            }
        } else if outcome.conflicts.is_empty() && outcome.skipped.is_empty() {
            println!("Already in sync.");
        }
    }

    if !outcome.conflicts.is_empty() {
        if !opts.quiet && !opts.dry_run && io::stdin().is_terminal() {
            let mut resolved_any = false;
            for conflict in &outcome.conflicts {
                println!("\nResolve {} {}:", conflict.kind, conflict.name);
                for (index, candidate) in conflict.candidates.iter().enumerate() {
                    println!("  {}) {}", index + 1, candidate.source);
                }
                print!("Choose a source number, or press Enter to cancel: ");
                io::stdout().flush().map_err(|e| e.to_string())?;
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .map_err(|e| e.to_string())?;
                let Some(candidate) = answer
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| conflict.candidates.get(n.saturating_sub(1)))
                else {
                    report.absorb(&outcome);
                    return Ok(());
                };
                let source = candidate
                    .source
                    .split(',')
                    .next()
                    .unwrap_or(&candidate.source);
                engine::resolve_conflict_locked(&conflict.id, source)?;
                resolved_any = true;
            }
            if resolved_any {
                println!("All choices confirmed; applying one transactional sync.");
                report.applied += outcome.actions.len();
                return run_global(cfg, active, opts, report);
            }
        }
        report.absorb(&outcome);
        return Ok(());
    }

    report.absorb(&outcome);
    Ok(())
}

/// One block listing every skipped feature and every conflict found in the same
/// analysis, each with the file and field that caused it.
fn report_findings(outcome: &engine::Outcome, opts: &Options) {
    if !outcome.skipped.is_empty() {
        eprintln!(
            "Skipped {} feature(s); everything else was applied normally.",
            outcome.skipped.len()
        );
        for skipped in &outcome.skipped {
            eprintln!("  [skip] {}", skipped.summary());
        }
    }
    if !outcome.conflicts.is_empty() {
        let scope = if outcome.actions.is_empty() {
            "no files were changed"
        } else {
            "unaffected features were still applied"
        };
        eprintln!(
            "Sync stopped for {} conflicted unit(s); {scope}.",
            outcome.conflicts.len()
        );
        for conflict in &outcome.conflicts {
            eprintln!("  {} {} [{}]", conflict.kind, conflict.name, conflict.id);
            eprintln!(
                "    sources: {}",
                conflict
                    .candidates
                    .iter()
                    .map(|c| c.source.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        eprintln!("Discuss safely with your AI CLI using `cli-switch conflicts show <id> --json`, then confirm with `cli-switch conflicts resolve <id> --source <source>`. ");
    }
    if (!outcome.skipped.is_empty() || !outcome.conflicts.is_empty()) && !opts.quiet {
        eprintln!("Run `cli-switch doctor` to list every remaining blocker at once.");
    }
}

fn confirm(prompt: &str) -> R<bool> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn sync_project(cfg: &Config, opts: &Options, report: &mut Report) -> R<()> {
    if cfg.clis.is_empty() {
        return Ok(());
    }
    if cfg.agents {
        let active = config::active_clis(cfg);
        let agent_out = engine::run_project_agents(
            &active,
            &engine::Options {
                dry_run: opts.dry_run,
                prune: opts.prune,
                allow_migration: false,
            },
        )?;
        for skipped in &agent_out.skipped {
            eprintln!("  project: [skip] {}", skipped.summary());
        }
        for conflict in &agent_out.conflicts {
            eprintln!(
                "  project: conflict {} {} [{}]",
                conflict.kind, conflict.name, conflict.id
            );
        }
        if !opts.quiet {
            for action in &agent_out.actions {
                println!("  project: {action}");
            }
            if let Some(id) = &agent_out.transaction {
                println!("  project transaction: {id}");
            }
        }
        report.absorb(&agent_out);
        // Project instructions and skills are a different feature and keep
        // syncing even when the opt-in agent pass could not.
    }
    let out = project::sync(
        &cfg.clis,
        &project::Options {
            instructions: cfg.instructions,
            skills: cfg.skills,
            dry_run: opts.dry_run,
        },
    )?;
    report.applied += out.actions.len();
    report.conflicts += out.conflicts.len();
    if out.transaction.is_some() {
        report.transaction = out.transaction.clone();
    }
    report
        .notes
        .extend(out.conflicts.iter().map(|reason| health::Note {
            feature: "project mappings".into(),
            unit: None,
            reason: reason.clone(),
        }));
    if !opts.quiet {
        if let Some(id) = &out.transaction {
            println!("  project transaction: {id}");
        }
        for action in out.actions {
            println!("  project: {action}");
        }
        for note in out.notes {
            println!("  project: {note}");
        }
    }
    for conflict in out.conflicts {
        eprintln!("  project: {conflict}");
    }
    Ok(())
}
