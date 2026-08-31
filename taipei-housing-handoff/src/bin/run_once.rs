//! 手動立刻跑一次 8 組抓取+對帳，不用等排程器到台北時間 08:00 才觸發。
//! 用途：第一次部署後想馬上看到結果、或平常想手動補跑一次。

use taipei_housing::{db, pipeline};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://taipei_housing.db".to_string());
    let pool = db::connect(&database_url).await?;

    let outcomes = pipeline::run_all_tracked_searches(&pool).await;

    let mut failed = 0;
    for o in &outcomes {
        match &o.result {
            Ok(()) => println!("✓ {}", o.label),
            Err(e) => {
                failed += 1;
                println!("✗ {}：{e}", o.label);
            }
        }
    }
    println!("完成，共 {} 組，{failed} 組失敗", outcomes.len());

    Ok(())
}
