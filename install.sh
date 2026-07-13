#!/usr/bin/env bash
# Install cli-switch to ~/.local/bin, then scaffold the store.
#
# By default this downloads a prebuilt binary from GitHub Releases for your
# platform — no Rust required. If no binary matches your platform (or the
# download fails, or you set CLI_SWITCH_FROM_SOURCE=1) it builds from source.
#
# Overrides:
#   CLI_SWITCH_REPO=owner/repo     which repo's releases to pull from
#   CLI_SWITCH_VERSION=v0.2.0      a specific tag (default: latest)
#   CLI_SWITCH_BIN_DIR=~/.local/bin  install location
#   CLI_SWITCH_FROM_SOURCE=1       force building from source
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${CLI_SWITCH_BIN_DIR:-$HOME/.local/bin}"
REPO="${CLI_SWITCH_REPO:-fdsf53451001/cli-switch}"
VERSION="${CLI_SWITCH_VERSION:-latest}"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[x]${NC} $*"; }

# When run inside a clone, prefer the repo's own origin for the release slug.
if remote=$(git -C "$ROOT" remote get-url origin 2>/dev/null); then
  slug=$(printf '%s' "$remote" | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')
  case "$slug" in */*) REPO="$slug" ;; esac
fi

# Map this machine to a release target triple (empty = build from source).
target=""
case "$(uname -s)" in
  Darwin)
    case "$(uname -m)" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64)        target="x86_64-apple-darwin" ;;
    esac ;;
  Linux)
    case "$(uname -m)" in
      x86_64)        target="x86_64-unknown-linux-musl" ;;
    esac ;;
esac

build_from_source() {
  command -v cargo >/dev/null 2>&1 || {
    err "需要 cargo（Rust）才能從原始碼編譯。安裝：https://rustup.rs"; exit 1;
  }
  info "從原始碼編譯 release binary…"
  ( cd "$ROOT" && cargo build --release )
  mkdir -p "$BIN_DIR"
  install -m755 "$ROOT/target/release/cli-switch" "$BIN_DIR/cli-switch"
}

download_prebuilt() {
  local triple="$1" url tmp
  if [ "$VERSION" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download/cli-switch-$triple"
  else
    url="https://github.com/$REPO/releases/download/$VERSION/cli-switch-$triple"
  fi
  command -v curl >/dev/null 2>&1 || return 1
  tmp="$(mktemp)"
  info "下載預編 binary：$url"
  if ! curl -fsSL "$url" -o "$tmp"; then
    rm -f "$tmp"; return 1
  fi
  mkdir -p "$BIN_DIR"
  install -m755 "$tmp" "$BIN_DIR/cli-switch"
  rm -f "$tmp"
}

if [ "${CLI_SWITCH_FROM_SOURCE:-0}" = "1" ]; then
  build_from_source
elif [ -z "$target" ]; then
  warn "沒有對應此平台（$(uname -s)/$(uname -m)）的預編 binary，改用原始碼編譯。"
  build_from_source
elif ! download_prebuilt "$target"; then
  warn "下載預編 binary 失敗，改用原始碼編譯。"
  build_from_source
fi

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
