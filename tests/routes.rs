//! Router 層級的整合測試：LINE 與 Gemini 都指向 wiremock。

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use linebot_helper::app::router;
use linebot_helper::config::Config;
use linebot_helper::state::{AppState, Endpoints};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sign(body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(Config::for_test().channel_secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

fn gemini_text(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "candidates": [{"content": {"parts": [{"text": text}]}}]
    }))
}

async fn line_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

/// 等到 mock 收到指定 path 的 request（背景任務是非同步的）。
async fn wait_for(server: &MockServer, p: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bodies: Vec<Value> = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path() == p)
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect();
        if bodies.len() >= count || Instant::now() > deadline {
            return bodies;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn webhook_request(body: &str, signature: &str) -> Request<Body> {
    Request::post("/")
        .header("x-line-signature", signature)
        .header("host", "bot.example.com")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn text_event(text: &str) -> String {
    json!({"events": [{
        "type": "message", "replyToken": "rt-1",
        "source": {"type": "user", "userId": "U-alice"},
        "message": {"type": "text", "id": "m1", "text": text}
    }]})
    .to_string()
}

#[tokio::test]
async fn webhook_returns_immediately_and_processes_in_background() {
    let line = MockServer::start().await;
    let gemini = MockServer::start().await;
    line_ok(&line).await;
    Mock::given(method("POST"))
        .respond_with(gemini_text("你好！").set_delay(Duration::from_millis(800)))
        .mount(&gemini)
        .await;
    let state = AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: Some(gemini.uri()),
        },
    );
    let app = router(state.clone());

    let body = text_event("嗨");
    let started = Instant::now();
    let resp = app
        .oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "webhook should not wait for Gemini"
    );

    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    assert_eq!(replies.len(), 1, "background handler did not reply");
    assert_eq!(replies[0]["replyToken"], "rt-1");
    assert_eq!(replies[0]["messages"][0]["text"], "你好！");
    assert_eq!(
        state.app_base_url().as_deref(),
        Some("https://bot.example.com")
    );
}

#[tokio::test]
async fn webhook_invalid_signature_returns_400() {
    let app = router(AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: None,
            gemini: None,
        },
    ));
    let body = text_event("x");
    let resp = app.oneshot(webhook_request(&body, "bad")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn report_route_serves_existing_and_404s_unknown() {
    let state = AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: None,
            gemini: None,
        },
    );
    let id = state.reports.put("<p>研究報告內容</p>").await;
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/reports/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&html).contains("研究報告內容"));

    let resp = app
        .oneshot(Request::get("/reports/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let html = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&html).contains("找不到"));
}

#[tokio::test]
async fn audio_message_single_combined_reply_with_tts_quick_reply() {
    let line = MockServer::start().await;
    let gemini = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/bot/message/a1/content"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"m4a".to_vec()))
        .mount(&line)
        .await;
    line_ok(&line).await;
    // 轉錄（帶 inlineData）→ 今天天氣如何；對話 → 天氣回答
    Mock::given(method("POST"))
        .and(body_string_contains("inlineData"))
        .respond_with(gemini_text("今天天氣如何"))
        .mount(&gemini)
        .await;
    Mock::given(method("POST"))
        .respond_with(gemini_text("今天晴朗，30 度"))
        .mount(&gemini)
        .await;
    let app = router(AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: Some(gemini.uri()),
        },
    ));

    let body = json!({"events": [{
        "type": "message", "replyToken": "rt-1",
        "source": {"type": "user", "userId": "U-alice"},
        "message": {"type": "audio", "id": "a1", "duration": 1000}
    }]})
    .to_string();
    app.oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();

    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    assert_eq!(replies.len(), 1);
    let msg = &replies[0]["messages"][0];
    let text = msg["text"].as_str().unwrap();
    assert!(text.starts_with("🎤 你說的是：今天天氣如何"), "{text}");
    assert!(text.contains("今天晴朗，30 度"));
    let labels: Vec<&str> = msg["quickReply"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["action"]["label"].as_str())
        .collect();
    assert!(labels.iter().any(|l| l.contains("語音")), "{labels:?}");

    let loading = wait_for(&line, "/v2/bot/chat/loading/start", 1).await;
    assert_eq!(
        loading[0],
        json!({"chatId": "U-alice", "loadingSeconds": 60})
    );
    assert!(wait_for(&line, "/v2/bot/message/push", 0).await.is_empty());
}

#[tokio::test]
async fn audio_message_empty_transcription_replies_error() {
    let line = MockServer::start().await;
    let gemini = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"m4a".to_vec()))
        .mount(&line)
        .await;
    line_ok(&line).await;
    Mock::given(method("POST"))
        .respond_with(gemini_text("   "))
        .mount(&gemini)
        .await;
    let app = router(AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: Some(gemini.uri()),
        },
    ));
    let body = json!({"events": [{
        "type": "message", "replyToken": "rt-1",
        "source": {"type": "user", "userId": "U-alice"},
        "message": {"type": "audio", "id": "a1"}
    }]})
    .to_string();
    app.oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();
    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    assert!(
        replies[0]["messages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("無法辨識")
    );
}

#[tokio::test]
async fn bookmark_list_command_replies_with_carousel() {
    let line = MockServer::start().await;
    line_ok(&line).await;
    let state = AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: None,
        },
    );
    state
        .bookmarks
        .save_direct("U-alice", "https://example.com/a", "標題A", "摘要A")
        .await;
    let app = router(state);
    let body = text_event("/list");
    app.oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();
    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    let msg = &replies[0]["messages"][0];
    assert_eq!(msg["type"], "flex");
    assert_eq!(
        msg["contents"]["contents"][0]["body"]["contents"][0]["text"],
        "標題A"
    );
}

#[tokio::test]
async fn save_bookmark_postback_confirms_candidate() {
    let line = MockServer::start().await;
    line_ok(&line).await;
    let state = AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: None,
        },
    );
    let id = state
        .bookmarks
        .save_candidate("U-alice", "https://example.com/b", "標題B", "s")
        .await;
    let app = router(state.clone());
    let body = json!({"events": [{
        "type": "postback", "replyToken": "rt-2",
        "source": {"type": "user", "userId": "U-alice"},
        "postback": {"data": json!({"action": "save_bookmark", "id": id}).to_string()}
    }]})
    .to_string();
    app.oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();
    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    assert!(
        replies[0]["messages"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("✅ 已存入書籤：標題B")
    );
    assert_eq!(state.bookmarks.list_saved("U-alice", 10).await.len(), 1);
}

#[tokio::test]
async fn location_message_offers_nearby_search() {
    let line = MockServer::start().await;
    line_ok(&line).await;
    let state = AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: Some(line.uri()),
            gemini: None,
        },
    );
    let app = router(state.clone());
    let body = json!({"events": [{
        "type": "message", "replyToken": "rt-3",
        "source": {"type": "user", "userId": "U-alice"},
        "message": {"type": "location", "id": "l1", "latitude": 25.03, "longitude": 121.56, "address": "台北"}
    }]})
    .to_string();
    app.oneshot(webhook_request(&body, &sign(&body)))
        .await
        .unwrap();
    let replies = wait_for(&line, "/v2/bot/message/reply", 1).await;
    let items = replies[0]["messages"][0]["quickReply"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 3);
    let data: Value = serde_json::from_str(items[2]["action"]["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["place_type"], "restaurant");
    let session = state.sessions.get_session("U-alice").await.unwrap();
    assert_eq!(session.metadata["last_location"]["address"], "台北");
}

#[tokio::test]
async fn removed_push_endpoints_return_404() {
    let app = router(AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: None,
            gemini: None,
        },
    ));
    for uri in ["/hn", "/hf", "/urls"] {
        let resp = app
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"urls": ["https://example.com"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn expired_media_returns_404() {
    let app = router(AppState::for_test(
        Config::for_test(),
        Endpoints {
            line: None,
            gemini: None,
        },
    ));
    for uri in ["/images/x", "/audio/x"] {
        let resp = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
