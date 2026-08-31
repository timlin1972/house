use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::Utc;
use chrono_tz::Asia::Taipei;
use serde::Deserialize;

use super::templates::{Group, IndexTemplate, ListingCard, Stats};
use super::AppState;

pub async fn index(State(state): State<AppState>) -> Response {
    match render_index(&state).await {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "渲染首頁失敗");
            (StatusCode::INTERNAL_SERVER_ERROR, "頁面渲染失敗").into_response()
        }
    }
}

#[derive(sqlx::FromRow)]
struct TrackedSearchRow {
    id: i64,
    district: String,
    building_type: String,
    price_range: String,
    name: Option<String>,
}

fn group_label(row: &TrackedSearchRow) -> String {
    row.name
        .clone()
        .unwrap_or_else(|| format!("{}{}{}", row.district, row.building_type, row.price_range))
}

#[derive(sqlx::FromRow)]
struct ListingRow {
    tracked_search_id: i64,
    id: String,
    title: String,
    price: i64,
    unit_price: f64,
    rooms: String,
    area: f64,
    main_area: Option<f64>,
    age: Option<i64>,
    floor: String,
    community: Option<String>,
    address: String,
    agent: Option<String>,
    delisted: bool,
    note: Option<String>,
    decision: Option<String>,
    reason: Option<String>,
}

async fn render_index(state: &AppState) -> anyhow::Result<String> {
    let today = Utc::now().with_timezone(&Taipei).date_naive();

    // 統計數字改成「每組各自算」——之前是全部 8 組加總在一起顯示，切分頁看的時候數字
    // 卻不會變，看不出目前這組真正的狀況，所以改成每組獨立算、跟著側邊欄切換一起換。
    let tracked_counts: HashMap<i64, i64> = sqlx::query_as(
        "SELECT tracked_search_id, COUNT(*) FROM listings
         WHERE duplicate_of IS NULL AND delisted = 0 GROUP BY tracked_search_id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .collect();

    let delisted_counts: HashMap<i64, i64> = sqlx::query_as(
        "SELECT tracked_search_id, COUNT(*) FROM listings
         WHERE duplicate_of IS NULL AND delisted = 1 GROUP BY tracked_search_id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .collect();

    let reviewed_counts: HashMap<i64, i64> =
        sqlx::query_as("SELECT tracked_search_id, COUNT(*) FROM reviews GROUP BY tracked_search_id")
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .collect();

    let want_counts: HashMap<i64, i64> = sqlx::query_as(
        "SELECT tracked_search_id, COUNT(*) FROM reviews
         WHERE decision = 'want' GROUP BY tracked_search_id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .collect();

    let new_today_counts: HashMap<i64, i64> = sqlx::query_as(
        "SELECT tracked_search_id, SUM(new_count) FROM check_runs
         WHERE checked_date = ? GROUP BY tracked_search_id",
    )
    .bind(today.to_string())
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .collect();

    // 先把所有追蹤清單都列出來（就算還沒抓過、一筆物件都沒有），新增的清單才會馬上在
    // 側邊欄看到，不用等排程跑過一次才出現。
    let tracked_searches: Vec<TrackedSearchRow> = sqlx::query_as(
        "SELECT id, district, building_type, price_range, name FROM tracked_searches ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut groups: Vec<Group> = tracked_searches
        .iter()
        .map(|ts| Group {
            tracked_search_id: ts.id,
            label: group_label(ts),
            stats: Stats {
                tracked_count: *tracked_counts.get(&ts.id).unwrap_or(&0),
                new_today: *new_today_counts.get(&ts.id).unwrap_or(&0),
                reviewed_count: *reviewed_counts.get(&ts.id).unwrap_or(&0),
                want_count: *want_counts.get(&ts.id).unwrap_or(&0),
                delisted_count: *delisted_counts.get(&ts.id).unwrap_or(&0),
            },
            listings: Vec::new(),
        })
        .collect();

    let rows: Vec<ListingRow> = sqlx::query_as(
        "SELECT
            l.tracked_search_id, l.id, l.title, l.price, l.unit_price, l.rooms, l.area,
            l.main_area, l.age, l.floor, l.community, l.address, l.agent, l.delisted, l.note,
            r.decision, r.reason
         FROM listings l
         LEFT JOIN reviews r ON r.tracked_search_id = l.tracked_search_id AND r.listing_id = l.id
         WHERE l.duplicate_of IS NULL
         ORDER BY l.tracked_search_id, l.delisted ASC, l.price ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    for row in rows {
        let card = ListingCard {
            tracked_search_id: row.tracked_search_id,
            id: row.id,
            title: row.title,
            price: row.price,
            unit_price: row.unit_price,
            rooms: row.rooms,
            area: row.area,
            main_area: row.main_area,
            age: row.age,
            floor: row.floor,
            community: row.community,
            address: row.address,
            agent: row.agent,
            delisted: row.delisted,
            note: row.note,
            decision: row.decision,
            reason: row.reason,
        };

        if let Some(g) = groups.iter_mut().find(|g| g.tracked_search_id == row.tracked_search_id) {
            g.listings.push(card);
        }
    }

    let template = IndexTemplate { groups };
    Ok(template.render()?)
}

/// 網頁上「現在全部更新」按鈕的進入點。抓取整輪要好幾分鐘（8組依序跑、組跟組之間還
/// 刻意停頓），所以丟到背景跑，這裡立刻回應，不然瀏覽器那個請求會等到逾時。
pub async fn run_now(State(state): State<AppState>) -> Response {
    if state.runner.try_spawn() {
        Redirect::to("/?updating=started").into_response()
    } else {
        Redirect::to("/?updating=already").into_response()
    }
}

/// 給頁面 JS 輪詢用的進度查詢，回傳目前是否還在跑、跑到第幾組、上一輪的結果。
pub async fn run_status(State(state): State<AppState>) -> Response {
    axum::Json(state.runner.status()).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ReviewForm {
    tracked_search_id: i64,
    listing_id: String,
    decision: String,
    reason: String,
}

pub async fn submit_review(State(state): State<AppState>, Form(form): Form<ReviewForm>) -> Response {
    if form.decision != "want" && form.decision != "pass" {
        return (StatusCode::BAD_REQUEST, "decision 必須是 want 或 pass").into_response();
    }

    let reason = if form.reason.trim().is_empty() {
        None
    } else {
        Some(form.reason.trim().to_string())
    };

    let result = sqlx::query(
        "INSERT INTO reviews (tracked_search_id, listing_id, decision, reason)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(tracked_search_id, listing_id) DO UPDATE SET
            decision = excluded.decision, reason = excluded.reason,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
    )
    .bind(form.tracked_search_id)
    .bind(&form.listing_id)
    .bind(&form.decision)
    .bind(&reason)
    .execute(&state.pool)
    .await;

    match result {
        // 帶回 group 參數，讓評價完重新整理頁面時停在原本選的那個分頁，不用每次都跳回第一組。
        Ok(_) => Redirect::to(&format!("/?group={}", form.tracked_search_id)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "寫入評價失敗");
            (StatusCode::INTERNAL_SERVER_ERROR, "寫入評價失敗").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AddTrackedSearchForm {
    name: String,
    search_url: String,
}

pub async fn add_tracked_search(
    State(state): State<AppState>,
    Form(form): Form<AddTrackedSearchForm>,
) -> Response {
    let name = form.name.trim();
    let search_url = form.search_url.trim();

    if name.is_empty() || search_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "名稱和搜尋網址都要填").into_response();
    }
    if !(search_url.starts_with("http://") || search_url.starts_with("https://")) {
        return (StatusCode::BAD_REQUEST, "搜尋網址看起來不是有效的網址").into_response();
    }

    // district/building_type/price_range 是原本 8 組固定 regionid=1/section=3或4/shape=2或5
    // 那套組合才有意義的欄位，自訂搜尋不一定符合那個樣式，這裡不強求解析成功，
    // 解析不出來就留空字串，顯示一律看 name 就好。
    let (district, building_type, price_range) =
        parse_known_pattern(search_url).unwrap_or_default();

    let criteria_json = match serde_json::to_string(name) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "序列化 criteria_json 失敗");
            return (StatusCode::INTERNAL_SERVER_ERROR, "新增失敗").into_response();
        }
    };

    let result: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO tracked_searches (district, building_type, price_range, search_url, criteria_json, name)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(search_url) DO UPDATE SET
            name = excluded.name,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         RETURNING id",
    )
    .bind(&district)
    .bind(&building_type)
    .bind(&price_range)
    .bind(search_url)
    .bind(&criteria_json)
    .bind(name)
    .fetch_one(&state.pool)
    .await;

    match result {
        Ok(id) => {
            tracing::info!(id, name, search_url, "新增追蹤清單");
            Redirect::to(&format!("/?group={id}")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "新增追蹤清單失敗");
            (StatusCode::INTERNAL_SERVER_ERROR, "新增失敗").into_response()
        }
    }
}

/// 跟 import_json.rs 裡同名函式一樣的解析規則（section/shape/price 參數），
/// 只是這裡解析失敗不當作錯誤，只是回傳 None，讓呼叫端自行決定要不要填空字串。
fn parse_known_pattern(url: &str) -> Option<(String, String, String)> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    let params: std::collections::HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();

    let district = match *params.get("section")? {
        "3" => "中山區",
        "4" => "松山區",
        _ => return None,
    };
    let building_type = match *params.get("shape")? {
        "5" => "華廈",
        "2" => "電梯大樓",
        _ => return None,
    };
    let price_range = (*params.get("price")?).to_string();

    Some((district.to_string(), building_type.to_string(), price_range))
}
