//! Interactive configuration wizard.

use crate::adapters;
use crate::config::{self, Config, Scope};
use crate::model::Cli;
use crate::{mount, paths, store, sync, util};
use std::io::{self, Write};

pub fn run(args: &[String]) -> util::R<()> {
    let has_config = paths::store_config().exists();
    let existing = config::load().unwrap_or_default();
    let scope_arg = arg_value(args, "--scope").and_then(|s| Scope::from_id(&s));
    let clis_arg = match arg_value(args, "--clis") {
        Some(raw) => Some(parse_cli_selection(&raw, &[])?),
        None => None,
    };
    let yes = has_flag(args, "--yes") || has_flag(args, "-y");
    let no_mount = has_flag(args, "--no-mount");

    let default_clis = if has_config {
        existing.clis.clone()
    } else {
        installed_clis()
    };

    let clis = match (clis_arg, yes) {
        (Some(clis), _) => clis,
        (None, true) => default_clis,
        (None, false) => prompt_clis(&default_clis)?,
    };

    let scope = match (scope_arg, yes) {
        (Some(scope), _) => scope,
        (None, true) => existing.scope,
        (None, false) => prompt_scope(existing.scope)?,
    };

    if scope == Scope::Global {
        store::ensure_scaffold()?;
    } else {
        util::ensure_dir(&paths::store_root())?;
    }

    let cfg = Config {
        scope,
        clis: clis.clone(),
        mcp: scope == Scope::Global,
        skills: true,
        instructions: true,
    };
    config::save(&cfg)?;

    println!("Configured cli-switch:");
    println!("  scope: {}", scope.id());
    println!(
        "  clis:  {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config: {}", paths::store_config().display());

    if !no_mount {
        let installed = clis
            .iter()
            .copied()
            .filter(|&cli| adapters::installed(cli))
            .collect::<Vec<_>>();
        if installed.is_empty() {
            println!("No selected CLI is installed; startup auto-sync was not mounted.");
        } else {
            let report = mount::mount(&installed)?;
            println!("Mounted startup auto-sync:");
            for line in report.lines {
                println!("  [+] {line}");
            }
        }
    }

    println!();
    println!("Running initial sync...");
    sync::run(&sync::Options {
        prune: false,
        quiet: false,
        dry_run: false,
    })?;

    println!();
    println!("Current status:");
    crate::print_status()?;

    Ok(())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    for (idx, arg) in args.iter().enumerate() {
        if arg == key {
            return args.get(idx + 1).cloned();
        }
        if let Some(rest) = arg.strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn installed_clis() -> Vec<Cli> {
    Cli::ALL
        .into_iter()
        .filter(|&cli| adapters::installed(cli))
        .collect()
}

fn prompt_scope(default: Scope) -> util::R<Scope> {
    println!("Sync scope:");
    println!("  1) global  - sync ~/.config/cli-switch to global CLI config");
    println!("  2) project - sync the current directory's AGENTS.md/.agents");
    loop {
        let input = prompt(&format!("Choose scope [{}]: ", default.id()))?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(default);
        }
        match trimmed {
            "1" | "global" => return Ok(Scope::Global),
            "2" | "project" | "current" | "cwd" => return Ok(Scope::Project),
            _ => println!("Please enter 1/global or 2/project."),
        }
    }
}

fn prompt_clis(default: &[Cli]) -> util::R<Vec<Cli>> {
    println!();
    println!("Install/sync which CLIs?");
    let mut out = Vec::new();
    for cli in Cli::ALL {
        let installed = adapters::installed(cli);
        let marker = if installed {
            "installed"
        } else {
            "not installed"
        };
        if prompt_yes_no(
            &format!("  {} ({})", cli.id(), marker),
            default.contains(&cli),
        )? {
            out.push(cli);
        }
    }
    if out.is_empty() {
        println!("Select at least one CLI.");
        return prompt_clis(default);
    }
    Ok(out)
}

fn parse_cli_selection(input: &str, default: &[Cli]) -> util::R<Vec<Cli>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default.to_vec());
    }
    match trimmed {
        "all" => return Ok(Cli::ALL.to_vec()),
        "installed" => return Ok(installed_clis()),
        _ => {}
    }

    let mut out = Vec::new();
    let mut invalid = Vec::new();
    for raw in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let cli = token
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|idx| Cli::ALL.get(idx).copied())
            .or_else(|| Cli::from_id(token));
        match cli {
            Some(cli) if !out.contains(&cli) => out.push(cli),
            Some(_) => {}
            None => invalid.push(token.to_string()),
        }
    }

    if invalid.is_empty() {
        Ok(out)
    } else {
        Err(format!(
            "Unknown CLI selection: {}. Use numbers, names, all, or installed.",
            invalid.join(", ")
        ))
    }
}

fn prompt_yes_no(label: &str, default: bool) -> util::R<bool> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        let input = prompt(&format!("{label} {suffix}: "))?;
        let trimmed = input.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            return Ok(default);
        }
        match trimmed.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please enter y or n."),
        }
    }
}

fn prompt(message: &str) -> util::R<String> {
    print!("{message}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    Ok(input)
}
