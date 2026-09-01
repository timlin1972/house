mod handlers;
mod templates;

use axum::routing::{get, post};
use axum::Router;
use sqlx::sqlite::SqlitePool;

use crate::pipeline::PipelineRunner;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub runner: PipelineRunner,
}

pub fn router(pool: SqlitePool, runner: PipelineRunner) -> Router {
    Router::new()
        .route("/", get(handlers::index))
        .route("/reviews", post(handlers::submit_review))
        .route("/tracked-searches", post(handlers::add_tracked_search))
        .route("/run-now", post(handlers::run_now))
        .route("/run-status", get(handlers::run_status))
        .route("/tracked-searches/{id}/refresh", post(handlers::refresh_one))
        .route("/tracked-searches/{id}/delete", post(handlers::delete_tracked_search))
        .with_state(AppState { pool, runner })
}
