#!/usr/bin/env bash
# Build agent-sync and install it to ~/.local/bin, then scaffold the store.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${AGENT_SYNC_BIN_DIR:-$HOME/.local/bin}"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[x]${NC} $*"; }

command -v cargo >/dev/null 2>&1 || { err "需要 cargo（Rust）才能編譯。安裝：https://rustup.rs"; exit 1; }

info "編譯 release binary…"
( cd "$ROOT" && cargo build --release )

mkdir -p "$BIN_DIR"
install -m755 "$ROOT/target/release/agent-sync" "$BIN_DIR/agent-sync"
info "已安裝：$BIN_DIR/agent-sync"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR 不在 PATH 中——請加入後再使用 agent-sync。" ;;
esac

info "建立真理來源…"
"$BIN_DIR/agent-sync" init

cat <<EOF

下一步：
  agent-sync sync          # 吸入並散播現有設定
  agent-sync mount         # 掛載啟動時自動同步
  agent-sync status        # 檢視狀態
EOF
