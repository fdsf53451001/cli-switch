#!/usr/bin/env bash
# Build cli-switch and install it to ~/.local/bin, then scaffold the store.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${CLI_SWITCH_BIN_DIR:-$HOME/.local/bin}"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[x]${NC} $*"; }

command -v cargo >/dev/null 2>&1 || { err "需要 cargo（Rust）才能編譯。安裝：https://rustup.rs"; exit 1; }

info "編譯 release binary…"
( cd "$ROOT" && cargo build --release )

mkdir -p "$BIN_DIR"
install -m755 "$ROOT/target/release/cli-switch" "$BIN_DIR/cli-switch"
info "已安裝：$BIN_DIR/cli-switch"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR 不在 PATH 中——請加入後再使用 cli-switch。" ;;
esac

info "建立真理來源…"
"$BIN_DIR/cli-switch" init

cat <<EOF

下一步：
  cli-switch sync          # 吸入並散播現有設定
  cli-switch mount         # 掛載啟動時自動同步
  cli-switch status        # 檢視狀態
EOF
