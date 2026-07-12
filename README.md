# cli-switch

Keep your **MCP servers, skills, and instructions** in sync across multiple AI CLIs — automatically.

Supports: **Claude Code · Codex · opencode · Kiro · Antigravity CLI (`agy`) · GitHub Copilot CLI (`copilot`)**

---

## The Problem

You're managing MCP servers, skills, and instruction files separately in multiple AI CLIs, all with incompatible formats. `cli-switch` gives you a single place to edit, and syncs everything everywhere.

---

## Install

```bash
git clone https://github.com/fdsf53451001/cli-switch && cd cli-switch
./install.sh
```

Pre-built binaries for macOS (Apple Silicon / Intel), Linux x86_64, and Windows x86_64. No Rust required.

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

The Windows installer places `cli-switch.exe` in `~/.local/bin`. Core synchronization is supported on macOS, Linux, and Windows; startup-hook support is reported separately by `cli-switch status`.

---

## Commands

```bash
cli-switch           # Interactive menu
cli-switch sync      # Sync now
cli-switch status    # Check sync status across all CLIs
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
- **Bidirectional** — changes made inside any CLI are merged back on the next sync
- **Fail closed** — divergent edits create a conflict packet and leave every managed file untouched
- **Transactional** — a failed write rolls the entire sync back; successful transactions can be explicitly restored

Conflict JSON always masks MCP environment and header values. Discuss the packet with your AI CLI, choose a source, then run the explicit `conflicts resolve` command. Startup hooks apply only conflict-free plans.

### Project-level sync

Run `cli-switch` inside a project directory and choose **Set project level** to sync that project's instructions and skills across CLIs. Uses `AGENTS.md` and `.agents/skills/` as the shared source.

### Auto-sync on startup

Run `cli-switch mount` to hook `cli-switch sync` into each CLI's startup. `status` labels each hook mechanism as stable, experimental, or conditional; all six CLIs share the same manual core-sync safety contract.

### Upgrading from v0.1

v0.1 used symlinks for instructions and skills. v0.2 detects them and makes no changes until you review the migration message. Run `cli-switch sync --migrate` to explicitly convert them into independently versioned copies with a rollback journal.

---

## Source of Truth

Everything lives in `~/.config/cli-switch/`:

```
mcp.json     # Canonical MCP servers
AGENTS.md    # Canonical shared instructions
skills/      # Canonical shared skills
config.toml  # Which CLIs and scopes are active
state/       # Private snapshots, pending conflicts, and last 10 transactions
```
