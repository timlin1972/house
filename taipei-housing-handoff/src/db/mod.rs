pub mod models;

use std::str::FromStr;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

/// `.foreign_keys(true)` is applied per-connection via SqliteConnectOptions —
/// a one-off `PRAGMA foreign_keys = ON` run through the pool only affects
/// whichever single connection executes it, leaving FK enforcement off on
/// every other pooled connection.
pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
