-- 8 組追蹤清單（行政區 x 建物類型 x 價格區間）
CREATE TABLE tracked_searches (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    district        TEXT NOT NULL,          -- 中山區 / 松山區
    building_type   TEXT NOT NULL,          -- 華廈 / 電梯大樓
    price_range     TEXT NOT NULL,          -- '2000_3000' / '3000_4000'
    search_url      TEXT NOT NULL UNIQUE,   -- 591 原始搜尋網址
    artifact_url    TEXT,                   -- 舊系統 Claude Artifact 網址，新系統可為 NULL
    criteria_json   TEXT NOT NULL,          -- 原 JSON criteria 欄位，原樣保留（實測是描述字串，非物件）
    dedup_note       TEXT,                   -- 原 JSON dedupNote：這組清單專屬的去重規則與歷史修正案例，務必保留
    last_checked    TEXT,                   -- ISO date 'YYYY-MM-DD'
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- 每次排程檢查的紀錄（對應 JSON 的 history[]）
-- 沒有加 UNIQUE(tracked_search_id, checked_date)：實測舊資料常常同一天手動重跑檢查好幾次
-- （例如代表轉移發生後當天又核對一次），同一天多筆 check_run 是合法情況。
-- 定義在 listings 之前：listings.last_seen_check_run_id 需要參照這張表。
CREATE TABLE check_runs (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    tracked_search_id INTEGER NOT NULL REFERENCES tracked_searches(id),
    checked_date      TEXT NOT NULL,           -- ISO date
    new_count         INTEGER NOT NULL,
    total_count       INTEGER NOT NULL,
    note              TEXT,                    -- 人類可讀的當次異動說明
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_check_runs_tracked_search ON check_runs(tracked_search_id);

-- 所有曾出現過的物件（含重複刊登）
-- PK 用 (tracked_search_id, id) 而非單獨 id：實測發現同一個591 id可能出現在兩組不同追蹤清單裡
-- （中山華廈-4000 與 中山電梯大樓-4000 都有 20759274），且 duplicateOf 一律只在同一組清單內互相參照，
-- 用複合鍵可以忠實對應原始資料的分組方式，也天然避免這種跨組id碰撞造成匯入失敗。
CREATE TABLE listings (
    tracked_search_id INTEGER NOT NULL REFERENCES tracked_searches(id),
    id                TEXT NOT NULL,           -- 591 物件 id
    title             TEXT NOT NULL,
    price             INTEGER NOT NULL,        -- 萬元
    unit_price        REAL NOT NULL,           -- 萬/坪
    rooms             TEXT NOT NULL,
    area              REAL NOT NULL,           -- 權狀坪數，去重比對允許 0.1 坪誤差
    main_area         REAL,                    -- 主建坪數，可能為 NULL
    age               INTEGER,                 -- 屋齡，可能為 NULL
    floor             TEXT NOT NULL,
    community         TEXT,                    -- 社區/大樓名稱，可能為 NULL
    address           TEXT NOT NULL,
    agent             TEXT,                    -- 實測偶爾為 NULL（例：店面刊登無仲介姓名）
    first_seen        TEXT NOT NULL,           -- ISO date
    duplicate_of      TEXT,                    -- NULL = 代表刊登；非NULL指向同一組清單裡的代表刊登id
    delisted          INTEGER NOT NULL DEFAULT 0,   -- 0/1，真下架時為 1
    delisted_date     TEXT,
    note              TEXT,                    -- 原 JSON listings[].note：該筆刊登的人工註記（如代表轉移說明）
    -- 這個 id 最後一次出現在抓取結果裡，是哪一次 check_run（不是哪一天——同一天可能跑好幾次）。
    -- 下架判斷要求「連續兩次都沒看到」，就是拿這個值跟「上一次 check_run 的 id」比對：
    -- 等於上一次 → 這次沒看到只是第一次沒看到，先不下架；小於上一次 → 已經連續兩次沒看到，確定下架。
    last_seen_check_run_id INTEGER REFERENCES check_runs(id),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (tracked_search_id, id),
    FOREIGN KEY (tracked_search_id, duplicate_of) REFERENCES listings(tracked_search_id, id)
);

CREATE INDEX idx_listings_duplicate_of   ON listings(tracked_search_id, duplicate_of);
CREATE INDEX idx_listings_delisted_active ON listings(tracked_search_id) WHERE duplicate_of IS NULL AND delisted = 0;
CREATE INDEX idx_listings_last_seen ON listings(last_seen_check_run_id);
-- 去重比對主要依 address/floor 縮小候選範圍，再用 area/main_area/age 精算
CREATE INDEX idx_listings_dedup_lookup   ON listings(tracked_search_id, address, floor);

-- history[].newIds / delistedIds 攤平成事件列表，一個 check_run 對多個 listing
CREATE TABLE check_run_events (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    check_run_id      INTEGER NOT NULL REFERENCES check_runs(id),
    tracked_search_id INTEGER NOT NULL,
    listing_id        TEXT NOT NULL,
    event_type        TEXT NOT NULL CHECK (event_type IN ('new', 'delisted')),
    FOREIGN KEY (tracked_search_id, listing_id) REFERENCES listings(tracked_search_id, id)
);

CREATE INDEX idx_check_run_events_run     ON check_run_events(check_run_id);
CREATE INDEX idx_check_run_events_listing ON check_run_events(tracked_search_id, listing_id);

-- 使用者評價。PK 用 (tracked_search_id, listing_id) 而非單獨 591_id：
-- 理由同 listings 表，避免跨組 id 碰撞時評價寫錯物件。
CREATE TABLE reviews (
    tracked_search_id INTEGER NOT NULL,
    listing_id        TEXT NOT NULL,
    decision          TEXT NOT NULL CHECK (decision IN ('want', 'pass')),
    reason            TEXT,
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (tracked_search_id, listing_id),
    FOREIGN KEY (tracked_search_id, listing_id) REFERENCES listings(tracked_search_id, id)
);
