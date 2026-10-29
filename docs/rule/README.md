# Team TODO 專案規格

本文件定義 Team TODO 的專案目標、功能、架構、權限規則與開發指令，給所有參與開發的工程師與 AI Agent 閱讀；開始任何實作前請先讀完。

## Project Overview
> 給忙碌工作團隊使用的多使用者 TODO（Issue）網站，同一份 Rust 程式碼同時部署為 Docker（MariaDB）與 Cloudflare Worker（D1）兩個版本，兩版功能與畫面完全一致。

- Project: Team-TODO
- Purpose: 讓團隊成員開立、指派、排程與追蹤 Issue（即 TODO），並以可見度控制誰能看到
- Owner: JW-Albert
- Date: 2026-10-24

### 功能範圍

- 帳號
  - 登入、登出（帳號 + 密碼，Session Cookie）。帳號名稱不分大小寫（一律轉小寫儲存）。
  - 預設管理員 `admin`，初始密碼寫在 migration 中（見 [Environment & Setup Notes](#environment--setup-notes)）。
  - 初始設定（類似 WordPress 安裝頁）：新部署的網站在完成初始設定前，登入後的頁面都導向 `/setup`（非管理員只看到等待說明）；只有管理員能填寫（部署時設定了 `SETUP_TOKEN` 時還需輸入它）：品牌名稱、網站網址、時區、是否開放註冊，以及管理員的顯示名稱、Email、新密碼。完成時會登出管理員帳號的其他 Session，之後設定頁永久關閉，改由「管理 › 網站設定」修改。設定完成前不能用「帳號設定」改密碼，以免他人用預設密碼搶先改掉。整個完成動作是一個原子操作，失敗時不會留下「已設定」但密碼未改的狀態。
  - 登入保護：同一帳號連續 5 次密碼錯誤會鎖定 10 分鐘（期間正確密碼也無法登入）；成功登入即清除計數。
  - 被管理員新增或重設密碼的帳號，登入後必須先修改密碼才能使用其他功能。
  - 個人資料：每位使用者可修改顯示名稱與 Email（Email 選填，目前僅紀錄，供日後通知使用）。
  - 密碼規則：至少 6 個字元，且包含大寫英文、小寫英文、數字、符號（ASCII 標點符號）四類中的至少兩類；中文等其他字元可使用但不算任何一類。
  - 新增使用者的方式：
    1. 管理員於後台新增（設定臨時密碼，使用者首次登入須修改；可一併填寫選填的 Email）。
    2. 自行註冊，管理員可於後台開啟 / 關閉（預設關閉）。
    3. AD / Keycloak 等外部身分登入：暫緩實作。
  - 管理員可重設密碼、設定 / 取消管理員、停用 / 啟用帳號；不提供刪除帳號，以保留 Issue 歷史。
  - 停用帳號會立即登出所有 Session，並把指派給他的未完成 Issue 改為未指派，讓團隊可以重新認領；已完成的 Issue 保留他為負責人（編輯時下拉選單會顯示「（已停用）」），重新開啟時才改為未指派。
  - 開立者被停用後，管理員可以編輯或刪除他開立、且管理員看得到的 Issue，避免工作卡住。
- Issue
  - 欄位：標題、備註、狀態（待處理 / 進行中 / 已完成）、可見度、指派對象、標籤（可多選）、開始時間、到期時間。
  - 標籤：系統預設（緊急、嚴重、重要、一般、Bug、功能、文件），使用者可自訂新增；建立者或管理員可刪除自訂標籤，系統標籤僅管理員可刪除。
  - 提醒：每位使用者可對自己看得到、未完成的 Issue 設定「自己的」提醒（Issue 完成時，尚未發出的提醒會一併取消），三選一：
    - 指定時間。
    - 開始前：需先設定開始時間；提前量同「到期前」。開始時間變更時自動重算，清除開始時間則刪除這類提醒。
    - 到期前：1 天、6～1 小時（每小時）、59～1 分鐘（每分鐘）。到期時間變更時自動重算，清除到期時間則刪除這類提醒。
  - 提醒時間到時，網頁以瀏覽器通知（Notification API，停留到使用者點掉）與頁內通知顯示；未授權瀏覽器通知時只顯示頁內通知。分頁在背景時，標題前加上「(n) ⏰」。
  - 提醒時間的偵測：`/api/reminders` 回傳到期的提醒與「距離下一個提醒的毫秒數」，頁面在那一刻再查詢（至少每 60 秒一次）；查到後先 `ack` 再顯示，多個分頁只會有一個顯示。網站分頁需保持開啟（可在背景，背景分頁可能因瀏覽器節流稍晚）。
  - 通知權限：導覽列在「尚未設定」時顯示「🔔 啟用通知」，「已封鎖」時顯示「🔕 通知已封鎖」並說明開啟方式；帳號設定頁顯示目前狀態並可傳送測試通知。
- 週期活動（例如每週三 14:00～15:00 週報）
  - 新增 Issue 時選擇「重複」：每天、每週（可複選星期幾，未勾選時用第一場的星期）、每月（固定在第一場的日期，沒有該日的月份跳過）；可設結束日期。
  - 「開始時間」是第一場，「開始～到期」是每場的時段（最長 7 天），兩者都必填。
  - 每一場是一筆獨立的 Issue（`issues.series_id` 指向系列），複製系列的標題、備註、可見度、負責人（已停用則為未指派）與標籤，可各自完成、認領、編輯或刪除；儀表板、清單、看板與團隊負載直接包含這些 Issue，清單上以「🔁 開始時間」標示。
  - 系統預先建立未來 14 天內的場次。沒有排程器：登入後的請求順便檢查（Docker 版每分鐘最多一次，Worker 版每個請求一次索引查詢），每次最多處理 5 個系列；無人開網站期間的日期不補建。
  - 個人提醒設在系列上（每場開始前或結束前），套用到每一場尚未開始的活動，並取代自己在這些場次上的個別提醒。
  - 編輯系列（開立者；開立者停用後為管理員）：尚未開始且仍為「待處理」的場次刪除後依新設定重建，已開始、已有人處理或已完成的場次保留。單獨編輯某一場只影響那一場。
  - 停止重複：刪除尚未開始且仍為「待處理」的場次，其餘保留為歷史；停止後無法恢復或編輯。
- 團隊調度
  - 儀表板：指派給我的未完成、已逾期、24 小時內到期、可認領的公開工作數量，以及依到期日排序的我的工作。
  - 工作清單：分頁（指派給我 / 我開立的 / 公開 / 全部可見），可依狀態、到期（已逾期 / 24 小時內）、標籤、負責人、關鍵字篩選，可依到期日、更新時間、建立時間排序；未指派的工作可直接在清單上認領。關鍵字搜尋標題與備註，不分大小寫（含全形與重音字母），`%`、`_`、`\` 都視為一般字元。
  - 看板：依狀態分三欄，有權限的卡片可直接切換狀態；每欄最多顯示 100 項（已完成 30 項），超過時標示「+」並提供完整清單連結。
  - 團隊負載：每位成員（不含系統管理員）的待處理、進行中、已逾期、7 天內到期數量，協助分配工作；可認領（未指派的公開 Issue）數量一併顯示。
  - 認領：任何人可把「未指派、未完成的公開 Issue」指派給自己；負責人可放棄指派（退回）。
  - 編輯衝突：編輯表單帶有版本（`updated_at`），開啟後若有人認領、退回或改變狀態，儲存會被擋下並套用最新的狀態與負責人，請使用者確認後再存。
- 不在範圍內（需要時再開 Issue）：留言、附件、Email 通知、多位負責人、SPA 前端、AD / Keycloak 登入。

### 可見度與權限規則

可見度只有兩種值，再搭配可選的負責人（assignee），即可涵蓋所有需求情境：

| `visibility` | 負責人 | 誰看得到 | 對應需求 |
|---|---|---|---|
| `private` | 無 | 只有開立者 | 私人 TODO |
| `private` | 有 | 開立者 + 負責人 | 指派且僅雙方可見 |
| `public` | 無 | 所有登入使用者 | 公開 TODO（可被認領） |
| `public` | 有 | 所有登入使用者 | 指派且公開 |

- 可見判斷在 `src/core/src/permission.rs`：`can_view` 用於單筆讀取，`VISIBLE_TO` SQL 片段用於清單查詢，兩者放在同一個檔案並以相同規則撰寫，修改時必須一起改。
- 管理員沒有額外的 Issue 檢視權限（看不到別人的私人 Issue）；團隊負載頁的數字對管理員為全部 Issue，對其他人只計算自己看得到的 Issue。
- 未登入使用者看不到任何 Issue，一律導向登入頁。
- 操作權限：

| 操作 | 開立者 | 負責人 | 其他人（public 時） |
|---|---|---|---|
| 檢視、設定自己的提醒 | ✅ | ✅ | ✅ |
| 編輯標題 / 備註 / 可見度 / 指派 / 標籤 / 時間 | ✅ | ❌ | ❌ |
| 切換狀態 | ✅ | ✅ | ❌ |
| 認領（未指派、未完成時） | ✅ | — | ✅ |
| 放棄指派 | — | ✅ | ❌ |
| 刪除 | ✅ | ❌ | ❌ |

- 例外：開立者帳號被停用後，管理員對該 Issue 擁有開立者的「編輯」、「切換狀態」與「刪除」權限。
- 管理員可以刪除任何自己看得到的 Issue（例如重複或不當的內容），但不能編輯別人的 Issue；刪除前的確認訊息會註明是以管理員身分刪除。
- 週期活動系列的檢視、編輯、停止規則與 Issue 相同（`can_view_series`、`can_manage_series`）；系列提醒只要看得到就能設定。

- 看不到的 Issue 回傳 `404`（而非 `403`），避免洩漏 Issue 是否存在。

## Tech Stack
> 主要 crate 版本以 `Cargo.lock` 為準，此處只列主版本線。

- Language: Rust（edition 2024）
- Framework: `axum` 0.8（兩個版本共用同一個 Router）
- Key dependencies:
  - 共用（`todo-core`）：`askama`（編譯期 HTML 模板）、`argon2`（密碼雜湊）、`chrono` + `chrono-tz`（時區）、`serde_json`
  - Docker 版：`tokio`、`sqlx` 0.9（`mysql` + `runtime-tokio`）、`getrandom`
  - Worker 版：`worker` 0.8（`http`、`d1` features）、`worker-build` 0.8、`wrangler` 4
- Rust/runtime version:
  - Rust stable ≥ 1.94（`sqlx` 0.9 的下限，定義於根目錄 `Cargo.toml` 的 `rust-version`）；Docker build image 使用 `rust:1.99-slim-trixie`
  - Worker 版需 `wasm32-unknown-unknown` target 與 Node.js 24 LTS
  - Docker 版使用 MariaDB 12.3 LTS，執行 image 為 `debian:trixie-slim`
- 版本原則：一律採用最新穩定版或 LTS 版本；升級時同步更新 `Cargo.toml`、`src/docker/Dockerfile`、`docker-compose.yml` 與本段。

### 架構

```text
            ┌──────────────── todo-core（lib）────────────────┐
            │ domain / 權限 / 密碼規則 / 時區                  │
            │ store：所有 SQL（唯一一份）                       │
            │ web：axum Router + askama templates + 靜態檔      │
            │ trait Runtime：query / execute / batch / 時鐘 / 亂數 / log │
            └───────────────────────┬────────────────────────┘
                       implements    │ Runtime
          ┌──────────────────────────┴──────────────┐
   todo-docker（bin）                        todo-worker（cdylib）
   tokio + sqlx MySQL Pool                  workers-rs + D1 binding
   MariaDB                                  Cloudflare D1（SQLite）
```

- `core` 不依賴任何執行環境：只透過 `Runtime` trait 存取資料庫、時鐘、亂數與 log（依賴反轉，DIP）。Workers 上 `SystemTime` 會 panic、`getrandom` 需要特別設定，因此時鐘與亂數也由 adapter 提供。
- 兩個 adapter 只做三件事：實作 `Runtime`、讀取設定、把 Router 接上各自的執行環境；不得放任何業務邏輯或 SQL。
- 所有 SQL 都在 `src/core/src/store.rs`，只使用 MariaDB 與 SQLite 都支援的語法子集與 `?` placeholder。
- 多個寫入需要一起成功時使用 `Runtime::batch`（MariaDB 為 transaction、D1 為 batch），不使用互動式 transaction（D1 不支援）。MariaDB 遇到 deadlock 時自動重試，以符合 D1 依序執行 batch 的行為。
- 需要「檢查後寫入」時，把條件寫進同一個 `UPDATE … WHERE`（例如認領要求 `assignee_id IS NULL`、編輯要求 `updated_at` 未變），以影響列數判斷是否成功；兩種資料庫回報的影響列數一致。
- 主鍵使用應用程式產生的 ULID（`CHAR(26)`），避免 `AUTO_INCREMENT` 與 SQLite `INTEGER PRIMARY KEY` 的語法差異，讓 migration 只需一份。
- 時間一律存 UTC Unix 毫秒 `BIGINT`；顯示與表單輸入依設定的時區（預設 `Asia/Taipei`）轉換。

### 資料模型

Schema 只寫在 [`src/migrations/`](../../src/migrations/)，本文件不複製。補充 SQL 看不出來的規則：

- `issues.status`：`open`（待處理）、`in_progress`（進行中）、`done`（已完成）。
- `issues.visibility`：`private`、`public`。
- `reminders` 以（Issue, 使用者）為主鍵，每人每個 Issue 一個提醒；`notified_at` 為已通知（或因 Issue 完成而取消）的時間，提醒時間變更時清空。
- `series` 存週期活動的規則：`start_date`、`until_date`、`generated_until` 為系列時區（`timezone`）的本地日期字串 `YYYY-MM-DD`，`start_minute` 為本地開始時間（午夜起算的分鐘），`weekdays` 為位元遮罩（bit 0 = 週一）。`effective_from`（建立或最後編輯時間）之前開始的場次不會產生；`generated_until` 記錄已建立到哪一天，編輯時清空。
- `issues.occurrence` 是該場的本地日期；`(series_id, occurrence)` 唯一索引確保同時產生時不會重複，衝突的那一批整批放棄。
- `series_labels`、`series_reminders` 是系列的標籤與成員提醒，產生場次時以 `INSERT … SELECT` 複製到 `issue_labels`、`reminders`；場次以多列 `INSERT` 寫入，每句不超過 90 個參數。
- `issues.search_text` 是 `標題 + 換行 + 備註` 經 Rust 轉小寫後的內容，建立與編輯時寫入，只供搜尋使用。
- `settings` 存放網站設定：`site_name`（品牌名稱，顯示在「TODO」前）、`site_url`（`scheme://host[:port]`）、`timezone`（IANA 名稱）、`allow_signup`、`setup_completed`。`site_name`、`site_url`、`timezone` 為空字串時使用部署設定的初始值（`SITE_NAME`、`SITE_URL`、`APP_TIMEZONE`）。
- Docker 版把網站設定快取在記憶體，儲存後立即失效；只支援單一 app container（多個 container 時其他 container 不會得知變更）。Worker 版每個請求讀一次。
- 欄位長度在 `core` 驗證（標題 200 字元、備註 10000 字元、帳號 3～32 字元、顯示名稱 1～32 字元、標籤 1～32 字元），不依賴資料庫：SQLite 不檢查 `VARCHAR` 長度。

### 兩種資料庫的行為差異與對策

| 差異 | 對策 |
|---|---|
| MariaDB 預設 collation 不分大小寫、重音與尾端空白；SQLite 逐位元比較 | compose 以 `--collation-server=utf8mb4_nopad_bin` 啟動 MariaDB；帳號一律轉小寫儲存 |
| SQLite 非 INTEGER 主鍵允許 NULL | 主鍵欄位一律寫 `NOT NULL` |
| SQLite 不檢查 `VARCHAR(n)` 長度 | 長度在 `core` 驗證 |
| `LOWER()` 在 SQLite 只處理 ASCII、MariaDB 處理全部 Unicode；`LIKE` 的跳脫字元不同；D1 限制 LIKE pattern 50 bytes | 不用 `LOWER`/`LIKE` 搜尋：比對 Rust 轉小寫後存入的 `search_text`，以 `INSTR(search_text, ?) > 0` 查詢 |
| D1 每個查詢最多綁定 100 個參數 | `IN (...)` 清單一律分批（每批 90 個） |
| D1 沒有互動式 transaction | 使用 `Runtime::batch` |
| D1 無法綁定 64 位元整數參數 | Worker adapter 以 `f64` 綁定（毫秒時間戳在安全整數範圍內） |
| 唯一鍵衝突的錯誤格式不同 | adapter 轉成 `DbError::UniqueViolation` |

### 路由

| Method | Path | 說明 |
|---|---|---|
| `GET` / `POST` | `/login`、`/signup` | 登入、自行註冊（管理員關閉時 404） |
| `POST` | `/logout` | 登出 |
| `GET` / `POST` | `/password` | 帳號設定：修改密碼（首次登入強制） |
| `POST` | `/account/profile` | 修改顯示名稱與 Email |
| `GET` / `POST` | `/setup` | 初始設定（僅限未完成設定時的管理員） |
| `GET` | `/` | 儀表板 |
| `GET` | `/issues` | 工作清單，`?scope=assigned\|created\|public\|all&status=&due=overdue\|soon&label=&assignee=&q=&sort=` |
| `GET` | `/board` | 看板 |
| `GET` | `/team` | 團隊負載 |
| `GET` / `POST` | `/issues/new` | 新增 Issue |
| `GET` | `/issues/{id}` | 檢視 Issue |
| `GET` / `POST` | `/issues/{id}/edit` | 編輯 Issue（僅開立者） |
| `POST` | `/issues/{id}/status`、`/claim`、`/release`、`/reminder`、`/delete` | 切換狀態、認領、放棄指派、設定提醒、刪除 |
| `GET` | `/series/{id}` | 週期活動：規則、即將到來與最近的場次、我的提醒 |
| `GET` / `POST` | `/series/{id}/edit` | 編輯系列（僅開立者） |
| `POST` | `/series/{id}/stop`、`/series/{id}/reminder` | 停止重複、設定系列提醒 |
| `GET` / `POST` | `/labels`、`/labels/{id}/delete` | 標籤管理 |
| `GET` / `POST` | `/admin`、`/admin/users`、`/admin/users/{id}/*`、`/admin/settings` | 管理後台 |
| `GET` / `POST` | `/api/reminders`、`/api/reminders/{issue_id}/ack` | 到期提醒查詢與確認（JSON） |
| `GET` | `/healthz` | 健康檢查（會查詢資料庫） |

- 前端為 server-side render 的 HTML 表單，搭配少量原生 JavaScript（提醒輪詢、表單欄位切換、相對時間更新、回到分頁時重新載入），不使用 JS 框架。
- 相對時間（「3 天後到期」「1 分鐘後」）由伺服器先算好，頁面另外帶 `data-rel`、`data-due` 時間戳與 `body[data-now]` 伺服器時間，前端每 30 秒依伺服器時鐘重算，用詞與 `time.rs` 相同（兩處要一起改）。
- 儀表板、工作清單、看板、團隊負載在分頁離開 5 分鐘以上、回到前景時重新載入，以顯示其他人的變更；表單頁與畫面上還有提醒提示時不重新載入。
- 靜態檔網址帶內容雜湊（`?v=`），可長期快取；修改 `static/` 後不需要手動清快取。
- 所有非 `GET` 請求都需 CSRF 防護：Session Cookie 設 `SameSite=Lax`；有 `Sec-Fetch-Site` 時以它為準，必須是 `same-origin`（或 `none`）；沒有時才比對 `Origin` 與 `X-Forwarded-Host`（沒有則用 `Host`）或網站網址。前面有 reverse proxy 時不需要保留原始 `Host`。
- 未登入時送出表單（POST）會導向登入頁，登入後回到該 Issue 頁面（或儀表板），不會重送表單。
- 登入後的 `next` 只接受站內路徑，含控制字元、空白或 `\` 的值一律忽略。
- Session token 為 32 bytes 隨機值的 hex；資料庫只存它的 SHA-256，資料庫或備份外洩也無法拿來登入。Cookie 設定 `HttpOnly`、`SameSite=Lax`；`Secure` 預設依網站網址決定（`https://` 開頭才加上），`COOKIE_SECURE=true/false` 可強制覆寫（Worker 固定為 `true`）。
- 以 http 連線時不能把網站網址存成 `https://`（否則之後的登入 Cookie 會加上 Secure 而無法使用）；請先完成 HTTPS 再修改。
- 所有回應都帶安全標頭：`Content-Security-Policy`（只允許同源腳本、禁止被嵌入 frame）、`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: same-origin`；靜態檔以外一律 `Cache-Control: no-store`；`COOKIE_SECURE=true` 時加上 HSTS。
- JSON API（`/api/*`）在未登入時回 401、需要先完成初始設定或改密碼時回 403，不做頁面導向。

## Directory Structure
> Cargo workspace 根目錄為 repo 根目錄，程式碼全部放在 `src/`。

```text
Cargo.toml            - workspace 定義
docker-compose.yml    - Docker 版（app + MariaDB）
.env.example          - Docker 版設定範例（port、container 名稱、時區、DB 帳密）
src/
  core/               - todo-core
    src/              - domain、permission、password、time、store（SQL）、web（Router 與 handlers）
    templates/        - askama HTML 模板
    static/           - CSS、JavaScript
  docker/             - todo-docker：sqlx MySQL 的 Runtime 實作 + tokio 進入點、Dockerfile
  worker/             - todo-worker：D1 的 Runtime 實作 + Worker 進入點、wrangler.toml
  migrations/         - 共用 SQL migration（sqlx 與 wrangler 都讀這裡）
docs/                 - 文件；規範見 docs/rule/
```

- Migration 檔名使用 `NNNN_<name>.sql`，sqlx 與 wrangler 都能辨識。已套用的 migration 不可修改（sqlx 會因 checksum 不符拒絕啟動，wrangler 則會靜默略過），一律新增檔案。
  - 例外紀錄：`0001_init.sql` 在第一次正式部署前、只有測試環境套用過時重寫過一次；在那之前建立的測試資料庫（MariaDB volume、本機 D1 的 `.wrangler/`）需刪除重建。
- 單元測試放在各 crate 的 `#[cfg(test)]` 模組；`core` 測試權限、密碼規則、時間換算、表單驗證、SQL 參數與欄位的對應，以及路由層（以 stub `Runtime` 測試登入導向、CSRF、健康檢查）。

## Build & Run Commands
> 根目錄 `Cargo.toml` 設定 `default-members` 排除 `todo-worker`，因此 `cargo build` / `cargo test` 不帶參數時只處理 core 與 docker。以下指令皆已在測試環境執行過；標示「未驗證」者需要 Cloudflare 帳號登入，尚未執行。

- Build:
  - Docker 版：`cargo build -p todo-docker --release`
  - Worker 版：`cd src/worker && worker-build --release`
- Run:
  - Docker 版：`cp .env.example .env`，填入兩組資料庫密碼（留空時 compose 會拒絕啟動）後執行 `docker compose up -d --build`，以 `curl localhost:<APP_PORT>/healthz` 確認
  - Worker 版（本機）：先套用本機 D1 migration，再執行 `cd src/worker && npx wrangler dev`，以 `curl localhost:8787/healthz` 確認
- Test: `cargo test --workspace --exclude todo-worker`
- Lint/Check:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --exclude todo-worker -- -D warnings`
  - `cargo clippy -p todo-worker --target wasm32-unknown-unknown -- -D warnings`
- Other:
  - D1：`Team-TODO`（PRISVALIS 帳號）；先用 `npx wrangler d1 create Team-TODO` 建立，再把印出的 id 填入 `src/worker/wrangler.toml` 的 `database_id`
  - D1 migration（本機）：`cd src/worker && npx wrangler d1 migrations apply Team-TODO --local`
  - D1 migration（遠端）：`cd src/worker && npx wrangler d1 migrations apply Team-TODO --remote`（未驗證）
  - Worker 部署：`cd src/worker && npx wrangler deploy`（Worker 名稱 `team-todo`：Worker 名稱只能用小寫；需先 `npx wrangler login` 或設定 `CLOUDFLARE_API_TOKEN`）（未驗證）
  - MariaDB migration：Docker 版啟動時以 `sqlx::migrate!("../migrations")` 自動套用；`src/docker/build.rs` 確保新增 migration 檔時會重新編譯

## Environment & Setup Notes

- 開發與測試在 Linux 主機（`<test-host>`）上進行：安裝 Rust stable、`wasm32-unknown-unknown`、Docker、Node.js 24 LTS 後，把專案同步過去執行上述指令。
- 安裝 wasm target 與 worker-build：`rustup target add wasm32-unknown-unknown`、`cargo install worker-build@^0.8 --locked`（worker-build 的 major.minor 必須與 `worker` crate 相同）。
- 預設管理員：帳號 `admin`、初始密碼 `Admin@12345`，由 `src/migrations/0001_init.sql` 寫入資料庫；登入後先進入初始設定頁設定新密碼。部署後要立即完成初始設定：在那之前任何連得到網站的人都能用預設帳密接管管理員。對外公開的部署建議設定 `SETUP_TOKEN`（Docker 寫在 `.env`；Worker 用 `npx wrangler secret put SETUP_TOKEN`），設定頁會要求輸入它。
- Docker 版設定（`.env`，範例見 `.env.example`；compose 會自動讀取）：
  - `APP_PORT`：對外 port（預設 `8080`，綁定所有網路介面，可從外部連線）
  - `APP_CONTAINER_NAME`、`DB_CONTAINER_NAME`：container 名稱
  - `TZ`：container 系統時區（預設 `Asia/Taipei`），也是網站時區的初始值；完成初始設定後網站時區以「網站設定」為準
  - `SITE_NAME`、`SITE_URL`：初始設定頁的預設值（品牌名稱預設 `Team`）；完成設定後以資料庫為準
  - `SETUP_TOKEN`：選填；設定後初始設定頁需要輸入它
  - `COOKIE_SECURE`：留空時依網站網址決定，見上方路由說明
  - `MARIADB_DATABASE`、`MARIADB_USER`、`MARIADB_PASSWORD`、`MARIADB_ROOT_PASSWORD`：只在資料庫 volume 第一次建立時生效；之後要改密碼須進資料庫修改，或刪除 volume 重建
- `todo-docker` 程式本身讀取的環境變數（compose 已依上面的設定自動帶入；不用 compose 直接執行時需自行設定）：`DB_HOST`（預設 `127.0.0.1`）、`DB_PORT`（預設 `3306`）、`DB_USER`、`DB_PASSWORD`（必填）、`DB_NAME`、`BIND_ADDR`（預設 `0.0.0.0:8080`）、`APP_TIMEZONE`（初始時區，預設 `Asia/Taipei`）、`SITE_NAME`、`SITE_URL`、`COOKIE_SECURE`。
- 不使用 compose 的 MariaDB 時，資料庫必須使用 `utf8mb4_nopad_bin` collation（例如 `CREATE DATABASE todo CHARACTER SET utf8mb4 COLLATE utf8mb4_nopad_bin`），否則帳號、標籤名稱的比較會與 D1 不同。
- Worker 版設定：D1 binding 名稱固定為 `DB`；`APP_TIMEZONE`、`SITE_NAME`、`COOKIE_SECURE`（以及選用的 `SITE_URL`）定義於 `src/worker/wrangler.toml` 的 `[vars]`。
- 已知的坑：
  - D1 / Worker 的 JS future 不是 `Send`，axum handler 卻要求 `Send` future；Worker 端以 `worker::send::SendFuture` 包裝（wasm 為單執行緒，安全）。
  - Worker 收到的 request URI 是完整 URL（含 scheme 與 host），Docker 收到的是 `/path`；`core` 只能使用 `uri().path()` / `query()` 與 `Host` header，不可直接使用 `uri()` 字串。
  - `todo-worker` 只能編譯成 wasm，因此 workspace 層級的 test / clippy 指令要排除它，另以 `--target wasm32-unknown-unknown` 檢查。
  - Argon2 雜湊耗 CPU；Workers 免費方案每次請求 CPU 上限 10ms，登入 / 改密碼請求需付費方案。
  - sqlx 0.9 拒絕執行非 `&'static str` 的 SQL；Docker adapter 以 `AssertSqlSafe` 包裝，前提是 SQL 只由 `core` 的常數組成、所有值都用參數綁定，修改 `store.rs` 時不得把使用者輸入拼進 SQL 字串。
  - 共用 SQL 只能使用 MariaDB 與 SQLite 都支援的語法；不得使用 `AUTO_INCREMENT`、`ON DUPLICATE KEY`、`RETURNING`、日期函式、`SUM`（MariaDB 回傳 DECIMAL）等，計數一律用 `COUNT(CASE WHEN … THEN 1 END)`。

## Workflow Rules

- Branch naming: `<type>/<issue-id>-<kebab-desc>`，例如 `feat/12-issue-assign`
- Commit format: Conventional Commits，`<type>(<scope>): <subject>`；scope 使用 `core`、`docker`、`worker`、`db`、`docs`
- Before commit: `cargo fmt --all --check`、兩個 clippy 指令、`cargo test --workspace --exclude todo-worker` 全部通過
- 新功能必須同時在兩個版本可用；只動到一個 adapter 的業務行為變更不得合併。

## MCP / External Tools
> 你用了哪些 MCP，Claude 需要知道什麼時候該用哪個

- Context7: use when looking up library/framework documentation（axum、sqlx、workers-rs、askama）
- Cloudflare MCP: 查詢 Cloudflare 文件、檢視 D1 資料庫與 Worker 部署狀態時使用；建立 / 刪除 D1 等有副作用的操作須先經使用者確認
- markitdown: use when converting documents to markdown for reading
- codebase-memory-mcp: 把整個 codebase 建成知識圖譜，做結構化的程式查詢。任何「探索程式碼」的需求一律先用它，再退回 Grep/Glob/Read。
  - 建立索引：對話中說「Index this project」或呼叫 `index_repository(repo_path)`。改動後用 `detect_changes` / `index_status` 確認是否需要重建。
  - 查詢工具與使用時機：
    - `search_graph(name_pattern / label / qn_pattern)`：找函式、類別、路由等符號。
    - `get_code_snippet(qualified_name)`：取某符號的精確原始碼。
    - `trace_path(function_name, mode=calls|data_flow|cross_service)`：追呼叫鏈、資料流、跨服務呼叫。
    - `query_graph(query)`：用 Cypher 做複雜圖查詢（誰呼叫我、高 fan-out、dead code、影響分析）。
    - `search_code(pattern)`：圖增強的文字搜尋（graph-augmented grep）。
    - `get_architecture(aspects)`：取得專案結構總覽。
    - `manage_adr(mode=store|...)`：保存架構決策（ADR）以跨 session 留存。
  - 使用準則：程式結構/呼叫關係用圖查詢；純文字、設定檔、非程式檔用 Grep/Glob/Read，且編輯前一定先 Read。專案若尚未索引，先 `index_repository`。
- ponytail: 強制「最懶但能動」的解法（YAGNI、優先 stdlib/原生功能、能一行就一行、最短 diff）。預設常駐 full 模式，會在每次回應生效，直到使用者說 "stop ponytail" / "normal mode" 才關閉。
  - `/ponytail lite|full|ultra`：切換強度（full 為預設）。
  - `/ponytail-review`：只審查 over-engineering，列出可刪除/可用 stdlib 取代的部分（針對 diff，不改檔）。使用時機：使用者問「這是不是過度設計」「有什麼可以刪」「review for over-engineering」。
  - `/ponytail-audit`：對整個 repo 做 over-engineering 稽核，給出可刪/可簡化的排序清單（不改檔）。使用時機：「audit this codebase」「find bloat」。
  - `/ponytail-debt`：收集程式中所有 `ponytail:` 註解，整理成技術債清單（不改檔）。使用時機：「list the shortcuts」「what did ponytail defer」。
  - `/ponytail-gain`：顯示 ponytail 的效益計分板（less code/cost、more speed）。
  - `/ponytail-help`：所有 ponytail 模式與指令的速查卡。
  - 不可因懶而省略：信任邊界的輸入驗證、避免資料遺失的錯誤處理、安全措施、無障礙基本要求、使用者明確要求的功能。非平凡邏輯要留一個可執行的最小檢查（Rust 為 `#[cfg(test)]` 單元測試）。

## Reference Docs
> 指向 docs/ 資料夾，讓 Claude 自行判斷是否需要讀

- [`開發守則.md`](./開發守則.md) - 開發原則（YAGNI、KISS、DRY、SOLID、單元測試）
- [`文件製作與維護規範.md`](./文件製作與維護規範.md) - 文件存放、格式與更新時機
- [`AI 行為準則.md`](./AI%20行為準則.md) - AI Agent 行為準則
