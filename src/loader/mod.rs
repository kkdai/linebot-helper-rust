//! URL 內容載入：依網域選擇抓取方式，每種方式各自有時間上限，失敗就換下一種。
//!
//! 移植自 Python 版 `loader/url.py`。

pub mod document;
pub mod html;
pub mod text;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";
const FIRECRAWL_BASE: &str = "https://api.firecrawl.dev";

const HTTPX_DOMAINS: &[&str] = &[
    "https://ncode.syosetu.com",
    "https://pubmed.ncbi.nlm.nih.gov",
    "https://www.bnext.com.tw",
    "https://github.com",
    "https://www.twreporter.org",
    "https://telegra.ph",
    "https://www.jiqizhixin.com",
];
const CLOUDSCRAPER_DOMAINS: &[&str] = &["https://blog.tripplus.cc"];

/// YouTube 交給 Gemini 影片理解（實作在 ai 模組，避免 loader 依賴 ai）。
#[async_trait]
pub trait VideoSummarizer: Send + Sync {
    async fn summarize_youtube(&self, url: &str, mode: &str) -> anyhow::Result<String>;
}

#[derive(Debug, Clone)]
pub struct LoaderTimeouts {
    pub singlefile: Duration,
    pub httpx: Duration,
    pub cloudscraper: Duration,
    pub firecrawl: Duration,
    pub document: Duration,
    pub head: Duration,
}

impl Default for LoaderTimeouts {
    fn default() -> Self {
        Self {
            singlefile: Duration::from_secs(60), // 要啟動 headless Chromium，給最長
            httpx: Duration::from_secs(20),
            cloudscraper: Duration::from_secs(30),
            firecrawl: Duration::from_secs(45),
            document: Duration::from_secs(60), // 下載 + 轉換
            head: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Httpx,
    Cloudscraper,
    SingleFile,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Httpx => "httpx",
            Method::Cloudscraper => "cloudscraper",
            Method::SingleFile => "singlefile",
        }
    }
}

#[derive(Clone)]
pub struct UrlLoader {
    http: reqwest::Client,
    firecrawl_key: Option<String>,
    firecrawl_base: String,
    singlefile_path: String,
    chromium_path: String,
    timeouts: LoaderTimeouts,
    video: Option<Arc<dyn VideoSummarizer>>,
}

pub fn is_youtube_url(url: &str) -> bool {
    [
        "https://www.youtube.com",
        "https://youtu.be",
        "https://m.youtube.com",
        "https://youtube.com",
    ]
    .iter()
    .any(|p| url.starts_with(p))
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn is_ptt_url(url: &str) -> bool {
    url.starts_with("https://www.ptt.cc/bbs")
}

pub fn is_firecrawl_url(url: &str) -> bool {
    is_ptt_url(url)
        || url.starts_with("https://medium.com")
        || host(url).ends_with("medium.com")
        || url.starts_with("https://openai.com")
}

/// twitter.com / x.com → api.fxtwitter.com（取得可讀的推文 JSON）。
pub fn replace_domain(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    if matches!(parsed.host_str(), Some("twitter.com" | "x.com"))
        && parsed.set_host(Some("api.fxtwitter.com")).is_ok()
    {
        return parsed.to_string();
    }
    url.to_string()
}

impl UrlLoader {
    pub fn new(
        http: reqwest::Client,
        firecrawl_key: Option<String>,
        singlefile_path: &str,
        chromium_path: &str,
    ) -> Self {
        Self {
            http,
            firecrawl_key,
            firecrawl_base: FIRECRAWL_BASE.into(),
            singlefile_path: singlefile_path.into(),
            chromium_path: chromium_path.into(),
            timeouts: LoaderTimeouts::default(),
            video: None,
        }
    }

    pub fn with_video_summarizer(mut self, video: Arc<dyn VideoSummarizer>) -> Self {
        self.video = Some(video);
        self
    }

    pub fn with_timeouts(mut self, timeouts: LoaderTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn with_firecrawl_base(mut self, base: &str) -> Self {
        self.firecrawl_base = base.into();
        self
    }

    /// 載入 URL 內容（YouTube 回傳影片摘要），所有方式都失敗才回錯誤。
    pub async fn load(&self, url: &str) -> anyhow::Result<String> {
        self.load_with_mode(url, "normal").await
    }

    pub async fn load_with_mode(&self, url: &str, youtube_mode: &str) -> anyhow::Result<String> {
        let url = replace_domain(url);
        let url = url.as_str();

        if is_youtube_url(url) {
            let video = self
                .video
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("YouTube summarizer not configured"))?;
            return video.summarize_youtube(url, youtube_mode).await;
        }

        if is_firecrawl_url(url) {
            tracing::info!("Handling URL with Firecrawl priority: {url}");
            if self.firecrawl_key.is_some() {
                match tokio::time::timeout(self.timeouts.firecrawl, self.firecrawl(url)).await {
                    Ok(Ok(content)) => return Ok(content),
                    Ok(Err(e)) => tracing::warn!("Firecrawl failed, falling back: {e}"),
                    Err(_) => tracing::warn!("Firecrawl timed out, falling back"),
                }
            }
            use Method::*;
            let (methods, msg): (&[Method], _) = if is_ptt_url(url) {
                (
                    &[Cloudscraper, Httpx, SingleFile],
                    "無法從 PTT 讀取內容，請稍後再試",
                )
            } else if url.starts_with("https://openai.com") {
                (&[SingleFile, Httpx], "無法從 OpenAI 讀取內容，請稍後再試")
            } else {
                (
                    &[Httpx, Cloudscraper, SingleFile],
                    "無法從 Medium 讀取內容，請稍後再試",
                )
            };
            return self.fallback_chain(url, methods, msg).await;
        }

        if let Some(format) = self.detect_document_format(url).await {
            return match tokio::time::timeout(
                self.timeouts.document,
                self.load_document(url, format),
            )
            .await
            {
                Ok(Ok(content)) => Ok(content),
                Ok(Err(e)) => {
                    tracing::warn!("document conversion failed for {url} ({format}): {e}");
                    anyhow::bail!("無法讀取文件內容，請確認網址是否正確或稍後再試")
                }
                Err(_) => {
                    tracing::warn!("document loading timed out for {url} ({format})");
                    anyhow::bail!("無法讀取文件內容，請確認網址是否正確或稍後再試")
                }
            };
        }

        const GENERIC_ERROR: &str = "無法從網址讀取內容，請確認網址是否正確或稍後再試";
        use Method::*;
        let methods: &[Method] = if HTTPX_DOMAINS.iter().any(|d| url.starts_with(d)) {
            &[Httpx, SingleFile]
        } else if CLOUDSCRAPER_DOMAINS.iter().any(|d| url.starts_with(d)) {
            &[Cloudscraper, SingleFile]
        } else {
            tracing::info!("Using default fallback chain for: {url}");
            &[SingleFile, Httpx, Cloudscraper]
        };
        self.fallback_chain(url, methods, GENERIC_ERROR).await
    }

    async fn fallback_chain(
        &self,
        url: &str,
        methods: &[Method],
        error_message: &str,
    ) -> anyhow::Result<String> {
        let mut last_error = None;
        for &method in methods {
            tracing::info!("Trying {} for URL: {url}", method.name());
            let budget = match method {
                Method::Httpx => self.timeouts.httpx,
                Method::Cloudscraper => self.timeouts.cloudscraper,
                Method::SingleFile => self.timeouts.singlefile,
            };
            let fut = async {
                match method {
                    Method::Httpx => self.fetch_httpx(url).await,
                    Method::Cloudscraper => self.fetch_cloudscraper(url).await,
                    Method::SingleFile => self.fetch_singlefile(url).await,
                }
            };
            match tokio::time::timeout(budget, fut).await {
                Ok(Ok(content)) if !content.trim().is_empty() => return Ok(content),
                Ok(Ok(_)) => tracing::warn!("{} returned empty content for {url}", method.name()),
                Ok(Err(e)) => {
                    tracing::warn!("{} failed for {url}: {e}", method.name());
                    last_error = Some(e);
                }
                Err(_) => tracing::warn!("{} timed out after {budget:?} for {url}", method.name()),
            }
        }
        tracing::error!("All methods failed for URL: {url}");
        // 保留最後一個 HTTP 錯誤，讓呼叫端能給出 403／404 等具體訊息
        Err(match last_error {
            Some(e) if e.downcast_ref::<reqwest::Error>().is_some() => {
                e.context(error_message.to_string())
            }
            _ => anyhow::anyhow!(error_message.to_string()),
        })
    }

    async fn get_html(&self, url: &str, headers: &[(&str, &str)]) -> anyhow::Result<String> {
        let mut req = self.http.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send().await?.error_for_status()?;
        Ok(resp.text().await?)
    }

    pub async fn fetch_httpx(&self, url: &str) -> anyhow::Result<String> {
        tracing::info!("Loading HTML with httpx: {url}");
        let html = self
            .get_html(
                url,
                &[
                    (
                        "Accept-Language",
                        "zh-TW,zh;q=0.9,ja;q=0.8,en-US;q=0.7,en;q=0.6",
                    ),
                    ("User-Agent", BROWSER_UA),
                    ("Cookie", "over18=1"), // PTT 年齡確認
                ],
            )
            .await?;
        Ok(html::html_to_markdown(&html))
    }

    /// Python 版的 cloudscraper：Rust 沒有 Cloudflare JS challenge 解法，
    /// 這裡改送一組完整的瀏覽器 headers；真的遇到 challenge 時交給 SingleFile。
    pub async fn fetch_cloudscraper(&self, url: &str) -> anyhow::Result<String> {
        tracing::info!("Loading HTML with browser headers: {url}");
        let html = self
            .get_html(
                url,
                &[
                    ("User-Agent", BROWSER_UA),
                    (
                        "Accept",
                        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
                    ),
                    ("Accept-Language", "zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7"),
                    ("Cookie", "over18=1"),
                    ("Sec-Ch-Ua", r#""Chromium";v="129", "Not=A?Brand";v="8""#),
                    ("Sec-Ch-Ua-Mobile", "?0"),
                    ("Sec-Ch-Ua-Platform", r#""Windows""#),
                    ("Sec-Fetch-Dest", "document"),
                    ("Sec-Fetch-Mode", "navigate"),
                    ("Sec-Fetch-Site", "none"),
                    ("Sec-Fetch-User", "?1"),
                    ("Upgrade-Insecure-Requests", "1"),
                ],
            )
            .await?;
        if html.contains("cf-browser-verification") || html.contains("challenge-platform") {
            anyhow::bail!("Cloudflare challenge page");
        }
        Ok(html::html_to_markdown(&html))
    }

    /// SingleFile CLI + headless Chromium。逾時由外層 timeout 處理，
    /// `kill_on_drop` 確保被放棄時子行程一定被砍掉。
    pub async fn fetch_singlefile(&self, url: &str) -> anyhow::Result<String> {
        tracing::info!("Downloading HTML by SingleFile: {url}");
        let path: PathBuf =
            std::env::temp_dir().join(format!("{}.html", uuid::Uuid::new_v4().simple()));
        let _cleanup = TempFile(path.clone());

        let output = tokio::process::Command::new(&self.singlefile_path)
            .args([
                "--browser-executable-path",
                &self.chromium_path,
                "--filename-conflict-action",
                "overwrite",
                url,
            ])
            .arg(&path)
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!(
                "SingleFile exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        let html = tokio::fs::read_to_string(&path).await?;
        Ok(html::html_to_text(&html))
    }

    pub async fn firecrawl(&self, url: &str) -> anyhow::Result<String> {
        let key = self
            .firecrawl_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("firecrawl_key environment variable not set"))?;
        tracing::info!("Loading HTML with Firecrawl API: {url}");

        let mut headers = json!({
            "User-Agent": BROWSER_UA,
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8",
            "Accept-Language": "en-US,en;q=0.9",
        });
        let mut body = json!({
            "url": url,
            "formats": ["markdown"],
            "onlyMainContent": true,
            "removeBase64Images": true,
            "blockAds": true,
            "timeout": 30000,
        });
        if is_ptt_url(url) {
            headers["Cookie"] = "over18=1".into();
            body["actions"] = json!([{ "type": "wait", "selector": ".bbs-screen" }]);
        } else if host(url).ends_with("medium.com") {
            headers["Cookie"] = "uid=lo_5f5a79a81615; sid=1:zKvtbbPVwGuLiOQjwgkt".into();
        } else if url.starts_with("https://openai.com") {
            headers["Cookie"] = "cookieConsent=true; OptanonAlertBoxClosed=true".into();
        }
        body["headers"] = headers;

        let resp: Value = self
            .http
            .post(format!("{}/v1/scrape", self.firecrawl_base))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let data = &resp["data"];
        let content = if let Some(md) = data["markdown"].as_str().filter(|s| !s.is_empty()) {
            md.to_string()
        } else if let Some(h) = data["html"].as_str().filter(|s| !s.is_empty()) {
            html::html_to_markdown(h)
        } else {
            anyhow::bail!("Firecrawl API did not return expected content format");
        };
        let lower = content.to_lowercase();
        if lower.contains("enable javascript") || lower.contains("enable cookies") {
            anyhow::bail!("Firecrawl returned a JavaScript/cookie wall");
        }
        Ok(content)
    }

    /// 先看副檔名；認不出來才發 HEAD 看 content-type（PTT 對 HEAD 回 403，直接跳過）。
    pub async fn detect_document_format(&self, url: &str) -> Option<&'static str> {
        if let Some(f) = document::format_from_url(url) {
            return Some(f);
        }
        if is_ptt_url(url) {
            return None;
        }
        let resp = self
            .http
            .head(url)
            .header("User-Agent", BROWSER_UA)
            .timeout(self.timeouts.head)
            .send()
            .await
            .and_then(|r| r.error_for_status());
        match resp {
            Ok(r) => r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(document::format_from_content_type),
            Err(e) => {
                tracing::warn!("HEAD request failed checking document type: {e}");
                None
            }
        }
    }

    pub async fn load_document(&self, url: &str, format: &'static str) -> anyhow::Result<String> {
        tracing::info!("Loading document ({format}): {url}");
        let bytes = self
            .http
            .get(url)
            .header("User-Agent", BROWSER_UA)
            .header(
                "Accept-Language",
                "zh-TW,zh;q=0.9,ja;q=0.8,en-US;q=0.7,en;q=0.6",
            )
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let text = tokio::task::spawn_blocking(move || document::convert(&bytes, format)).await??;
        tracing::info!("Converted document to {} chars", text.chars().count());
        Ok(text)
    }
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn loader() -> UrlLoader {
        UrlLoader::new(
            reqwest::Client::new(),
            None,
            "/nonexistent/single-file",
            "/nonexistent/chromium",
        )
    }

    #[test]
    fn url_classification() {
        assert!(is_youtube_url("https://youtu.be/abc"));
        assert!(!is_youtube_url("https://example.com/youtube.com"));
        assert!(is_firecrawl_url(
            "https://www.ptt.cc/bbs/Gossiping/M.1.html"
        ));
        assert!(is_firecrawl_url("https://someone.medium.com/post"));
        assert!(!is_firecrawl_url("https://example.com"));
        assert_eq!(
            replace_domain("https://x.com/user/status/1"),
            "https://api.fxtwitter.com/user/status/1"
        );
        assert_eq!(
            replace_domain("https://example.com/a"),
            "https://example.com/a"
        );
    }

    #[tokio::test]
    async fn default_chain_falls_back_from_singlefile_to_httpx() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/html"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body><h1>Hi</h1><p>content</p></body></html>"),
            )
            .mount(&server)
            .await;
        let text = loader()
            .load(&format!("{}/article", server.uri()))
            .await
            .unwrap();
        assert!(text.contains("Hi") && text.contains("content"), "{text}");
    }

    #[tokio::test]
    async fn slow_method_times_out_and_chain_continues() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/html"))
            .mount(&server)
            .await;
        // httpx 與 cloudscraper 打同一個 endpoint：先慢後快，用 up_to_n_times 區分
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<p>slow</p>")
                    .set_delay(Duration::from_secs(5)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<p>fast</p>"))
            .mount(&server)
            .await;
        let timeouts = LoaderTimeouts {
            httpx: Duration::from_millis(300),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let text = loader()
            .with_timeouts(timeouts)
            .load(&format!("{}/page", server.uri()))
            .await
            .unwrap();
        assert!(text.contains("fast"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn all_methods_failing_returns_friendly_error() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let err = loader()
            .load(&format!("{}/blocked", server.uri()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("無法從網址讀取內容"));
        let status = err
            .chain()
            .find_map(|e| e.downcast_ref::<reqwest::Error>())
            .and_then(|e| e.status());
        assert_eq!(status.map(|s| s.as_u16()), Some(403));
    }

    #[tokio::test]
    async fn document_detected_by_content_type_is_converted() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/csv"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a,b\n1,2\n"))
            .mount(&server)
            .await;
        let text = loader()
            .load(&format!("{}/download.php?id=1", server.uri()))
            .await
            .unwrap();
        assert_eq!(text, "a,b\n1,2");
    }

    #[tokio::test]
    async fn firecrawl_is_used_for_priority_domains() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "data": {"markdown": "# PTT 文章"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let loader = UrlLoader::new(reqwest::Client::new(), Some("fc-key".into()), "x", "y")
            .with_firecrawl_base(&server.uri());
        let text = loader
            .load("https://www.ptt.cc/bbs/Tech_Job/M.123.html")
            .await
            .unwrap();
        assert_eq!(text, "# PTT 文章");
    }

    struct FakeVideo;
    #[async_trait]
    impl VideoSummarizer for FakeVideo {
        async fn summarize_youtube(&self, url: &str, mode: &str) -> anyhow::Result<String> {
            Ok(format!("{mode}:{url}"))
        }
    }

    #[tokio::test]
    async fn youtube_goes_to_video_summarizer() {
        let text = loader()
            .with_video_summarizer(Arc::new(FakeVideo))
            .load_with_mode("https://youtu.be/abc", "detail")
            .await
            .unwrap();
        assert_eq!(text, "detail:https://youtu.be/abc");
    }
}
