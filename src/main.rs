//! agent-sync — keep MCP servers, skills, and instructions in sync across
//! Claude Code, Codex, opencode, Kiro, and Antigravity.

mod adapters;
mod config;
mod links;
mod merge;
mod model;
mod mount;
mod paths;
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
            eprintln!("agent-sync: error: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(args: &[String]) -> R<()> {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[args.len().min(1)..];
    match cmd {
        "sync" => cmd_sync(rest),
        "status" => cmd_status(),
        "mount" => cmd_mount(rest),
        "init" => cmd_init(),
        "-V" | "--version" | "version" => {
            println!("agent-sync {VERSION}");
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (try `agent-sync help`)")),
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
        "Initialized agent-sync store at {}",
        paths::store_root().display()
    );
    println!("  canonical MCP:   {}", paths::store_mcp().display());
    println!(
        "  instructions:    {}",
        paths::store_instructions().display()
    );
    println!("  skills/:         {}", paths::store_skills().display());
    println!("  config:          {}", paths::store_config().display());
    println!("\nNext: `agent-sync sync` to pull existing config in, then `agent-sync mount` to auto-sync on startup.");
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
        config::active_clis(&cfg)
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
    let cfg = config::load()?;
    let canonical = store::load_canonical()?;

    println!("agent-sync {VERSION}");
    println!("store: {}", paths::store_root().display());
    println!(
        "canonical: {} MCP server(s), instructions={}, skills={}",
        canonical.servers.len(),
        yn(paths::store_instructions().exists()),
        count_dirs(&paths::store_skills())
    );
    println!();
    println!(
        "{:<13} {:<10} {:<7} {:<13} {:<8}",
        "CLI", "installed", "mcp", "instructions", "skills"
    );
    println!("{}", "-".repeat(54));

    for cli in Cli::ALL {
        let installed = adapters::installed(cli);
        let configured = cfg.clis.contains(&cli);
        let mcp_n = if installed {
            adapters::read_mcp(cli).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let instr = link_state(&paths::instructions_file(cli), &paths::store_instructions());
        let skills_n = count_links(&paths::skills_dir(cli));
        let tag = if !configured { " (off)" } else { "" };
        println!(
            "{:<13} {:<10} {:<7} {:<13} {:<8}",
            format!("{}{}", cli.id(), tag),
            yn(installed),
            mcp_n,
            instr,
            skills_n
        );
    }

    println!();
    if canonical.servers.is_empty() {
        println!("No canonical MCP servers yet. Run `agent-sync sync` to import existing ones.");
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

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
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

fn count_links(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path()
                        .symlink_metadata()
                        .map(|m| m.file_type().is_symlink())
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
        r#"agent-sync {VERSION} — sync MCP servers, skills & instructions across AI CLIs

USAGE:
    agent-sync <command> [options]

COMMANDS:
    init            Create the canonical store (~/.config/agent-sync) and config
    sync            Run a full sync (MCP merge + skills/instructions links)
        --prune       remove servers gone from every CLI (default: keep + warn)
        --dry-run     show what would change without writing
        --quiet       only print warnings/errors (used by startup hooks)
    status          Show install state, server counts, and link status
    mount [clis…]   Install startup hooks so each CLI syncs on launch
    help            This message

CLIs: claude, codex, opencode, kiro, antigravity

The store at ~/.config/agent-sync is the single source of truth:
    mcp.json        canonical MCP servers (neutral format)
    AGENTS.md       shared instructions (symlinked into every CLI)
    skills/         shared SKILL.md folders (symlinked into every CLI)
    config.toml     which CLIs / features to sync
"#
    );
}
