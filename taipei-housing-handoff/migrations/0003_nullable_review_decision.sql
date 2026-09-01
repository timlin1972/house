-- 讓「寫原因」跟「按要/不要」變成各自獨立送出：使用者可能只打了原因、還沒點決定，
-- 這種「尚未決定」的中繼狀態也要能存住，所以 decision 不能再是 NOT NULL。
-- SQLite 不支援直接 ALTER COLUMN 拿掉 NOT NULL，只能整張表重建。
CREATE TABLE reviews_new (
    tracked_search_id INTEGER NOT NULL,
    listing_id        TEXT NOT NULL,
    decision          TEXT CHECK (decision IN ('want', 'pass')),
    reason            TEXT,
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (tracked_search_id, listing_id),
    FOREIGN KEY (tracked_search_id, listing_id) REFERENCES listings(tracked_search_id, id)
);

INSERT INTO reviews_new (tracked_search_id, listing_id, decision, reason, updated_at)
SELECT tracked_search_id, listing_id, decision, reason, updated_at FROM reviews;

DROP TABLE reviews;
ALTER TABLE reviews_new RENAME TO reviews;
