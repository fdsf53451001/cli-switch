//! cli-switch — keep MCP servers, skills, and instructions in sync across
//! Claude Code, Codex, opencode, Kiro, and Antigravity.

mod adapters;
mod config;
mod configure;
mod links;
mod merge;
mod model;
mod mount;
mod paths;
mod project;
mod store;
mod sync;
mod util;

use model::Cli;
use util::R;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match dispatch(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cli-switch: error: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(args: &[String]) -> R<()> {
    let (cmd, rest) = match args.first().map(|s| s.as_str()) {
        None => ("configure", &args[0..]),
        Some("-h" | "--help" | "-V" | "--version") => (args[0].as_str(), &args[1..]),
        Some(first) => (first, &args[1..]),
    };
    match cmd {
        "sync" => cmd_sync(rest),
        "status" => cmd_status(),
        "mount" => cmd_mount(rest),
        "configure" | "config" => configure::run(rest),
        "init" => cmd_init(),
        "-V" | "--version" | "version" => {
            println!("cli-switch {VERSION}");
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (try `cli-switch help`)")),
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn cmd_sync(args: &[String]) -> R<()> {
    let opts = sync::Options {
        prune: has_flag(args, "--prune"),
        quiet: has_flag(args, "--quiet"),
        dry_run: has_flag(args, "--dry-run"),
    };
    sync::run(&opts)
}

fn cmd_init() -> R<()> {
    store::ensure_scaffold()?;
    if util::read_to_string_opt(&paths::store_config())?.is_none() {
        config::write_default()?;
    }
    println!(
        "Initialized cli-switch store at {}",
        paths::store_root().display()
    );
    println!("  canonical MCP:   {}", paths::store_mcp().display());
    println!(
        "  instructions:    {}",
        paths::store_instructions().display()
    );
    println!("  skills/:         {}", paths::store_skills().display());
    println!("  config:          {}", paths::store_config().display());
    println!("\nNext: `cli-switch sync` to pull existing config in, then `cli-switch mount` to auto-sync on startup.");
    Ok(())
}

fn cmd_mount(args: &[String]) -> R<()> {
    // Optional positional CLI names; default to all active.
    let named: Vec<Cli> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .filter_map(|a| Cli::from_id(a))
        .collect();
    let clis = if named.is_empty() {
        let cfg = config::load()?;
        let mut clis = config::active_clis(&cfg);
        if let Some(project_cfg) = config::load_project()? {
            for cli in config::active_clis(&project_cfg) {
                if !clis.contains(&cli) {
                    clis.push(cli);
                }
            }
        }
        clis
    } else {
        named
    };
    if clis.is_empty() {
        println!("No CLIs to mount.");
        return Ok(());
    }
    let report = mount::mount(&clis)?;
    println!("Mounted auto-sync triggers:");
    for line in report.lines {
        println!("  [+] {line}");
    }
    Ok(())
}

fn cmd_status() -> R<()> {
    print_status()
}

pub(crate) fn print_status() -> R<()> {
    let cfg = config::load()?;
    let project_cfg = config::load_project()?;
    let setup_clis = config::load_setup()?;

    println!("cli-switch {VERSION}");
    println!("store: {}", paths::store_root().display());
    println!(
        "setup CLIs: {}",
        if setup_clis.is_empty() {
            "not set".to_string()
        } else {
            setup_clis
                .iter()
                .map(|c| c.id())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!();
    println!("Global sync");
    println!(
        "selected CLIs: {}",
        cli_list(&cfg.clis).unwrap_or_else(|| "none".to_string())
    );
    status_global(&cfg)?;

    println!();
    println!("Project sync");
    match project_cfg {
        Some(project_cfg) => status_project(&project_cfg),
        None => {
            println!("project: {}", paths::project_root().display());
            println!("state: not joined");
            println!("Run `cli-switch` and choose `3) set project level` to join.");
            Ok(())
        }
    }
}

fn status_global(cfg: &config::Config) -> R<()> {
    let canonical = store::load_canonical()?;
    let canonical_skill_count = count_dirs(&paths::store_skills());
    println!(
        "canonical: {} MCP server(s), instructions={}, skills={}",
        canonical.servers.len(),
        file_state(paths::store_instructions().exists()),
        canonical_skill_count
    );
    println!();
    println!(
        "{:<13} {:<8} {:<10} {:<7} {:<13} {:<8} {:<12}",
        "CLI", "state", "app", "mcp", "instructions", "skills", "startup"
    );
    println!("{}", "-".repeat(80));

    for cli in Cli::ALL {
        let installed = adapters::installed(cli);
        let configured = cfg.clis.contains(&cli);
        let active = configured && installed;
        let state = global_sync_state(configured, installed);
        let mcp_n = if configured && installed {
            adapters::read_mcp(cli)
                .map(|m| m.len().to_string())
                .unwrap_or_else(|_| "?".to_string())
        } else {
            "-".to_string()
        };
        let instr = if active {
            link_state(&paths::instructions_file(cli), &paths::store_instructions())
        } else if configured {
            "skipped"
        } else {
            "off"
        };
        let skills = if active {
            format!(
                "{}/{}",
                count_synced_skill_links(&paths::skills_dir(cli), &paths::store_skills()),
                canonical_skill_count
            )
        } else if configured {
            "skipped".to_string()
        } else {
            "-".to_string()
        };
        println!(
            "{:<13} {:<8} {:<10} {:<7} {:<13} {:<8} {:<12}",
            cli.id(),
            state,
            installed_state(installed),
            mcp_n,
            instr,
            skills,
            startup_state(cli)
        );
    }

    println!();
    if canonical.servers.is_empty() {
        println!("No canonical MCP servers yet. Run `cli-switch sync` to import existing ones.");
    } else {
        println!("Canonical MCP servers:");
        for (name, s) in &canonical.servers {
            let kind = match s.transport {
                model::Transport::Stdio => "stdio",
                model::Transport::Http => "http",
            };
            let dis = if s.enabled { "" } else { " [disabled]" };
            println!("  - {name} ({kind}){dis}");
        }
    }
    Ok(())
}

fn global_sync_state(configured: bool, installed: bool) -> &'static str {
    match (configured, installed) {
        (true, true) => "active",
        (true, false) => "skipped",
        (false, _) => "off",
    }
}

fn status_project(cfg: &config::Config) -> R<()> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let agents = root.join("AGENTS.md");
    let skills = root.join(".agents").join("skills");
    let antigravity_rule = root.join(".agents").join("rules").join("agents-root.md");

    println!("project: {}", root.display());
    println!("AGENTS.md: {}", file_state(agents.exists()));
    println!(
        ".agents/skills: {} skill dir(s)",
        if skills.exists() {
            count_dirs(&skills)
        } else {
            0
        }
    );
    println!(
        "antigravity rule: {}",
        antigravity_rule_state(&antigravity_rule)
    );
    println!();
    println!(
        "{:<13} {:<9} {:<10} {:<18} {:<18} {:<12}",
        "CLI", "project", "app", "instructions", "skills", "startup"
    );
    println!("{}", "-".repeat(86));

    for cli in Cli::ALL {
        let configured = cfg.clis.contains(&cli);
        let instructions = if configured && cfg.instructions {
            project_instruction_state(cli, &root, &agents, &antigravity_rule)
        } else {
            "off"
        };
        let skills_state = if configured && cfg.skills {
            project_skills_state(cli, &root, &skills)
        } else {
            "off"
        };
        println!(
            "{:<13} {:<9} {:<10} {:<18} {:<18} {:<12}",
            cli.id(),
            enabled_state(configured),
            installed_state(adapters::installed(cli)),
            instructions,
            skills_state,
            startup_state(cli)
        );
    }

    if !agents.exists() {
        println!();
        println!("Project sync is not ready: run `cli-switch sync` to create AGENTS.md.");
    }
    Ok(())
}

fn enabled_state(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "off"
    }
}

fn installed_state(installed: bool) -> &'static str {
    if installed {
        "installed"
    } else {
        "not found"
    }
}

fn file_state(exists: bool) -> &'static str {
    if exists {
        "present"
    } else {
        "missing"
    }
}

fn cli_list(clis: &[Cli]) -> Option<String> {
    if clis.is_empty() {
        None
    } else {
        Some(clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", "))
    }
}

fn project_instruction_state(
    cli: Cli,
    root: &std::path::Path,
    agents: &std::path::Path,
    antigravity_rule: &std::path::Path,
) -> &'static str {
    match cli {
        Cli::Claude => link_state(&root.join("CLAUDE.md"), agents),
        Cli::Codex | Cli::Opencode => {
            if agents.exists() {
                "direct"
            } else {
                "missing"
            }
        }
        Cli::Kiro => link_state(
            &root.join(".kiro").join("steering").join("AGENTS.md"),
            agents,
        ),
        Cli::Antigravity => antigravity_rule_state(antigravity_rule),
    }
}

fn project_skills_state(
    cli: Cli,
    root: &std::path::Path,
    skills: &std::path::Path,
) -> &'static str {
    match cli {
        Cli::Claude => link_state(&root.join(".claude").join("skills"), skills),
        Cli::Kiro => link_state(&root.join(".kiro").join("skills"), skills),
        Cli::Codex | Cli::Opencode | Cli::Antigravity => {
            if skills.exists() {
                "direct"
            } else {
                "missing"
            }
        }
    }
}

fn antigravity_rule_state(path: &std::path::Path) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(text) if text.contains("@/AGENTS.md") => "rule",
        Ok(_) => "custom",
        Err(_) => "missing",
    }
}

fn count_dirs(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
        .unwrap_or(0)
}

fn startup_state(cli: Cli) -> &'static str {
    if matches!(cli, Cli::Kiro) {
        return shell_startup_state();
    }
    if matches!(cli, Cli::Antigravity) {
        return antigravity_startup_state();
    }
    let path = match cli {
        Cli::Claude => paths::claude_settings(),
        Cli::Codex => paths::codex_hooks(),
        Cli::Opencode => paths::opencode_plugin(),
        Cli::Kiro | Cli::Antigravity => unreachable!(),
    };
    match std::fs::read_to_string(path) {
        Ok(text) if text.contains("cli-switch") || text.contains("__cli_switch_run") => match cli {
            Cli::Opencode => "plugin",
            _ => "hook",
        },
        Ok(_) => "custom",
        Err(_) => "missing",
    }
}

fn antigravity_startup_state() -> &'static str {
    match std::fs::read_to_string(paths::antigravity_hooks()) {
        Ok(text) if text.contains("cli-switch-sync") && text.contains("PreInvocation") => "hook",
        Ok(_) => "custom",
        Err(_) => "missing",
    }
}

fn shell_startup_state() -> &'static str {
    let init = paths::shell_init();
    let Ok(init_text) = std::fs::read_to_string(&init) else {
        return "missing";
    };
    if !init_text.contains("__cli_switch_run") {
        return "custom";
    }
    "wrapper"
}

fn count_synced_skill_links(cli_dir: &std::path::Path, store_skills: &std::path::Path) -> usize {
    std::fs::read_dir(store_skills)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    let name = e.file_name();
                    let link = cli_dir.join(&name);
                    std::fs::read_link(link)
                        .map(|target| target == e.path())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn link_state(link: &std::path::Path, want: &std::path::Path) -> &'static str {
    match link.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(link).ok().as_deref() == Some(want) {
                "linked"
            } else {
                "other-link"
            }
        }
        Ok(_) => "real-file",
        Err(_) => "missing",
    }
}

fn print_help() {
    println!(
        r#"cli-switch {VERSION} — sync MCP servers, skills & instructions across AI CLIs

USAGE:
    cli-switch [command]

COMMANDS:
    configure       Open the setup menu; default when no command is given
    init            Create the canonical store (~/.config/cli-switch) and config
    sync            Run a full sync (MCP merge + skills/instructions links)
        --prune       remove servers gone from every CLI (default: keep + warn)
        --dry-run     show what would change without writing
        --quiet       only print warnings/errors (used by startup hooks)
    status          Show install state, server counts, and link status
    mount [clis…]   Install startup hooks so each CLI syncs on launch
    help            This message

CLIs: claude, codex, opencode, kiro, antigravity

Global sync store at ~/.config/cli-switch:
    mcp.json        canonical MCP servers (neutral format)
    AGENTS.md       shared instructions (symlinked into every CLI)
    skills/         shared SKILL.md folders (symlinked into every CLI)
    config.toml     which CLIs / features to sync

Project sync uses the current directory:
    AGENTS.md       project instructions source of truth
    .agents/skills/ shared project skills
    .agents/rules/  Antigravity rule files

Run `cli-switch configure --help` for non-interactive setup options.
"#
    );
}
