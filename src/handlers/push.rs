//! 外部排程呼叫的推播端點：`/hn`（Hacker News）、`/hf`（HuggingFace papers）、`/urls`。

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::{AppStateRef, detail};
use crate::line::errors::friendly_error_message;
use crate::line::messages::text;
use crate::state::AppState;

pub async fn hacker_news(State(state): State<AppStateRef>, Json(data): Json<Value>) -> Response {
    tracing::info!("/hn data={data}");
    let title = data["title"].as_str().unwrap_or_default();
    let mut urls = vec![data["url"].as_str().unwrap_or_default().to_string()];
    if let Some(story) = data["StoryUrl"].as_str().filter(|s| !s.is_empty()) {
        urls.push(story.to_string());
    }
    let token = state.config.channel_access_token.clone();
    let user = state.config.line_user_id.clone();
    push_url_summaries(&state, title, &urls, &user, &token).await;
    Json(json!({ "status": "ok" })).into_response()
}

pub async fn huggingface(State(state): State<AppStateRef>, Json(data): Json<Value>) -> Response {
    tracing::info!("/hf data={data}");
    let title = data["title"].as_str().unwrap_or_default();
    let url = data["url"]
        .as_str()
        .unwrap_or_default()
        .replace("paperswithcode.com", "huggingface.co");
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return detail(StatusCode::BAD_REQUEST, "Invalid URL protocol");
    }
    let token = state.config.channel_access_token_hf.clone();
    let user = state.config.line_user_id.clone();
    push_url_summaries(&state, title, &[url], &user, &token).await;
    Json(json!({ "status": "ok" })).into_response()
}

pub async fn multi_urls(State(state): State<AppStateRef>, Json(data): Json<Value>) -> Response {
    tracing::info!("/urls data={data}");
    let title = data["title"].as_str().unwrap_or_default();
    let Some(urls) = data["urls"].as_array().filter(|u| !u.is_empty()) else {
        return detail(StatusCode::BAD_REQUEST, "urls must be a non-empty array");
    };
    if urls.len() > 5 {
        return detail(StatusCode::BAD_REQUEST, "Maximum 5 URLs allowed");
    }
    let urls: Vec<String> = urls
        .iter()
        .filter_map(|u| u.as_str().map(str::to_string))
        .collect();
    let token = state.config.channel_access_token.clone();
    let user = state.config.line_user_id.clone();
    push_url_summaries(&state, title, &urls, &user, &token).await;
    Json(json!({ "status": "ok", "processed_urls": urls.len() })).into_response()
}

/// 每個網址：爬取 → 條列摘要 → `{url}\n{title}\n\n{summary}`，最後一次推播給指定使用者。
pub async fn push_url_summaries(
    state: &AppState,
    title: &str,
    urls: &[String],
    user_id: &str,
    token: &str,
) {
    let mut results = Vec::new();
    for url in urls {
        let body = match state.loader.load(url).await {
            Ok(content) if content.is_empty() => {
                tracing::error!("Empty result for URL: {url}");
                "⚠️ 無法從這個網址提取內容。".to_string()
            }
            Ok(content) => match state.ai.summarize_text(&content, "normal").await {
                Ok(summary) => summary,
                Err(e) => {
                    tracing::error!("Summarization failed: {e:#}");
                    friendly_error_message(&e, url)
                }
            },
            Err(e) => {
                tracing::error!("Unexpected error: {e:#}");
                friendly_error_message(&e, url)
            }
        };
        results.push(text(format!("{url}\n{title}\n\n{body}")));
    }
    if results.is_empty() || user_id.is_empty() || token.is_empty() {
        return;
    }
    if let Err(e) = state.line.with_token(token).push(user_id, results).await {
        tracing::error!("Failed to push message: {e:#}");
    }
}
