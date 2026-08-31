use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;
use sqlx::sqlite::SqlitePool;
use taipei_housing::db;
use taipei_housing::delist::reconcile_tracked_search;
use taipei_housing::scraper::ScrapedCard;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

async fn fresh_pool(name: &str) -> SqlitePool {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("taipei_housing_test_{name}_{n}.db"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-journal", path.display()));
    db::connect(&format!("sqlite://{}", path.display())).await.unwrap()
}

async fn make_tracked_search(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO tracked_searches (district, building_type, price_range, search_url, criteria_json)
         VALUES ('松山區', '電梯大樓', '3000_4000', 'https://example.com/test', '\"測試\"')
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn card(id: &str, address: &str, floor: &str, area: f64, main_area: Option<f64>, age: Option<i64>) -> ScrapedCard {
    ScrapedCard {
        id: id.to_string(),
        title: format!("測試物件 {id}"),
        price: 3500,
        unit_price: 95.0,
        rooms: "3房2廳2衛".to_string(),
        area,
        main_area,
        age,
        floor: floor.to_string(),
        community: Some("測試社區".to_string()),
        address: address.to_string(),
        agent: Some("仲介測試".to_string()),
    }
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// 這是最早驅動整個 delist.rs 重寫的案例：兩筆內容完全相同（同一戶）、但591 id不同的刊登
/// 同時第一次出現在同一次抓取裡（松山電梯大樓-4000那組實測108筆原始物件裡79%都是這種重複）。
/// 一定要能互相配對成同一戶，而不是各自變成獨立代表。
#[tokio::test]
async fn two_brand_new_cards_in_same_scrape_dedupe_to_one_household() {
    let pool = fresh_pool("first_scrape_dedupe").await;
    let ts_id = make_tracked_search(&pool).await;

    let summary = reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![
            card("30000050", "松山區-測試路五", "8F/15F", 36.0, Some(27.0), Some(30)),
            card("30000051", "松山區-測試路五", "8F/15F", 36.0, Some(27.0), Some(30)),
        ],
    )
    .await
    .unwrap();

    assert_eq!(summary.total_active_representatives, 1, "兩筆同戶刊登應該只算一個代表");

    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, duplicate_of FROM listings WHERE tracked_search_id = ? ORDER BY id",
    )
    .bind(ts_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], ("30000050".to_string(), None), "先處理到的那筆是代表");
    assert_eq!(
        rows[1],
        ("30000051".to_string(), Some("30000050".to_string())),
        "後處理到的那筆要掛在代表底下，不是自己變成獨立代表"
    );
}

/// PASS 1（代表轉移）不需要任何新卡片就該發生：代表本身消失、已知重複刊登還在架上，
/// 這次抓取結果裡完全沒有新id也一樣要轉移。
#[tokio::test]
async fn representative_transfer_happens_without_any_new_card() {
    let pool = fresh_pool("transfer_no_new_card").await;
    let ts_id = make_tracked_search(&pool).await;

    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![
            card("30000060", "松山區-測試路六", "9F/11F", 38.0, Some(29.0), Some(25)),
            card("30000061", "松山區-測試路六", "9F/11F", 38.0, Some(29.0), Some(25)),
        ],
    )
    .await
    .unwrap();

    // Day2：代表 30000060 消失，只剩下已知的重複刊登 30000061，沒有任何新id出現。
    let summary = reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-02"),
        vec![card("30000061", "松山區-測試路六", "9F/11F", 38.0, Some(29.0), Some(25))],
    )
    .await
    .unwrap();

    assert_eq!(summary.id_changes, vec![("30000060".to_string(), "30000061".to_string())]);
    assert!(summary.delisted_ids.is_empty(), "該戶還在架上（換了代表而已），不該被判下架");

    let (rep_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000061")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rep_dup_of, None);
}

#[tokio::test]
async fn new_household_is_inserted_as_representative() {
    let pool = fresh_pool("new_household").await;
    let ts_id = make_tracked_search(&pool).await;

    let cards = vec![card("30000001", "松山區-測試路", "5F/10F", 35.0, Some(28.0), Some(20))];
    let summary = reconcile_tracked_search(&pool, ts_id, date("2026-09-01"), cards)
        .await
        .unwrap();

    assert_eq!(summary.new_ids, vec!["30000001".to_string()]);
    assert!(summary.delisted_ids.is_empty());
    assert_eq!(summary.total_active_representatives, 1);

    let (duplicate_of, delisted): (Option<String>, bool) = sqlx::query_as(
        "SELECT duplicate_of, delisted FROM listings WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(ts_id)
    .bind("30000001")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(duplicate_of, None);
    assert!(!delisted);
}

#[tokio::test]
async fn id_change_transfers_representative_and_review() {
    let pool = fresh_pool("id_change").await;
    let ts_id = make_tracked_search(&pool).await;

    // Day 1：物件用 id A 出現，是代表刊登，使用者評價「要」。
    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![card("30000010", "松山區-測試路", "5F/10F", 35.0, Some(28.0), Some(20))],
    )
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO reviews (tracked_search_id, listing_id, decision, reason) VALUES (?, ?, 'want', '喜歡')",
    )
    .bind(ts_id)
    .bind("30000010")
    .execute(&pool)
    .await
    .unwrap();

    // Day 2：id A 消失，換成內容完全一樣（同一戶）的新 id B。
    let summary = reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-02"),
        vec![card("30000099", "松山區-測試路", "5F/10F", 35.0, Some(28.0), Some(20))],
    )
    .await
    .unwrap();

    assert_eq!(summary.id_changes, vec![("30000010".to_string(), "30000099".to_string())]);
    assert!(summary.delisted_ids.is_empty(), "換id不該被判成下架");

    let (old_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000010")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_dup_of, Some("30000099".to_string()), "舊代表應該改指向新代表");

    let (new_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000099")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(new_dup_of, None, "新id應該成為代表");

    let old_review: Option<(String,)> =
        sqlx::query_as("SELECT decision FROM reviews WHERE tracked_search_id = ? AND listing_id = ?")
            .bind(ts_id)
            .bind("30000010")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(old_review.is_none(), "評價不該留在舊id上");

    let new_review: (String, Option<String>) =
        sqlx::query_as("SELECT decision, reason FROM reviews WHERE tracked_search_id = ? AND listing_id = ?")
            .bind(ts_id)
            .bind("30000099")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(new_review.0, "want");
    assert_eq!(new_review.1, Some("喜歡".to_string()));
}

#[tokio::test]
async fn duplicate_listing_transfer_cascades_to_other_duplicates() {
    let pool = fresh_pool("cascade").await;
    let ts_id = make_tracked_search(&pool).await;

    // Day1：代表 A，加一筆重複刊登 B（同戶不同仲介）。
    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![
            card("30000020", "松山區-測試路二", "6F/12F", 40.0, Some(30.0), Some(15)),
            card("30000021", "松山區-測試路二", "6F/12F", 40.0, Some(30.0), Some(15)),
        ],
    )
    .await
    .unwrap();

    // Day2：代表 A 本身消失，但既有重複刊登 B 還在架上，另外多冒出一筆全新 id C（同戶第三筆刊登）。
    // 按 spec「通常選現存id中數字最小的一筆」，代表應該轉移給既有的 B，而不是憑空轉給全新的 C；
    // C 則單純成為 B 底下的第三筆重複刊登。
    let summary = reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-02"),
        vec![
            card("30000099", "松山區-測試路二", "6F/12F", 40.0, Some(30.0), Some(15)),
            card("30000021", "松山區-測試路二", "6F/12F", 40.0, Some(30.0), Some(15)),
        ],
    )
    .await
    .unwrap();

    assert_eq!(summary.id_changes, vec![("30000020".to_string(), "30000021".to_string())]);

    let (a_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000020")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(a_dup_of, Some("30000021".to_string()), "舊代表要指向新代表（既有的B，不是憑空冒出的C）");

    let (b_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000021")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(b_dup_of, None, "B 應該成為新代表");

    let (c_dup_of,): (Option<String>,) =
        sqlx::query_as("SELECT duplicate_of FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000099")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(c_dup_of, Some("30000021".to_string()), "C 是同戶第三筆刊登，應該掛在新代表B底下");
}

#[tokio::test]
async fn delisting_requires_two_consecutive_misses() {
    let pool = fresh_pool("two_miss").await;
    let ts_id = make_tracked_search(&pool).await;

    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![card("30000030", "松山區-測試路三", "7F/9F", 33.0, None, None)],
    )
    .await
    .unwrap();

    // Day2：沒看到，應該只是「第一次沒看到」，還不算下架。
    let day2 = reconcile_tracked_search(&pool, ts_id, date("2026-09-02"), vec![])
        .await
        .unwrap();
    assert!(day2.delisted_ids.is_empty(), "第一次沒看到不該直接判下架");

    let (delisted_after_day2,): (bool,) =
        sqlx::query_as("SELECT delisted FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000030")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!delisted_after_day2);

    // Day3：連續第二次沒看到，確定下架。
    let day3 = reconcile_tracked_search(&pool, ts_id, date("2026-09-03"), vec![])
        .await
        .unwrap();
    assert_eq!(day3.delisted_ids, vec!["30000030".to_string()]);

    let (delisted_after_day3, delisted_date): (bool, Option<String>) = sqlx::query_as(
        "SELECT delisted, delisted_date FROM listings WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(ts_id)
    .bind("30000030")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delisted_after_day3);
    assert_eq!(delisted_date, Some("2026-09-03".to_string()));
}

#[tokio::test]
async fn relisting_with_same_id_clears_delisted_flag() {
    let pool = fresh_pool("relist").await;
    let ts_id = make_tracked_search(&pool).await;

    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-01"),
        vec![card("30000040", "松山區-測試路四", "2F/5F", 28.0, None, None)],
    )
    .await
    .unwrap();
    reconcile_tracked_search(&pool, ts_id, date("2026-09-02"), vec![]).await.unwrap();
    reconcile_tracked_search(&pool, ts_id, date("2026-09-03"), vec![]).await.unwrap();

    let (delisted_before,): (bool,) =
        sqlx::query_as("SELECT delisted FROM listings WHERE tracked_search_id = ? AND id = ?")
            .bind(ts_id)
            .bind("30000040")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(delisted_before, "前置條件：這時應該已經判定下架");

    // Day4：同一個 id 又重新出現在抓取結果裡。
    reconcile_tracked_search(
        &pool,
        ts_id,
        date("2026-09-04"),
        vec![card("30000040", "松山區-測試路四", "2F/5F", 28.0, None, None)],
    )
    .await
    .unwrap();

    let (delisted_after, delisted_date): (bool, Option<String>) = sqlx::query_as(
        "SELECT delisted, delisted_date FROM listings WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(ts_id)
    .bind("30000040")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!delisted_after);
    assert_eq!(delisted_date, None);
}
