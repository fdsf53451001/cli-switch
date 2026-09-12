use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Sandbox {
    root: PathBuf,
    home: PathBuf,
    store: PathBuf,
    project: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("cli-switch-{name}-{}-{unique}", std::process::id()));
        let home = root.join("home");
        let store = root.join("store");
        let project = root.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&project).unwrap();
        Self {
            root,
            home,
            store,
            project,
        }
    }

    fn command(&self, args: &[&str]) -> Output {
        self.command_with_env(args, None)
    }

    fn command_with_env(&self, args: &[&str], extra: Option<(&str, &str)>) -> Output {
        self.command_with_envs(args, &extra.into_iter().collect::<Vec<_>>())
    }

    fn command_with_envs(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cli-switch"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("CLI_SWITCH_HOME", &self.store)
            .env("PATH", "")
            .current_dir(&self.project);
        for (key, value) in extra {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn configure(&self, features: &str) {
        fs::write(
            self.store.join("config.toml"),
            format!("scope = \"global\"\nclis = [\"claude\", \"codex\"]\n[features]\n{features}\n"),
        )
        .unwrap();
    }

    fn install_two_clis(&self) {
        fs::create_dir_all(self.home.join(".codex")).unwrap();
        fs::write(
            self.home.join(".claude.json"),
            r#"{"mcpServers":{"alpha":{"command":"alpha","env":{"TOKEN":"one"}}}}"#,
        )
        .unwrap();
        fs::write(
            self.home.join(".codex/config.toml"),
            "[mcp_servers.beta]\ncommand = \"beta\"\n",
        )
        .unwrap();
        fs::create_dir_all(self.home.join(".claude/skills/demo")).unwrap();
        fs::create_dir_all(self.home.join(".codex/skills/demo")).unwrap();
        fs::write(self.home.join(".claude/CLAUDE.md"), "shared instructions\n").unwrap();
        fs::write(self.home.join(".codex/AGENTS.md"), "shared instructions\n").unwrap();
        fs::write(self.home.join(".claude/skills/demo/SKILL.md"), "# demo\n").unwrap();
        fs::write(self.home.join(".codex/skills/demo/SKILL.md"), "# demo\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for root in [".claude/skills/demo", ".codex/skills/demo"] {
                let script = self.home.join(root).join("scripts/run.sh");
                fs::create_dir_all(script.parent().unwrap()).unwrap();
                fs::write(&script, "#!/bin/sh\necho ok\n").unwrap();
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_same(path_a: impl AsRef<Path>, path_b: impl AsRef<Path>) {
    assert_eq!(fs::read(path_a).unwrap(), fs::read(path_b).unwrap());
}

fn claude_agent(id: &str, prompt: &str) -> String {
    format!("---\nname: {id}\ndescription: Reviews changes\n---\n\n{prompt}\n")
}

#[test]
fn custom_agent_import_fanout_delete_and_rollback_are_transactional() {
    let sandbox = Sandbox::new("agents-global");
    sandbox.configure("mcp = false\nskills = false\ninstructions = false\nagents = true");
    sandbox.install_two_clis();
    fs::create_dir_all(sandbox.home.join(".claude/agents")).unwrap();
    fs::write(
        sandbox.home.join(".claude/agents/reviewer.md"),
        claude_agent("reviewer", "Review carefully."),
    )
    .unwrap();

    let imported = sandbox.command(&["sync"]);
    assert!(imported.status.success(), "{}", text(&imported));
    assert!(sandbox.store.join("agents/reviewer/agent.toml").is_file());
    assert!(sandbox.store.join("agents/reviewer/prompt.md").is_file());
    let codex = sandbox.home.join(".codex/agents/reviewer.toml");
    assert!(codex.is_file());
    assert!(fs::read_to_string(&codex)
        .unwrap()
        .contains("developer_instructions = \"Review carefully.\""));
    let second = sandbox.command(&["sync"]);
    assert!(second.status.success(), "{}", text(&second));
    assert!(text(&second).contains("Already in sync"));

    fs::remove_file(sandbox.home.join(".claude/agents/reviewer.md")).unwrap();
    let deleted = sandbox.command(&["sync"]);
    assert!(deleted.status.success(), "{}", text(&deleted));
    let transaction = text(&deleted)
        .lines()
        .find_map(|line| line.strip_prefix("Transaction: "))
        .unwrap()
        .to_string();
    assert!(!sandbox.store.join("agents/reviewer").exists());
    assert!(!codex.exists());

    let rollback = sandbox.command(&["rollback", &transaction]);
    assert!(rollback.status.success(), "{}", text(&rollback));
    assert!(sandbox.store.join("agents/reviewer/agent.toml").is_file());
    assert!(codex.is_file());
}

/// A field one destination cannot express costs that feature and nothing else:
/// MCP, skills and instructions still land, and the failure names the file.
#[test]
fn untranslatable_agent_skips_only_the_agents_feature() {
    let sandbox = Sandbox::new("agents-isolated");
    sandbox.configure("mcp = true\nskills = true\ninstructions = true\nagents = true");
    sandbox.install_two_clis();
    fs::create_dir_all(sandbox.home.join(".claude/agents")).unwrap();
    fs::write(
        sandbox.home.join(".claude/agents/tooled.md"),
        "---\nname: tooled\ndescription: Runs commands\ntools: [Bash]\n---\n\nDo work.\n",
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    let output = text(&result);
    assert!(output.contains("[skip] agents"), "{output}");
    assert!(output.contains("tooled"), "{output}");
    // Reported paths are native, so compare separator-independently.
    assert!(
        output
            .replace('\\', "/")
            .contains(".claude/agents/tooled.md"),
        "the failure must name its source file: {output}"
    );
    assert!(output.contains("target:"), "{output}");
    assert!(output.contains("Bash=allow"), "{output}");

    let canonical = fs::read_to_string(sandbox.store.join("mcp.json")).unwrap();
    assert!(canonical.contains("alpha") && canonical.contains("beta"));
    assert!(sandbox.store.join("skills/demo/SKILL.md").is_file());
    assert_same(
        sandbox.home.join(".claude/CLAUDE.md"),
        sandbox.home.join(".codex/AGENTS.md"),
    );
    assert!(!sandbox.store.join("agents/tooled").exists());
    assert!(!sandbox.home.join(".codex/agents/tooled.toml").exists());

    let status = sandbox.command(&["status"]);
    assert_eq!(status.status.code(), Some(3), "{}", text(&status));
    let status_text = text(&status);
    assert!(status_text.contains("DEGRADED"), "{status_text}");
    assert!(status_text.contains("not synced: agents"), "{status_text}");
}

/// The opt-in agent feature must not adopt a file the CLI wrote for itself.
#[test]
fn vendor_default_agent_is_never_a_sync_source() {
    let sandbox = Sandbox::new("agents-vendor-default");
    fs::write(
        sandbox.store.join("config.toml"),
        "scope = \"global\"\nclis = [\"claude\", \"kiro\"]\n[features]\nmcp = false\nskills = false\ninstructions = false\nagents = true\n",
    )
    .unwrap();
    fs::write(sandbox.home.join(".claude.json"), "{}").unwrap();
    fs::create_dir_all(sandbox.home.join(".kiro/agents")).unwrap();
    fs::write(
        sandbox.home.join(".kiro/agents/default.json"),
        r#"{"name":"q_ide_default","description":"Default agent configuration","prompt":"","tools":["fs_read"],"allowedTools":["fs_read","execute_bash"],"toolsSettings":{"execute_bash":{"alwaysAllow":[{"preset":"readOnly"}]}}}"#,
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    assert!(!sandbox.store.join("agents/default").exists());
    assert!(!sandbox.home.join(".claude/agents/default.md").exists());
    assert!(!text(&result).contains("[skip]"), "{}", text(&result));
    assert_eq!(sandbox.command(&["status"]).status.code(), Some(0));
}

/// Divergent MCP edits are no reason to stop writing skills, and vice versa.
#[test]
fn conflict_in_one_feature_still_applies_the_others() {
    let sandbox = Sandbox::new("conflict-isolated");
    sandbox.configure("mcp = true\nskills = true\ninstructions = true");
    sandbox.install_two_clis();
    assert!(sandbox.command(&["sync"]).status.success());

    fs::write(sandbox.home.join(".claude/CLAUDE.md"), "claude choice\n").unwrap();
    fs::write(sandbox.home.join(".codex/AGENTS.md"), "codex choice\n").unwrap();
    fs::write(
        sandbox.home.join(".claude.json"),
        r#"{"mcpServers":{"alpha":{"command":"alpha","env":{"TOKEN":"one"}},"gamma":{"command":"gamma"}}}"#,
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert_eq!(result.status.code(), Some(2), "{}", text(&result));

    let canonical = fs::read_to_string(sandbox.store.join("mcp.json")).unwrap();
    assert!(canonical.contains("gamma"), "{canonical}");
    assert!(fs::read_to_string(sandbox.home.join(".codex/config.toml"))
        .unwrap()
        .contains("gamma"));

    assert_eq!(
        fs::read_to_string(sandbox.store.join("AGENTS.md")).unwrap(),
        "shared instructions\n",
        "the conflicted feature must keep its canonical value"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".claude/CLAUDE.md")).unwrap(),
        "claude choice\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".codex/AGENTS.md")).unwrap(),
        "codex choice\n"
    );

    // A partial apply must not erase the record of what it refused to touch.
    let pending: serde_json::Value =
        serde_json::from_slice(&sandbox.command(&["conflicts", "list", "--json"]).stdout).unwrap();
    assert_eq!(pending.as_array().map(Vec::len), Some(1), "{pending}");
}

#[cfg(unix)]
#[test]
fn doctor_lists_every_blocker_in_one_pass() {
    use std::os::unix::fs::symlink;
    let sandbox = Sandbox::new("doctor");
    sandbox.configure("mcp = false\nskills = false\ninstructions = true\nagents = true");
    sandbox.install_two_clis();
    fs::write(sandbox.store.join("AGENTS.md"), "legacy canonical\n").unwrap();
    fs::remove_file(sandbox.home.join(".claude/CLAUDE.md")).unwrap();
    symlink(
        sandbox.store.join("AGENTS.md"),
        sandbox.home.join(".claude/CLAUDE.md"),
    )
    .unwrap();
    fs::create_dir_all(sandbox.home.join(".claude/agents")).unwrap();
    fs::write(
        sandbox.home.join(".claude/agents/tooled.md"),
        "---\nname: tooled\ndescription: Runs commands\ntools: [Bash]\n---\n\nDo work.\n",
    )
    .unwrap();

    let doctor = sandbox.command(&["doctor"]);
    assert_eq!(doctor.status.code(), Some(2), "{}", text(&doctor));
    let output = text(&doctor);
    // Three unrelated blockers, all surfaced by one command.
    assert!(output.contains("Blockers: 3"), "{output}");
    assert!(output.contains("[migration]"), "{output}");
    assert!(output.contains("[translate] agents/tooled"), "{output}");
    assert!(
        output.contains("[conflict] instructions/global"),
        "{output}"
    );
    assert!(
        output.contains("no sync has been recorded yet"),
        "health must not be implied from the filesystem: {output}"
    );
}

#[test]
fn project_agents_use_independent_canonical_and_native_paths() {
    let sandbox = Sandbox::new("agents-project");
    sandbox.install_two_clis();
    fs::create_dir_all(sandbox.project.join(".cli-switch")).unwrap();
    fs::write(
        sandbox.project.join(".cli-switch/config.toml"),
        "scope = \"project\"\nclis = [\"claude\", \"codex\"]\n[features]\nmcp = false\nskills = false\ninstructions = false\nagents = true\n",
    )
    .unwrap();
    fs::create_dir_all(sandbox.project.join(".claude/agents")).unwrap();
    fs::write(
        sandbox.project.join(".claude/agents/project-reviewer.md"),
        claude_agent("project-reviewer", "Review only this project."),
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    assert!(sandbox
        .project
        .join(".cli-switch/agents/project-reviewer/agent.toml")
        .is_file());
    assert!(sandbox
        .project
        .join(".codex/agents/project-reviewer.toml")
        .is_file());
    assert!(!sandbox.store.join("agents/project-reviewer").exists());
}

#[test]
fn first_sync_is_transactional_and_second_sync_is_idempotent() {
    let sandbox = Sandbox::new("initial");
    sandbox.configure("mcp = true\nskills = true\ninstructions = true");
    sandbox.install_two_clis();

    let first = sandbox.command(&["sync"]);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains("Transaction: tx-"));
    let canonical = fs::read_to_string(sandbox.store.join("mcp.json")).unwrap();
    assert!(canonical.contains("alpha") && canonical.contains("beta"));
    assert_same(
        sandbox.home.join(".claude/CLAUDE.md"),
        sandbox.home.join(".codex/AGENTS.md"),
    );
    assert!(
        !fs::symlink_metadata(sandbox.home.join(".claude/CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_same(
        sandbox.home.join(".claude/skills/demo/SKILL.md"),
        sandbox.home.join(".codex/skills/demo/SKILL.md"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(sandbox.home.join(".codex/skills/demo/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0);
    }

    let state_before = fs::read(sandbox.store.join("state/sync-state-v2.json")).unwrap();
    let second = sandbox.command(&["sync"]);
    assert!(second.status.success(), "{}", text(&second));
    assert!(
        text(&second).contains("Already in sync"),
        "{}",
        text(&second)
    );
    assert_eq!(
        fs::read(sandbox.store.join("state/sync-state-v2.json")).unwrap(),
        state_before
    );
}

#[test]
fn conflict_stops_all_writes_and_explicit_resolution_applies() {
    let sandbox = Sandbox::new("conflict");
    sandbox.configure("mcp = true\nskills = true\ninstructions = true");
    sandbox.install_two_clis();
    assert!(sandbox.command(&["sync"]).status.success());

    fs::write(sandbox.home.join(".claude/CLAUDE.md"), "claude choice\n").unwrap();
    fs::write(sandbox.home.join(".codex/AGENTS.md"), "codex choice\n").unwrap();
    let canonical_before = fs::read(sandbox.store.join("AGENTS.md")).unwrap();
    let conflict = sandbox.command(&["sync"]);
    assert!(!conflict.status.success());
    assert_eq!(conflict.status.code(), Some(2));
    assert!(text(&conflict).contains("no files were changed"));
    assert_eq!(
        fs::read(sandbox.store.join("AGENTS.md")).unwrap(),
        canonical_before
    );

    let listed = sandbox.command(&["conflicts", "list", "--json"]);
    assert!(listed.status.success());
    let records: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let id = records[0]["id"].as_str().unwrap();
    let hook = sandbox.command(&["hook", "--json"]);
    assert!(hook.status.success(), "{}", text(&hook));
    let hook_packet: serde_json::Value = serde_json::from_slice(&hook.stdout).unwrap();
    assert_eq!(hook_packet["requires_user"], true);
    let shown =
        String::from_utf8(sandbox.command(&["conflicts", "show", id, "--json"]).stdout).unwrap();
    assert!(shown.contains("claude choice"));

    let resolved = sandbox.command(&["conflicts", "resolve", id, "--source", "claude"]);
    assert!(resolved.status.success(), "{}", text(&resolved));
    let resolved_text = text(&resolved);
    let transaction = resolved_text
        .lines()
        .find_map(|line| line.strip_prefix("Transaction: "))
        .unwrap()
        .to_string();
    assert_eq!(
        fs::read_to_string(sandbox.store.join("AGENTS.md")).unwrap(),
        "claude choice\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".codex/AGENTS.md")).unwrap(),
        "claude choice\n"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &sandbox.command(&["conflicts", "list", "--json"]).stdout
        )
        .unwrap(),
        serde_json::json!([])
    );

    let rolled_back = sandbox.command(&["rollback", &transaction]);
    assert!(rolled_back.status.success(), "{}", text(&rolled_back));
    assert_eq!(
        fs::read_to_string(sandbox.store.join("AGENTS.md")).unwrap(),
        "shared instructions\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".claude/CLAUDE.md")).unwrap(),
        "claude choice\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".codex/AGENTS.md")).unwrap(),
        "codex choice\n"
    );
}

#[test]
fn dry_run_does_not_create_scaffold_state_or_targets() {
    let sandbox = Sandbox::new("dry-run");
    sandbox.configure("mcp = true\nskills = false\ninstructions = false");
    sandbox.install_two_clis();
    let before_claude = fs::read(sandbox.home.join(".claude.json")).unwrap();
    let result = sandbox.command(&["sync", "--dry-run"]);
    assert!(result.status.success(), "{}", text(&result));
    assert_eq!(
        fs::read(sandbox.home.join(".claude.json")).unwrap(),
        before_claude
    );
    assert!(!sandbox.store.join("mcp.json").exists());
    assert!(!sandbox.store.join("state").exists());
}

#[test]
fn injected_apply_failure_rolls_every_managed_path_back() {
    let sandbox = Sandbox::new("rollback-on-failure");
    sandbox.configure("mcp = true\nskills = true\ninstructions = true");
    sandbox.install_two_clis();
    let claude_before = fs::read(sandbox.home.join(".claude.json")).unwrap();
    let codex_before = fs::read(sandbox.home.join(".codex/config.toml")).unwrap();
    let instructions_before = fs::read(sandbox.home.join(".claude/CLAUDE.md")).unwrap();

    let failed = sandbox.command_with_env(&["sync"], Some(("CLI_SWITCH_TEST_FAIL_AFTER", "2")));
    assert!(!failed.status.success());
    assert!(text(&failed).contains("was rolled back"));
    assert_eq!(
        fs::read(sandbox.home.join(".claude.json")).unwrap(),
        claude_before
    );
    assert_eq!(
        fs::read(sandbox.home.join(".codex/config.toml")).unwrap(),
        codex_before
    );
    assert_eq!(
        fs::read(sandbox.home.join(".claude/CLAUDE.md")).unwrap(),
        instructions_before
    );
    assert!(!sandbox.store.join("state/sync-state-v2.json").exists());
}

#[test]
fn enabling_a_feature_later_does_not_silently_choose_canonical() {
    let sandbox = Sandbox::new("feature-enable");
    sandbox.configure("mcp = true\nskills = false\ninstructions = false");
    sandbox.install_two_clis();
    fs::write(sandbox.home.join(".claude/CLAUDE.md"), "claude only\n").unwrap();
    fs::write(sandbox.home.join(".codex/AGENTS.md"), "codex only\n").unwrap();
    assert!(sandbox.command(&["sync"]).status.success());

    sandbox.configure("mcp = true\nskills = false\ninstructions = true");
    let enabled = sandbox.command(&["sync"]);
    assert_eq!(enabled.status.code(), Some(2), "{}", text(&enabled));
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".claude/CLAUDE.md")).unwrap(),
        "claude only\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home.join(".codex/AGENTS.md")).unwrap(),
        "codex only\n"
    );
}

#[cfg(unix)]
#[test]
fn legacy_symlink_requires_explicit_migration() {
    use std::os::unix::fs::symlink;
    let sandbox = Sandbox::new("migration");
    sandbox.configure("mcp = false\nskills = false\ninstructions = true");
    sandbox.install_two_clis();
    fs::write(sandbox.store.join("AGENTS.md"), "legacy canonical\n").unwrap();
    fs::remove_file(sandbox.home.join(".claude/CLAUDE.md")).unwrap();
    symlink(
        sandbox.store.join("AGENTS.md"),
        sandbox.home.join(".claude/CLAUDE.md"),
    )
    .unwrap();

    let refused = sandbox.command(&["sync", "--quiet"]);
    assert!(!refused.status.success());
    assert!(fs::symlink_metadata(sandbox.home.join(".claude/CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());

    fs::write(sandbox.home.join(".codex/AGENTS.md"), "legacy canonical\n").unwrap();
    let migrated = sandbox.command(&["sync", "--migrate"]);
    assert!(migrated.status.success(), "{}", text(&migrated));
    assert!(
        !fs::symlink_metadata(sandbox.home.join(".claude/CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
fn configure_project(sandbox: &Sandbox, features: &str) {
    fs::create_dir_all(sandbox.project.join(".cli-switch")).unwrap();
    fs::write(
        sandbox.project.join(".cli-switch/config.toml"),
        format!("scope = \"project\"\nclis = [\"claude\", \"codex\"]\n[features]\n{features}\n",),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn project_contradictory_instructions_with_shared_heading_are_never_merged() {
    let sandbox = Sandbox::new("project-contradiction");
    configure_project(
        &sandbox,
        "mcp = false\nskills = false\ninstructions = true\nagents = false",
    );
    let original = "# Rules\nAlways run tests.\n";
    let native = "# Rules\nNever run tests.\n";
    fs::write(sandbox.project.join("AGENTS.md"), original).unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), native).unwrap();
    let result = sandbox.command(&["sync", "--quiet"]);
    assert_eq!(result.status.code(), Some(2), "{}", text(&result));
    assert_eq!(
        fs::read_to_string(sandbox.project.join("AGENTS.md")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(sandbox.project.join("CLAUDE.md")).unwrap(),
        native
    );
    assert!(!fs::symlink_metadata(sandbox.project.join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!sandbox.project.join(".gitignore").exists());
    let health: serde_json::Value =
        serde_json::from_slice(&fs::read(sandbox.store.join("state/last-sync.json")).unwrap())
            .unwrap();
    assert_eq!(health["result"], "conflicts");
    assert_eq!(health["conflicts"], 1);
    assert_eq!(health["applied"], 0);
    assert_eq!(sandbox.command(&["status"]).status.code(), Some(2));
    let doctor = sandbox.command(&["doctor"]);
    assert_eq!(doctor.status.code(), Some(2));
    assert!(text(&doctor).contains("cannot be auto-merged"));
    let hook = sandbox.command(&["hook", "--json"]);
    assert!(hook.status.success());
    let value: serde_json::Value = serde_json::from_slice(&hook.stdout).unwrap();
    assert_eq!(value["requires_user"], true);
    assert!(!value["project_conflicts"].as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn project_link_claude_md_with_no_common_anchor_is_reported_as_conflict() {
    let sandbox = Sandbox::new("project-merge-conflict");
    sandbox.install_two_clis();
    configure_project(
        &sandbox,
        "mcp = false\nskills = false\ninstructions = true\nagents = false",
    );
    fs::write(
        sandbox.project.join("AGENTS.md"),
        "completely different content A\n",
    )
    .unwrap();
    fs::write(
        sandbox.project.join("CLAUDE.md"),
        "totally unrelated content B\n",
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert_eq!(result.status.code(), Some(2), "{}", text(&result));
    assert!(
        !fs::symlink_metadata(sandbox.project.join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "CLAUDE.md should not have been replaced on merge failure"
    );
    assert_eq!(
        fs::read_to_string(sandbox.project.join("AGENTS.md")).unwrap(),
        "completely different content A\n",
        "canonical must not change when merge fails"
    );
    assert_eq!(
        fs::read_to_string(sandbox.project.join("CLAUDE.md")).unwrap(),
        "totally unrelated content B\n",
        "native file must not change when merge fails"
    );
    assert!(text(&result).contains("cannot be auto-merged"));
}

#[cfg(unix)]
#[test]
fn project_sync_maintains_gitignore_for_cli_private_dirs() {
    let sandbox = Sandbox::new("project-gitignore");
    sandbox.install_two_clis();
    configure_project(
        &sandbox,
        "mcp = false\nskills = false\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "# instructions\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "# instructions\n").unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));

    let gi = fs::read_to_string(sandbox.project.join(".gitignore")).unwrap();
    assert!(gi.contains(".claude/"));
    assert!(gi.contains(".cli-switch/"));
    assert!(
        !gi.contains(".agents/"),
        "canonical .agents/ must stay tracked"
    );
    assert!(
        !gi.contains("AGENTS.md"),
        "canonical AGENTS.md must stay tracked"
    );

    // Second run is a no-op for .gitignore.
    let second = sandbox.command(&["sync"]);
    assert!(second.status.success(), "{}", text(&second));
    assert_eq!(
        fs::read_to_string(sandbox.project.join(".gitignore")).unwrap(),
        gi,
        ".gitignore must not gain duplicate entries on re-sync"
    );
}

#[cfg(unix)]
#[test]
fn project_symlinks_are_relative_to_the_link_location() {
    use std::os::unix::fs::symlink;
    let sandbox = Sandbox::new("project-relative-links");
    sandbox.install_two_clis();
    configure_project(
        &sandbox,
        "mcp = false\nskills = true\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "# instructions\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "# instructions\n").unwrap();
    fs::create_dir_all(sandbox.project.join(".agents/skills/demo")).unwrap();
    fs::write(
        sandbox.project.join(".agents/skills/demo/SKILL.md"),
        "# demo\n",
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));

    let claude_link = fs::read_link(sandbox.project.join("CLAUDE.md")).unwrap();
    assert!(
        claude_link.is_relative(),
        "CLAUDE.md symlink must be relative, got {}",
        claude_link.display()
    );
    assert_eq!(claude_link, std::path::Path::new("AGENTS.md"));

    let skills_link = fs::read_link(sandbox.project.join(".claude/skills")).unwrap();
    assert!(
        skills_link.is_relative(),
        ".claude/skills symlink must be relative, got {}",
        skills_link.display()
    );
    assert_eq!(skills_link, std::path::Path::new("../.agents/skills"));

    // Re-running detects the relative symlink as already correct (idempotent).
    let second = sandbox.command(&["sync"]);
    assert!(second.status.success(), "{}", text(&second));
    assert!(text(&second).contains("already linked"));

    // An older absolute symlink is auto-rewritten to relative on the next sync.
    fs::remove_file(sandbox.project.join("CLAUDE.md")).unwrap();
    symlink(
        sandbox.project.join("AGENTS.md"),
        sandbox.project.join("CLAUDE.md"),
    )
    .unwrap();
    let third = sandbox.command(&["sync"]);
    assert!(third.status.success(), "{}", text(&third));
    assert!(
        text(&third).contains("relinked"),
        "an absolute symlink must be rewritten to relative"
    );
    let after = fs::read_link(sandbox.project.join("CLAUDE.md")).unwrap();
    assert!(
        after.is_relative(),
        "CLAUDE.md symlink must now be relative, got {}",
        after.display()
    );
    assert_eq!(after, std::path::Path::new("AGENTS.md"));
}

#[cfg(unix)]
#[test]
fn project_sync_adds_kiro_to_gitignore_when_kiro_enabled() {
    let sandbox = Sandbox::new("project-gitignore-kiro");
    sandbox.install_two_clis();
    fs::create_dir_all(sandbox.project.join(".cli-switch")).unwrap();
    fs::write(
        sandbox.project.join(".cli-switch/config.toml"),
        "scope = \"project\"\nclis = [\"claude\", \"kiro\"]\n[features]\nmcp = false\nskills = false\ninstructions = true\nagents = false\n",
    )
    .unwrap();
    fs::write(sandbox.project.join("AGENTS.md"), "# instructions\n").unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));

    let gi = fs::read_to_string(sandbox.project.join(".gitignore")).unwrap();
    assert!(gi.contains(".claude/"));
    assert!(gi.contains(".kiro/"));
    assert!(gi.contains(".cli-switch/"));
    // Existing user line must survive — we only append, never rewrite.
    let with_user = {
        fs::write(sandbox.project.join(".gitignore"), "/node_modules/\n").unwrap();
        sandbox.command(&["sync"])
    };
    assert!(with_user.status.success(), "{}", text(&with_user));
    let gi2 = fs::read_to_string(sandbox.project.join(".gitignore")).unwrap();
    assert!(gi2.contains("/node_modules/"));
    assert!(gi2.contains(".claude/"));
    assert!(gi2.contains(".kiro/"));
    assert!(gi2.contains(".cli-switch/"));
}

#[cfg(unix)]
#[test]
fn project_preflight_failure_leaves_all_files_unchanged() {
    let sandbox = Sandbox::new("project-preflight");
    configure_project(
        &sandbox,
        "skills = true\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "identical\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "identical\n").unwrap();
    fs::write(sandbox.project.join(".claude"), "not a directory").unwrap();
    let result = sandbox.command(&["sync"]);
    assert_eq!(result.status.code(), Some(1), "{}", text(&result));
    assert_eq!(
        fs::read_to_string(sandbox.project.join("AGENTS.md")).unwrap(),
        "identical\n"
    );
    assert!(fs::symlink_metadata(sandbox.project.join("CLAUDE.md"))
        .unwrap()
        .is_file());
    assert!(!sandbox.project.join(".agents").exists());
    assert!(!sandbox.project.join(".gitignore").exists());
}

#[cfg(unix)]
#[test]
fn project_write_failure_restores_original_native_file() {
    let sandbox = Sandbox::new("project-rollback");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "identical\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "identical\n").unwrap();
    let result = sandbox.command_with_env(&["sync"], Some(("CLI_SWITCH_TEST_FAIL_AFTER", "1")));
    assert_eq!(result.status.code(), Some(1), "{}", text(&result));
    assert!(text(&result).contains("was rolled back"));
    assert!(fs::symlink_metadata(sandbox.project.join("CLAUDE.md"))
        .unwrap()
        .is_file());
    assert_eq!(
        fs::read_to_string(sandbox.project.join("CLAUDE.md")).unwrap(),
        "identical\n"
    );
    assert!(!sandbox.project.join(".gitignore").exists());
}

#[cfg(unix)]
#[test]
fn project_transaction_can_be_explicitly_rolled_back() {
    let sandbox = Sandbox::new("project-explicit-rollback");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "identical\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "identical\n").unwrap();
    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    let health: serde_json::Value =
        serde_json::from_slice(&fs::read(sandbox.store.join("state/last-sync.json")).unwrap())
            .unwrap();
    assert_eq!(health["applied"], 2);
    let id = health["transaction"].as_str().unwrap();
    let rollback = sandbox.command(&["rollback", id]);
    assert!(rollback.status.success(), "{}", text(&rollback));
    assert!(fs::symlink_metadata(sandbox.project.join("CLAUDE.md"))
        .unwrap()
        .is_file());
    assert_eq!(
        fs::read_to_string(sandbox.project.join("CLAUDE.md")).unwrap(),
        "identical\n"
    );
    assert!(!sandbox.project.join(".gitignore").exists());
}

#[cfg(unix)]
#[test]
fn failed_recovery_is_reported_and_original_journal_is_retained() {
    let sandbox = Sandbox::new("recovery-failure");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = true\nagents = false",
    );
    fs::write(sandbox.project.join("AGENTS.md"), "original\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "original\n").unwrap();
    let result = sandbox.command_with_envs(
        &["sync"],
        &[
            ("CLI_SWITCH_TEST_FAIL_AFTER", "1"),
            ("CLI_SWITCH_TEST_FAIL_RESTORE", "1"),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    let output = text(&result);
    assert!(output.contains("recovery incomplete"), "{output}");
    assert!(!output.contains("was rolled back"));
    let dirs: Vec<_> = fs::read_dir(sandbox.store.join("state/transactions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1);
    let journal = &dirs[0];
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(journal.join("journal.json")).unwrap()).unwrap();
    assert_eq!(original["entries"][0]["before"]["kind"], "file");
    assert!(!journal.join("committed").exists());
    assert!(!journal.join("rolled-back").exists());
    // Subsequent successful syncs must not prune an incomplete recovery.
    for i in 0..12 {
        fs::write(
            sandbox.project.join(".gitignore"),
            format!("# revision {i}\n"),
        )
        .unwrap();
        let next = sandbox.command(&["sync"]);
        assert!(next.status.success(), "{}", text(&next));
    }
    assert!(journal.join("journal.json").exists());
}

#[cfg(unix)]
#[test]
fn project_adopts_one_native_file_without_placeholder_pollution() {
    let sandbox = Sandbox::new("project-adopt");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = true\nagents = false",
    );
    fs::write(
        sandbox.project.join("CLAUDE.md"),
        "# Native instructions\nKeep this intact.\n",
    )
    .unwrap();
    let dry = sandbox.command(&["sync", "--dry-run"]);
    assert!(dry.status.success(), "{}", text(&dry));
    assert!(!sandbox.project.join("AGENTS.md").exists());
    assert!(!sandbox.store.exists() || !sandbox.store.join("state").exists());
    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    assert_eq!(
        fs::read_to_string(sandbox.project.join("AGENTS.md")).unwrap(),
        "# Native instructions\nKeep this intact.\n"
    );
}

#[test]
fn disabled_project_mappings_create_no_instruction_or_skill_files() {
    let sandbox = Sandbox::new("project-disabled");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = false\nagents = false",
    );
    assert!(sandbox.command(&["sync"]).status.success());
    assert!(!sandbox.project.join("AGENTS.md").exists());
    assert!(!sandbox.project.join(".agents").exists());
    assert!(!sandbox.project.join(".gitignore").exists());
}

#[cfg(unix)]
#[test]
fn unrelated_broken_symlink_is_not_retargeted() {
    let sandbox = Sandbox::new("project-foreign-link");
    configure_project(
        &sandbox,
        "skills = false\ninstructions = true\nagents = false",
    );
    std::os::unix::fs::symlink("other-missing.md", sandbox.project.join("CLAUDE.md")).unwrap();
    let result = sandbox.command(&["sync"]);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(
        fs::read_link(sandbox.project.join("CLAUDE.md")).unwrap(),
        PathBuf::from("other-missing.md")
    );
    assert!(!sandbox.project.join("AGENTS.md").exists());
}

#[test]
fn sync_and_rollback_respect_a_live_process_lock() {
    let sandbox = Sandbox::new("live-lock");
    let lock_path = sandbox.store.join(".lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    file.lock().unwrap();
    let old = SystemTime::now() - std::time::Duration::from_secs(3600);
    file.set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
    assert!(text(&result).contains("in progress"));
    assert!(!sandbox.store.join("state/last-sync.json").exists());
    let rollback = sandbox.command(&["rollback", "any-id"]);
    assert_eq!(rollback.status.code(), Some(1));
    assert!(text(&rollback).contains("in progress"));
    drop(file);
    assert!(sandbox.command(&["sync"]).status.success());
    assert!(lock_path.exists());
}

#[test]
fn unreadable_instruction_is_not_propagated_as_a_deletion() {
    let sandbox = Sandbox::new("unreadable-instruction");
    sandbox.configure("mcp = true\nskills = false\ninstructions = true");
    sandbox.install_two_clis();
    let first = sandbox.command(&["sync"]);
    assert!(first.status.success(), "{}", text(&first));
    let canonical = fs::read(sandbox.store.join("AGENTS.md")).unwrap();
    let path = sandbox.home.join(".claude/CLAUDE.md");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("keep"), "do not delete").unwrap();
    let next = sandbox.command(&["sync"]);
    assert!(next.status.success(), "{}", text(&next));
    assert!(text(&next).contains("instructions"));
    assert_eq!(
        fs::read(sandbox.store.join("AGENTS.md")).unwrap(),
        canonical
    );
    assert_eq!(
        fs::read(sandbox.home.join(".codex/AGENTS.md")).unwrap(),
        canonical
    );
    assert_eq!(fs::read(path.join("keep")).unwrap(), b"do not delete");
    let health: serde_json::Value =
        serde_json::from_slice(&fs::read(sandbox.store.join("state/last-sync.json")).unwrap())
            .unwrap();
    assert_eq!(health["result"], "degraded");
}

#[test]
fn project_failure_keeps_the_record_of_an_already_committed_global_sync() {
    let sandbox = Sandbox::new("partial-scope-report");
    sandbox.configure("mcp = true\nskills = false\ninstructions = false");
    sandbox.install_two_clis();
    configure_project(
        &sandbox,
        "skills = true\ninstructions = false\nagents = false",
    );
    fs::write(sandbox.project.join(".claude"), "not a directory").unwrap();
    let result = sandbox.command(&["sync"]);
    assert_eq!(result.status.code(), Some(1));
    let health: serde_json::Value =
        serde_json::from_slice(&fs::read(sandbox.store.join("state/last-sync.json")).unwrap())
            .unwrap();
    assert_eq!(health["result"], "failed");
    assert!(health["applied"].as_u64().unwrap() > 0);
    assert!(sandbox
        .store
        .join("state/transactions")
        .join(health["transaction"].as_str().unwrap())
        .join("journal.json")
        .exists());
}
