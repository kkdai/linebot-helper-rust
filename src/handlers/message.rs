//! 訊息事件：文字／網址／書籤指令／圖片／語音／位置。

use std::sync::Arc;

use serde_json::{Value, json};

use crate::agents::format_orchestrator_response;
use crate::line::errors::format_error_message;
use crate::line::flex::{
    build_bookmark_carousel, build_platform_bubble, build_summary_bubble, parse_bookmark_command,
};
use crate::line::messages::{self, postback_item, text, text_with_quick_reply, truncate_reply};
use crate::line::webhook::{Message, MessageEvent, Source};
use crate::loader::text::extract_url_and_mode;
use crate::state::AppState;

/// 回覆目標：reply token 只能用一次，背景任務完成後改用 push。
#[derive(Debug, Clone)]
pub enum Target {
    Reply(String),
    Push(String),
}

impl AppState {
    pub async fn send(&self, target: &Target, messages: Vec<Value>) -> anyhow::Result<()> {
        match target {
            Target::Reply(token) => self.line.reply(token, messages).await,
            Target::Push(to) => self.line.push(to, messages).await,
        }
    }
}

pub async fn handle_message_event(
    state: &Arc<AppState>,
    event: MessageEvent,
) -> anyhow::Result<()> {
    let user_id = match &event.source {
        Source::User { user_id } => user_id.clone(),
        Source::Group { group_id, .. } => {
            tracing::info!("Group ID: {group_id}");
            return Ok(());
        }
        Source::Room { room_id, .. } => {
            tracing::info!("Room ID: {room_id}");
            return Ok(());
        }
    };
    let reply = Target::Reply(event.reply_token.clone());

    match &event.message {
        Message::Text { text: msg, .. } => {
            tracing::info!("UID: {user_id}");
            if state.pending_agentic_vision.contains_key(&user_id) {
                return handle_agentic_vision_with_prompt(state, &reply, &user_id, msg).await;
            }
            // 書籤指令要在 URL 抽取之前攔截，否則 "/save https://..." 會被當成一般網址
            if let Some((command, argument)) = parse_bookmark_command(msg) {
                return handle_bookmark_command(state, &reply, &user_id, &command, &argument).await;
            }
            let (urls, _mode) = extract_url_and_mode(msg);
            if !urls.is_empty() {
                return handle_url_message(state, &reply, &user_id, &urls).await;
            }
            handle_text_via_orchestrator(state, &reply, &user_id, msg.trim(), None, false).await
        }
        Message::Image { id } => handle_image_message(state, &reply, &user_id, id).await,
        Message::Audio { id, .. } => {
            handle_audio_message(state, &event.reply_token, &user_id, id).await
        }
        Message::Location {
            latitude,
            longitude,
            address,
            ..
        } => {
            handle_location_message(
                state,
                &reply,
                &user_id,
                *latitude,
                *longitude,
                address.as_deref().unwrap_or(""),
            )
            .await
        }
        Message::Other => Ok(()),
    }
}

/// 網址 → 爬取 → 摘要卡 + FB / LinkedIn / Threads 爆款文案 carousel，再附三則純文字方便電腦複製。
pub async fn handle_url_message(
    state: &Arc<AppState>,
    reply: &Target,
    user_id: &str,
    urls: &[String],
) -> anyhow::Result<()> {
    let mut results: Vec<Value> = Vec::new();
    for url in urls {
        tracing::info!("Crawling URL: {url}");
        let crawled = match state.loader.load(url).await {
            Ok(c) if !c.trim().is_empty() => c,
            Ok(_) => {
                results.push(text(format!("⚠️ 無法從此網址擷取內容：\n{url}")));
                continue;
            }
            Err(e) => {
                tracing::error!("Unexpected error processing URL: {e:#}");
                results.push(text(format!(
                    "{url}\n\n{}",
                    format_error_message(&format!("{e:#}"), "處理網址")
                )));
                continue;
            }
        };
        tracing::info!(
            "Successfully crawled content from {url}. Length: {} characters.",
            crawled.chars().count()
        );

        let posts = state.ai.generate_social_media_posts(&crawled).await;

        // 預寫書籤候選（saved=false），「儲存書籤」按鈕的 postback 只帶 doc id
        let doc_id = if state.bookmarks.available() && !posts.summary_analysis.is_empty() {
            Some(
                state
                    .bookmarks
                    .save_candidate(user_id, url, &posts.title, &posts.summary_analysis)
                    .await,
            )
        } else {
            None
        };

        let fb = format!("{}\n\n🔗 原文連結： {url}", posts.facebook);
        let li = format!("{}\n\n🔗 原文連結： {url}", posts.linkedin);
        let th = format!("{}\n\n🔗 原文連結： {url}", posts.threads);
        let carousel = json!({
            "type": "carousel",
            "contents": [
                build_summary_bubble(&posts.title, &posts.summary_analysis, url, doc_id.as_deref()),
                build_platform_bubble("📘 Facebook 爆款文案", "#1877F2", &fb, "📋 複製 FB 文案"),
                build_platform_bubble("💼 LinkedIn 專業貼文", "#0A66C2", &li, "📋 複製 LinkedIn 文案"),
                build_platform_bubble("💬 Threads 脆友討論", "#000000", &th, "📋 複製 Threads 文案"),
            ],
        });
        results.push(messages::flex("📝 社群爆款文案摘要", carousel));
        results.push(text(format!(
            "📘 Facebook 爆款文案：\n--------------------\n{fb}"
        )));
        results.push(text(format!(
            "💼 LinkedIn 專業貼文：\n--------------------\n{li}"
        )));
        results.push(text(format!(
            "💬 Threads 脆友討論：\n--------------------\n{th}"
        )));
    }
    if !results.is_empty() {
        state.send(reply, results).await?;
    }
    Ok(())
}

/// 一般文字交給 Orchestrator。`offer_tts` 時附上「🔊 用語音聽」按鈕（語音訊息來源）。
pub async fn handle_text_via_orchestrator(
    state: &Arc<AppState>,
    target: &Target,
    user_id: &str,
    msg: &str,
    reply_prefix: Option<&str>,
    offer_tts: bool,
) -> anyhow::Result<()> {
    if let Some(name) = crate::handlers::foodie::restaurant_dish_query(msg) {
        return crate::handlers::foodie::start_specific_restaurant_analysis(
            state, target, user_id, &name,
        )
        .await;
    }

    tracing::info!(
        "Processing via Orchestrator for user {user_id}: {}",
        msg.chars().take(50).collect::<String>()
    );
    let result = state.orchestrator.process_text(user_id, msg).await;
    let answer = format_orchestrator_response(&result);
    let response = truncate_reply(&format!("{}{answer}", reply_prefix.unwrap_or("")));

    let mut items = Vec::new();
    if let Some((lat, lng)) = result.restaurant_location() {
        items.push(postback_item(
            "🔍 深度評論分析 (Batch)",
            &json!({"action": "foodie_deep_analysis", "latitude": lat, "longitude": lng})
                .to_string(),
            "🔍 進行深度評論與招牌菜色分析",
        ));
    }
    if offer_tts && !answer.trim().is_empty() {
        let summary_id = state.summaries.insert(answer.clone());
        items.push(postback_item(
            "🔊 用語音聽",
            &json!({"action": "read_aloud", "summary_id": summary_id}).to_string(),
            "🔊 用語音聽回覆",
        ));
    }
    state
        .send(target, vec![text_with_quick_reply(response, items)])
        .await?;
    tracing::info!("Orchestrator successfully responded to user {user_id}");
    Ok(())
}

async fn handle_image_message(
    state: &Arc<AppState>,
    reply: &Target,
    user_id: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let image = match state.line.get_content(message_id).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            tracing::error!("Image processing error: {e:#}");
            let msg = format_error_message(&e.to_string(), "處理圖片");
            return state.send(reply, vec![text(msg)]).await;
        }
    };
    tracing::info!("Stored image for user {user_id}: {} bytes", image.len());
    state.image_temp.insert(user_id.to_string(), image);
    let items = vec![
        postback_item(
            "識別圖片",
            &json!({"action": "image_analyze", "mode": "recognize"}).to_string(),
            "識別圖片",
        ),
        postback_item(
            "Agentic Vision",
            &json!({"action": "image_analyze", "mode": "agentic_vision"}).to_string(),
            "Agentic Vision",
        ),
    ];
    state
        .send(
            reply,
            vec![text_with_quick_reply(
                "📷 已收到圖片，請選擇分析方式：",
                items,
            )],
        )
        .await
}

/// 語音訊息：loading 動畫（不耗 reply token）→ 轉錄 → 單一回覆（轉錄 + 回答 + 用語音聽按鈕）。
pub async fn handle_audio_message(
    state: &Arc<AppState>,
    reply_token: &str,
    user_id: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let reply = Target::Reply(reply_token.to_string());
    let result: anyhow::Result<()> = async {
        state.line.show_loading_animation(user_id, 60).await;
        let audio = state.line.get_content(message_id).await?;
        tracing::info!("Downloaded audio for user {user_id}: {} bytes", audio.len());
        let transcription = state.ai.transcribe_audio(&audio).await?;
        let t = transcription.trim();
        if t.is_empty() {
            return state
                .send(&reply, vec![text("無法辨識語音內容，請重新錄製。")])
                .await;
        }
        handle_text_via_orchestrator(
            state,
            &reply,
            user_id,
            t,
            Some(&format!("🎤 你說的是：{t}\n\n")),
            true,
        )
        .await
    }
    .await;

    if let Err(e) = result {
        tracing::error!("Error handling audio message for user {user_id}: {e:#}");
        let msg = text(format_error_message(&e.to_string(), "處理語音訊息"));
        // reply token 可能已被消耗或過期 — 改用 push
        if state
            .line
            .reply(reply_token, vec![msg.clone()])
            .await
            .is_err()
        {
            state.line.push(user_id, vec![msg]).await?;
        }
    }
    Ok(())
}

async fn handle_agentic_vision_with_prompt(
    state: &Arc<AppState>,
    reply: &Target,
    user_id: &str,
    prompt: &str,
) -> anyhow::Result<()> {
    state.pending_agentic_vision.remove(user_id);
    let Some((_, image)) = state.image_temp.remove(user_id) else {
        return state
            .send(reply, vec![text("⚠️ 圖片已過期，請重新傳送圖片。")])
            .await;
    };
    state
        .send(
            reply,
            vec![text(format!(
                "⏳ 正在使用 Agentic Vision 分析中，請稍候...\n\n📝 指令：{prompt}"
            ))],
        )
        .await?;

    let result = state
        .orchestrator
        .process_image_agentic(&image, prompt)
        .await;
    let mut msgs = vec![text(truncate_reply(&result.analysis))];
    for img in result.images {
        match state.app_base_url() {
            Some(base) => {
                let id = state.annotated_images.insert(img);
                let url = format!("{base}/images/{id}");
                tracing::info!("Serving annotated image at: {url}");
                msgs.push(messages::image(&url));
            }
            None => tracing::warn!("app_base_url not set, cannot serve annotated image"),
        }
    }
    state.line.push(user_id, msgs).await
}

async fn handle_location_message(
    state: &Arc<AppState>,
    reply: &Target,
    user_id: &str,
    latitude: f64,
    longitude: f64,
    address: &str,
) -> anyhow::Result<()> {
    tracing::info!("Received location: ({latitude}, {longitude}) - {address}");
    let button = |label: &str, place_type: &str| {
        postback_item(
            label,
            &json!({
                "action": "search_nearby", "place_type": place_type,
                "latitude": latitude, "longitude": longitude, "address": address,
            })
            .to_string(),
            label,
        )
    };
    let items = vec![
        button("⛽ 找加油站", "gas_station"),
        button("🅿️ 找停車場", "parking"),
        button("🍴 找餐廳", "restaurant"),
    ];
    state
        .orchestrator
        .remember_location(user_id, latitude, longitude, address)
        .await;
    let place = if address.is_empty() {
        "位置已記錄"
    } else {
        address
    };
    state
        .send(
            reply,
            vec![text_with_quick_reply(
                format!("📍 已收到你的位置\n\n{place}\n\n請選擇要搜尋的類型："),
                items,
            )],
        )
        .await
}

/// `/save` `/list` `/search`
async fn handle_bookmark_command(
    state: &Arc<AppState>,
    reply: &Target,
    user_id: &str,
    command: &str,
    argument: &str,
) -> anyhow::Result<()> {
    let svc = &state.bookmarks;
    if !svc.available() {
        return state
            .send(reply, vec![text("🔖 書籤功能暫時無法使用，請稍後再試。")])
            .await;
    }
    match command {
        "save" => {
            let url = argument.trim();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return state
                    .send(
                        reply,
                        vec![text("用法：/save <網址>\n例如：/save https://example.com")],
                    )
                    .await;
            }
            // 爬取 + 摘要可能耗時數十秒，先回 ack，完成後用 push 通知
            state
                .send(
                    reply,
                    vec![text("🔖 收到！正在擷取內容與產生摘要，完成後通知你。")],
                )
                .await?;
            let msg = match state.loader.load(url).await {
                Ok(crawled) => {
                    let summary = state.ai.summarize_for_bookmark(&crawled).await;
                    svc.save_direct(user_id, url, &summary.title, &summary.summary)
                        .await;
                    format!(
                        "✅ 已存入書籤：{}\n\n{}\n\n用 /list 查看所有書籤",
                        summary.title, summary.summary
                    )
                }
                Err(e) => {
                    tracing::error!("/save failed for {url}: {e:#}");
                    format!(
                        "{url}\n\n{}",
                        format_error_message(&e.to_string(), "儲存書籤")
                    )
                }
            };
            state.line.push(user_id, vec![text(msg)]).await
        }
        "list" => {
            let bookmarks = svc.list_saved(user_id, 10).await;
            if bookmarks.is_empty() {
                return state
                    .send(
                        reply,
                        vec![text(
                            "📭 還沒有任何書籤。\n傳網址後按「🔖 儲存書籤」，或用 /save <網址> 新增。",
                        )],
                    )
                    .await;
            }
            state
                .send(
                    reply,
                    vec![messages::flex(
                        "🔖 我的書籤",
                        build_bookmark_carousel(&bookmarks),
                    )],
                )
                .await
        }
        "search" => {
            let keyword = argument.trim();
            if keyword.is_empty() {
                return state
                    .send(
                        reply,
                        vec![text("用法：/search <關鍵字>\n例如：/search python")],
                    )
                    .await;
            }
            let results = svc.search(user_id, keyword, 10).await;
            if results.is_empty() {
                return state
                    .send(
                        reply,
                        vec![text(format!("🔍 找不到包含「{keyword}」的書籤。"))],
                    )
                    .await;
            }
            state
                .send(
                    reply,
                    vec![messages::flex(
                        &format!("🔍 搜尋：{keyword}"),
                        build_bookmark_carousel(&results),
                    )],
                )
                .await
        }
        _ => Ok(()),
    }
}
