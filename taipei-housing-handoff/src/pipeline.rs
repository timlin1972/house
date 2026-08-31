//! 串起「抓取 → 對帳」的每日排程主流程，跑過 tracked_searches 裡的每一組。
//!
//! 8 組共用同一個瀏覽器（换分頁），不用每組都重開一次 Chrome；組跟組之間刻意停頓，
//! 呼應規格「控制抓取頻率、避免造成 591 負擔」的提醒——這是排程任務，不趕時間。

use std::sync::Mutex;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use chrono_tz::Asia::Taipei;
use headless_chrome::{Browser, LaunchOptionsBuilder};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;

use crate::delist::reconcile_tracked_search;
use crate::scraper;

/// 組跟組之間的停頓——不需要很趕，排程一天只跑一次。
const PAUSE_BETWEEN_GROUPS: Duration = Duration::from_secs(5);

pub struct RunOutcome {
    pub tracked_search_id: i64,
    pub label: String,
    pub result: Result<()>,
}

/// 網頁可以直接查詢的即時進度，讓「現在全部更新」按鈕不用只是丟出去就沒有下文。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunStatus {
    pub running: bool,
    pub total: usize,
    pub completed: usize,
    pub current_label: Option<String>,
    pub last_result: Option<LastResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastResult {
    pub total: usize,
    pub failed: usize,
    pub finished_at: String,
}

/// 排程器（每天 08:00）跟網頁的「現在全部更新」按鈕共用同一個入口，確保兩邊不會同時
/// 跑起來互相干擾（8組共用一個瀏覽器，兩輪同時跑很容易撞在一起），也共用同一份進度狀態。
///
/// `run_all_tracked_searches` 內部混雜了同步的瀏覽器操作（`std::thread::sleep`、
/// headless_chrome 的同步 API）跟非同步的資料庫操作，直接 `tokio::spawn` 會佔用
/// async worker 執行緒好幾分鐘，讓網頁伺服器在這段期間變遲鈍——尤其小VPS核心數不多時
/// 影響更明顯。這裡用 `spawn_blocking` 讓整個流程跑在專門的阻塞執行緒池上。
#[derive(Clone)]
pub struct PipelineRunner {
    pool: SqlitePool,
    status: Arc<Mutex<RunStatus>>,
}

impl PipelineRunner {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            status: Arc::new(Mutex::new(RunStatus::default())),
        }
    }

    pub fn status(&self) -> RunStatus {
        self.status.lock().unwrap().clone()
    }

    /// 目前沒有其他輪次在跑的話，在背景啟動一輪並立刻回傳 true；
    /// 已經有一輪在跑的話什麼都不做，回傳 false。
    pub fn try_spawn(&self) -> bool {
        {
            let mut s = self.status.lock().unwrap();
            if s.running {
                return false;
            }
            s.running = true;
            s.total = 0;
            s.completed = 0;
            s.current_label = None;
        }

        let pool = self.pool.clone();
        let status = self.status.clone();
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let outcomes = run_all_tracked_searches_reporting(&pool, Some(&status)).await;
                let failed = outcomes.iter().filter(|o| o.result.is_err()).count();
                tracing::info!(total = outcomes.len(), failed, "這輪抓取跑完");

                let mut s = status.lock().unwrap();
                s.running = false;
                s.current_label = None;
                s.last_result = Some(LastResult {
                    total: outcomes.len(),
                    failed,
                    finished_at: Utc::now().to_rfc3339(),
                });
            });
        });
        true
    }
}

pub async fn run_all_tracked_searches(pool: &SqlitePool) -> Vec<RunOutcome> {
    run_all_tracked_searches_reporting(pool, None).await
}

async fn run_all_tracked_searches_reporting(
    pool: &SqlitePool,
    status: Option<&Arc<Mutex<RunStatus>>>,
) -> Vec<RunOutcome> {
    let groups: Vec<(i64, String, String, String, String)> = match sqlx::query_as(
        "SELECT id, district, building_type, price_range, search_url FROM tracked_searches ORDER BY id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "讀取 tracked_searches 失敗，本次排程整批取消");
            return Vec::new();
        }
    };

    if groups.is_empty() {
        tracing::warn!("tracked_searches 是空的——先跑過 import-json 匯入 8 組追蹤清單，或手動新增");
        return Vec::new();
    }

    if let Some(status) = status {
        status.lock().unwrap().total = groups.len();
    }

    let launch_options = match LaunchOptionsBuilder::default().sandbox(false).build() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "建立 headless_chrome LaunchOptions 失敗");
            return Vec::new();
        }
    };
    let browser = match Browser::new(launch_options) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "啟動 headless_chrome 失敗");
            return Vec::new();
        }
    };

    let checked_date = Utc::now().with_timezone(&Taipei).date_naive();

    let mut outcomes = Vec::with_capacity(groups.len());
    for (i, (tracked_search_id, district, building_type, price_range, search_url)) in
        groups.iter().enumerate()
    {
        let label = format!("{district}{building_type}{price_range}");
        tracing::info!(label = %label, "開始抓取");
        if let Some(status) = status {
            status.lock().unwrap().current_label = Some(label.clone());
        }

        let result = run_one(&browser, pool, *tracked_search_id, search_url, checked_date).await;
        if let Err(e) = &result {
            tracing::error!(label = %label, error = %e, "這組抓取/對帳失敗");
        }
        outcomes.push(RunOutcome {
            tracked_search_id: *tracked_search_id,
            label,
            result,
        });
        if let Some(status) = status {
            status.lock().unwrap().completed = i + 1;
        }

        if i + 1 < groups.len() {
            std::thread::sleep(PAUSE_BETWEEN_GROUPS);
        }
    }

    outcomes
}

async fn run_one(
    browser: &Browser,
    pool: &SqlitePool,
    tracked_search_id: i64,
    search_url: &str,
    checked_date: chrono::NaiveDate,
) -> Result<()> {
    let tab = browser.new_tab().context("開新分頁失敗")?;
    let (declared_total, cards) =
        scraper::scrape_all_pages(&tab, search_url).context("抓取591搜尋結果失敗")?;
    let _ = tab.close(true);

    let summary = reconcile_tracked_search(pool, tracked_search_id, checked_date, cards)
        .await
        .context("對帳寫入資料庫失敗")?;

    tracing::info!(
        declared_total,
        new = summary.new_ids.len(),
        delisted = summary.delisted_ids.len(),
        id_changes = summary.id_changes.len(),
        total_active = summary.total_active_representatives,
        check_run_id = summary.check_run_id,
        "完成一組對帳"
    );

    Ok(())
}
