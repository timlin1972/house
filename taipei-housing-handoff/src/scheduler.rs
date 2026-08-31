//! 每天固定時間（台北時間 08:00）自動跑一次 8 組抓取，對應規格建議技術棧的
//! `tokio-cron-scheduler`。

use anyhow::Result;
use tokio_cron_scheduler::{Job, JobScheduler};

use crate::pipeline::PipelineRunner;

/// cron 表達式是 6 欄（含秒）：秒 分 時 日 月 星期。每天台北時間 08:00:00。
const DAILY_AT_08_00_TAIPEI: &str = "0 0 8 * * *";

pub async fn start(runner: PipelineRunner) -> Result<JobScheduler> {
    let scheduler = JobScheduler::new().await?;

    let job = Job::new_async_tz(DAILY_AT_08_00_TAIPEI, chrono_tz::Asia::Taipei, move |_uuid, _l| {
        let runner = runner.clone();
        Box::pin(async move {
            tracing::info!("排程觸發，開始跑每日抓取");
            if !runner.try_spawn() {
                tracing::warn!("排程觸發時已經有一輪抓取在跑（可能是手動觸發的），跳過這次");
            }
        })
    })?;

    scheduler.add(job).await?;
    scheduler.start().await?;
    Ok(scheduler)
}
