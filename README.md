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

That's it. Edit `~/.config/cli-switch/mcp.json`, `AGENTS.md`, or `skills/` once — `cli-switch sync` propagates changes to all your CLIs.

---

## Commands

```bash
cli-switch           # Interactive menu
cli-switch sync      # Sync now
cli-switch status    # Check sync status across all CLIs
cli-switch mount     # Hook cli-switch into each CLI's startup
```

---

## How It Works

- **MCP servers** — edit once in `~/.config/cli-switch/mcp.json`; converted to each CLI's native format on sync
- **Instructions** — one `AGENTS.md` symlinked as `CLAUDE.md`, `copilot-instructions.md`, etc.
- **Skills** — one `skills/` folder symlinked into each CLI
- **Bidirectional** — changes made inside any CLI are merged back on the next sync

### Project-level sync

Run `cli-switch` inside a project directory and choose **Set project level** to sync that project's instructions and skills across CLIs. Uses `AGENTS.md` and `.agents/skills/` as the shared source.

### Auto-sync on startup

Run `cli-switch mount` to hook `cli-switch sync` into each CLI's startup — so everything stays in sync without running it manually.

---

## Source of Truth

Everything lives in `~/.config/cli-switch/`:

```
mcp.json     # Your MCP servers (edit this)
AGENTS.md    # Your shared instructions (edit this)
skills/      # Your shared skills (edit this)
config.toml  # Which CLIs and scopes are active
```
