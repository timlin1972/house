# 台北房屋觀察簿 — 專案筆記

改寫自「跟 Claude.ai 對話 + claude-in-chrome 手動維運」的舊流程，規格見 `PROJECT_SPEC.md`。
這份筆記是給接手這個專案的 Claude Code session 看的：現況、關鍵設計決策、真的踩過的坑。

## 現況

`PROJECT_SPEC.md` 的待辦①-④都做完了：SQLite schema + migration、591抓取層（headless_chrome）、
去重/下架判斷邏輯、排程器 + 網頁前端 + 評價寫入 API。待辦⑤（Email/推播通知）還沒做。

`taipei_housing.db` **有 commit 進 repo**（故意的，見 `.gitignore` 裡的說明）：換機器不用重新從
`data/` 匯入一次 8 個 JSON。裡面已經有 8 組追蹤清單、實際抓過的刊登資料。

## 檔案結構

```
src/
  db/mod.rs, models.rs   -- SQLite 連線 + migration 執行 + 資料表對應的 struct
  scraper/mod.rs          -- 591 抓取（headless_chrome，見下方「591 相關眉角」）
  dedup.rs                -- 「同一戶被不同仲介重複刊登」的內容比對規則
  delist.rs               -- 每次抓取後的對帳：新戶/多一筆重複/換id轉移/下架判斷
  pipeline.rs             -- 串起「開瀏覽器→8組依序抓取+對帳→關閉」，PipelineRunner 做進度追蹤+互斥
  scheduler.rs            -- tokio-cron-scheduler，每天台北時間 08:00 觸發
  web/                    -- axum + askama SSR，見下方「網頁功能」
  bin/import_json.rs      -- 一次性匯入 data/*.json（舊系統累積資料）
  bin/run_once.rs         -- CLI 手動跑一次全部抓取（不透過網頁）
migrations/
  0001_init.sql            -- 初始 schema。**資料庫已經套用過這個版本了，不能再改內容**
                              （sqlx 用 checksum 檔案內容比對已套用的 migration，改了會直接報錯拒絕啟動）。
  0002_custom_tracked_searches.sql -- 加 tracked_searches.name 欄位。之後要改 schema，一律加新的
                              migrations/000N_xxx.sql 檔案，不要回頭改已經存在的檔案。
templates/index.html       -- 唯一的頁面模板
data/                      -- 舊系統的 8 個 JSON（import_json.rs 的輸入來源，已經匯入過，正常不用再動）
```

## 591 相關眉角（都是實測踩出來的，不是憑空猜的）

- **591 沒有可直接呼叫的公開 JSON API**（賣屋這邊；租屋那邊有但需要前端動態生成的簽章參數，
  沒人記錄過）。已改用 `headless_chrome` 直接讀 DOM，不用再花時間重新研究 API。
- **卡片選擇器**：真正的中古屋卡片是 `.ware-item` 且內部有 `.ware-item__attrs`。591 會混入
  「熱銷建案/好康推薦」卡片，一樣用 `.ware-item` class 但沒有 `.ware-item__attrs`（不同的
  Vue 元件、不同的 class 命名法），抓取時已經用這個判斷式濾掉。
- **「24開頭」追蹤連結**：id 每次抓都會變、內容（標題/價格/坪數）跟同一批裡另一筆正常 id
  完全一樣，不是真實新刊登。已經用內容比對可靠濾掉（見 `scraper::drop_tracking_redirects`）。
- **分頁會 drift**：591 列表是即時排序（可能依更新時間），單次抓取翻頁翻到一半，內容可能已經
  變動，導致同一個 id 出現在兩、三個不同分頁上、也可能整批漏抓一部分（實測某組 103 筆宣稱
  總數，單次抓取只拿到 77 筆不重複）。這不是抓取速度問題，是 591 分頁機制本身的限制。
  **因此下架判斷不能只看單次抓取結果**——見下方。
- **591 id 有時候會換，即使該戶從沒真的消失過**：不能只靠 id 比對「這是不是新戶」，一律要用
  `dedup::same_household`（比對 address/community/floor/area/main_area/age）跟這組清單裡所有
  已知物件比對內容，比對到才判斷是新戶還是換id/多一筆重複。

## 核心邏輯設計決策

- **下架判斷用「連續兩次都沒看到」**，不是 spec 原文字面上的單次比對——因為上面提到的分頁
  drift，單次沒看到的風險太高。這是跟使用者確認過、刻意偏離 spec 原文的決策。
  實作方式：`listings.last_seen_check_run_id` 記錄「最後一次被看到是哪次 check_run」，
  跟「上一次 check_run 的 id」比對，連續兩次都沒看到才真的標記下架。
- **代表轉移的優先順序**：該戶的代表消失時，優先轉移給「已知、現存的其他刊登」（例如原本就
  判定為重複刊登、今天還在架上的那筆），只有在完全沒有已知現存成員時，才會把代表轉移給
  一筆全新的 id（見 `delist.rs` 的 PASS 1 跟 PASS 2）。
- **`duplicate_of` 永遠只有一層**，不會有 A→B→C 這種鏈——匯入舊資料時發現歷史資料有這種
  沒攤平的鏈（代表轉移後其他重複刊登沒有跟著更新），已經在 `import_json.rs` 攤平過；
  之後的程式邏輯（`delist.rs`）本身也維持這個不變量，每次轉移都會 cascade 更新所有
  原本指向舊代表的重複刊登。
- **`listings` 的 PK 是 `(tracked_search_id, id)`，不是單獨 `id`**——實測發現同一個 591 id
  可能同時出現在兩組不同追蹤清單裡。`reviews`/`check_run_events` 同理。

## 網頁功能

- `GET /`：側邊欄 8+ 組按鈕（純前端 JS 切換，不重新整理頁面），每組自己的統計數字
  （追蹤中/今日新增/已評價/要的/已下架，跟著分頁一起換，不是全部加總），物件卡片按
  **價格排序**。
- `POST /reviews`：要/不要 + 原因，upsert 進 `reviews` 表。
- `POST /tracked-searches`：網頁上新增自訂追蹤清單（名稱 + 591搜尋網址），不限定原本
  8 組固定的 region/shape 組合。
- `POST /run-now` + `GET /run-status`：手動觸發全部抓取，網頁會輪詢進度（第幾組/共幾組、
  目前在跑哪組），跑完會跳出瀏覽器通知（`Notification` API，需要使用者允許）。
  跟排程器共用同一個 `PipelineRunner`，兩邊不會同時跑。

## 怎麼跑

```bash
cargo run --bin import-json   # 只有全新資料庫、data/*.json 還沒匯入時才需要
cargo run --bin run-once      # 手動立刻跑一次全部抓取（不用等排程或開網頁）
cargo run --bin taipei-housing  # 啟動服務：排程器（每天台北時間08:00）+ 網頁(預設:3000)
cargo test                     # dedup 的規則測試 + delist 對帳邏輯的整合測試（用真的sqlite+migrations）
```

環境變數：`DATABASE_URL`（預設 `sqlite://taipei_housing.db`）、`PORT`（預設 3000）、
`RUST_LOG=info` 看詳細 log。

## 全新機器上要注意

- **ARM 機器（樹莓派等）不能用 headless_chrome 內建的自動下載**：`headless_chrome`（開了
  `fetch` feature）在 Linux 上 `path` 沒設定時一律下載 x86_64 版 Chromium，完全沒有依 arch
  判斷（看過 crate 原始碼 `browser/process.rs`／`browser/fetcher.rs` 確認過），在 aarch64
  上執行會是 `啟動 headless_chrome 失敗 error=Exec format error (os error 8)`。
  `pipeline.rs` 已經改成：先用 `which` 找系統裝好的 `chromium`/`chromium-browser`/
  `google-chrome`/`google-chrome-stable`（也可以用 `CHROME_PATH` 環境變數指定），找不到才
  falls back 給 headless_chrome 自己的下載邏輯。ARM 機器要先 `sudo apt-get install chromium`
  （Raspberry Pi OS 內建就有）。
- **headless_chrome 第一次用會自動下載 Chromium**（沒有系統瀏覽器可用時才會走這條路；
  ~150-250MB，存在 `~/.local/share/headless-chrome/`，這個目錄**不在 repo 裡、也不會跟著
  git 走**，全新機器第一次跑 `run-once`/`taipei-housing` 都要重新下載——x86_64 機器沒裝
  系統瀏覽器也能用這個）。
- **Chromium 執行需要系統函式庫**，缺的話會報 `error while loading shared libraries: libnss3.so...`。
  Ubuntu/Debian 系列先跑：
  ```
  sudo apt-get update && sudo apt-get install -y libnss3 libnspr4 libatk1.0-0 libatk-bridge2.0-0 \
    libcups2 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 \
    libasound2 libpangocairo-1.0-0 libpango-1.0-0 libcairo2 libx11-6 libxext6 libxrender1
  ```

## 還沒做的部分

- Email/推播通知（spec 待辦⑤，優先度最低）
- 部署腳本/Dockerfile/systemd service——目前是「直接在機器上跑一個長駐 process」，沒有
  開機自動啟動、當機自動重啟的機制
