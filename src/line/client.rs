//! LINE Messaging API client（只實作用得到的端點）。

use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};

const API_BASE: &str = "https://api.line.me";
const DATA_BASE: &str = "https://api-data.line.me";
/// LINE 一次 reply / push 最多 5 則
pub const MAX_MESSAGES: usize = 5;

#[derive(Clone)]
pub struct LineClient {
    http: reqwest::Client,
    token: String,
    api_base: String,
    data_base: String,
}

impl LineClient {
    pub fn new(http: reqwest::Client, token: &str) -> Self {
        Self {
            http,
            token: token.into(),
            api_base: API_BASE.into(),
            data_base: DATA_BASE.into(),
        }
    }

    /// 測試用：指向 mock server。
    pub fn with_base_url(http: reqwest::Client, token: &str, base: &str) -> Self {
        Self {
            http,
            token: token.into(),
            api_base: base.into(),
            data_base: base.into(),
        }
    }

    /// 換一個 channel token（`/hf` 推播用另一個 bot）。
    pub fn with_token(&self, token: &str) -> Self {
        Self {
            token: token.into(),
            ..self.clone()
        }
    }

    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(format!("{}{path}", self.api_base))
            .bearer_auth(&self.token)
            .json(body)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LINE API {path} returned {status}: {text:.300}");
        }
        Ok(())
    }

    pub async fn reply(&self, reply_token: &str, messages: Vec<Value>) -> anyhow::Result<()> {
        let messages: Vec<Value> = messages.into_iter().take(MAX_MESSAGES).collect();
        self.post(
            "/v2/bot/message/reply",
            &json!({ "replyToken": reply_token, "messages": messages }),
        )
        .await
    }

    pub async fn push(&self, to: &str, messages: Vec<Value>) -> anyhow::Result<()> {
        let messages: Vec<Value> = messages.into_iter().take(MAX_MESSAGES).collect();
        self.post(
            "/v2/bot/message/push",
            &json!({ "to": to, "messages": messages }),
        )
        .await
    }

    /// 超過 5 則時分批推送。
    pub async fn push_batched(&self, to: &str, messages: Vec<Value>) -> anyhow::Result<()> {
        for chunk in messages.chunks(MAX_MESSAGES) {
            self.push(to, chunk.to_vec()).await?;
        }
        Ok(())
    }

    /// 下載使用者傳來的圖片／語音。
    pub async fn get_content(&self, message_id: &str) -> anyhow::Result<Bytes> {
        let resp = self
            .http
            .get(format!(
                "{}/v2/bot/message/{message_id}/content",
                self.data_base
            ))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(60))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.bytes().await?)
    }

    /// 聊天室 loading 動畫（不耗訊息額度；只在 1:1 有效）。失敗只記 log。
    pub async fn show_loading_animation(&self, chat_id: &str, seconds: u32) {
        if let Err(e) = self
            .post(
                "/v2/bot/chat/loading/start",
                &json!({ "chatId": chat_id, "loadingSeconds": seconds }),
            )
            .await
        {
            tracing::warn!("show_loading_animation failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::messages;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn reply_sends_token_and_truncates_to_five() {
        let server = MockServer::start().await;
        let msgs: Vec<Value> = (0..7).map(|i| messages::text(format!("m{i}"))).collect();
        let expected: Vec<Value> = msgs[..5].to_vec();
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(json!({"replyToken": "r", "messages": expected})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let client = LineClient::with_base_url(reqwest::Client::new(), "tok", &server.uri());
        client.reply("r", msgs).await.unwrap();
    }

    #[tokio::test]
    async fn push_batched_splits_by_five() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(3)
            .mount(&server)
            .await;
        let client = LineClient::with_base_url(reqwest::Client::new(), "tok", &server.uri());
        let msgs: Vec<Value> = (0..11).map(|i| messages::text(format!("m{i}"))).collect();
        client.push_batched("U1", msgs).await.unwrap();
    }

    #[tokio::test]
    async fn api_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid reply token"))
            .mount(&server)
            .await;
        let client = LineClient::with_base_url(reqwest::Client::new(), "tok", &server.uri());
        let err = client
            .reply("r", vec![messages::text("x")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid reply token"));
    }

    #[tokio::test]
    async fn get_content_returns_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/bot/message/m1/content"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
            .mount(&server)
            .await;
        let client = LineClient::with_base_url(reqwest::Client::new(), "tok", &server.uri());
        assert_eq!(&client.get_content("m1").await.unwrap()[..], b"abc");
    }
}
