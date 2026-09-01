use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct TrackedSearch {
    pub id: i64,
    pub district: String,
    pub building_type: String,
    pub price_range: String,
    /// 使用者自訂新增的追蹤清單才會有值；原本匯入的 8 組是 NULL，顯示時 fallback
    /// 回 district+building_type+price_range。
    pub name: Option<String>,
    pub search_url: String,
    pub artifact_url: Option<String>,
    pub criteria_json: String,
    pub dedup_note: Option<String>,
    pub last_checked: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// (tracked_search_id, id) is the real primary key — the same 591 id can
/// legitimately appear under two different tracked searches (seen in real
/// data: 20759274 shows up in both 中山華廈-4000 and 中山電梯大樓-4000).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Listing {
    pub tracked_search_id: i64,
    pub id: String,
    pub title: String,
    pub price: i64,
    pub unit_price: f64,
    pub rooms: String,
    pub area: f64,
    pub main_area: Option<f64>,
    pub age: Option<i64>,
    pub floor: String,
    pub community: Option<String>,
    pub address: String,
    pub agent: Option<String>,
    pub first_seen: NaiveDate,
    pub duplicate_of: Option<String>,
    pub delisted: bool,
    pub delisted_date: Option<NaiveDate>,
    pub note: Option<String>,
    pub last_seen_check_run_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: i64,
    pub tracked_search_id: i64,
    pub checked_date: NaiveDate,
    pub new_count: i64,
    pub total_count: i64,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum CheckRunEventType {
    New,
    Delisted,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct CheckRunEvent {
    pub id: i64,
    pub check_run_id: i64,
    pub tracked_search_id: i64,
    pub listing_id: String,
    pub event_type: CheckRunEventType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum ReviewDecision {
    Want,
    Pass,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Review {
    pub tracked_search_id: i64,
    pub listing_id: String,
    pub decision: Option<ReviewDecision>,
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}
