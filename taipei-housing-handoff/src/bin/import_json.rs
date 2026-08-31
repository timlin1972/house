//! 匯入 data/*.json（舊系統累積的追蹤清單）進 SQLite。
//!
//! 重跑安全：每個檔案匯入前，先刪掉該 tracked_search 底下所有 listings/check_runs/
//! check_run_events/reviews 再重建，永遠反映當次 JSON 檔案的完整內容。
//!
//! duplicateOf 攤平：舊資料裡發現代表轉移後，其他仲介刊登的 duplicateOf 有時沒有
//! 跟著更新，仍指向「已經不是代表」的舊刊登（形成一條鏈）。這裡一律沿鏈往下解到
//! 最終代表（duplicate_of 為 null 的那筆）才寫入資料庫，讓 duplicate_of 永遠只有一層。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;

use taipei_housing::db;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    search_url: String,
    artifact_url: Option<String>,
    criteria: serde_json::Value,
    #[serde(default)]
    dedup_note: Option<String>,
    #[serde(default)]
    last_checked: Option<String>,
    #[serde(default)]
    history: Vec<RawHistoryEntry>,
    #[serde(default)]
    listings: Vec<RawListing>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryEntry {
    date: String,
    new_count: i64,
    total_count: i64,
    #[serde(default)]
    new_ids: Vec<String>,
    #[serde(default)]
    delisted_ids: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListing {
    id: String,
    title: String,
    price: i64,
    unit_price: f64,
    rooms: String,
    area: String,
    #[serde(default)]
    main_area: Option<String>,
    #[serde(default)]
    age: Option<i64>,
    floor: String,
    #[serde(default)]
    community: Option<String>,
    address: String,
    #[serde(default)]
    agent: Option<String>,
    first_seen: String,
    #[serde(default)]
    duplicate_of: Option<String>,
    #[serde(default)]
    delisted: bool,
    #[serde(default)]
    delisted_date: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// 沿 duplicate_of 鏈往下追，直到找到 duplicate_of 為 null 的那筆（真正的代表刊登），
/// 回傳它的 id。若鏈本身斷掉（極端情況，指向一個不存在的 id），停在最後已知的 id 上。
fn resolve_representative(start: &str, raw_duplicate_of: &HashMap<String, Option<String>>) -> String {
    let mut current = start.to_string();
    let mut hops = 0;
    loop {
        match raw_duplicate_of.get(&current) {
            Some(Some(next)) => {
                current = next.clone();
                hops += 1;
                if hops > 1000 {
                    panic!("duplicate_of 鏈疑似循環，起點 id={start}");
                }
            }
            _ => return current,
        }
    }
}

fn parse_search_url(url: &str) -> Result<(String, String, String)> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    let params: HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();

    let section = *params
        .get("section")
        .with_context(|| format!("searchUrl 缺少 section 參數: {url}"))?;
    let shape = *params
        .get("shape")
        .with_context(|| format!("searchUrl 缺少 shape 參數: {url}"))?;
    let price = *params
        .get("price")
        .with_context(|| format!("searchUrl 缺少 price 參數: {url}"))?;

    let district = match section {
        "3" => "中山區",
        "4" => "松山區",
        other => bail!("未知的 section={other}，searchUrl: {url}"),
    };
    let building_type = match shape {
        "5" => "華廈",
        "2" => "電梯大樓",
        other => bail!("未知的 shape={other}，searchUrl: {url}"),
    };

    Ok((district.to_string(), building_type.to_string(), price.to_string()))
}

fn parse_date(s: &str, field: &str, listing_or_run_id: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("{field} 日期格式錯誤 {s:?}（{listing_or_run_id}）"))
}

async fn import_file(pool: &SqlitePool, path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("讀取 {} 失敗", path.display()))?;
    let file: RawFile =
        serde_json::from_str(&raw).with_context(|| format!("解析 {} 失敗", path.display()))?;

    let (district, building_type, price_range) = parse_search_url(&file.search_url)?;
    let criteria_json = serde_json::to_string(&file.criteria)?;
    let last_checked = file
        .last_checked
        .as_deref()
        .map(|s| parse_date(s, "lastChecked", &file.search_url))
        .transpose()?;

    let mut tx = pool.begin().await?;

    // listings 陣列裡重複刊登有時排在它的代表刊登「之前」，SQLite 的外鍵預設是逐筆立即
    // 檢查，defer_foreign_keys 把檢查延到 commit 時，讓同一個 transaction 內的寫入順序不再重要。
    sqlx::query("PRAGMA defer_foreign_keys = ON;")
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO tracked_searches
            (district, building_type, price_range, search_url, artifact_url, criteria_json, dedup_note, last_checked)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(search_url) DO UPDATE SET
            district = excluded.district,
            building_type = excluded.building_type,
            price_range = excluded.price_range,
            artifact_url = excluded.artifact_url,
            criteria_json = excluded.criteria_json,
            dedup_note = excluded.dedup_note,
            last_checked = excluded.last_checked,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(&district)
    .bind(&building_type)
    .bind(&price_range)
    .bind(&file.search_url)
    .bind(&file.artifact_url)
    .bind(&criteria_json)
    .bind(&file.dedup_note)
    .bind(last_checked.map(|d| d.to_string()))
    .execute(&mut *tx)
    .await?;

    let tracked_search_id: i64 =
        sqlx::query_scalar("SELECT id FROM tracked_searches WHERE search_url = ?")
            .bind(&file.search_url)
            .fetch_one(&mut *tx)
            .await?;

    // 讓腳本可重複執行：先清掉這組清單既有的子資料，再依當次 JSON 內容重建。
    sqlx::query("DELETE FROM check_run_events WHERE tracked_search_id = ?")
        .bind(tracked_search_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM check_runs WHERE tracked_search_id = ?")
        .bind(tracked_search_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM reviews WHERE tracked_search_id = ?")
        .bind(tracked_search_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM listings WHERE tracked_search_id = ?")
        .bind(tracked_search_id)
        .execute(&mut *tx)
        .await?;

    // check_runs 要先插入，才能在插入 listings 時回填 last_seen_check_run_id
    // （沒被標記 delisted 的物件，視為在「最後一次已知的 check_run」仍然在架)。
    let mut last_check_run_id: Option<i64> = None;
    for h in &file.history {
        let checked_date = parse_date(&h.date, "history.date", &file.search_url)?;

        let check_run_id: i64 = sqlx::query_scalar(
            "INSERT INTO check_runs (tracked_search_id, checked_date, new_count, total_count, note)
             VALUES (?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(tracked_search_id)
        .bind(checked_date.to_string())
        .bind(h.new_count)
        .bind(h.total_count)
        .bind(&h.note)
        .fetch_one(&mut *tx)
        .await?;
        last_check_run_id = Some(check_run_id);

        for id in &h.new_ids {
            sqlx::query(
                "INSERT INTO check_run_events (check_run_id, tracked_search_id, listing_id, event_type)
                 VALUES (?, ?, ?, 'new')",
            )
            .bind(check_run_id)
            .bind(tracked_search_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        for id in &h.delisted_ids {
            sqlx::query(
                "INSERT INTO check_run_events (check_run_id, tracked_search_id, listing_id, event_type)
                 VALUES (?, ?, ?, 'delisted')",
            )
            .bind(check_run_id)
            .bind(tracked_search_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
    }

    let raw_duplicate_of: HashMap<String, Option<String>> = file
        .listings
        .iter()
        .map(|l| (l.id.clone(), l.duplicate_of.clone()))
        .collect();

    let mut representative_count = 0;
    let mut duplicate_count = 0;
    let mut delisted_count = 0;

    for l in &file.listings {
        let area: f64 = l
            .area
            .parse()
            .with_context(|| format!("area 格式錯誤 {:?}（listing {}）", l.area, l.id))?;
        let main_area: Option<f64> = l
            .main_area
            .as_deref()
            .map(|s| s.parse::<f64>())
            .transpose()
            .with_context(|| format!("mainArea 格式錯誤（listing {}）", l.id))?;
        let first_seen = parse_date(&l.first_seen, "firstSeen", &l.id)?;
        let delisted_date = l
            .delisted_date
            .as_deref()
            .map(|s| parse_date(s, "delistedDate", &l.id))
            .transpose()?;

        let representative = resolve_representative(&l.id, &raw_duplicate_of);
        let duplicate_of = if representative == l.id {
            None
        } else {
            Some(representative)
        };

        match &duplicate_of {
            None => representative_count += 1,
            Some(_) => duplicate_count += 1,
        }
        if l.delisted {
            delisted_count += 1;
        }

        // 沒被標記 delisted 的物件，視為在最後一次已知的 check_run 仍然在架；
        // 已經 delisted 的物件不知道確切是哪一次沒看到的，留 NULL（下次抓取若再沒看到，
        // 對照邏輯本來就會把它當「早就不在架」處理，不影響下架判斷正確性）。
        let last_seen_check_run_id = if l.delisted { None } else { last_check_run_id };

        sqlx::query(
            "INSERT INTO listings
                (tracked_search_id, id, title, price, unit_price, rooms, area, main_area, age,
                 floor, community, address, agent, first_seen, duplicate_of, delisted,
                 delisted_date, note, last_seen_check_run_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(tracked_search_id)
        .bind(&l.id)
        .bind(&l.title)
        .bind(l.price)
        .bind(l.unit_price)
        .bind(&l.rooms)
        .bind(area)
        .bind(main_area)
        .bind(l.age)
        .bind(&l.floor)
        .bind(&l.community)
        .bind(&l.address)
        .bind(&l.agent)
        .bind(first_seen.to_string())
        .bind(&duplicate_of)
        .bind(l.delisted)
        .bind(delisted_date.map(|d| d.to_string()))
        .bind(&l.note)
        .bind(last_seen_check_run_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    println!(
        "{}: {district}{building_type}{price_range} — {} 筆刊登（代表 {representative_count} / 重複 {duplicate_count} / 下架 {delisted_count}），{} 筆 check run",
        path.display(),
        file.listings.len(),
        file.history.len(),
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = std::env::args().nth(1).unwrap_or_else(|| "data".to_string());
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://taipei_housing.db".to_string());

    let pool = db::connect(&database_url).await?;

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&data_dir)
        .with_context(|| format!("讀取資料夾 {data_dir} 失敗"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!("{data_dir} 底下找不到任何 *.json 檔案");
    }

    for path in &paths {
        import_file(&pool, path)
            .await
            .with_context(|| format!("匯入 {} 失敗", path.display()))?;
    }

    println!("完成，共匯入 {} 組追蹤清單", paths.len());
    Ok(())
}
