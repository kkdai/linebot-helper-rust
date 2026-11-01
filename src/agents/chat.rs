//! ChatAgent：多輪對話 + Google Search grounding。
//!
//! 每次呼叫都從 SessionManager 的 history 重建 contents（instance 重啟後也接得上）。

use std::sync::Arc;

use crate::ai::prompts::CHAT_INSTRUCTION;
use crate::gemini::{
    Content, GeminiClient, GenerateContentRequest, GenerationConfig, Part, WebSource, tools,
};
use crate::store::session::{HistoryMessage, SessionManager};

#[derive(Debug, Clone, PartialEq)]
pub struct ChatReply {
    pub answer: String,
    pub sources: Vec<WebSource>,
    pub has_history: bool,
}

#[derive(Clone)]
pub struct ChatSettings {
    pub model: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub enable_grounding: bool,
}

#[derive(Clone)]
pub struct ChatAgent {
    client: GeminiClient,
    pub sessions: Arc<SessionManager>,
    settings: ChatSettings,
}

pub fn history_to_contents(history: &[HistoryMessage]) -> Vec<Content> {
    history
        .iter()
        .filter(|m| !m.content.is_empty())
        .map(|m| {
            let part = vec![Part::text(m.content.clone())];
            if m.role == "user" {
                Content::user(part)
            } else {
                Content::model(part)
            }
        })
        .collect()
}

impl ChatAgent {
    pub fn new(
        client: GeminiClient,
        sessions: Arc<SessionManager>,
        settings: ChatSettings,
    ) -> Self {
        Self {
            client,
            sessions,
            settings,
        }
    }

    pub async fn chat(&self, user_id: &str, message: &str) -> Result<ChatReply, String> {
        let session = self.sessions.get_or_create_session(user_id).await;
        let has_history = !session.history.is_empty();

        let mut contents = history_to_contents(&session.history);
        contents.push(Content::user_text(format!(
            "{CHAT_INSTRUCTION}\n\n問題：{message}"
        )));
        let mut req = GenerateContentRequest::new(contents).with_config(GenerationConfig {
            temperature: Some(self.settings.temperature),
            max_output_tokens: Some(self.settings.max_output_tokens),
            ..Default::default()
        });
        if self.settings.enable_grounding {
            req = req.with_tools(vec![tools::google_search()]);
        }

        tracing::info!(
            "Processing message for user {user_id}: {}",
            message.chars().take(50).collect::<String>()
        );
        let result = async {
            let resp = self
                .client
                .generate_content(&self.settings.model, req)
                .await
                .map_err(|e| e.to_string())?;
            let answer = resp
                .text()
                .ok_or_else(|| "API returned empty response".to_string())?;
            Ok::<_, String>((answer, resp.grounding_sources()))
        }
        .await;

        match result {
            Ok((answer, sources)) => {
                self.sessions.add_to_history(user_id, "user", message).await;
                self.sessions
                    .add_to_history(user_id, "assistant", &answer)
                    .await;
                Ok(ChatReply {
                    answer,
                    sources,
                    has_history,
                })
            }
            Err(e) => {
                tracing::error!("Chat failed: {e}");
                Err(format!(
                    "對話處理失敗: {}",
                    e.chars().take(100).collect::<String>()
                ))
            }
        }
    }
}

pub fn format_chat_response(reply: &ChatReply, include_sources: bool) -> String {
    let mut text = if reply.has_history {
        format!("💬 [對話中]\n\n{}", reply.answer)
    } else {
        reply.answer.clone()
    };
    if include_sources && !reply.sources.is_empty() {
        text.push_str("\n\n📚 參考來源：\n");
        for (i, s) in reply
            .sources
            .iter()
            .filter(|s| !s.uri.is_empty())
            .take(3)
            .enumerate()
        {
            let title = if s.title.is_empty() {
                "Unknown"
            } else {
                &s.title
            };
            text.push_str(&format!("{}. {title}\n   {}\n", i + 1, s.uri));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryKv;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn agent(server: &MockServer) -> ChatAgent {
        ChatAgent::new(
            GeminiClient::with_base_url(reqwest::Client::new(), &server.uri(), true),
            Arc::new(SessionManager::new(Arc::new(MemoryKv::new()), 30, 20)),
            ChatSettings {
                model: "m".into(),
                temperature: 0.7,
                max_output_tokens: 2048,
                enable_grounding: true,
            },
        )
    }

    #[tokio::test]
    async fn second_turn_sends_history_and_marks_conversation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "contents": [
                    {"role": "user", "parts": [{"text": "你好"}]},
                    {"role": "model", "parts": [{"text": "答1"}]}
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "答2"}]},
                  "groundingMetadata": {"groundingChunks": [{"web": {"uri": "https://s", "title": "S"}}]}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "答1"}]}}]
            })))
            .mount(&server)
            .await;

        let agent = agent(&server);
        let first = agent.chat("U1", "你好").await.unwrap();
        assert!(!first.has_history);
        let second = agent.chat("U1", "再說一次").await.unwrap();
        assert_eq!(second.answer, "答2");
        assert!(second.has_history);
        assert_eq!(
            format_chat_response(&second, true),
            "💬 [對話中]\n\n答2\n\n📚 參考來源：\n1. S\n   https://s\n"
        );
        assert_eq!(format_chat_response(&second, false), "💬 [對話中]\n\n答2");
    }

    #[tokio::test]
    async fn failure_is_reported_and_history_untouched() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let agent = agent(&server);
        let err = agent.chat("U1", "hi").await.unwrap_err();
        assert!(err.starts_with("對話處理失敗: "));
        assert!(
            agent
                .sessions
                .get_session("U1")
                .await
                .unwrap()
                .history
                .is_empty()
        );
    }
}
