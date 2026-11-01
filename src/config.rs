//! 環境變數設定。
//!
//! 變數名稱刻意與 Python 版（linebot-helper-python）完全相同，
//! 讓現有 Cloud Run 服務的環境變數可以直接沿用。

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,

    // LINE
    pub channel_secret: String,
    pub channel_access_token: String,
    pub channel_access_token_hf: String,
    pub line_user_id: String,
    pub liff_id: String,

    // Vertex AI
    pub project_id: String,
    pub location: String,
    pub chat_model: String,
    pub fast_model: String,
    pub session_timeout_minutes: i64,
    pub max_history_length: usize,
    pub max_output_tokens: u32,
    pub temperature: f32,
    pub enable_grounding: bool,
    pub enable_maps_grounding: bool,
    /// 每 60 秒輪詢 Gemini Batch jobs。與 Python 版並行期間要關掉，避免兩邊重複推播
    pub enable_batch_polling: bool,

    // Gemini Developer API（TTS / Batch / Live）。Live 模型寫死在 voice.rs（同 Python 版）
    pub google_ai_api_key: String,

    // 選用功能
    pub firecrawl_key: Option<String>,
    pub github_token: Option<String>,
    pub webhook_signing_secret: Option<String>,
    pub webhook_domain: Option<String>,
    pub singlefile_path: String,
    pub chromium_path: String,
    pub static_dir: String,
}

#[derive(Debug, thiserror::Error)]
#[error("Specify {0} as environment variable.")]
pub struct MissingEnv(pub &'static str);

fn required(name: &'static str) -> Result<String, MissingEnv> {
    match env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(MissingEnv(name)),
    }
}

fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn or_default(name: &str, default: &str) -> String {
    optional(name).unwrap_or_else(|| default.to_string())
}

fn parse_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    optional(name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn flag(name: &str, default: bool) -> bool {
    optional(name)
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Result<Self, MissingEnv> {
        Ok(Self {
            port: parse_or("PORT", 8080),
            channel_secret: required("ChannelSecret")?,
            channel_access_token: required("ChannelAccessToken")?,
            channel_access_token_hf: required("ChannelAccessTokenHF")?,
            line_user_id: required("LINE_USER_ID")?,
            liff_id: or_default("LIFF_ID", ""),
            project_id: required("GOOGLE_CLOUD_PROJECT")?,
            // gemini-3.x 模型只在 global 端點提供，設成 us-central1 會 404
            location: or_default("GOOGLE_CLOUD_LOCATION", "global"),
            chat_model: or_default("CHAT_MODEL", "gemini-3.1-flash-lite"),
            fast_model: or_default("FAST_MODEL", "gemini-3.1-flash-lite"),
            session_timeout_minutes: parse_or("SESSION_TIMEOUT_MINUTES", 30),
            max_history_length: parse_or("MAX_HISTORY_LENGTH", 20),
            max_output_tokens: parse_or("MAX_OUTPUT_TOKENS", 2048),
            temperature: parse_or("AGENT_TEMPERATURE", 0.7),
            enable_grounding: flag("ENABLE_GROUNDING", true),
            enable_maps_grounding: flag("ENABLE_MAPS_GROUNDING", true),
            enable_batch_polling: flag("ENABLE_BATCH_POLLING", true),
            google_ai_api_key: optional("GOOGLE_AI_API_KEY")
                .or_else(|| optional("GEMINI_API_KEY"))
                .unwrap_or_default(),
            firecrawl_key: optional("firecrawl_key"),
            github_token: optional("GITHUB_TOKEN"),
            webhook_signing_secret: optional("WEBHOOK_SIGNING_SECRET"),
            webhook_domain: optional("WEBHOOK_DOMAIN"),
            singlefile_path: or_default("SINGLEFILE_PATH", "single-file"),
            chromium_path: or_default("CHROMIUM_PATH", "/usr/bin/chromium"),
            static_dir: or_default("STATIC_DIR", "static"),
        })
    }

    /// 測試用的最小設定，不讀環境變數。
    pub fn for_test() -> Self {
        Self {
            port: 8080,
            channel_secret: "test-secret".into(),
            channel_access_token: "test-token".into(),
            channel_access_token_hf: "test-token-hf".into(),
            line_user_id: "Utest".into(),
            liff_id: "liff-test".into(),
            project_id: "test-project".into(),
            location: "global".into(),
            chat_model: "gemini-3.1-flash-lite".into(),
            fast_model: "gemini-3.1-flash-lite".into(),
            session_timeout_minutes: 30,
            max_history_length: 20,
            max_output_tokens: 2048,
            temperature: 0.7,
            enable_grounding: true,
            enable_maps_grounding: true,
            enable_batch_polling: false,
            google_ai_api_key: "test-key".into(),
            firecrawl_key: None,
            github_token: None,
            webhook_signing_secret: None,
            webhook_domain: None,
            singlefile_path: "single-file".into(),
            chromium_path: "/usr/bin/chromium".into(),
            static_dir: "static".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_env_message_names_the_variable() {
        assert_eq!(
            MissingEnv("ChannelSecret").to_string(),
            "Specify ChannelSecret as environment variable."
        );
    }
}
