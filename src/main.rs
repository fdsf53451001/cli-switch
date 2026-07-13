//! cli-switch — keep MCP servers, skills, instructions, and custom agents in sync across
//! Claude Code, Codex, opencode, Kiro, Antigravity, and GitHub Copilot.

mod adapters;
mod agents;
mod config;
mod configure;
mod engine;
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
            if e == "unresolved conflicts" {
                2
            } else {
                1
            }
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
        "conflicts" => cmd_conflicts(rest),
        "rollback" => cmd_rollback(rest),
        "hook" => cmd_hook(rest),
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
        migrate: has_flag(args, "--migrate"),
    };
    sync::run(&opts)
}

fn cmd_conflicts(args: &[String]) -> R<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            let records = engine::list_conflicts()?;
            if has_flag(args, "--json") {
                let values = records
                    .iter()
                    .map(engine::public_conflict)
                    .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&values).map_err(|e| e.to_string())?
                );
            } else if records.is_empty() {
                println!("No pending conflicts.");
            } else {
                for record in records {
                    println!("{}  {} {}", record.id, record.kind, record.name);
                }
            }
            Ok(())
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or("usage: cli-switch conflicts show <id> [--json]")?;
            let record = engine::list_conflicts()?
                .into_iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("conflict not found: {id}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine::public_conflict(&record))
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        "resolve" => {
            let id = args
                .get(1)
                .ok_or("usage: cli-switch conflicts resolve <id> --source <source>")?;
            let source = args
                .windows(2)
                .find(|w| w[0] == "--source")
                .map(|w| w[1].as_str())
                .ok_or("missing --source <source>")?;
            engine::resolve_conflict(id, source)?;
            println!("Resolution recorded for {id}; running transactional sync.");
            sync::run(&sync::Options {
                prune: false,
                quiet: false,
                dry_run: false,
                migrate: false,
            })
        }
        other => Err(format!("unknown conflicts command '{other}'")),
    }
}

fn cmd_rollback(args: &[String]) -> R<()> {
    let id = args
        .first()
        .ok_or("usage: cli-switch rollback <transaction-id>")?;
    let new_id = engine::rollback(id)?;
    println!("Rolled back {id} in transaction {new_id}");
    Ok(())
}

fn cmd_hook(args: &[String]) -> R<()> {
    let json_output = has_flag(args, "--json");
    match sync::run(&sync::Options {
        prune: false,
        quiet: true,
        dry_run: false,
        migrate: false,
    }) {
        Ok(()) => {
            if json_output {
                println!("{{\"ok\":true}}");
            }
            Ok(())
        }
        Err(error) if error == "unresolved conflicts" => {
            let conflicts = engine::list_conflicts()?
                .iter()
                .map(engine::public_conflict)
                .collect::<Vec<_>>();
            if json_output {
                println!("{}", serde_json::to_string(&serde_json::json!({
                    "ok": false,
                    "requires_user": true,
                    "message": "cli-switch found divergent edits. Discuss the masked candidates with the user and ask for explicit confirmation before resolving.",
                    "conflicts": conflicts,
                })).map_err(|e| e.to_string())?);
            } else {
                println!("cli-switch needs your help: divergent configuration edits were found and nothing was changed.");
                println!("Discuss these masked candidates with the user and ask for explicit confirmation:");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&conflicts).map_err(|e| e.to_string())?
                );
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
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
    println!("  agents/:         {}", paths::store_agents().display());
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
    let pending = engine::list_conflicts()?.len();
    println!("pending conflicts: {pending}");
    println!(
        "last transaction: {}",
        engine::last_transaction()?.unwrap_or_else(|| "none".to_string())
    );
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
    let canonical_agent_count = count_dirs(&paths::store_agents());
    println!(
        "canonical: {} MCP server(s), instructions={}, skills={}, agents={} ({})",
        canonical.servers.len(),
        yn(paths::store_instructions().exists()),
        canonical_skill_count,
        canonical_agent_count,
        if cfg.agents { "enabled" } else { "disabled" }
    );
    println!();
    println!(
        "{:<13} {:<8} {:<10} {:<7} {:<13} {:<8} {:<8} {:<10}",
        "CLI", "sync", "installed", "mcp", "instructions", "skills", "agents", "startup"
    );
    println!("{}", "-".repeat(87));

    for cli in Cli::ALL {
        let installed = adapters::installed(cli);
        let configured = cfg.clis.contains(&cli);
        let active = configured && installed;
        let mcp = active && adapters::read_mcp(cli).is_ok();
        let instr =
            active && same_file(&paths::instructions_file(cli), &paths::store_instructions());
        let skills = active
            && count_synced_skills(&paths::skills_dir(cli), &paths::store_skills())
                == canonical_skill_count;
        let agents = active
            && cfg.agents
            && count_native_agents(cli, &paths::agents_dir(cli)) == canonical_agent_count;
        println!(
            "{:<13} {:<8} {:<10} {:<7} {:<13} {:<8} {:<8} {:<10}",
            cli.id(),
            yn(configured),
            yn(installed),
            yn(mcp),
            yn(instr),
            yn(skills),
            yn(agents),
            yn(startup_ok(cli))
        );
    }
    println!();
    println!("Startup hook support:");
    for cli in Cli::ALL {
        println!(
            "  {:<13} {:<12} {}",
            cli.id(),
            hook_tier(cli),
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

fn status_project(cfg: &config::Config) -> R<()> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let agents = root.join("AGENTS.md");
    let skills = root.join(".agents").join("skills");
    let antigravity_rule = root.join(".agents").join("rules").join("agents-root.md");

    println!("project: {}", root.display());
    println!("AGENTS.md: {}", yn(agents.exists()));
    println!(
        ".agents/skills: {} skill dir(s)",
        if skills.exists() {
            count_dirs(&skills)
        } else {
            0
        }
    );
    println!(
        ".cli-switch/agents: {} agent(s) ({})",
        count_dirs(&paths::project_agents()),
        if cfg.agents { "enabled" } else { "disabled" }
    );
    println!(
        "antigravity rule: {}",
        antigravity_rule_state(&antigravity_rule)
    );
    println!();
    println!(
        "{:<13} {:<8} {:<10} {:<13} {:<8} {:<8} {:<10}",
        "CLI", "sync", "installed", "instructions", "skills", "agents", "startup"
    );
    println!("{}", "-".repeat(77));

    let canonical_agent_count = count_dirs(&paths::project_agents());
    for cli in Cli::ALL {
        let configured = cfg.clis.contains(&cli);
        let installed = adapters::installed(cli);
        let instructions = configured
            && cfg.instructions
            && project_instruction_ok(cli, &root, &agents, &antigravity_rule);
        let skills_state = configured && cfg.skills && project_skills_ok(cli, &root, &skills);
        let agents_state = configured
            && installed
            && cfg.agents
            && count_native_agents(cli, &paths::project_agents_dir(cli)) == canonical_agent_count;
        println!(
            "{:<13} {:<8} {:<10} {:<13} {:<8} {:<8} {:<10}",
            cli.id(),
            yn(configured),
            yn(installed),
            yn(instructions),
            yn(skills_state),
            yn(agents_state),
            yn(startup_ok(cli))
        );
    }

    if !agents.exists() {
        println!();
        println!("Project sync is not ready: run `cli-switch sync` to create AGENTS.md.");
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

fn cli_list(clis: &[Cli]) -> Option<String> {
    if clis.is_empty() {
        None
    } else {
        Some(clis.iter().map(|c| c.id()).collect::<Vec<_>>().join(", "))
    }
}

fn project_instruction_ok(
    cli: Cli,
    root: &std::path::Path,
    agents: &std::path::Path,
    antigravity_rule: &std::path::Path,
) -> bool {
    match cli {
        Cli::Claude => link_ok(&root.join("CLAUDE.md"), agents),
        Cli::Codex | Cli::Opencode | Cli::Copilot => agents.exists(),
        Cli::Kiro => link_ok(
            &root.join(".kiro").join("steering").join("AGENTS.md"),
            agents,
        ),
        Cli::Antigravity => antigravity_rule_ok(antigravity_rule),
    }
}

fn project_skills_ok(cli: Cli, root: &std::path::Path, skills: &std::path::Path) -> bool {
    match cli {
        Cli::Claude => link_ok(&root.join(".claude").join("skills"), skills),
        Cli::Kiro => link_ok(&root.join(".kiro").join("skills"), skills),
        Cli::Codex | Cli::Opencode | Cli::Antigravity | Cli::Copilot => skills.exists(),
    }
}

fn antigravity_rule_state(path: &std::path::Path) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(text) if text.contains("@/AGENTS.md") => "rule",
        Ok(_) => "custom",
        Err(_) => "missing",
    }
}

fn antigravity_rule_ok(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains("@/AGENTS.md"))
        .unwrap_or(false)
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

fn count_native_agents(cli: Cli, dir: &std::path::Path) -> usize {
    let format = match cli {
        Cli::Claude => agents::NativeAgentFormat::Claude,
        Cli::Codex => agents::NativeAgentFormat::Codex,
        Cli::Opencode => agents::NativeAgentFormat::OpenCode,
        Cli::Kiro => agents::NativeAgentFormat::Kiro,
        Cli::Antigravity => agents::NativeAgentFormat::Agy,
        Cli::Copilot => agents::NativeAgentFormat::Copilot,
    };
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    let path = entry.path();
                    let filename = entry.file_name().to_string_lossy().to_string();
                    let id = match cli {
                        Cli::Antigravity => {
                            if !path.join("agent.json").is_file() {
                                return false;
                            }
                            filename
                        }
                        Cli::Claude | Cli::Opencode => {
                            let Some(id) = filename.strip_suffix(".md") else {
                                return false;
                            };
                            id.to_string()
                        }
                        Cli::Codex => {
                            let Some(id) = filename.strip_suffix(".toml") else {
                                return false;
                            };
                            id.to_string()
                        }
                        Cli::Kiro => {
                            let Some(id) = filename.strip_suffix(".json") else {
                                return false;
                            };
                            id.to_string()
                        }
                        Cli::Copilot => {
                            let Some(id) = filename.strip_suffix(".agent.md") else {
                                return false;
                            };
                            id.to_string()
                        }
                    };
                    !format.is_reserved(&id)
                })
                .count()
        })
        .unwrap_or(0)
}

fn startup_state(cli: Cli) -> &'static str {
    if matches!(cli, Cli::Kiro) {
        return kiro_startup_state();
    }
    if matches!(cli, Cli::Antigravity) {
        return antigravity_startup_state();
    }
    let path = match cli {
        Cli::Claude => paths::claude_settings(),
        Cli::Codex => paths::codex_hooks(),
        Cli::Opencode => paths::opencode_plugin(),
        Cli::Copilot => paths::copilot_hook(),
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

fn startup_ok(cli: Cli) -> bool {
    matches!(
        startup_state(cli),
        "hook" | "plugin" | "wrapper" | "mounted" | "written"
    )
}

fn antigravity_startup_state() -> &'static str {
    match std::fs::read_to_string(paths::antigravity_hooks()) {
        Ok(text) if text.contains("cli-switch-sync") && text.contains("PreInvocation") => "hook",
        Ok(_) => "custom",
        Err(_) => "missing",
    }
}

fn kiro_startup_state() -> &'static str {
    // Native agentSpawn hook injected into the default agent wins; otherwise
    // fall back to reporting the shell wrapper state.
    if let Some(path) = mount::kiro_default_agent_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if text.contains("agentSpawn")
                && (text.contains("cli-switch") || text.contains("agent-sync"))
            {
                return "hook";
            }
        }
    }
    shell_startup_state()
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

fn count_synced_skills(cli_dir: &std::path::Path, store_skills: &std::path::Path) -> usize {
    std::fs::read_dir(store_skills)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| same_tree(&cli_dir.join(e.file_name()), &e.path()))
                .count()
        })
        .unwrap_or(0)
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    std::fs::read(a).ok() == std::fs::read(b).ok() && a.exists() && b.exists()
}

fn same_tree(a: &std::path::Path, b: &std::path::Path) -> bool {
    fn files(
        root: &std::path::Path,
    ) -> Option<std::collections::BTreeMap<String, (Vec<u8>, bool)>> {
        fn walk(
            root: &std::path::Path,
            dir: &std::path::Path,
            out: &mut std::collections::BTreeMap<String, (Vec<u8>, bool)>,
        ) -> Option<()> {
            let mut entries = std::fs::read_dir(dir).ok()?.flatten().collect::<Vec<_>>();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out)?;
                } else if path.is_file() {
                    #[cfg(unix)]
                    let executable = {
                        use std::os::unix::fs::PermissionsExt;
                        entry.metadata().ok()?.permissions().mode() & 0o111 != 0
                    };
                    #[cfg(not(unix))]
                    let executable = false;
                    out.insert(
                        path.strip_prefix(root).ok()?.to_string_lossy().to_string(),
                        (std::fs::read(path).ok()?, executable),
                    );
                }
            }
            Some(())
        }
        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out)?;
        Some(out)
    }
    files(a).is_some() && files(a) == files(b)
}

fn hook_tier(cli: Cli) -> &'static str {
    match cli {
        Cli::Claude | Cli::Antigravity | Cli::Copilot => "stable",
        Cli::Codex | Cli::Opencode => "experimental",
        Cli::Kiro => "conditional",
    }
}

fn link_ok(link: &std::path::Path, want: &std::path::Path) -> bool {
    std::fs::read_link(link)
        .map(|target| target == want)
        .unwrap_or(false)
}

fn print_help() {
    println!(
        r#"cli-switch {VERSION} — sync MCP servers, skills, instructions & custom agents across AI CLIs

USAGE:
    cli-switch [command]

COMMANDS:
    configure       Open the setup menu; default when no command is given
    init            Create the canonical store (~/.config/cli-switch) and config
    sync            Transactionally sync MCP, skills, instructions, and agents
        --prune       remove servers gone from every CLI (default: keep + warn)
        --dry-run     show what would change without writing
        --quiet       only print warnings/errors (used by startup hooks)
        --migrate     confirm v0.1 symlink migration to independent copies
    conflicts       List/show/resolve fail-closed synchronization conflicts
    rollback <id>   Restore every path changed by a transaction
    hook             Safe startup entrypoint; emits masked conflict context
    status          Show sync health, conflicts, transactions, and hook tiers
    mount [clis…]   Install startup hooks so each CLI syncs on launch
    help            This message

CLIs: claude, codex, opencode, kiro, antigravity, copilot

Global sync store at ~/.config/cli-switch:
    mcp.json        canonical MCP servers (neutral format)
    AGENTS.md       canonical shared instructions
    skills/         canonical atomic skill directories
    agents/         canonical custom-agent bundles (opt-in)
    config.toml     which CLIs / features to sync

Project sync uses the current directory:
    AGENTS.md       project instructions source of truth
    .agents/skills/ shared project skills
    .cli-switch/agents/ canonical project-agent bundles (opt-in)
    .agents/rules/  Antigravity rule files

Run `cli-switch configure --help` for non-interactive setup options.
"#
    );
}
