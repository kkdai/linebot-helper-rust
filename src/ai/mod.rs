//! 所有「呼叫一次 Gemini 就完成」的功能（移植自 tools/、loader/langtools.py 等）。
//! prompt 一字不改，見 [`prompts`]。

pub mod json;
pub mod prompts;
pub mod tts;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::gemini::{
    Content, GeminiClient, GenerateContentRequest, GenerationConfig, Part, ThinkingConfig,
    WebSource, tools,
};
use crate::loader::VideoSummarizer;

pub const FLASH_LITE: &str = "gemini-3.1-flash-lite";
pub const AGENTIC_VISION_MODEL: &str = "gemini-3-flash-preview";
const GITHUB_REPO: &str = "kkdai/bookmarks";

#[derive(Clone)]
pub struct Ai {
    /// Vertex AI（ADC）
    pub vertex: GeminiClient,
    /// Developer API（API key）：TTS
    pub developer: GeminiClient,
    http: reqwest::Client,
    github_token: Option<String>,
    github_base: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocialPosts {
    pub title: String,
    pub summary_analysis: String,
    pub facebook: String,
    pub linkedin: String,
    pub threads: String,
}

impl SocialPosts {
    fn fallback(reason: &str) -> Self {
        Self {
            title: "摘要生成失敗".into(),
            summary_analysis: format!("生成摘要失敗：{reason}"),
            facebook: format!("生成 Facebook 文案失敗：{reason}"),
            linkedin: format!("生成 LinkedIn 文案失敗：{reason}"),
            threads: format!("生成 Threads 文案失敗：{reason}"),
        }
    }

    fn empty_input() -> Self {
        Self {
            title: "無法取得網頁內容".into(),
            summary_analysis: "無法取得網頁內容，無法產生摘要。".into(),
            facebook: "無法取得網頁內容，無法產生文案。".into(),
            linkedin: "無法取得網頁內容，無法產生文案。".into(),
            threads: "無法取得網頁內容，無法產生文案。".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BookmarkSummary {
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResearchReport {
    pub markdown: String,
    pub sources: Vec<WebSource>,
}

#[derive(Debug, Clone, Default)]
pub struct AgenticVision {
    pub analysis: String,
    pub images: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NearbyPlaces {
    /// 已加上 emoji 標題的結果文字
    pub places: String,
    pub restaurant_names: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Restaurant {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
    #[serde(default, deserialize_with = "de_display")]
    pub rating: String,
    #[serde(default)]
    pub reviews: Vec<String>,
}

fn de_display<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(json::display(&Value::deserialize(d)?))
}

pub fn place_type_info(place_type: &str) -> (&'static str, &'static str, &'static str) {
    // (emoji, 名稱, 查詢語句)
    match place_type {
        "gas_station" => (
            "⛽",
            "加油站",
            "請幫我找出附近的加油站，並列出名稱、距離和地址。",
        ),
        "parking" => (
            "🅿️",
            "停車場",
            "請幫我找出附近的停車場，並列出名稱、收費方式（如果有）和地址。",
        ),
        _ => (
            "🍴",
            "餐廳",
            "請幫我找出附近評價不錯的餐廳，並列出名稱、類型和地址。",
        ),
    }
}

fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        _ => "image/png",
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn string_schema(description: &str) -> Value {
    json!({ "type": "STRING", "description": description })
}

fn social_posts_schema() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {
            "title": string_schema("文章標題（15 字內，取自原文重點，繁體中文台灣用語）"),
            "summary_analysis": string_schema("文章摘要與重點分析（150-250 字繁體中文：先 2-3 句摘要文章核心內容，再 2-3 句分析重點、為什麼值得讀、對讀者的意義。純文字不用 markdown）"),
            "facebook": string_schema("適合 Facebook 的爆款分享貼文文案，包含吸引人的標題、Emoji、條列重點、互動問題及相關 Hashtag"),
            "linkedin": string_schema("適合 LinkedIn 的專業商務貼文文案，著重專業洞察、核心收穫、引人深思的問題及專業 Hashtag"),
            "threads": string_schema("適合 Threads 的口語化貼文文案，以脆友語氣撰寫，第一句需有強烈共鳴或槽點，段落極短，少用 Hashtag，著重引導留言討論"),
        },
        "required": ["title", "summary_analysis", "facebook", "linkedin", "threads"],
        "propertyOrdering": ["title", "summary_analysis", "facebook", "linkedin", "threads"],
    })
}

fn bookmark_schema() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {
            "title": string_schema("文章標題（15 字內，取自原文重點，繁體中文台灣用語）"),
            "summary": string_schema("文章摘要與重點分析（150-250 字繁體中文：先摘要核心內容，再點出重點與值得注意之處。純文字不用 markdown）"),
        },
        "required": ["title", "summary"],
        "propertyOrdering": ["title", "summary"],
    })
}

impl Ai {
    pub fn new(
        vertex: GeminiClient,
        developer: GeminiClient,
        http: reqwest::Client,
        github_token: Option<String>,
    ) -> Self {
        Self {
            vertex,
            developer,
            http,
            github_token,
            github_base: "https://api.github.com".into(),
        }
    }

    pub fn with_github_base(mut self, base: &str) -> Self {
        self.github_base = base.into();
        self
    }

    async fn text_call(
        &self,
        request: GenerateContentRequest,
        model: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(self.vertex.generate_content(model, request).await?.text())
    }

    /// 條列式摘要（short / normal / detailed）。
    pub async fn summarize_text(&self, text: &str, mode: &str) -> anyhow::Result<String> {
        if text.trim().is_empty() {
            anyhow::bail!("No text content provided for summarization");
        }
        let prompt = prompts::fill(prompts::summarize(mode), &[("text", text)]);
        let req = GenerateContentRequest::text(prompt).with_config(GenerationConfig {
            temperature: Some(0.0),
            max_output_tokens: Some(2048),
            ..Default::default()
        });
        Ok(self
            .text_call(req, FLASH_LITE)
            .await?
            .unwrap_or_else(|| "無法生成摘要".into()))
    }

    /// FB / LinkedIn / Threads 爆款文案 + 標題 + 摘要分析。失敗時回 fallback（不丟錯誤）。
    pub async fn generate_social_media_posts(&self, text: &str) -> SocialPosts {
        if text.trim().is_empty() {
            return SocialPosts::empty_input();
        }
        let req = GenerateContentRequest::text(prompts::social_media(text)).with_config(
            GenerationConfig {
                temperature: Some(0.7),
                response_mime_type: Some("application/json".into()),
                response_schema: Some(social_posts_schema()),
                // flash-lite 是思考模型，上限為「思考 + 輸出」共用；4096 偶爾會把 JSON 擠爆
                max_output_tokens: Some(8192),
                ..Default::default()
            },
        );
        let result: anyhow::Result<SocialPosts> = async {
            let text = self
                .text_call(req, FLASH_LITE)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Empty response text from Gemini"))?;
            Ok(serde_json::from_str(&text)?)
        }
        .await;
        result.unwrap_or_else(|e| {
            tracing::error!("Error generating social media posts: {e}");
            SocialPosts::fallback(&truncate(&e.to_string(), 100))
        })
    }

    /// `/save` 書籤用的標題 + 摘要。失敗時回 fallback。
    pub async fn summarize_for_bookmark(&self, text: &str) -> BookmarkSummary {
        if text.trim().is_empty() {
            return BookmarkSummary {
                title: "無法取得網頁內容".into(),
                summary: "無法取得網頁內容，無法產生摘要。".into(),
            };
        }
        let prompt = prompts::fill(prompts::BOOKMARK_SUMMARY, &[("text", text)]);
        let req = GenerateContentRequest::text(prompt).with_config(GenerationConfig {
            temperature: Some(0.3),
            response_mime_type: Some("application/json".into()),
            response_schema: Some(bookmark_schema()),
            max_output_tokens: Some(4096),
            ..Default::default()
        });
        let result: anyhow::Result<BookmarkSummary> = async {
            let text = self
                .text_call(req, FLASH_LITE)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Empty response text from Gemini"))?;
            Ok(serde_json::from_str(&text)?)
        }
        .await;
        result.unwrap_or_else(|e| {
            tracing::error!("Error generating bookmark summary: {e}");
            BookmarkSummary {
                title: "摘要生成失敗".into(),
                summary: format!("生成摘要失敗：{}", truncate(&e.to_string(), 100)),
            }
        })
    }

    /// 深入研究報告：先帶 Google Search grounding，失敗就拿掉工具重試一次。
    pub async fn generate_research_report(
        &self,
        text: &str,
        url: &str,
    ) -> anyhow::Result<ResearchReport> {
        if text.trim().is_empty() {
            return Ok(ResearchReport {
                markdown: String::new(),
                sources: vec![],
            });
        }
        let prompt = prompts::fill(prompts::RESEARCH_REPORT, &[("url", url), ("text", text)]);
        let build = |grounding: bool| {
            let req = GenerateContentRequest::text(prompt.clone()).with_config(GenerationConfig {
                temperature: Some(0.4),
                max_output_tokens: Some(16384),
                ..Default::default()
            });
            if grounding {
                req.with_tools(vec![tools::google_search()])
            } else {
                req
            }
        };
        let resp = match self.vertex.generate_content(FLASH_LITE, build(true)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Grounded research call failed, retrying without tools: {e}");
                self.vertex
                    .generate_content(FLASH_LITE, build(false))
                    .await?
            }
        };
        let markdown = resp
            .text()
            .ok_or_else(|| anyhow::anyhow!("Empty response text from Gemini"))?;
        Ok(ResearchReport {
            markdown,
            sources: resp.grounding_sources(),
        })
    }

    /// YouTube 影片摘要（Gemini 直接看影片）。429 時退避重試一次。
    pub async fn summarize_youtube(&self, url: &str, mode: &str) -> anyhow::Result<String> {
        tracing::info!("Summarizing YouTube video: {url} (mode: {mode})");
        let mut delay = Duration::from_secs(5);
        let max_retries = 2;
        for attempt in 0..max_retries {
            let req = GenerateContentRequest::new(vec![Content::user(vec![
                Part::file_uri(url, "video/mp4"),
                Part::text(prompts::youtube(mode)),
            ])]);
            match self.vertex.generate_content(FLASH_LITE, req).await {
                Ok(resp) => {
                    return resp
                        .text()
                        .ok_or_else(|| anyhow::anyhow!("無法從影片中提取摘要。"));
                }
                Err(e) if e.is_rate_limited() && attempt + 1 < max_retries => {
                    tracing::warn!("Rate limit hit (429), retrying in {delay:?}...");
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(e) if e.is_rate_limited() => {
                    anyhow::bail!("Vertex AI 使用量已達上限，請稍後再試。建議等待 1-2 分鐘後重試。")
                }
                Err(e) => {
                    tracing::error!("Vertex AI API error: {e}");
                    anyhow::bail!("處理影片時發生錯誤: {}", truncate(&e.to_string(), 100))
                }
            }
        }
        anyhow::bail!("處理影片時發生未預期的錯誤")
    }

    pub async fn analyze_image(
        &self,
        image: &[u8],
        prompt: Option<&str>,
    ) -> anyhow::Result<String> {
        if image.is_empty() {
            anyhow::bail!("No image data provided");
        }
        let req = GenerateContentRequest::new(vec![Content::user(vec![
            Part::text(prompt.unwrap_or(prompts::VISION_DEFAULT)),
            Part::inline(image, sniff_image_mime(image)),
        ])])
        .with_config(GenerationConfig {
            temperature: Some(0.5),
            max_output_tokens: Some(8192),
            thinking_config: Some(ThinkingConfig { thinking_budget: 0 }),
            ..Default::default()
        });
        let resp = self.vertex.generate_content(FLASH_LITE, req).await?;
        tracing::info!("Image analysis finish_reason: {:?}", resp.finish_reason());
        resp.text()
            .ok_or_else(|| anyhow::anyhow!("No analysis generated"))
    }

    /// Agentic Vision：模型可寫程式放大、裁切、標註圖片，回傳文字與標註圖。
    pub async fn analyze_image_agentic(
        &self,
        image: &[u8],
        prompt: Option<&str>,
    ) -> anyhow::Result<AgenticVision> {
        if image.is_empty() {
            anyhow::bail!("No image data provided");
        }
        let req = GenerateContentRequest::new(vec![Content::user(vec![
            Part::text(prompt.unwrap_or(prompts::VISION_DEFAULT)),
            Part::inline(image, sniff_image_mime(image)),
        ])])
        .with_tools(vec![tools::code_execution()])
        .with_config(GenerationConfig {
            temperature: Some(0.5),
            max_output_tokens: Some(4096),
            thinking_config: Some(ThinkingConfig {
                thinking_budget: 2048,
            }),
            ..Default::default()
        });
        let resp = self
            .vertex
            .generate_content(AGENTIC_VISION_MODEL, req)
            .await?;
        let mut texts = Vec::new();
        for part in resp.all_parts() {
            if part.is_thought() {
                continue;
            }
            if let Some(t) = part.text {
                texts.push(t);
            }
            if let Some(r) = part.code_execution_result {
                texts.push(format!("[Code Output]: {}", r.output));
            }
        }
        let images = resp.inline_images();
        if texts.is_empty() && images.is_empty() {
            anyhow::bail!("No analysis generated");
        }
        Ok(AgenticVision {
            analysis: if texts.is_empty() {
                "（圖片已標註完成）".into()
            } else {
                texts.join("\n")
            },
            images,
        })
    }

    /// 語音轉文字（LINE 語音是 m4a / audio/mp4）。
    pub async fn transcribe_audio(&self, audio: &[u8]) -> anyhow::Result<String> {
        let req = GenerateContentRequest::new(vec![Content::user(vec![
            Part::inline(audio, "audio/mp4"),
            Part::text("請將以上語音內容完整轉錄成文字，保留原語言，不要加任何說明或前綴。"),
        ])]);
        let text = self.text_call(req, FLASH_LITE).await?.unwrap_or_default();
        tracing::info!("Transcription result ({} chars)", text.chars().count());
        Ok(text)
    }

    fn maps_request(&self, query: String, lat: f64, lng: f64) -> GenerateContentRequest {
        GenerateContentRequest::text(query)
            .with_tools(vec![tools::google_maps()])
            .with_tool_config(tools::lat_lng_config(lat, lng, "zh-TW"))
    }

    /// Google Maps grounding 找附近地點；餐廳另外抽出前三個店名給 Quick Reply。
    pub async fn search_nearby_places(
        &self,
        latitude: f64,
        longitude: f64,
        place_type: &str,
        custom_query: Option<&str>,
    ) -> anyhow::Result<NearbyPlaces> {
        let (emoji, name, default_query) = place_type_info(place_type);
        let query = custom_query.unwrap_or(default_query).to_string();
        tracing::info!("Searching for {place_type} at ({latitude}, {longitude})");
        let result = self
            .text_call(self.maps_request(query, latitude, longitude), FLASH_LITE)
            .await?
            .unwrap_or_default();

        let mut restaurant_names = Vec::new();
        if place_type == "restaurant" {
            let prompt = prompts::fill(prompts::RESTAURANT_NAMES, &[("result", &result)]);
            match self
                .text_call(GenerateContentRequest::text(prompt), FLASH_LITE)
                .await
            {
                Ok(text) => {
                    restaurant_names = json::extract_string_array(&text.unwrap_or_default());
                    restaurant_names.truncate(3);
                }
                Err(e) => tracing::error!("Failed to extract restaurant names: {e}"),
            }
        }
        Ok(NearbyPlaces {
            places: format!("{emoji} 附近的{name}：\n\n{result}"),
            restaurant_names,
        })
    }

    /// 深度分析用：附近餐廳清單（Maps grounding）+ 每家用 Search grounding 補評論。
    pub async fn get_nearby_restaurants_for_batch(
        &self,
        latitude: f64,
        longitude: f64,
        limit: usize,
    ) -> anyhow::Result<Vec<Restaurant>> {
        let query = prompts::fill(prompts::NEARBY_BATCH, &[("limit", &limit.to_string())]);
        let text = self
            .text_call(self.maps_request(query, latitude, longitude), FLASH_LITE)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("Vertex AI returned an empty response. It might have been blocked or Maps grounding returned nothing.")
            })?;
        let parsed = json::extract_object(&text)?;
        let mut restaurants: Vec<Restaurant> =
            serde_json::from_value(parsed["restaurants"].clone()).unwrap_or_default();
        restaurants.truncate(limit);

        for r in restaurants.iter_mut().filter(|r| !r.name.is_empty()) {
            let prompt = prompts::fill(
                prompts::RESTAURANT_REVIEWS,
                &[("name", &r.name), ("address", &r.address)],
            );
            let req = GenerateContentRequest::text(prompt).with_tools(vec![tools::google_search()]);
            r.reviews = match self.text_call(req, FLASH_LITE).await {
                Ok(Some(t)) if !t.trim().is_empty() => vec![t.trim().to_string()],
                Ok(_) => vec!["暫無網路評價資訊".into()],
                Err(e) => {
                    tracing::error!("Failed to search reviews for {}: {e}", r.name);
                    vec!["暫無網路評價資訊".into()]
                }
            };
        }
        Ok(restaurants)
    }

    /// 用店名搜尋特定餐廳（Search grounding），回傳結構化資料。
    pub async fn search_specific_restaurant_by_name(
        &self,
        restaurant_name: &str,
    ) -> anyhow::Result<Restaurant> {
        let prompt = prompts::fill(
            prompts::SPECIFIC_RESTAURANT,
            &[("restaurant_name", restaurant_name)],
        );
        let req = GenerateContentRequest::text(prompt).with_tools(vec![tools::google_search()]);
        let text = self.text_call(req, FLASH_LITE).await?.unwrap_or_default();
        Ok(serde_json::from_value(json::extract_object(&text)?)?)
    }

    /// `@g`：摘要 kkdai/bookmarks 最近的 issues（至少 6 筆，最多往回 30 天）。
    pub async fn summarize_github_issues(&self) -> String {
        let mut issues = Vec::new();
        let mut past_days = 1;
        while issues.len() <= 5 && past_days < 30 {
            let since = (chrono::Utc::now() - chrono::Duration::days(past_days))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            issues = self.fetch_github_issues(&since).await;
            tracing::info!("GitHub issues: {} (past {past_days} days)", issues.len());
            past_days += 1;
        }
        if issues.is_empty() {
            return "最近沒有新的 GitHub issues".into();
        }
        let issues_text = format_issues(&issues);
        let prompt = prompts::fill(prompts::GITHUB_SUMMARY, &[("issues_text", &issues_text)]);
        let req = GenerateContentRequest::text(prompt).with_config(GenerationConfig {
            temperature: Some(0.0),
            max_output_tokens: Some(2048),
            ..Default::default()
        });
        match self.text_call(req, FLASH_LITE).await {
            Ok(text) => text.unwrap_or_else(|| "無法生成摘要".into()),
            Err(e) => format!("摘要 GitHub issues 時發生錯誤: {e}"),
        }
    }

    async fn fetch_github_issues(&self, since: &str) -> Vec<Value> {
        let mut req = self
            .http
            .get(format!("{}/repos/{GITHUB_REPO}/issues", self.github_base))
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "linebot-helper-rust")
            .query(&[("state", "all"), ("per_page", "100"), ("since", since)])
            .timeout(Duration::from_secs(30));
        if let Some(token) = &self.github_token {
            req = req.header("Authorization", format!("token {token}"));
        }
        let result: anyhow::Result<Vec<Value>> =
            async { Ok(req.send().await?.error_for_status()?.json().await?) }.await;
        match result {
            // issues API 也會回 PR，過濾掉
            Ok(items) => items
                .into_iter()
                .filter(|i| i.get("pull_request").is_none())
                .collect(),
            Err(e) => {
                tracing::error!("Error fetching GitHub issues: {e}");
                Vec::new()
            }
        }
    }
}

fn format_issues(issues: &[Value]) -> String {
    issues
        .iter()
        .map(|issue| {
            let labels: Vec<&str> = issue["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str())
                .collect();
            format!(
                "\n標題: {}\nURL: {}\n標籤: {}\n內容: {}...\n---\n",
                issue["title"].as_str().unwrap_or("No title"),
                issue["html_url"].as_str().unwrap_or_default(),
                if labels.is_empty() {
                    "None".to_string()
                } else {
                    labels.join(", ")
                },
                truncate(issue["body"].as_str().unwrap_or("No description"), 500),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl VideoSummarizer for Ai {
    async fn summarize_youtube(&self, url: &str, mode: &str) -> anyhow::Result<String> {
        Ai::summarize_youtube(self, url, mode).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ai(server: &MockServer) -> Ai {
        let http = reqwest::Client::new();
        Ai::new(
            GeminiClient::with_base_url(http.clone(), &server.uri(), true),
            GeminiClient::with_base_url(http.clone(), &server.uri(), false),
            http,
            None,
        )
        .with_github_base(&server.uri())
    }

    fn reply(text: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"parts": [{"text": text}]}}]
        }))
    }

    #[tokio::test]
    async fn social_posts_parse_structured_json() {
        let server = MockServer::start().await;
        let posts = json!({"title": "T", "summary_analysis": "S", "facebook": "F", "linkedin": "L", "threads": "H"});
        Mock::given(method("POST"))
            .and(body_string_contains("responseSchema"))
            .respond_with(reply(&posts.to_string()))
            .mount(&server)
            .await;
        let result = ai(&server).generate_social_media_posts("文章內容").await;
        assert_eq!(result.facebook, "F");
        assert_eq!(result.summary_analysis, "S");
    }

    #[tokio::test]
    async fn social_posts_fall_back_on_error_and_empty_input() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let ai = ai(&server);
        let failed = ai.generate_social_media_posts("文章").await;
        assert_eq!(failed.title, "摘要生成失敗");
        let empty = ai.generate_social_media_posts("   ").await;
        for v in [
            &empty.title,
            &empty.summary_analysis,
            &empty.facebook,
            &empty.linkedin,
            &empty.threads,
        ] {
            assert!(v.contains("無法"));
        }
        let bm = ai.summarize_for_bookmark("").await;
        assert!(!bm.title.is_empty() && !bm.summary.is_empty());
    }

    #[tokio::test]
    async fn research_report_retries_without_grounding() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("googleSearch"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(reply("## 執行摘要"))
            .mount(&server)
            .await;
        let report = ai(&server)
            .generate_research_report("內容", "https://x")
            .await
            .unwrap();
        assert_eq!(report.markdown, "## 執行摘要");
        assert!(report.sources.is_empty());
    }

    #[tokio::test]
    async fn nearby_restaurants_extract_names() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("googleMaps"))
            .respond_with(reply("1. 鼎泰豐\n2. 欣葉\n3. 阜杭豆漿\n4. 其他"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(reply(
                r#"```json
["鼎泰豐", "欣葉", "阜杭豆漿", "其他"]
```"#,
            ))
            .mount(&server)
            .await;
        let result = ai(&server)
            .search_nearby_places(25.0, 121.5, "restaurant", None)
            .await
            .unwrap();
        assert!(result.places.starts_with("🍴 附近的餐廳："));
        assert_eq!(result.restaurant_names, vec!["鼎泰豐", "欣葉", "阜杭豆漿"]);
    }

    #[tokio::test]
    async fn specific_restaurant_accepts_numeric_rating() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(reply(r#"結果：{"name": "鼎泰豐", "address": "台北", "rating": 4.6, "reviews": ["好吃"]}"#))
            .mount(&server)
            .await;
        let r = ai(&server)
            .search_specific_restaurant_by_name("鼎泰豐")
            .await
            .unwrap();
        assert_eq!(r.rating, "4.6");
        assert_eq!(r.reviews, vec!["好吃"]);
    }

    #[tokio::test]
    async fn agentic_vision_collects_text_code_output_and_images() {
        let server = MockServer::start().await;
        let img = Part::inline(b"PNGDATA", "image/png");
        Mock::given(method("POST"))
            .and(path("/models/gemini-3-flash-preview:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [
                    {"text": "hmm", "thought": true},
                    {"text": "有 3 個人"},
                    {"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "3"}},
                    serde_json::to_value(&img).unwrap()
                ]}}]
            })))
            .mount(&server)
            .await;
        let result = ai(&server)
            .analyze_image_agentic(b"\xFF\xD8\xFFjpeg", Some("數人數"))
            .await
            .unwrap();
        assert_eq!(result.analysis, "有 3 個人\n[Code Output]: 3");
        assert_eq!(result.images, vec![b"PNGDATA".to_vec()]);
    }

    #[tokio::test]
    async fn github_summary_filters_pull_requests() {
        let server = MockServer::start().await;
        let issues: Vec<Value> = (0..6)
            .map(|i| json!({"title": format!("issue{i}"), "html_url": "u", "body": "b", "labels": [{"name": "ai"}]}))
            .chain([json!({"title": "pr", "pull_request": {}})])
            .collect();
        Mock::given(method("GET"))
            .and(path("/repos/kkdai/bookmarks/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issues))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issue5"))
            .respond_with(reply("昨天學到了很多"))
            .mount(&server)
            .await;
        assert_eq!(
            ai(&server).summarize_github_issues().await,
            "昨天學到了很多"
        );
    }

    #[test]
    fn sniffs_common_image_types() {
        assert_eq!(sniff_image_mime(b"\xFF\xD8\xFF\xE0"), "image/jpeg");
        assert_eq!(sniff_image_mime(b"\x89PNG"), "image/png");
    }
}
