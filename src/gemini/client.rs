//! Gemini REST client。
//!
//! - Vertex AI：ADC 認證，摘要／對話／grounding／圖片／YouTube／轉錄
//! - Developer API：API key，TTS／Batch／Files／Live

use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::types::{GenerateContentRequest, GenerateContentResponse};
use crate::gcp::GcpAuth;

/// Python 版每個 Vertex 呼叫都帶這個 label，方便在帳單裡篩選
const CLIENT_LABEL: &str = "info_helper";
const DEVELOPER_BASE: &str = "https://generativelanguage.googleapis.com";

#[derive(Debug, thiserror::Error)]
pub enum GeminiError {
    #[error("Gemini API HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("Gemini request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Gemini auth failed: {0}")]
    Auth(String),
    #[error("Gemini response decode failed: {0}")]
    Decode(String),
}

impl GeminiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Request(e) => e.status().map(|s| s.as_u16()),
            _ => None,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        self.status() == Some(429)
    }
}

pub type Result<T> = std::result::Result<T, GeminiError>;

#[derive(Clone)]
enum Auth {
    Adc(GcpAuth),
    ApiKey(String),
    /// 測試用：不帶任何認證
    None,
}

#[derive(Clone)]
pub struct GeminiClient {
    http: reqwest::Client,
    /// 例如 `https://aiplatform.googleapis.com/v1/projects/p/locations/global/publishers/google`
    /// 或 `https://generativelanguage.googleapis.com/v1beta`
    base: String,
    auth: Auth,
    vertex: bool,
}

impl GeminiClient {
    pub fn vertex(http: reqwest::Client, auth: GcpAuth, project: &str, location: &str) -> Self {
        let host = if location == "global" {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{location}-aiplatform.googleapis.com")
        };
        Self {
            http,
            base: format!(
                "https://{host}/v1/projects/{project}/locations/{location}/publishers/google"
            ),
            auth: Auth::Adc(auth),
            vertex: true,
        }
    }

    pub fn developer(http: reqwest::Client, api_key: &str) -> Self {
        Self {
            http,
            base: format!("{DEVELOPER_BASE}/v1beta"),
            auth: Auth::ApiKey(api_key.to_string()),
            vertex: false,
        }
    }

    /// 測試用：指向 mock server。
    pub fn with_base_url(http: reqwest::Client, base: &str, vertex: bool) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            auth: Auth::None,
            vertex,
        }
    }

    pub fn is_vertex(&self) -> bool {
        self.vertex
    }

    pub fn api_key(&self) -> Option<&str> {
        match &self.auth {
            Auth::ApiKey(k) => Some(k),
            _ => None,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Developer API 的 host（Files 上傳／下載用的路徑不在 `/v1beta` 底下）。
    pub fn developer_root(&self) -> String {
        if self.base.ends_with("/v1beta") {
            self.base.trim_end_matches("/v1beta").to_string()
        } else {
            self.base.clone()
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn authorize(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(match &self.auth {
            Auth::Adc(auth) => {
                let token = auth
                    .token()
                    .await
                    .map_err(|e| GeminiError::Auth(e.to_string()))?;
                req.bearer_auth(token)
            }
            Auth::ApiKey(key) => req.header("x-goog-api-key", key),
            Auth::None => req,
        })
    }

    fn prepare(&self, mut request: GenerateContentRequest) -> GenerateContentRequest {
        if self.vertex {
            request
                .labels
                .get_or_insert_with(|| json!({ "client_id": CLIENT_LABEL }));
        } else {
            request.labels = None;
        }
        request
    }

    pub async fn generate_content(
        &self,
        model: &str,
        request: GenerateContentRequest,
    ) -> Result<GenerateContentResponse> {
        let url = format!("{}/models/{model}:generateContent", self.base);
        self.post_json(&url, &self.prepare(request)).await
    }

    /// SSE 串流，把每個 chunk 依序收集起來回傳（TTS 用）。
    pub async fn stream_generate_content(
        &self,
        model: &str,
        request: GenerateContentRequest,
    ) -> Result<Vec<GenerateContentResponse>> {
        let url = format!("{}/models/{model}:streamGenerateContent?alt=sse", self.base);
        let req = self
            .authorize(self.http.post(&url).json(&self.prepare(request)))
            .await?;
        let resp = check_status(req.timeout(Duration::from_secs(180)).send().await?).await?;

        let mut chunks = Vec::new();
        let mut buffer = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(bytes) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(&bytes?));
            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                if let Some(chunk) = parse_sse_line(&line)? {
                    chunks.push(chunk);
                }
            }
        }
        if let Some(chunk) = parse_sse_line(&buffer)? {
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    pub async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let req = self.authorize(self.http.post(url).json(body)).await?;
        let resp = check_status(req.timeout(Duration::from_secs(300)).send().await?).await?;
        decode(resp).await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let req = self.authorize(self.http.get(url)).await?;
        let resp = check_status(req.timeout(Duration::from_secs(60)).send().await?).await?;
        decode(resp).await
    }

    pub async fn delete(&self, url: &str) -> Result<()> {
        let req = self.authorize(self.http.delete(url)).await?;
        check_status(req.timeout(Duration::from_secs(60)).send().await?).await?;
        Ok(())
    }
}

pub(crate) async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(GeminiError::Http {
        status: status.as_u16(),
        body: body.chars().take(500).collect(),
    })
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let text = resp.text().await?;
    serde_json::from_str(&text).map_err(|e| GeminiError::Decode(format!("{e}: {text:.200}")))
}

fn parse_sse_line(line: &str) -> Result<Option<GenerateContentResponse>> {
    let Some(data) = line.trim().strip_prefix("data:") else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data)
        .map(Some)
        .map_err(|e| GeminiError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::types::tools;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn vertex_request_adds_client_label() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.1-flash-lite:generateContent"))
            .and(body_partial_json(json!({
                "labels": {"client_id": "info_helper"},
                "tools": [{"googleSearch": {}}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "ok"}]}}]
            })))
            .mount(&server)
            .await;

        let client = GeminiClient::with_base_url(reqwest::Client::new(), &server.uri(), true);
        let resp = client
            .generate_content(
                "gemini-3.1-flash-lite",
                GenerateContentRequest::text("hi").with_tools(vec![tools::google_search()]),
            )
            .await
            .unwrap();
        assert_eq!(resp.text().as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn http_error_keeps_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("quota"))
            .mount(&server)
            .await;
        let client = GeminiClient::with_base_url(reqwest::Client::new(), &server.uri(), false);
        let err = client
            .generate_content("m", GenerateContentRequest::text("hi"))
            .await
            .unwrap_err();
        assert!(err.is_rate_limited());
        assert!(err.to_string().contains("429"));
    }

    #[tokio::test]
    async fn stream_collects_sse_chunks() {
        let server = MockServer::start().await;
        let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}]}\r\n\r\n\
                    data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"b\"}]}}]}\r\n\r\n";
        Mock::given(method("POST"))
            .and(path("/models/tts:streamGenerateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let client = GeminiClient::with_base_url(reqwest::Client::new(), &server.uri(), false);
        let chunks = client
            .stream_generate_content("tts", GenerateContentRequest::text("hi"))
            .await
            .unwrap();
        let texts: Vec<_> = chunks.iter().filter_map(|c| c.text()).collect();
        assert_eq!(texts, vec!["a", "b"]);
    }
}
