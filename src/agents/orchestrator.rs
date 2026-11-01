//! Orchestrator：偵測意圖並分派給對應的 agent。
//!
//! Python 版雖然建立了 ADK Agent 物件，但實際路由全靠 regex／關鍵字，
//! 這裡直接移植路由邏輯，不引入 agent framework。

use std::sync::{Arc, LazyLock};

use futures::future::join_all;
use regex::Regex;
use serde_json::json;

use super::chat::{ChatAgent, ChatReply, format_chat_response};
use crate::ai::{AgenticVision, Ai, NearbyPlaces, prompts};
use crate::loader::UrlLoader;

static URL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"{}|\\^`\[\]]+"#).unwrap());
static YOUTUBE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https?://(?:www\.)?(?:youtube\.com|youtu\.be|m\.youtube\.com)[^\s]*").unwrap()
});

const COMMANDS: &[&str] = &[
    "/clear",
    "/清除",
    "/reset",
    "/重置",
    "/status",
    "/狀態",
    "/info",
    "/help",
    "/幫助",
    "/說明",
    "/session-stats",
    "/stats",
];
const RESTAURANT_KEYWORDS: &[&str] = &[
    "餐廳",
    "美食",
    "好吃",
    "小吃",
    "餐酒館",
    "咖啡廳",
    "早午餐",
    "火鍋",
    "燒肉",
    "壽司",
    "拉麵",
    "牛肉麵",
    "吃什麼",
    "點餐",
    "吃東西",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Command(String),
    Github,
    YouTube(String),
    Url(String),
    Restaurant(String),
    Chat(String),
}

impl Intent {
    pub fn name(&self) -> &'static str {
        match self {
            Intent::Command(_) => "command",
            Intent::Github => "github",
            Intent::YouTube(_) => "youtube",
            Intent::Url(_) => "url_summary",
            Intent::Restaurant(_) => "restaurant_search",
            Intent::Chat(_) => "chat",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentResponse {
    Command(String),
    Chat(ChatReply),
    Content {
        url: String,
        content: String,
    },
    Github(String),
    /// 沒有已知位置時請使用者傳送位置
    NeedLocation(String),
    Restaurant {
        places: NearbyPlaces,
        latitude: f64,
        longitude: f64,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrchestratorResult {
    pub responses: Vec<AgentResponse>,
}

impl OrchestratorResult {
    /// 有已知位置的餐廳搜尋成功時，回傳座標（給「深度評論分析」按鈕）。
    pub fn restaurant_location(&self) -> Option<(f64, f64)> {
        self.responses.iter().find_map(|r| match r {
            AgentResponse::Restaurant {
                latitude,
                longitude,
                ..
            } => Some((*latitude, *longitude)),
            _ => None,
        })
    }
}

pub fn detect_intents(message: &str) -> Vec<Intent> {
    let lower = message.trim().to_lowercase();
    if COMMANDS.contains(&lower.as_str()) {
        return vec![Intent::Command(lower)];
    }
    if lower == "@g" {
        return vec![Intent::Github];
    }

    let mut intents: Vec<Intent> = URL_PATTERN
        .find_iter(message)
        .map(|m| {
            let url = m.as_str().to_string();
            if YOUTUBE_PATTERN.is_match(&url) {
                Intent::YouTube(url)
            } else {
                Intent::Url(url)
            }
        })
        .collect();

    if intents.is_empty() && RESTAURANT_KEYWORDS.iter().any(|k| lower.contains(k)) {
        intents.push(Intent::Restaurant(message.to_string()));
    }
    if intents.is_empty() {
        intents.push(Intent::Chat(message.to_string()));
    }
    intents
}

#[derive(Clone)]
pub struct Orchestrator {
    pub chat: ChatAgent,
    pub ai: Arc<Ai>,
    pub loader: Arc<UrlLoader>,
    pub enable_maps_grounding: bool,
}

impl Orchestrator {
    pub fn new(
        chat: ChatAgent,
        ai: Arc<Ai>,
        loader: Arc<UrlLoader>,
        enable_maps_grounding: bool,
    ) -> Self {
        Self {
            chat,
            ai,
            loader,
            enable_maps_grounding,
        }
    }

    pub async fn process_text(&self, user_id: &str, message: &str) -> OrchestratorResult {
        self.process_text_with_mode(user_id, message, "normal")
            .await
    }

    pub async fn process_text_with_mode(
        &self,
        user_id: &str,
        message: &str,
        mode: &str,
    ) -> OrchestratorResult {
        let intents = detect_intents(message);
        tracing::info!("Detected {} intent(s) for user {user_id}", intents.len());
        // 多個意圖（例如多個網址）平行處理
        let responses = join_all(intents.iter().map(|i| self.route(user_id, i, mode))).await;
        OrchestratorResult { responses }
    }

    async fn route(&self, user_id: &str, intent: &Intent, mode: &str) -> AgentResponse {
        match intent {
            Intent::Command(cmd) => self.handle_command(user_id, cmd).await,
            Intent::Github => AgentResponse::Github(self.ai.summarize_github_issues().await),
            Intent::YouTube(url) => {
                let yt_mode = match mode {
                    "detailed" | "detail" => "detail",
                    "twitter" => "twitter",
                    _ => "normal",
                };
                match self.ai.summarize_youtube(url, yt_mode).await {
                    Ok(content) => AgentResponse::Content {
                        url: url.clone(),
                        content,
                    },
                    Err(e) => AgentResponse::Error(e.to_string()),
                }
            }
            Intent::Url(url) => self.process_url(url, mode).await,
            Intent::Restaurant(_) => self.restaurant_search(user_id).await,
            Intent::Chat(message) => match self.chat.chat(user_id, message).await {
                Ok(reply) => AgentResponse::Chat(reply),
                Err(e) => AgentResponse::Error(e),
            },
        }
    }

    pub async fn process_url(&self, url: &str, mode: &str) -> AgentResponse {
        tracing::info!("Processing URL: {url} (mode: {mode})");
        let content = match self.loader.load(url).await {
            Ok(c) => c,
            Err(e) => return AgentResponse::Error(e.to_string()),
        };
        match self.ai.summarize_text(&content, mode).await {
            Ok(summary) => AgentResponse::Content {
                url: url.to_string(),
                content: summary,
            },
            Err(e) => AgentResponse::Error(format!(
                "處理 URL 時發生錯誤: {}",
                e.to_string().chars().take(100).collect::<String>()
            )),
        }
    }

    async fn restaurant_search(&self, user_id: &str) -> AgentResponse {
        let location = self
            .chat
            .sessions
            .get_session(user_id)
            .await
            .and_then(|s| s.metadata.get("last_location").cloned());
        let Some(loc) = location else {
            return AgentResponse::NeedLocation(
                "📍 偵測到您想要尋找美食！請點選下方按鈕或加號傳送您的『位置資訊』，我將利用 Google Maps Grounding 與 Gemini Batch API 為您進行深度評論與菜色分析！".into(),
            );
        };
        let (Some(latitude), Some(longitude)) =
            (loc["latitude"].as_f64(), loc["longitude"].as_f64())
        else {
            return AgentResponse::Error("位置資訊不完整".into());
        };
        match self.search_nearby(latitude, longitude, "restaurant").await {
            Ok(places) => AgentResponse::Restaurant {
                places,
                latitude,
                longitude,
            },
            Err(e) => AgentResponse::Error(e),
        }
    }

    /// LocationAgent.search
    pub async fn search_nearby(
        &self,
        latitude: f64,
        longitude: f64,
        place_type: &str,
    ) -> Result<NearbyPlaces, String> {
        if !self.enable_maps_grounding {
            return Err("Maps 搜尋功能已停用".into());
        }
        self.ai
            .search_nearby_places(latitude, longitude, place_type, None)
            .await
            .map_err(|e| {
                format!(
                    "無法取得附近地點資訊: {}",
                    e.to_string().chars().take(100).collect::<String>()
                )
            })
    }

    /// 儲存使用者最後一次傳送的位置（餐廳意圖會用到）。
    pub async fn remember_location(
        &self,
        user_id: &str,
        latitude: f64,
        longitude: f64,
        address: &str,
    ) {
        self.chat.sessions.get_or_create_session(user_id).await;
        self.chat
            .sessions
            .set_metadata(
                user_id,
                "last_location",
                json!({ "latitude": latitude, "longitude": longitude, "address": address }),
            )
            .await;
    }

    pub async fn process_image(&self, image: &[u8]) -> String {
        match self.ai.analyze_image(image, None).await {
            Ok(analysis) => format!("🖼️ 圖片分析結果\n\n{analysis}"),
            Err(e) => format!("❌ 分析圖片時發生錯誤: {}", truncate(&e.to_string(), 100)),
        }
    }

    pub async fn process_image_agentic(&self, image: &[u8], prompt: &str) -> AgenticVision {
        match self.ai.analyze_image_agentic(image, Some(prompt)).await {
            Ok(mut result) => {
                result.analysis = format!("🖼️ 圖片分析結果\n\n{}", result.analysis);
                result
            }
            Err(e) => AgenticVision {
                analysis: format!(
                    "❌ Agentic Vision 分析錯誤: {}",
                    truncate(&e.to_string(), 100)
                ),
                images: vec![],
            },
        }
    }

    async fn handle_command(&self, user_id: &str, command: &str) -> AgentResponse {
        let sessions = &self.chat.sessions;
        let text = match command {
            "/clear" | "/清除" | "/reset" | "/重置" => {
                if sessions.clear_session(user_id).await {
                    "✅ 對話已重置\n\n你可以開始新的對話了！".to_string()
                } else {
                    "📊 目前沒有進行中的對話。\n\n發送任何訊息開始新對話！".to_string()
                }
            }
            "/status" | "/狀態" | "/info" => match sessions.get_session_info(user_id).await {
                None => "📊 目前沒有進行中的對話。\n\n發送任何訊息開始新對話！".to_string(),
                Some(info) => format!(
                    "📊 對話狀態\n\n💬 對話輪數：{} 條訊息\n⏰ 開始時間：{}\n🕐 最後活動：{}\n\n使用 /clear 清除對話記憶",
                    info.history_count,
                    fmt_ts(info.created_at, "%Y-%m-%d %H:%M"),
                    fmt_ts(info.last_active, "%H:%M"),
                ),
            },
            "/help" | "/幫助" | "/說明" => prompts::HELP.to_string(),
            "/session-stats" | "/stats" => {
                let s = sessions.get_stats().await;
                format!(
                    "📈 Session 統計資訊\n\n👥 活躍對話數：{}\n💬 總訊息數：{}\n⏱️ 最舊對話：{:.1} 分鐘\n🧹 清理次數：{}\n🗑️ 已清理對話：{}",
                    s.active_sessions,
                    s.total_messages,
                    s.oldest_session_age_minutes,
                    s.cleanup_runs,
                    s.sessions_cleaned
                )
            }
            other => return AgentResponse::Error(format!("Unknown command: {other}")),
        };
        AgentResponse::Command(text)
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn fmt_ts(ts: f64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|d| d.format(fmt).to_string())
        .unwrap_or_default()
}

fn format_single(r: &AgentResponse) -> String {
    match r {
        AgentResponse::Command(t) | AgentResponse::Github(t) | AgentResponse::NeedLocation(t) => {
            t.clone()
        }
        AgentResponse::Chat(reply) => format_chat_response(reply, true),
        AgentResponse::Content { url, content } => format!("{url}\n\n{content}"),
        AgentResponse::Restaurant { places, .. } => places.places.clone(),
        AgentResponse::Error(e) => format!("❌ {e}"),
    }
}

pub fn format_orchestrator_response(result: &OrchestratorResult) -> String {
    match result.responses.as_slice() {
        [] => "❌ 無法處理您的請求".into(),
        [single] => format_single(single),
        many => many
            .iter()
            .enumerate()
            .map(|(i, r)| match r {
                AgentResponse::Chat(reply) => format_chat_response(reply, false),
                AgentResponse::Error(e) => format!("❌ 任務 {}: {e}", i + 1),
                other => format_single(other),
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::chat::ChatSettings;
    use crate::gemini::GeminiClient;
    use crate::store::MemoryKv;
    use crate::store::session::SessionManager;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn intent_detection_table() {
        let cases: Vec<(&str, Vec<Intent>)> = vec![
            ("/clear", vec![Intent::Command("/clear".into())]),
            ("  /HELP ", vec![Intent::Command("/help".into())]),
            ("@G", vec![Intent::Github]),
            (
                "看 https://youtu.be/abc 跟 https://example.com/a",
                vec![
                    Intent::YouTube("https://youtu.be/abc".into()),
                    Intent::Url("https://example.com/a".into()),
                ],
            ),
            (
                "附近有什麼好吃的",
                vec![Intent::Restaurant("附近有什麼好吃的".into())],
            ),
            (
                "這家餐廳 https://example.com",
                vec![Intent::Url("https://example.com".into())],
            ),
            ("今天天氣如何", vec![Intent::Chat("今天天氣如何".into())]),
        ];
        for (input, expected) in cases {
            assert_eq!(detect_intents(input), expected, "input: {input}");
        }
    }

    #[test]
    fn formats_single_and_multiple_responses() {
        let single = OrchestratorResult {
            responses: vec![AgentResponse::Error("壞了".into())],
        };
        assert_eq!(format_orchestrator_response(&single), "❌ 壞了");

        let multi = OrchestratorResult {
            responses: vec![
                AgentResponse::Content {
                    url: "https://a".into(),
                    content: "摘要".into(),
                },
                AgentResponse::Error("失敗".into()),
            ],
        };
        assert_eq!(
            format_orchestrator_response(&multi),
            "https://a\n\n摘要\n\n---\n\n❌ 任務 2: 失敗"
        );
        assert_eq!(
            format_orchestrator_response(&OrchestratorResult { responses: vec![] }),
            "❌ 無法處理您的請求"
        );
    }

    fn orchestrator(server: &MockServer) -> Orchestrator {
        let http = reqwest::Client::new();
        let vertex = GeminiClient::with_base_url(http.clone(), &server.uri(), true);
        let sessions = Arc::new(SessionManager::new(Arc::new(MemoryKv::new()), 30, 20));
        let chat = ChatAgent::new(
            vertex.clone(),
            sessions,
            ChatSettings {
                model: "m".into(),
                temperature: 0.7,
                max_output_tokens: 2048,
                enable_grounding: false,
            },
        );
        let ai = Arc::new(Ai::new(vertex.clone(), vertex, http.clone(), None));
        let loader = Arc::new(UrlLoader::new(http, None, "x", "y"));
        Orchestrator::new(chat, ai, loader, true)
    }

    #[tokio::test]
    async fn restaurant_intent_needs_location_then_uses_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("googleMaps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "1. 鼎泰豐"}]}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "[\"鼎泰豐\"]"}]}}]
            })))
            .mount(&server)
            .await;
        let o = orchestrator(&server);

        let first = o.process_text("U1", "想吃火鍋").await;
        assert!(matches!(first.responses[0], AgentResponse::NeedLocation(_)));
        assert_eq!(first.restaurant_location(), None);

        o.remember_location("U1", 25.03, 121.56, "台北").await;
        let second = o.process_text("U1", "想吃火鍋").await;
        assert_eq!(second.restaurant_location(), Some((25.03, 121.56)));
        assert!(format_orchestrator_response(&second).starts_with("🍴 附近的餐廳"));
    }

    #[tokio::test]
    async fn commands_manage_sessions() {
        let server = MockServer::start().await;
        let o = orchestrator(&server);
        let text = |r: OrchestratorResult| format_orchestrator_response(&r);
        assert!(text(o.process_text("U1", "/status").await).contains("目前沒有進行中的對話"));
        o.chat.sessions.get_or_create_session("U1").await;
        assert!(text(o.process_text("U1", "/status").await).starts_with("📊 對話狀態"));
        assert!(text(o.process_text("U1", "/clear").await).starts_with("✅ 對話已重置"));
        assert!(text(o.process_text("U1", "/stats").await).starts_with("📈 Session 統計資訊"));
        assert!(text(o.process_text("U1", "/help").await).contains("/clear"));
    }
}
