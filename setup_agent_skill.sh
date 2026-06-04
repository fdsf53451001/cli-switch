#!/usr/bin/env bash
# setup_agents_symlinks.sh
# Symlink each AI agent's Instructions and Skills paths to the canonical
# AGENTS.md and .agents/skills/ directory.
#
# Canonical sources (the files you actually edit):
#   AGENTS.md          — shared project instructions
#   .agents/skills/    — shared skills (SKILL.md folders)
#   .agents/rules/     — shared rules (for Antigravity, @-includes AGENTS.md)
#
# What this script creates:
#   CLAUDE.md                        → AGENTS.md
#   .github/copilot-instructions.md  → AGENTS.md
#   .kiro/steering/AGENTS.md         → AGENTS.md
#   .agents/rules/agents-root.md     (created, not symlinked — @-includes AGENTS.md)
#   .kiro/skills                     → .agents/skills
#   .claude/skills                   → .agents/skills

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[✗]${NC} $*"; }

# 建立標準目錄
mkdir -p "$ROOT/.agents/skills" "$ROOT/.agents/rules"

# 確認 AGENTS.md 存在
if [[ ! -f "$ROOT/AGENTS.md" ]]; then
  err "找不到 $ROOT/AGENTS.md — 請先建立此檔案再執行腳本。"
  exit 1
fi

# Tracks files/dirs that block symlinking (real, non-symlink paths)
CONFLICTS=()

# ── Helper ───────────────────────────────────────────────────────────────────
# make_symlink <target_absolute> <link_absolute>
make_symlink() {
  local target="$1"
  local link="$2"
  mkdir -p "$(dirname "$link")"
  # 若 target 為目錄（或尚不存在且非檔案），先建立，確保 symlink 不會指向空路徑
  if [[ ! -f "$target" ]]; then
    mkdir -p "$target"
  fi
  if [[ -L "$link" ]]; then
    local current_target
    current_target="$(readlink "$link")"
    if [[ "$current_target" == "$target" ]]; then
      warn "已連結（略過）：$link → $target"
    else
      warn "更新連結：$link（原指向 → $current_target）"
      ln -sf "$target" "$link"
      info "已更新：$link → $target"
    fi
  elif [[ -e "$link" ]]; then
    CONFLICTS+=("$link → $target")
  else
    ln -s "$target" "$link"
    info "已建立：$link → $target"
  fi
}

# ── Conflict reporter ────────────────────────────────────────────────────────
# Call after all make_symlink invocations to print actionable merge guidance.
report_conflicts() {
  if [[ ${#CONFLICTS[@]} -eq 0 ]]; then
    return
  fi
  echo ""
  echo -e "${RED}══════════════════════════════════════════════════════${NC}"
  err "發現 ${#CONFLICTS[@]} 個衝突 — 以下 symlink 尚未建立："
  echo ""
  for entry in "${CONFLICTS[@]}"; do
    echo -e "  ${YELLOW}衝突${NC}  $entry"
  done
  echo ""
  echo "  解決步驟："
  echo "  1. 開啟上方列出的衝突檔案。"
  echo "  2. 將其內容手動 merge 至標準目標："
  echo "       指令檔 → AGENTS.md"
  echo "       技能目錄 → .agents/skills/"
  echo "  3. Merge 完成後，將原始檔案改名或刪除："
  echo "       mv <衝突檔案> <衝突檔案>.bak"
  echo "  4. 重新執行本腳本。"
  echo -e "${RED}══════════════════════════════════════════════════════${NC}"
}

# ── Instructions ─────────────────────────────────────────────────────────────
echo ""
echo "=== 指令檔（Instructions）==="

# Claude Code：讀取 CLAUDE.md
make_symlink "$ROOT/AGENTS.md" "$ROOT/CLAUDE.md"

# GitHub Copilot：讀取 .github/copilot-instructions.md
make_symlink "$ROOT/AGENTS.md" "$ROOT/.github/copilot-instructions.md"

# Kiro：根目錄下所有 *.md 均會被讀取（AGENTS.md 已涵蓋）；
# 另外連結至 .kiro/steering/ 以支援明確的 steering 機制
make_symlink "$ROOT/AGENTS.md" "$ROOT/.kiro/steering/AGENTS.md"

# Google Antigravity：讀取 .agents/rules/ 目錄，無法直接 symlink 檔案至目錄；
# 改為建立規則檔，透過 @-mention 語法引用 AGENTS.md
ANTIGRAVITY_RULE="$ROOT/.agents/rules/agents-root.md"
if [[ ! -f "$ANTIGRAVITY_RULE" ]]; then
  cat > "$ANTIGRAVITY_RULE" <<'EOF'
---
description: Root project instructions. Always applied.
activation: always
---

@/AGENTS.md
EOF
  info "已建立：.agents/rules/agents-root.md（@-引用 AGENTS.md）"
else
  warn "已存在（略過）：.agents/rules/agents-root.md"
fi

# ── Skills ───────────────────────────────────────────────────────────────────
echo ""
echo "=== 技能目錄（Skills）==="

# Kiro：讀取 .kiro/skills/
make_symlink "$ROOT/.agents/skills" "$ROOT/.kiro/skills"

# Claude Code：讀取 .claude/skills/
make_symlink "$ROOT/.agents/skills" "$ROOT/.claude/skills"

# GitHub Copilot / Codex / Open Code / Antigravity 已直接使用 .agents/skills/，無需額外連結。

report_conflicts

if [[ ${#CONFLICTS[@]} -eq 0 ]]; then
  echo ""
  info "完成。請統一編輯 AGENTS.md 與 .agents/skills/ 作為唯一真理來源（SSOT）。"
fi
 
