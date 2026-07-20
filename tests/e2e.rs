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
        let mut command = Command::new(env!("CARGO_BIN_EXE_cli-switch"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("CLI_SWITCH_HOME", &self.store)
            .env("PATH", "")
            .current_dir(&self.project);
        if let Some((key, value)) = extra {
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

fn configure_project(sandbox: &Sandbox, features: &str) {
    fs::create_dir_all(sandbox.project.join(".cli-switch")).unwrap();
    fs::write(
        sandbox.project.join(".cli-switch/config.toml"),
        format!(
            "scope = \"project\"\nclis = [\"claude\", \"codex\"]\n[features]\n{features}\n",
        ),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn project_link_existing_claude_md_file_is_merged_into_agents_then_symlinked() {
    let sandbox = Sandbox::new("project-merge");
    sandbox.install_two_clis();
    configure_project(&sandbox, "mcp = false\nskills = false\ninstructions = true\nagents = false");
    fs::write(
        sandbox.project.join("AGENTS.md"),
        "# Shared\n\nLine only in AGENTS.md.\n",
    )
    .unwrap();
    fs::write(
        sandbox.project.join("CLAUDE.md"),
        "# Shared\n\nLine only in CLAUDE.md.\n",
    )
    .unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));

    // CLAUDE.md is now a symlink to AGENTS.md.
    let claude_meta = fs::symlink_metadata(sandbox.project.join("CLAUDE.md")).unwrap();
    assert!(
        claude_meta.file_type().is_symlink(),
        "CLAUDE.md should be a symlink after merge"
    );
    // Both paths resolve to the same file.
    assert_eq!(
        fs::canonicalize(sandbox.project.join("CLAUDE.md")).unwrap(),
        fs::canonicalize(sandbox.project.join("AGENTS.md")).unwrap(),
    );

    let merged = fs::read_to_string(sandbox.project.join("AGENTS.md")).unwrap();
    assert!(merged.contains("Line only in AGENTS.md."));
    assert!(merged.contains("Line only in CLAUDE.md."));
    assert_eq!(merged.matches("# Shared").count(), 1, "shared anchor duplicated");
    assert!(text(&result).contains("merged existing"));
}

#[cfg(unix)]
#[test]
fn project_link_claude_md_with_no_common_anchor_is_reported_as_conflict() {
    let sandbox = Sandbox::new("project-merge-conflict");
    sandbox.install_two_clis();
    configure_project(&sandbox, "mcp = false\nskills = false\ninstructions = true\nagents = false");
    fs::write(sandbox.project.join("AGENTS.md"), "completely different content A\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "totally unrelated content B\n").unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));
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
    configure_project(&sandbox, "mcp = false\nskills = false\ninstructions = true\nagents = false");
    fs::write(sandbox.project.join("AGENTS.md"), "# instructions\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "# instructions\n").unwrap();

    let result = sandbox.command(&["sync"]);
    assert!(result.status.success(), "{}", text(&result));

    let gi = fs::read_to_string(sandbox.project.join(".gitignore")).unwrap();
    assert!(gi.contains(".claude/"));
    assert!(gi.contains(".cli-switch/"));
    assert!(!gi.contains(".agents/"), "canonical .agents/ must stay tracked");
    assert!(!gi.contains("AGENTS.md"), "canonical AGENTS.md must stay tracked");

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
    configure_project(&sandbox, "mcp = false\nskills = true\ninstructions = true\nagents = false");
    fs::write(sandbox.project.join("AGENTS.md"), "# instructions\n").unwrap();
    fs::write(sandbox.project.join("CLAUDE.md"), "# instructions\n").unwrap();
    fs::create_dir_all(sandbox.project.join(".agents/skills/demo")).unwrap();
    fs::write(sandbox.project.join(".agents/skills/demo/SKILL.md"), "# demo\n").unwrap();

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
