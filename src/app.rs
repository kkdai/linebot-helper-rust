//! 組裝 axum Router 與背景任務。

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::handlers::{self, foodie, gemini_callback};
use crate::state::AppState;
use crate::voice;

pub async fn build(config: Config) -> anyhow::Result<Router> {
    let state = AppState::new(config).await;
    if state.config.enable_batch_polling {
        foodie::spawn_batch_polling(state.clone());
        tracing::info!("Batch job polling background task started");
    } else {
        tracing::info!("Batch job polling disabled (ENABLE_BATCH_POLLING=false)");
    }
    Ok(router(state))
}

pub fn router(state: Arc<AppState>) -> Router {
    let static_dir = state.config.static_dir.clone();
    Router::new()
        .route(
            "/",
            get(handlers::health_check).post(handlers::line_webhook),
        )
        .route("/reports/{id}", get(handlers::serve_report))
        .route("/liff/", get(voice::serve_liff))
        .route("/ws/voice/{session_id}", get(voice::voice_ws))
        .route("/images/{id}", get(handlers::serve_image))
        .route("/audio/{id}", get(handlers::serve_audio))
        .route(
            "/api/gemini-callback/static",
            post(gemini_callback::static_webhook),
        )
        .route(
            "/api/gemini-callback/dynamic",
            post(gemini_callback::dynamic_webhook),
        )
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
