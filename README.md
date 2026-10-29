# Team TODO

給忙碌工作團隊使用的多使用者 TODO（Issue）網站：開立、指派、排程、提醒與追蹤工作。同一份 Rust 程式碼可部署為 Docker（MariaDB）與 Cloudflare Worker（D1）兩個版本。

完整的功能、權限規則、架構與開發規範見 [`docs/rule/README.md`](docs/rule/README.md)。

## 功能

- 帳號：預設管理員、首次登入強制改密碼、管理員新增使用者、可開關的自行註冊
- Issue：標題、備註、狀態、私人 / 公開、指派、標籤、開始 / 到期時間
- 個人提醒：指定時間、開始前或到期前（1 天、6～1 小時、59～1 分鐘），以瀏覽器通知與頁內通知提示
- 週期活動：每天 / 每週（可選星期幾）/ 每月重複，例如每週三 14:00～15:00 週報；每場是獨立的工作
- 團隊調度：儀表板、工作清單篩選、看板、團隊負載、認領未指派工作

## 用 Docker 啟動

需要 Docker（含 Compose）。

1. 複製設定檔，填入兩組資料庫密碼（留空時 compose 會拒絕啟動）；對外公開時建議同時設定 `SETUP_TOKEN`：

   ```bash
   cp .env.example .env
   ```

2. 建置並啟動：

   ```bash
   docker compose up -d --build
   ```

3. 以瀏覽器開啟 `http://<主機 IP>:<APP_PORT>`（預設 port 8080，可從外部連線）。
4. 以預設管理員 `admin` / `Admin@12345` 登入，進入「初始設定」頁：設定品牌名稱、網站網址、時區、是否開放註冊，以及管理員的 Email 與新密碼。之後可在「管理 › 網站設定」修改。

> 啟動後請立即完成第 4 步：在完成初始設定之前，任何連得到網站的人都能用這組預設帳密登入並接管管理員帳號。設定了 `SETUP_TOKEN` 時，設定頁還會要求輸入它，只知道預設密碼的人無法完成設定。

port、container 名稱、時區（預設 `Asia/Taipei`）、品牌名稱與網址的初始值等設定都在 `.env`，說明見 [`docs/rule/README.md`](docs/rule/README.md#environment--setup-notes)。

## 開發

Rust stable ≥ 1.94。常用指令（完整清單見規格文件的 Build & Run Commands）：

```bash
cargo test --workspace --exclude todo-worker
```

```bash
cargo clippy --workspace --exclude todo-worker -- -D warnings
```

Cloudflare Worker 版的建置與部署步驟見 [`docs/rule/README.md`](docs/rule/README.md#build--run-commands)。
