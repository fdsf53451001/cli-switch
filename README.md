# cli-switch

把 **MCP servers、skills、instructions** 在多個 AI CLI 之間自動同步，並可掛載到各 CLI 啟動時自動執行。

支援：**Claude Code · Codex · opencode · Kiro · Antigravity CLI (`agy`) · GitHub Copilot CLI (`copilot`)**

單一 Rust 編譯 binary，**執行期零依賴**，原生跨 macOS / Linux / Windows。

---

## 它解決什麼問題

你在五個 CLI 裡各自維護 MCP server、skill、指令檔，格式還互不相容：

| 項目 | Claude | Codex | opencode | Kiro | Antigravity CLI (`agy`) | Copilot CLI |
|------|--------|-------|----------|------|-------------|-------------|
| MCP 檔 | `~/.claude.json` | `~/.codex/config.toml` | `~/.config/opencode/opencode.json` | `~/.kiro/settings/mcp.json` | `~/.gemini/antigravity-cli/mcp_config.json` | `~/.copilot/mcp-config.json` |
| MCP 格式 | JSON `mcpServers` | TOML `[mcp_servers.x]` | JSON `mcp`（command 為陣列、`environment`） | JSON `mcpServers` | JSON `mcpServers`（`serverUrl`、禁多餘鍵） | JSON `mcpServers`（type `local`/`http`） |
| Skills | `~/.claude/skills/` | `~/.codex/skills/` | `~/.config/opencode/skills/` | `~/.kiro/skills/` | `~/.gemini/antigravity-cli/skills/` | `~/.copilot/skills/` |
| 指令檔 | `CLAUDE.md` | `AGENTS.md` | `AGENTS.md` | `steering/AGENTS.md` | `GEMINI.md` | `copilot-instructions.md` |

`cli-switch` 用一個**中立的單一真理來源**（`~/.config/cli-switch/`）把這些拉平：MCP 用轉換器產生各家原生格式，skills/instructions 用 symlink。**雙向**——你在任一 CLI 改了，下次同步會合併回來再散播給其他家。

---

## 安裝

### 一鍵安裝（預編 binary，免裝 Rust）

`install.sh` 會依你的平台從 GitHub Releases 下載對應的預編 binary，裝到 `~/.local/bin` 並建立真理來源：

```bash
git clone https://github.com/fdsf53451001/cli-switch && cd cli-switch
./install.sh
```

下載不到對應平台的 binary 時會自動 fallback 成原始碼編譯（需要 Rust）。可用環境變數覆寫：`CLI_SWITCH_VERSION=v0.1.0`、`CLI_SWITCH_BIN_DIR=...`、`CLI_SWITCH_FROM_SOURCE=1`。

預編 binary 涵蓋：macOS（Apple Silicon / Intel）、Linux x86_64（musl 靜態）、Windows x86_64。其他平台請用下方原始碼編譯。

### 從原始碼編譯

需要 Rust（僅編譯期；產物是零依賴 binary）。

```bash
cargo build --release
# 把 binary 放進 PATH（mac/linux）
install -m755 target/release/cli-switch ~/.local/bin/cli-switch
```

跨平台交叉編譯：

```bash
# 在 mac 上產生三平台 binary
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-gnu
cargo build --release --target x86_64-unknown-linux-gnu
cargo build --release --target x86_64-pc-windows-gnu     # 產出 cli-switch.exe
```

### 發布新版本（維護者）

推一個版本 tag 即可觸發 `.github/workflows/release.yml`，自動 build 四平台 binary 並發佈到 GitHub Releases：

```bash
git tag v0.1.0 && git push origin v0.1.0
```

---

## 使用

```bash
cli-switch           # 開啟互動主選單
cli-switch configure # 同上
cli-switch init      # 建立 ~/.config/cli-switch 真理來源 + config.toml
cli-switch sync      # 跑一次完整同步（全域 + 已加入的目前資料夾）
cli-switch status    # 看各 CLI 安裝狀態、server 數、連結狀態
cli-switch mount     # 掛載「啟動時自動同步」到各 CLI
```

典型首次流程：

```bash
cli-switch
```

互動主選單：

```text
1) setup cli             # 選一次 CLI，安裝/更新 startup sync hook
2) set global level      # 用 1 的 CLI 清單啟用/更新使用者全域同步
3) set project level     # 用 1 的 CLI 清單啟用/更新目前資料夾專案同步
4) remove cli            # 從設定移除 CLI，並移除 cli-switch startup hook
5) remove global level   # 停用全域同步，保留 canonical store，不再問 CLI
6) remove project level  # 退出目前資料夾專案同步，不再問 CLI
```

`1) setup cli` 的結果會存在 `~/.config/cli-switch/setup.toml`。之後 `2) set global level` 和 `3) set project level` 直接套用這份 CLI 清單，不會重複問一次。

非互動設定也可以：

```bash
cli-switch configure --scope global --clis installed --yes   # 只調全域
cli-switch configure --scope project --clis claude,codex --yes # 只加入/更新目前資料夾
```

全域同步和專案同步可以並存：全域設定存在 `~/.config/cli-switch/config.toml`，每個已加入的資料夾會有自己的 `.cli-switch/config.toml`。

互動模式一律先進主選單，不會把 `--help`、`--version` 或未知 top-level flag 吃進 configure。非互動模式必須明確使用 `configure` 子命令，並可用 `--clis installed`、`--clis all` 或 `--clis claude,codex`。

### sync 選項

| 旗標 | 作用 |
|------|------|
| `--dry-run` | 只顯示會改什麼，不寫入 |
| `--prune` | 把「已從所有 CLI 移除」的 server 真正刪掉（預設只隔離+警告） |
| `--quiet` | 只印警告/錯誤（啟動 hook 用） |

### status 會顯示什麼

`cli-switch status` 會同時顯示：

- 全域同步：canonical store、每個 CLI 是否啟用、是否安裝、MCP 數量、instructions/skills 連結狀態、startup hook 狀態。
- 目前資料夾專案同步：是否已加入、是否有 `AGENTS.md`、`.agents/skills`、Antigravity rule，以及每個 CLI 的 project instructions/skills 是否已連好。

Global status 的 `state` 欄位中，`active` 代表已選且已安裝、`skipped` 代表已選但本機未安裝、`off` 代表未選。
Global status 的 `skills` 欄位是 `synced/expected`，只計算 cli-switch canonical skills 的 symlink，不計入 CLI 目錄裡其他既有 symlink。

---

## 真理來源（`~/.config/cli-switch/`）

```
mcp.json          # 中立格式的 MCP servers（唯一要編輯的 MCP 清單）
AGENTS.md         # 共用指令檔（symlink 到每個 CLI）
skills/           # 共用 SKILL.md 資料夾（symlink 到每個 CLI）
config.toml       # 要同步哪幾家、哪些項目
state/            # 各 CLI 上次同步快照（驅動三方 merge）
backups/          # 每次寫入前的各 CLI 原檔備份
```

直接編輯 `mcp.json` / `AGENTS.md` / `skills/` 是最乾淨的更新方式；也可以在任一 CLI 裡改，`sync` 會合併回來。

---

## 當前目錄同步（project scope）

在 `cli-switch` 互動設定中啟用目前資料夾專案同步，會建立 `.cli-switch/config.toml` 作為加入標記。這個模式不碰全域 MCP；它把專案內的 instructions/skills 拉成同一份：

```
AGENTS.md          # 專案指令檔 SSOT；不存在時會自動建立
.agents/skills/    # 專案共用 skills
.agents/rules/     # Antigravity rules
```

依選到的 CLI，`sync` 會建立：

| CLI | project instructions | project skills |
|-----|----------------------|----------------|
| Claude Code | `CLAUDE.md -> AGENTS.md` | `.claude/skills -> .agents/skills` |
| Codex | 直接讀 `AGENTS.md` | 直接使用 `.agents/skills` |
| opencode | 直接讀 `AGENTS.md` | 直接使用 `.agents/skills` |
| Kiro | `.kiro/steering/AGENTS.md -> AGENTS.md` | `.kiro/skills -> .agents/skills` |
| Antigravity | 建立 `.agents/rules/agents-root.md`，內容 `@/AGENTS.md` | 直接使用 `.agents/skills` |
| Copilot CLI | 直接讀 `AGENTS.md` | 直接使用 `.agents/skills` |

如果目標位置已經有真實檔案或目錄，`cli-switch` 只會報衝突，不會覆蓋；先手動 merge 到 `AGENTS.md` 或 `.agents/skills/` 後再重跑。

---

## 雙向 merge 怎麼運作

每次 `sync`：

1. 讀每個 CLI 的原生 MCP 設定 → 轉成中立格式。
2. 與該 CLI 的**上次快照**比對，找出它本地的新增/修改。
3. 三方合併進 canonical；同一 server 被兩家改成不同定義時，以**設定檔較新者勝**（並印出衝突警告）。
4. 把合併後的 canonical 寫回**所有** CLI（各自原生格式）。
5. 重新讀回每家當作新快照（避免「某家無法表達某欄位」造成的反覆震盪）。

**刪除是保守的**：只有當一個 server 從**所有** CLI 都消失時才算孤兒；預設**隔離+警告**（保留在 canonical、不再推回各家），要真正刪除需 `--prune`。從單一 CLI 刪除會被其他家的定義復原（union 語意）。

---

## 啟動時自動同步（`mount`）

| CLI | 機制 | 可靠度 |
|-----|------|--------|
| Claude Code | `settings.json` 的 `SessionStart` hook | ✅ 原生、穩定 |
| Codex | `~/.codex/hooks.json` 的 SessionStart | ⚠️ 實驗性，需 `codex /hooks` 核准 |
| opencode | `~/.config/opencode/plugin/cli-switch.js` | ⚠️ 實驗性，plugin 事件名稱可能改版 |
| Kiro | 原生 `agentSpawn` hook 注入 default agent（`~/.kiro/agents/<default>.json`）；無 default agent 時 fallback shell wrapper | ✅ 有 default agent 時原生；否則需手動 source wrapper |
| Antigravity CLI (`agy`) | `~/.gemini/config/hooks.json` 的 `PreInvocation` hook | ✅ 原生 hook；每次模型呼叫前同步 |
| GitHub Copilot CLI (`copilot`) | `~/.copilot/hooks/cli-switch.json` 的 `sessionStart` hook | ✅ 原生 hook；每次 session 啟動時同步 |

`mount` 會自動偵測既有設定並**就地合併**（不會覆蓋你 Claude 既有的其他 hook），且重複執行不會疊加。

Kiro CLI 有原生 `agentSpawn` hook，但它綁在單一 agent 上（沒有全域 hook，內建 `kiro_default` 沒有可寫的檔）。`mount` 會讀 `~/.kiro/settings/cli.json` 的 `chat.defaultAgent`，把 `cli-switch sync --quiet` 外科手術式注入那個 agent 的 `hooks.agentSpawn`（保留其餘設定、重複執行不疊加；用 `--quiet` 讓 stdout 為空，不污染 agent context）。沒有設定 default agent 時，fallback 成 shell wrapper，需手動把 `source ~/.config/cli-switch/shell-init.sh` 放進 shell rc。

Antigravity CLI (`agy`) 使用官方 hooks：`mount` 會在 `~/.gemini/config/hooks.json` 寫入 `cli-switch-sync`，掛到 `PreInvocation`，讓每次模型呼叫前先同步。

---

## 安全性

- **外科手術式寫入**：只改 MCP 區段，保留 `~/.claude.json` 的 auth、`config.toml` 的 `[projects]`/`[tui]` 等不相關設定。
- **原子寫入**：先寫暫存檔再 rename，避免半寫壞檔。
- **每次備份**：寫入前把各 CLI 原檔複製到 `backups/`。
- **並發鎖**：多個 CLI 同時啟動觸發同步時，只有一個實際執行，其餘跳過。
- **Antigravity CLI schema 合規**：嚴格只輸出 schema 允許的鍵（remote server 使用 `serverUrl`）。
