# cli-switch

把 **MCP servers、skills、instructions** 在多個 AI CLI 之間自動同步，並可掛載到各 CLI 啟動時自動執行。

支援：**Claude Code · Codex · opencode · Kiro · Antigravity**

單一 Rust 編譯 binary，**執行期零依賴**，原生跨 macOS / Linux / Windows。

---

## 它解決什麼問題

你在五個 CLI 裡各自維護 MCP server、skill、指令檔，格式還互不相容：

| 項目 | Claude | Codex | opencode | Kiro | Antigravity |
|------|--------|-------|----------|------|-------------|
| MCP 檔 | `~/.claude.json` | `~/.codex/config.toml` | `~/.config/opencode/opencode.json` | `~/.kiro/settings/mcp.json` | `~/.gemini/antigravity/mcp_config.json` |
| MCP 格式 | JSON `mcpServers` | TOML `[mcp_servers.x]` | JSON `mcp`（command 為陣列、`environment`） | JSON `mcpServers` | JSON `mcpServers`（`serverUrl`、禁多餘鍵） |
| Skills | `~/.claude/skills/` | `~/.codex/skills/` | `~/.config/opencode/skills/` | `~/.kiro/skills/` | `~/.gemini/antigravity/skills/` |
| 指令檔 | `CLAUDE.md` | `AGENTS.md` | `AGENTS.md` | `steering/AGENTS.md` | `GEMINI.md` |

`cli-switch` 用一個**中立的單一真理來源**（`~/.config/cli-switch/`）把這些拉平：MCP 用轉換器產生各家原生格式，skills/instructions 用 symlink。**雙向**——你在任一 CLI 改了，下次同步會合併回來再散播給其他家。

---

## 安裝

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

---

## 使用

```bash
cli-switch configure # 互動設定：全域/當前目錄、CLI 清單、啟動自動同步
cli-switch init      # 建立 ~/.config/cli-switch 真理來源 + config.toml
cli-switch sync      # 跑一次完整同步（MCP 合併 + skills/instructions 連結）
cli-switch status    # 看各 CLI 安裝狀態、server 數、連結狀態
cli-switch mount     # 掛載「啟動時自動同步」到各 CLI
```

典型首次流程：

```bash
cli-switch configure     # 建議使用；會寫 config 並對已安裝 CLI 掛 startup sync
cli-switch sync          # 把你現有各家設定吸進真理來源並互相散播
cli-switch status
```

非互動設定也可以：

```bash
cli-switch configure --scope global --clis installed --yes
cli-switch configure --scope project --clis claude,codex,kiro --yes
```

`--scope global` 會同步使用者全域設定；`--scope project` 會同步**目前工作目錄**。

互動模式會先逐項確認要同步的 CLI，再選全域或當前目錄同步。非互動模式仍可用 `--clis installed`、`--clis all` 或 `--clis claude,codex`。

### sync 選項

| 旗標 | 作用 |
|------|------|
| `--dry-run` | 只顯示會改什麼，不寫入 |
| `--prune` | 把「已從所有 CLI 移除」的 server 真正刪掉（預設只隔離+警告） |
| `--quiet` | 只印警告/錯誤（啟動 hook 用） |

### status 會顯示什麼

`cli-switch status` 會依目前 config 的 scope 顯示同步狀態：

- `global`：canonical store、每個 CLI 是否啟用、是否安裝、MCP 數量、instructions/skills 連結狀態、startup hook 狀態。
- `project`：目前目錄是否有 `AGENTS.md`、`.agents/skills`、Antigravity rule，以及每個 CLI 的 project instructions/skills 是否已連好。

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

`cli-switch configure --scope project` 會把目前目錄設成專案層級同步。這個模式不碰全域 MCP；它把專案內的 instructions/skills 拉成同一份：

```
AGENTS.md          # 專案指令檔 SSOT，必須先存在
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
| Kiro / Antigravity | 無原生 startup hook → 產生 shell wrapper（`~/.config/cli-switch/shell-init.sh`） | ⚠️ 僅終端機啟動；GUI/Dock 啟動需改用定時同步 |

`mount` 會自動偵測既有設定並**就地合併**（不會覆蓋你 Claude 既有的其他 hook），且重複執行不會疊加。

Kiro / Antigravity 是 GUI（VS Code fork），沒有給 CLI 用的啟動 hook。若要涵蓋 GUI 啟動，建議搭配定時同步（cron / launchd / 排程器）。

---

## 安全性

- **外科手術式寫入**：只改 MCP 區段，保留 `~/.claude.json` 的 auth、`config.toml` 的 `[projects]`/`[tui]` 等不相關設定。
- **原子寫入**：先寫暫存檔再 rename，避免半寫壞檔。
- **每次備份**：寫入前把各 CLI 原檔複製到 `backups/`。
- **並發鎖**：多個 CLI 同時啟動觸發同步時，只有一個實際執行，其餘跳過。
- **Antigravity schema 合規**：嚴格只輸出 schema 允許的鍵（`additionalProperties:false`）。
