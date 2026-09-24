# cli-switch

Keep your **MCP servers, skills, instructions, and custom agents** in sync across multiple AI CLIs — automatically.

Supports: **Claude Code · Codex · opencode · Kiro · Antigravity CLI (`agy`) · GitHub Copilot CLI (`copilot`)**

**Project instruction safety:** different `AGENTS.md` and native instruction files now stop with a conflict, even if they share headings. Review and reconcile their contents before syncing; cli-switch no longer combines their lines automatically.

---

## The Problem

You're managing MCP servers, skills, instruction files, and reusable agents separately in multiple AI CLIs, all with incompatible formats. `cli-switch` gives you a single place to edit, and syncs everything everywhere.

---

## Install

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/fdsf53451001/cli-switch/main/install.sh | bash
```

Pre-built binaries for macOS (Apple Silicon / Intel), Linux x86_64, and Windows x86_64. No Rust required.

### Update

Run the installer again; configuration and snapshots are preserved:

```bash
curl -fsSL https://raw.githubusercontent.com/fdsf53451001/cli-switch/main/install.sh | bash
cli-switch --version
```

To install an exact release instead of `latest`:

```bash
curl -fsSL https://raw.githubusercontent.com/fdsf53451001/cli-switch/main/install.sh \
  | CLI_SWITCH_VERSION=v0.3.0 bash
```

---

## Quick Start

```bash
cli-switch
```

This opens the interactive menu. Follow the steps:

1. **Setup CLI** — select which CLIs you use
2. **Set global level** — enable sync for your user-level config
3. **Set project level** — enable sync for the current project directory

That's it. Edit the canonical store or any managed CLI copy — `cli-switch sync` performs a three-way comparison and propagates conflict-free changes as one transaction.

### Windows

From PowerShell:

```powershell
irm https://raw.githubusercontent.com/fdsf53451001/cli-switch/main/install.ps1 | iex
```

The same command updates an existing Windows installation. For an exact release:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/fdsf53451001/cli-switch/main/install.ps1))) -Version v0.3.0
```

The Windows installer places `cli-switch.exe` in `~/.local/bin`. Core synchronization is supported on macOS, Linux, and Windows; startup-hook support is reported separately by `cli-switch status`.

---

## Commands

```bash
cli-switch           # Interactive menu
cli-switch sync      # Sync now
cli-switch status    # Health of the last sync, plus per-CLI target state
cli-switch doctor    # Every blocker standing in the next sync's way, at once
cli-switch mount     # Hook cli-switch into each CLI's startup
cli-switch conflicts list
cli-switch conflicts show <id> --json
cli-switch conflicts resolve <id> --source <source>
cli-switch rollback <transaction-id>
```

---

## How It Works

- **MCP servers** — edit once in `~/.config/cli-switch/mcp.json`; converted to each CLI's native format on sync
- **Global instructions** — independent native files compared with the last successful snapshot
- **Skills** — each skill directory is synchronized as one atomic unit
- **Custom agents** — opt-in, direct native files (no plugin or MCP control plane), with portable core fields and namespaced native extensions
- **Bidirectional** — changes made inside any CLI are merged back on the next sync
- **Fail closed** — divergent edits create a conflict packet and leave that feature's managed files untouched
- **Global feature isolation** — a problem in one global feature never stops the other three; the failing feature is skipped, reported with the file and field that caused it, and its snapshot is left un-advanced so the change is re-detected next run
- **Transactional per pass** — global sync, project agents, and project instruction/skill mappings each have their own journal. A failed write restores that pass's modified files; earlier committed passes remain applied and are recorded in sync health. Successful transactions can be explicitly restored.
- **Concurrent edits** — source changes during planning or destination changes detected before writing abort the pass. Sync, rollback, conflict-resolution writes, and hook installation/removal share an OS-held lock; long-running operations do not lose their lock after a timeout.
- **Recovery evidence** — every restoration is checked. If recovery fails or a file has been edited externally, the error names the retained journal and reports incomplete recovery. Journals without a completion marker are excluded from automatic pruning.

`cli-switch status` reports the recorded result of the **last sync attempt** and checks for current blockers. A failed pass can leave target paths looking healthy; if an earlier pass already committed, its applied count and transaction remain in the health record. `status` and `doctor` exit `0` only when the last sync succeeded and nothing blocks the next one; `3` means degraded, `2` means unresolved conflicts.

Conflict JSON always masks MCP environment and header values. Discuss the packet with your AI CLI, choose a source, then run the explicit `conflicts resolve` command. Startup hooks apply only conflict-free plans.

### Project-level sync

Run `cli-switch` inside a project directory and choose **Set project level** to sync that project's instructions, skills, and optionally custom agents across CLIs. Uses `AGENTS.md`, `.agents/skills/`, and `.cli-switch/agents/` as the shared sources. Global and project agent snapshots are independent.

Claude Code, Codex, opencode, and Copilot read the project `AGENTS.md` directly, so no native instruction file is created for them. A `CLAUDE.md -> AGENTS.md` symlink left by an older release is removed on the next sync so Claude does not load the same instructions twice; a real `CLAUDE.md` is treated as Claude-only instructions and left untouched.

Project instruction/skill mappings are planned together before any managed file is changed, including `.gitignore`. Identical native instruction files can become relative symlinks; an absent `AGENTS.md` can adopt matching native content. Different content, unrelated symlinks, and existing native skill directories require manual reconciliation. A conflict leaves the mapping pass untouched, exits `2`, and appears in `status`, `doctor`, and hook diagnostics. Disabling both mappings creates no project instruction or skill scaffolding.

To inspect a project instruction conflict:

```bash
cli-switch sync --dry-run
git diff --no-index -- AGENTS.md .kiro/steering/AGENTS.md
```

Review and edit the files into the intended shared content, then run `cli-switch sync`. The diff command exits `1` when the files differ. Project mapping conflicts are resolved by reconciling files; `conflicts resolve` handles the snapshot-based global/custom-agent conflict packets.

Successful project mapping runs print a transaction ID and record it in sync health; the existing `rollback` command can restore that transaction. On Windows, project symlink creation requires permission to create symlinks. A failure rolls the mapping pass back rather than silently substituting copies.

Rollback protects files changed after the transaction. If automatic recovery reports **recovery incomplete**, preserve the named `journal.json`: it contains the original file data. Restore the affected paths manually after addressing the reported error; the normal rollback command does not force-overwrite externally changed or partially recovered files. Automatic rollback handles returned write errors; interrupted processes or power loss can require manual recovery from the journal.


### Custom-agent sync

Agent sync is deliberately disabled on upgrade. Enable it interactively or with:

```bash
cli-switch configure --scope global --agents --yes
cli-switch configure --scope project --agents --yes
```

Canonical agents live at `~/.config/cli-switch/agents/<id>/` with `agent.toml`, `prompt.md`, and optional `extensions/<cli>.json`. They are rendered directly to each CLI's native agent directory. Deleting a previously snapshotted custom agent propagates without requiring `--prune`; an absent agent on first adoption does not count as deletion.

A CLI's own auto-generated agents are never a sync source. Reserved ids and names — including Kiro/Amazon Q's `default.json` / `q_ide_default` — are skipped on read and never written to: opting into agent sync means sharing *your* agents, not adopting whatever default file a vendor writes on first launch.

Missing skill/MCP references, unsupported permission translations, malformed native files, and divergent edits all fail closed before any write. Because agents are one isolated feature, that failure costs the agents feature only — MCP, skills and instructions still sync — and the message carries the source file, the target file, and the exact capabilities that could not be translated. `cli-switch doctor` lists all of them together.

On agy builds where file-based agents do not appear in the `/agents` picker, the native file can still be invoked by name; this is an upstream discovery UI limitation. Copilot may require a restart to discover newly created agent files.

### Auto-sync on startup

Run `cli-switch mount` to hook `cli-switch sync` into each CLI's startup. `status` labels each hook mechanism as stable, experimental, or conditional; all six CLIs share the same manual core-sync safety contract.

### Upgrading from v0.1

v0.1 used symlinks for instructions and skills. v0.2 detects them and makes no changes until you review the migration message, which lists every symlink by path. Run `cli-switch sync --migrate` to explicitly convert them into independently versioned copies with a rollback journal. Any other blocker found in the same pass is reported alongside it rather than after it.

---

## Source of Truth

Everything lives in `~/.config/cli-switch/`:

```
mcp.json     # Canonical MCP servers
AGENTS.md    # Canonical shared instructions
skills/      # Canonical shared skills
agents/      # Canonical custom-agent bundles (opt-in)
config.toml  # Which CLIs and scopes are active
state/       # Snapshots, health, conflicts, 10 completed transactions + retained recovery journals
```

## Building and testing

Building from source requires Rust 1.89 or newer. The test suite uses temporary homes and project directories, including injected write/recovery failures and concurrent-edit cases.

```bash
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```
