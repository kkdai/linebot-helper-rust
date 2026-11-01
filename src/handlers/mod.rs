//! HTTP routes 與 LINE 事件處理（移植自 main.py）。

pub mod foodie;
pub mod gemini_callback;
pub mod message;
pub mod postback;

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde_json::json;

use crate::line::webhook::{Event, parse_events, verify_signature};
use crate::report_page::render_expired_page;
use crate::state::AppState;

pub type AppStateRef = Arc<AppState>;

pub fn detail(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "detail": msg }))).into_response()
}

/// `POST /`：LINE webhook。驗完簽章立刻回 200，事件丟背景處理。
///
/// 爬網頁 + 呼叫 Gemini 可能耗時數十秒，超過 LINE webhook 的等待時間。
/// Cloud Run 需啟用 `--no-cpu-throttling`，否則回應後 CPU 被凍結，背景任務不會跑。
pub async fn line_webhook(
    State(state): State<AppStateRef>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    state.detect_base_url(
        h("x-forwarded-proto"),
        h("x-forwarded-host").or_else(|| h("host")),
    );

    let Some(signature) = h("x-line-signature") else {
        return detail(StatusCode::BAD_REQUEST, "Missing signature");
    };
    if !verify_signature(&state.config.channel_secret, &body, signature) {
        return detail(StatusCode::BAD_REQUEST, "Invalid signature");
    }
    let events = match parse_events(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("invalid webhook body: {e}");
            return detail(StatusCode::BAD_REQUEST, "Invalid body");
        }
    };

    for event in events {
        let state = state.clone();
        match event {
            Event::Message(ev) => {
                tokio::spawn(async move {
                    if let Err(e) = message::handle_message_event(&state, ev).await {
                        tracing::error!("Background event processing failed (message): {e:#}");
                    }
                });
            }
            Event::Postback(ev) => {
                tokio::spawn(async move {
                    if let Err(e) = postback::handle_postback_event(&state, ev).await {
                        tracing::error!("Background event processing failed (postback): {e:#}");
                    }
                });
            }
            Event::Other(kind) => tracing::debug!("ignoring {kind} event"),
        }
    }
    "OK".into_response()
}

pub async fn health_check() -> &'static str {
    "OK"
}

/// `GET /reports/{id}`：永久保存於 Firestore 的研究報告。
pub async fn serve_report(State(state): State<AppStateRef>, Path(id): Path<String>) -> Response {
    match state.reports.get(&id).await {
        Some(html) => Html(html).into_response(),
        None => (StatusCode::NOT_FOUND, Html(render_expired_page())).into_response(),
    }
}

/// `GET /images/{id}`：Agentic Vision 標註圖（5 分鐘）。
pub async fn serve_image(State(state): State<AppStateRef>, Path(id): Path<String>) -> Response {
    match state.annotated_images.get(&id) {
        Some(data) => ([(header::CONTENT_TYPE, "image/png")], data).into_response(),
        None => detail(StatusCode::NOT_FOUND, "Image not found or expired"),
    }
}

/// `GET /audio/{id}`：TTS 語音（5 分鐘）。
pub async fn serve_audio(State(state): State<AppStateRef>, Path(id): Path<String>) -> Response {
    match state.audio.get(&id) {
        Some(data) => ([(header::CONTENT_TYPE, "audio/mp4")], data).into_response(),
        None => detail(StatusCode::NOT_FOUND, "Audio not found or expired"),
    }
}
