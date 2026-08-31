use taipei_housing::pipeline::PipelineRunner;
use taipei_housing::{db, scheduler, web};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://taipei_housing.db".to_string());
    let pool = db::connect(&database_url).await?;
    tracing::info!("database ready, migrations applied");

    // 排程器跟網頁的「現在全部更新」按鈕共用同一個 PipelineRunner，確保不會同時跑兩輪。
    let runner = PipelineRunner::new(pool.clone());

    // 排程器背景跑，掛掉了就整個服務跟著掛掉——沒有排程等於這個服務失去存在意義，
    // 靜靜跑一個壞掉的排程器比讓人發現整個系統早就停止更新還糟。
    let _scheduler = scheduler::start(runner.clone()).await?;
    tracing::info!("scheduler started, will run daily at 08:00 Asia/Taipei");

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "web server listening");

    axum::serve(listener, web::router(pool, runner)).await?;
    Ok(())
}
