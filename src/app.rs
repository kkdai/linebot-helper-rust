//! 組裝 axum Router 與共用狀態。

use axum::{Router, routing::get};
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::config::Config;

pub async fn build(config: Config) -> anyhow::Result<Router> {
    let static_dir = config.static_dir.clone();
    Ok(Router::new()
        .route("/", get(health_check))
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http()))
}

async fn health_check() -> &'static str {
    "OK"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_check_returns_ok() {
        let app = build(Config::for_test()).await.unwrap();
        let resp = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
