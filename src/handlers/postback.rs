//! Postback 事件（Quick Reply / Flex 按鈕）。

use std::sync::Arc;

use serde_json::{Value, json};

use super::foodie;
use super::message::Target;
use crate::line::errors::format_error_message;
use crate::line::messages::{self, postback_item, text, text_with_quick_reply, truncate_reply};
use crate::line::webhook::PostbackEvent;
use crate::report_page::render_report_page;
use crate::state::AppState;

/// 效期內重複點「詳細研究報告」直接回傳舊連結，過了才重新產生覆蓋
pub const REPORT_CACHE_TTL_SECONDS: f64 = 7.0 * 86400.0;
/// 摘要截斷，讓語音維持在約 1 分鐘內
pub const MAX_TTS_CHARS: usize = 250;

pub async fn handle_postback_event(
    state: &Arc<AppState>,
    event: PostbackEvent,
) -> anyhow::Result<()> {
    let user_id = event.source.user_id().map(str::to_string);
    let reply = Target::Reply(event.reply_token.clone());
    // 舊版 query string 格式（gen_tweet / gen_slack）已不再支援，直接忽略
    let Ok(data) = serde_json::from_str::<Value>(&event.postback.data) else {
        tracing::warn!("Ignoring non-JSON postback data");
        return Ok(());
    };
    let Some(user_id) = user_id else {
        tracing::warn!("postback without user id, ignoring");
        return Ok(());
    };
    let user_id = user_id.as_str();

    match data["action"].as_str().unwrap_or_default() {
        "search_nearby" => handle_map_search(state, &reply, &data, user_id).await,
        "foodie_deep_analysis" => {
            foodie::handle_deep_analysis_postback(state, &reply, &data, user_id).await
        }
        "specific_foodie_deep_analysis" => {
            foodie::handle_specific_postback(state, &reply, &data, user_id).await
        }
        "image_analyze" => handle_image_analyze(state, &reply, &data, user_id).await,
        "youtube_summary" => handle_youtube_summary(state, &reply, &data, user_id).await,
        "read_aloud" => handle_read_aloud(state, &reply, &data, user_id).await,
        "save_bookmark" => handle_bookmark_save(state, &reply, &data, user_id).await,
        "delete_bookmark" => handle_bookmark_delete(state, &reply, &data, user_id).await,
        "research_report" => handle_research_report(state, &reply, &data, user_id).await,
        other => {
            tracing::warn!("Unknown postback action: {other}");
            Ok(())
        }
    }
}

async fn handle_map_search(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let (Some(place_type), Some(lat), Some(lng)) = (
        data["place_type"].as_str(),
        data["latitude"].as_f64(),
        data["longitude"].as_f64(),
    ) else {
        tracing::error!("Missing required data in postback: {data}");
        return state
            .send(reply, vec![text("❌ 位置資訊不完整，請重新傳送位置。")])
            .await;
    };
    tracing::info!("Searching for {place_type} at ({lat}, {lng})");
    state
        .send(reply, vec![text("🔍 搜尋中，請稍候...")])
        .await?;

    let msg = match state.orchestrator.search_nearby(lat, lng, place_type).await {
        Ok(result) => {
            let items: Vec<Value> = if place_type == "restaurant" {
                result
                    .restaurant_names
                    .iter()
                    .take(3)
                    .map(|name| {
                        // LINE label 上限 20 字
                        let label = if name.chars().count() > 10 {
                            format!("{}…", name.chars().take(9).collect::<String>())
                        } else {
                            name.clone()
                        };
                        postback_item(
                            &format!("🍴 分析 {label}"),
                            &json!({"action": "specific_foodie_deep_analysis", "restaurant_name": name})
                                .to_string(),
                            &format!("🔍 進行「{name}」深度評論與招牌菜色分析"),
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            text_with_quick_reply(result.places, items)
        }
        Err(e) => text(format!("❌ {e}")),
    };
    state.line.push(user_id, vec![msg]).await
}

async fn handle_image_analyze(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    if !state.image_temp.contains_key(user_id) {
        return state
            .send(reply, vec![text("⚠️ 圖片已過期，請重新傳送圖片。")])
            .await;
    }
    if data["mode"] == "agentic_vision" {
        state.pending_agentic_vision.insert(user_id.to_string(), ());
        return state
            .send(
                reply,
                vec![text(
                    "🔍 Agentic Vision 模式\n\n請輸入你想要分析的指令，例如：\n• 數一數圖片中有幾個人\n• 找出圖片中所有的文字\n• 分析圖表中的數據趨勢",
                )],
            )
            .await;
    }
    let Some((_, image)) = state.image_temp.remove(user_id) else {
        return Ok(());
    };
    state
        .send(reply, vec![text("⏳ 正在使用識別圖片分析中，請稍候...")])
        .await?;
    let analysis = state.orchestrator.process_image(&image).await;
    state
        .line
        .push(user_id, vec![text(truncate_reply(&analysis))])
        .await
}

async fn handle_youtube_summary(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let (Some(mode), Some(url)) = (data["mode"].as_str(), data["url"].as_str()) else {
        tracing::error!("Missing mode or url in YouTube summary postback");
        return Ok(());
    };
    let mode_text = if mode == "detail" {
        "詳細摘要"
    } else {
        "Twitter 分享文案"
    };
    state
        .send(
            reply,
            vec![text(format!("⏳ 正在生成{mode_text}，請稍候..."))],
        )
        .await?;
    let msg = match state.ai.summarize_youtube(url, mode).await {
        Ok(summary) => format!("{url}\n\n{summary}"),
        Err(e) => format!("⚠️ {e}"),
    };
    state.line.push(user_id, vec![text(msg)]).await
}

async fn handle_read_aloud(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let entry = data["summary_id"]
        .as_str()
        .and_then(|id| state.summaries.get(id));
    let Some(summary) = entry else {
        return state
            .send(reply, vec![text("摘要已過期，請重新傳送網址。")])
            .await;
    };
    let Some(base) = state.app_base_url() else {
        tracing::error!("app_base_url not set, cannot serve audio for user {user_id}");
        return state
            .send(reply, vec![text("語音服務暫時無法使用，請稍後再試。")])
            .await;
    };
    let snippet: String = summary.chars().take(MAX_TTS_CHARS).collect();
    match state.ai.text_to_speech(&snippet).await {
        Ok((m4a, duration_ms)) => {
            let id = state.audio.insert(m4a);
            state
                .send(
                    reply,
                    vec![messages::audio(&format!("{base}/audio/{id}"), duration_ms)],
                )
                .await?;
            tracing::info!("Read aloud sent to user {user_id}: {duration_ms}ms");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Read aloud error for user {user_id}: {e:#}");
            state
                .line
                .push(
                    user_id,
                    vec![text(format_error_message(&e.to_string(), "產生語音"))],
                )
                .await
        }
    }
}

async fn handle_bookmark_save(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let svc = &state.bookmarks;
    let Some(doc_id) = data["id"].as_str().filter(|_| svc.available()) else {
        return state
            .send(reply, vec![text("🔖 書籤功能暫時無法使用，請稍後再試。")])
            .await;
    };
    let msg = match svc.confirm_save(user_id, doc_id).await {
        Some(b) => format!("✅ 已存入書籤：{}\n\n用 /list 查看所有書籤", b.title),
        None => "⚠️ 書籤候選已過期，請重新傳送網址。".into(),
    };
    state.send(reply, vec![text(msg)]).await
}

async fn handle_bookmark_delete(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let svc = &state.bookmarks;
    let Some(doc_id) = data["id"].as_str().filter(|_| svc.available()) else {
        return state
            .send(reply, vec![text("🔖 書籤功能暫時無法使用，請稍後再試。")])
            .await;
    };
    let msg = if svc.delete(user_id, doc_id).await {
        "🗑 已刪除書籤。"
    } else {
        "⚠️ 找不到這個書籤（可能已刪除）。"
    };
    state.send(reply, vec![text(msg)]).await
}

/// 「📄 詳細研究報告」：深入研究文章並產生永久保存的網頁報告（7 天內重複點擊直接回舊連結）。
async fn handle_research_report(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let svc = &state.bookmarks;
    let doc = match data["id"].as_str() {
        Some(id) if svc.available() => svc.get_bookmark(user_id, id).await,
        _ => None,
    };
    let Some(doc) = doc else {
        return state
            .send(reply, vec![text("⚠️ 資料已過期，請重新傳送網址再試一次。")])
            .await;
    };

    if let (Some(report_id), Some(at), Some(base)) = (
        &doc.report_id,
        doc.report_generated_at,
        state.app_base_url(),
    ) && crate::store::now_ts() - at < REPORT_CACHE_TTL_SECONDS
    {
        return state
            .send(
                reply,
                vec![text(format!(
                    "📄 研究報告（先前已產生）：{}\n\n{base}/reports/{report_id}",
                    doc.title
                ))],
            )
            .await;
    }

    state
        .send(
            reply,
            vec![text(
                "🔬 開始深入研究這篇文章（約 1-2 分鐘），完成後把報告連結傳給你。",
            )],
        )
        .await?;

    let url = doc.url.clone();
    let result: anyhow::Result<String> = async {
        let crawled = state.loader.load(&url).await?;
        let report = state.ai.generate_research_report(&crawled, &url).await?;
        let title = if doc.title.is_empty() {
            "研究報告"
        } else {
            &doc.title
        };
        let html = render_report_page(title, &report.markdown, &url, &report.sources);
        let report_id = state.reports.put(&html).await;
        svc.record_report(&doc.doc_id, &report_id).await;
        Ok(report_id)
    }
    .await;

    let msg = match (result, state.app_base_url()) {
        (Ok(report_id), Some(base)) => {
            format!(
                "📄 研究報告完成：{}\n\n{base}/reports/{report_id}",
                doc.title
            )
        }
        (Ok(_), None) => {
            tracing::error!("app_base_url not set, cannot serve report page");
            "⚠️ 報告已產生但暫時無法提供連結，請稍後再試一次。".into()
        }
        (Err(e), _) => {
            tracing::error!("Research report failed for {url}: {e:#}");
            format!(
                "{url}\n\n{}",
                format_error_message(&e.to_string(), "產生研究報告")
            )
        }
    };
    state.line.push(user_id, vec![text(msg)]).await
}
