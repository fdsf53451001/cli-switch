//! Orchestration for the reliability-first global sync and project mappings.

use crate::config::{self, Config};
use crate::model::Cli;
use crate::util::R;
use crate::{engine, paths, project};
use std::io::{self, IsTerminal, Write};

pub struct Options {
    pub prune: bool,
    pub quiet: bool,
    pub dry_run: bool,
    pub migrate: bool,
}

pub fn run(opts: &Options) -> R<()> {
    if !opts.dry_run {
        crate::store::ensure_scaffold()?;
    }

    let _lock = if opts.dry_run {
        None
    } else {
        match crate::util::acquire_lock(&paths::store_root().join(".lock"), 120)? {
            Some(lock) => Some(lock),
            None => {
                if !opts.quiet {
                    println!("Another cli-switch run is in progress — skipping.");
                }
                return Ok(());
            }
        }
    };

    let cfg = config::load()?;
    let active = config::active_clis(&cfg);
    if active.is_empty() {
        if !opts.quiet {
            println!("No configured CLI is installed — nothing to sync.");
        }
    } else {
        run_global(&cfg, &active, opts)?;
    }

    if let Some(project_cfg) = config::load_project()? {
        sync_project(&project_cfg, opts)?;
    }
    Ok(())
}

fn run_global(cfg: &Config, active: &[Cli], opts: &Options) -> R<()> {
    let mut outcome = engine::run(
        active,
        cfg.mcp,
        cfg.instructions,
        cfg.skills,
        &engine::Options {
            dry_run: opts.dry_run,
            quiet: opts.quiet,
            prune: opts.prune,
            allow_migration: opts.migrate,
        },
    )?;

    if outcome.migration_required {
        let message = &outcome.actions[0];
        if opts.quiet {
            return Err(message.clone());
        }
        println!("Migration required:\n  {message}");
        if !opts.dry_run
            && io::stdin().is_terminal()
            && confirm("Convert these symlinks to independent copies now? [y/N] ")?
        {
            outcome = engine::run(
                active,
                cfg.mcp,
                cfg.instructions,
                cfg.skills,
                &engine::Options {
                    dry_run: false,
                    quiet: false,
                    prune: opts.prune,
                    allow_migration: true,
                },
            )?;
        } else {
            println!("No files were changed.");
            return Ok(());
        }
    }
    if !outcome.conflicts.is_empty() {
        eprintln!(
            "Sync stopped: {} conflict(s); no files were changed.",
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
        if !opts.quiet && !opts.dry_run && io::stdin().is_terminal() {
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
                    return Err("unresolved conflicts".into());
                };
                let source = candidate
                    .source
                    .split(',')
                    .next()
                    .unwrap_or(&candidate.source);
                engine::resolve_conflict(&conflict.id, source)?;
            }
            println!("All choices confirmed; applying one transactional sync.");
            return run_global(cfg, active, opts);
        }
        return Err("unresolved conflicts".into());
    }
    if !opts.quiet {
        if outcome.actions.is_empty() {
            println!("Already in sync.");
        } else {
            let verb = if opts.dry_run {
                "would update"
            } else {
                "updated"
            };
            println!("Sync plan: {} item(s) {verb}", outcome.actions.len());
            for action in outcome.actions {
                println!("  - {action}");
            }
            if let Some(id) = outcome.transaction {
                println!("Transaction: {id}");
            }
        }
    }
    Ok(())
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

fn sync_project(cfg: &Config, opts: &Options) -> R<()> {
    if cfg.clis.is_empty() {
        return Ok(());
    }
    let out = project::sync(
        &cfg.clis,
        &project::Options {
            instructions: cfg.instructions,
            skills: cfg.skills,
            dry_run: opts.dry_run,
        },
    )?;
    if !opts.quiet {
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
