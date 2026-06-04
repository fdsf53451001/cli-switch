//! Interactive configuration wizard.

use crate::adapters;
use crate::config::{self, Config, Scope};
use crate::model::Cli;
use crate::{mount, paths, project, store, sync, util};
use std::io::{self, Write};

pub fn run(args: &[String]) -> util::R<()> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        print_help();
        return Ok(());
    }

    let scope_arg = arg_value(args, "--scope").and_then(|s| Scope::from_id(&s));
    if scope_arg == Some(Scope::Project) {
        configure_project(args, true)?;
        return finish();
    }
    if scope_arg == Some(Scope::Global) {
        configure_global(args, true)?;
        return finish();
    }
    if has_flag(args, "--yes") || has_flag(args, "-y") {
        return Err("`--yes` needs an explicit scope: `cli-switch configure --scope global --yes` or `cli-switch configure --scope project --yes`".to_string());
    }

    run_menu()
}

fn run_menu() -> util::R<()> {
    loop {
        println!("cli-switch");
        println!("  1) setup cli");
        println!("  2) set global level");
        println!("  3) set project level");
        println!("  4) remove cli");
        println!("  5) remove global level");
        println!("  6) remove project level");
        println!();

        match prompt("Choose [1-6]: ")?.trim() {
            "1" => return setup_cli(),
            "2" => {
                configure_global_from_setup()?;
                return finish();
            }
            "3" => {
                configure_project_from_setup()?;
                return finish();
            }
            "4" => return remove_cli(),
            "5" => return remove_global_level(),
            "6" => return remove_project_level(),
            _ => println!("Please enter 1, 2, 3, 4, 5, or 6."),
        }
    }
}

fn configure_global(args: &[String], no_prompt: bool) -> util::R<Vec<Cli>> {
    let has_config = paths::store_config().exists();
    let existing = config::load().unwrap_or_default();
    let clis_arg = match arg_value(args, "--clis") {
        Some(raw) => Some(parse_cli_selection(&raw, &[])?),
        None => None,
    };

    let default_clis = if has_config {
        existing.clis.clone()
    } else {
        installed_clis()
    };

    let clis = match (clis_arg, no_prompt) {
        (Some(clis), _) => clis,
        (None, true) => default_clis,
        (None, false) => prompt_clis("Sync which CLIs globally?", &default_clis)?,
    };

    store::ensure_scaffold()?;

    let cfg = Config {
        scope: Scope::Global,
        clis: clis.clone(),
        mcp: true,
        skills: true,
        instructions: true,
    };
    config::save(&cfg)?;

    println!("Configured global cli-switch sync:");
    println!(
        "  clis:  {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config: {}", paths::store_config().display());

    Ok(clis)
}

fn configure_project(args: &[String], no_prompt: bool) -> util::R<Vec<Cli>> {
    let existing = config::load_project()?.unwrap_or_else(|| Config {
        scope: Scope::Project,
        clis: installed_clis(),
        mcp: false,
        skills: true,
        instructions: true,
    });
    let clis_arg = match arg_value(args, "--clis") {
        Some(raw) => Some(parse_cli_selection(&raw, &[])?),
        None => None,
    };

    let clis = match (clis_arg, no_prompt) {
        (Some(clis), _) => clis,
        (None, true) => existing.clis,
        (None, false) => prompt_clis("Sync which CLIs for this project?", &existing.clis)?,
    };

    util::ensure_dir(&paths::project_config_dir())?;
    let cfg = Config {
        scope: Scope::Project,
        clis: clis.clone(),
        mcp: false,
        skills: true,
        instructions: true,
    };
    config::save_project(&cfg)?;

    println!("Joined project sync:");
    println!("  project: {}", paths::project_root().display());
    println!(
        "  clis:    {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config:  {}", paths::project_config().display());

    Ok(clis)
}

fn configure_global_from_setup() -> util::R<()> {
    let clis = setup_clis()?;
    store::ensure_scaffold()?;
    let cfg = Config {
        scope: Scope::Global,
        clis: clis.clone(),
        mcp: true,
        skills: true,
        instructions: true,
    };
    config::save(&cfg)?;
    println!("Configured global cli-switch sync:");
    println!(
        "  clis:  {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config: {}", paths::store_config().display());
    Ok(())
}

fn configure_project_from_setup() -> util::R<()> {
    let clis = setup_clis()?;
    util::ensure_dir(&paths::project_config_dir())?;
    let cfg = Config {
        scope: Scope::Project,
        clis: clis.clone(),
        mcp: false,
        skills: true,
        instructions: true,
    };
    config::save_project(&cfg)?;
    println!("Joined project sync:");
    println!("  project: {}", paths::project_root().display());
    println!(
        "  clis:    {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config:  {}", paths::project_config().display());
    Ok(())
}

fn setup_cli() -> util::R<()> {
    let default = configured_clis()?;
    let default = if default.is_empty() {
        installed_clis()
    } else {
        default
    };
    let clis = prompt_clis("Setup startup sync for which CLIs?", &default)?;
    util::ensure_dir(&paths::store_root())?;
    config::save_setup(&clis)?;
    let installed = clis
        .iter()
        .copied()
        .filter(|&cli| adapters::installed(cli))
        .collect::<Vec<_>>();

    if installed.is_empty() {
        println!("No selected CLI is installed; nothing was mounted.");
    } else {
        let report = mount::mount(&installed)?;
        println!("Mounted startup auto-sync:");
        for line in report.lines {
            println!("  [+] {line}");
        }
    }
    println!("Saved CLI setup:");
    println!(
        "  clis:   {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    println!("  config: {}", paths::setup_config().display());

    println!();
    println!("Current status:");
    crate::print_status()?;
    Ok(())
}

fn remove_cli() -> util::R<()> {
    let default = configured_clis()?;
    if default.is_empty() {
        println!("No CLI is configured.");
        return Ok(());
    }

    let clis = prompt_clis("Remove which CLIs from cli-switch?", &default)?;
    let mut global_cfg = config::load()?;
    global_cfg.clis.retain(|cli| !clis.contains(cli));
    config::save(&global_cfg)?;

    let mut setup = config::load_setup()?;
    setup.retain(|cli| !clis.contains(cli));
    config::save_setup(&setup)?;

    if let Some(mut project_cfg) = config::load_project()? {
        project_cfg.clis.retain(|cli| !clis.contains(cli));
        config::save_project(&project_cfg)?;
    }

    let report = mount::unmount(&clis)?;
    println!("Removed CLI sync:");
    println!(
        "  clis: {}",
        clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", ")
    );
    for line in report.lines {
        println!("  [-] {line}");
    }

    finish()
}

fn remove_global_level() -> util::R<()> {
    let cfg = Config {
        scope: Scope::Global,
        clis: Vec::new(),
        mcp: true,
        skills: true,
        instructions: true,
    };
    store::ensure_scaffold()?;
    config::save(&cfg)?;
    println!("Removed global level:");
    println!("  config: {}", paths::store_config().display());
    println!("  kept:   canonical MCP, AGENTS.md, skills, state, backups");
    finish()
}

fn remove_project_level() -> util::R<()> {
    if config::project_joined() {
        leave_project()?;
    } else {
        println!("Project level is not enabled for this directory.");
    }
    finish()
}

fn leave_project() -> util::R<()> {
    project::leave()?;
    config::remove_project()?;
    println!("Left project sync:");
    println!("  project: {}", paths::project_root().display());
    println!("  removed: {}", paths::project_config().display());
    println!("  kept:    AGENTS.md and .agents/skills");
    Ok(())
}

fn finish() -> util::R<()> {
    println!();
    println!("Running sync...");
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

fn configured_clis() -> util::R<Vec<Cli>> {
    let mut out = config::load_setup()?;
    for cli in config::load()?.clis {
        if !out.contains(&cli) {
            out.push(cli);
        }
    }
    if let Some(project_cfg) = config::load_project()? {
        for cli in project_cfg.clis {
            if !out.contains(&cli) {
                out.push(cli);
            }
        }
    }
    Ok(out)
}

fn setup_clis() -> util::R<Vec<Cli>> {
    let clis = config::load_setup()?;
    if clis.is_empty() {
        return Err("No CLI setup found. Run `cli-switch`, choose `1) setup cli`, then run this action again.".to_string());
    }
    Ok(clis)
}

fn prompt_clis(title: &str, default: &[Cli]) -> util::R<Vec<Cli>> {
    println!();
    println!("{title}");
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
        return prompt_clis(title, default);
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

fn print_help() {
    println!(
        r#"Usage:
    cli-switch configure [--scope global|project --clis <list> --yes]

Without options, configure opens this menu:
    1) setup cli           select CLIs once and install startup sync
    2) set global level    use the CLI selection from 1
    3) set project level   use the CLI selection from 1
    4) remove cli
    5) remove global level no CLI prompt
    6) remove project level no CLI prompt

Examples:
    cli-switch
    cli-switch configure --scope global --clis claude,codex --yes
    cli-switch configure --scope project --clis installed --yes
"#
    );
}
