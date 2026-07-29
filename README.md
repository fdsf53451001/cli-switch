# cli-switch

Keep your **MCP servers, skills, instructions, and custom agents** in sync across multiple AI CLIs — automatically.

Supports: **Claude Code · Codex · opencode · Kiro · Antigravity CLI (`agy`) · GitHub Copilot CLI (`copilot`)**

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
- **Instructions** — independent native files compared with the last successful snapshot
- **Skills** — each skill directory is synchronized as one atomic unit
- **Custom agents** — opt-in, direct native files (no plugin or MCP control plane), with portable core fields and namespaced native extensions
- **Bidirectional** — changes made inside any CLI are merged back on the next sync
- **Fail closed** — divergent edits create a conflict packet and leave that feature's managed files untouched
- **Isolated per feature** — a problem in one feature never stops the other three; the failing feature is skipped, reported with the file and field that caused it, and its snapshot is left un-advanced so the change is re-detected next run
- **Transactional** — a failed write rolls the entire sync back; successful transactions can be explicitly restored

`cli-switch status` reports the recorded result of the **last sync attempt**, not the shape of the filesystem: a run that fails commits no transaction, so anything derived from the target paths alone would keep looking green. `status` and `doctor` exit `0` only when the last sync succeeded and nothing blocks the next one; `3` means degraded, `2` means unresolved conflicts.

Conflict JSON always masks MCP environment and header values. Discuss the packet with your AI CLI, choose a source, then run the explicit `conflicts resolve` command. Startup hooks apply only conflict-free plans.

### Project-level sync

Run `cli-switch` inside a project directory and choose **Set project level** to sync that project's instructions, skills, and optionally custom agents across CLIs. Uses `AGENTS.md`, `.agents/skills/`, and `.cli-switch/agents/` as the shared sources. Global and project agent snapshots are independent.

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
state/       # Private snapshots, last-sync health, pending conflicts, last 10 transactions
```
