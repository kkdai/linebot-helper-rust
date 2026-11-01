//! 美食深度評論分析：觸發（文字／postback）→ 背景搜尋餐廳 → 送 Batch job →
//! 完成後（webhook 或輪詢）推播結果。

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::Value;

use super::message::Target;
use crate::line::messages::text;
use crate::state::AppState;

static DISH_QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"幫我查一下\s*(.*?)\s*(?:的)?菜色").unwrap());

/// 「幫我查一下 X 的菜色」→ Some("X")
pub fn restaurant_dish_query(msg: &str) -> Option<String> {
    DISH_QUERY
        .captures(msg)
        .map(|c| c[1].trim().to_string())
        .filter(|s| !s.is_empty())
}

fn ack_text(name: &str) -> String {
    format!(
        "🔍 收到！正在為您對「{name}」進行深度評論與招牌菜色大數據分析，大約需要 1-2 分鐘，請稍候..."
    )
}

fn truncate(s: &str) -> String {
    s.chars().take(100).collect()
}

/// 文字指令觸發的單一餐廳分析。`target` 為 push 時改用 push 回覆。
pub async fn start_specific_restaurant_analysis(
    state: &Arc<AppState>,
    target: &Target,
    user_id: &str,
    restaurant_name: &str,
) -> anyhow::Result<()> {
    state
        .send(target, vec![text(ack_text(restaurant_name))])
        .await?;
    let push_to = match target {
        Target::Push(to) => to.clone(),
        Target::Reply(_) => user_id.to_string(),
    };
    spawn_specific_analysis(state, user_id, restaurant_name, &push_to);
    Ok(())
}

fn spawn_specific_analysis(state: &Arc<AppState>, user_id: &str, name: &str, push_to: &str) {
    let (state, user_id, name, push_to) = (
        state.clone(),
        user_id.to_string(),
        name.to_string(),
        push_to.to_string(),
    );
    tokio::spawn(async move {
        run_specific_restaurant_analysis(&state, &user_id, &name, &push_to).await;
    });
}

/// Quick Reply「🍴 分析 X」按鈕。
pub async fn handle_specific_postback(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let Some(name) = data["restaurant_name"].as_str().filter(|n| !n.is_empty()) else {
        tracing::error!("Missing restaurant_name for specific foodie analysis: {data}");
        return state.send(reply, vec![text("❌ 餐廳資訊不完整。")]).await;
    };
    tracing::info!(
        "Starting specific foodie deep analysis for user {user_id} for restaurant {name}"
    );

    // 已有執行中的 batch job 時不重複提交
    match state.batch.has_active_job(user_id).await {
        Ok(true) => {
            tracing::warn!("Active foodie analysis job already exists for user {user_id}");
            return state
                .send(
                    reply,
                    vec![text("⏳ 您的深度分析任務正在執行中，請耐心等候結果送達。")],
                )
                .await;
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!("Specific foodie analysis postback error: {e:#}");
            return state
                .line
                .push(
                    user_id,
                    vec![text(format!(
                        "❌ 處理指定餐廳分析請求失敗：{}",
                        truncate(&e.to_string())
                    ))],
                )
                .await;
        }
    }
    state.send(reply, vec![text(ack_text(name))]).await?;
    spawn_specific_analysis(state, user_id, name, user_id);
    Ok(())
}

/// 餐廳搜尋結果的「🔍 深度評論分析 (Batch)」按鈕：分析附近餐廳。
pub async fn handle_deep_analysis_postback(
    state: &Arc<AppState>,
    reply: &Target,
    data: &Value,
    user_id: &str,
) -> anyhow::Result<()> {
    let (Some(lat), Some(lng)) = (data["latitude"].as_f64(), data["longitude"].as_f64()) else {
        tracing::error!("Missing latitude/longitude for foodie deep analysis: {data}");
        return state
            .send(reply, vec![text("❌ 位置資訊不完整，請重新傳送位置。")])
            .await;
    };
    tracing::info!("Starting foodie deep analysis for user {user_id} at ({lat}, {lng})");
    state
        .send(
            reply,
            vec![text(
                "⏳ 正在為您搜集附近餐廳的 50+ 則最新評論，並建立 Gemini Batch API 批次大數據分析任務。\n\n本分析包含招牌菜推薦、整體口味與雷點摘要，預計需要 1~2 分鐘，分析完成後我會主動將專屬報告推播給您！",
            )],
        )
        .await?;
    let (state, user_id) = (state.clone(), user_id.to_string());
    tokio::spawn(async move {
        run_nearby_analysis(&state, &user_id, lat, lng).await;
    });
    Ok(())
}

async fn push_text(state: &AppState, to: &str, msg: String) {
    if let Err(e) = state.line.push(to, vec![text(msg)]).await {
        tracing::error!("push to {to} failed: {e:#}");
    }
}

async fn run_nearby_analysis(state: &AppState, user_id: &str, lat: f64, lng: f64) {
    // 只取 1 家餐廳，維持速度
    let restaurants = match state.ai.get_nearby_restaurants_for_batch(lat, lng, 1).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("get_nearby_restaurants_for_batch error: {e:#}");
            return push_text(
                state,
                user_id,
                format!("⚠️ 搜尋餐廳與評論失敗: {}", truncate(&e.to_string())),
            )
            .await;
        }
    };
    if restaurants.is_empty() {
        return push_text(
            state,
            user_id,
            "⚠️ 附近似乎沒有找到合適的餐廳進行深度分析。".into(),
        )
        .await;
    }
    let domain = state.webhook_domain();
    match state
        .batch
        .submit_restaurant_job(user_id, lat, lng, &restaurants, domain.as_deref())
        .await
    {
        Ok(job) => tracing::info!("Batch job {job} submitted successfully for user {user_id}"),
        Err(e) => {
            tracing::error!("Error submitting batch job: {e:#}");
            push_text(
                state,
                user_id,
                format!("❌ 建立批次分析工作失敗: {}", truncate(&e.to_string())),
            )
            .await;
        }
    }
}

async fn run_specific_restaurant_analysis(
    state: &AppState,
    user_id: &str,
    restaurant_name: &str,
    push_to: &str,
) {
    tracing::info!("Background task: Searching for specific restaurant {restaurant_name}");
    let restaurant = match state
        .ai
        .search_specific_restaurant_by_name(restaurant_name)
        .await
    {
        Ok(r) if !r.name.is_empty() => r,
        Ok(_) => {
            return push_text(
                state,
                push_to,
                format!("⚠️ 找不到關於餐廳「{restaurant_name}」的評價資訊。"),
            )
            .await;
        }
        Err(e) => {
            tracing::error!("search_specific_restaurant_by_name error: {e:#}");
            return push_text(
                state,
                push_to,
                format!("⚠️ 搜尋指定餐廳失敗: {}", truncate(&e.to_string())),
            )
            .await;
        }
    };
    let domain = state.webhook_domain();
    match state
        .batch
        .submit_restaurant_job(user_id, 0.0, 0.0, &[restaurant], domain.as_deref())
        .await
    {
        Ok(job) => {
            tracing::info!("Batch job {job} submitted for specific restaurant {restaurant_name}")
        }
        Err(e) => {
            tracing::error!("Error submitting batch job: {e:#}");
            push_text(
                state,
                push_to,
                format!("❌ 建立批次分析工作失敗: {}", truncate(&e.to_string())),
            )
            .await;
        }
    }
}

/// batch 完成：找出使用者（webhook metadata → Firestore 對應 → display name），推播分析結果。
/// 回傳 false 代表處理失敗（輪詢時不要刪 job，下次再試）。
pub async fn process_batch_completed(
    state: &AppState,
    job_name: &str,
    output_file: &str,
    user_id: Option<String>,
) -> bool {
    let mut user_id = user_id;
    if user_id.is_none() {
        user_id = state.batch.job_user(job_name).await;
    }
    if user_id.is_none() {
        match state.batch.get_job(job_name).await {
            Ok(job) => user_id = job.user_id().map(str::to_string),
            Err(e) => {
                tracing::error!("Failed to fetch job info for fallback user resolution: {e:#}")
            }
        }
    }
    let Some(user_id) = user_id else {
        tracing::error!("Cannot process batch {job_name}: no user_id associated");
        return true;
    };

    let results = match state.batch.download_results(output_file).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to download and parse batch results: {e:#}");
            Vec::new()
        }
    };
    if results.is_empty() {
        push_text(
            state,
            &user_id,
            "⚠️ 深度美食評論分析處理完成，但無法解析分析結果。".into(),
        )
        .await;
        return true;
    }
    tracing::info!(
        "Successfully retrieved batch analysis for {} restaurants",
        results.len()
    );
    let mut messages = vec![text(
        "🎉 **您的附近餐廳深度評論與菜色大數據分析已出爐！** 👇",
    )];
    messages.extend(
        results
            .iter()
            .map(|r| text(format!("🍴 **{}**\n\n{}", r.name, r.analysis))),
    );
    match state.line.push_batched(&user_id, messages).await {
        Ok(()) => {
            tracing::info!("Pushed deep analysis results to user {user_id}");
            true
        }
        Err(e) => {
            tracing::error!("Failed to process batch completed webhook: {e:#}");
            push_text(state, &user_id, "❌ 處理批次分析結果時發生錯誤。".into()).await;
            false
        }
    }
}

pub async fn process_batch_failed(state: &AppState, job_name: &str) -> bool {
    if let Some(user_id) = state.batch.job_user(job_name).await {
        push_text(
            state,
            &user_id,
            "❌ 很抱歉，您的美食深度評論分析任務執行失敗，請稍後重試。".into(),
        )
        .await;
    }
    true
}

/// 每 60 秒輪詢 Batch API，當作 webhook 沒送達時的備援。
pub fn spawn_batch_polling(state: Arc<AppState>) {
    tokio::spawn(async move {
        tracing::info!("Starting batch job status polling loop...");
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if state.config.google_ai_api_key.is_empty() {
                continue;
            }
            if let Err(e) = poll_once(&state).await {
                tracing::error!("Error in poll_batch_jobs_loop: {e:#}");
            }
        }
    });
}

pub async fn poll_once(state: &AppState) -> anyhow::Result<()> {
    for job in state.batch.list_jobs().await? {
        let Some(user_id) = job.user_id().map(str::to_string) else {
            continue;
        };
        let handled = if job.succeeded() {
            tracing::info!(
                "Polling found succeeded batch job {} for user {user_id}",
                job.name
            );
            process_batch_completed(state, &job.name, &job.output_file(), Some(user_id)).await
        } else if job.failed() {
            tracing::error!(
                "Polling found failed batch job {} for user {user_id}",
                job.name
            );
            process_batch_failed(state, &job.name).await
        } else {
            continue;
        };
        if handled {
            match state.batch.delete_job(&job.name).await {
                Ok(()) => tracing::info!("Deleted batch job {} after processing.", job.name),
                Err(e) => tracing::error!("Failed to delete batch job {}: {e:#}", job.name),
            }
        } else {
            tracing::error!(
                "Skipped deleting batch job {} due to processing failure.",
                job.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dish_query() {
        assert_eq!(
            restaurant_dish_query("幫我查一下鼎泰豐的菜色").as_deref(),
            Some("鼎泰豐")
        );
        assert_eq!(
            restaurant_dish_query("可以幫我查一下 阿宗麵線 菜色嗎").as_deref(),
            Some("阿宗麵線")
        );
        assert_eq!(restaurant_dish_query("幫我查一下的菜色"), None);
        assert_eq!(restaurant_dish_query("今天吃什麼"), None);
    }
}
