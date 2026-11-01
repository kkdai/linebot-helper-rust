//! 全域共用狀態。

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::agents::{ChatAgent, ChatSettings, Orchestrator};
use crate::ai::Ai;
use crate::batch::BatchService;
use crate::config::Config;
use crate::gcp::GcpAuth;
use crate::gemini::GeminiClient;
use crate::line::LineClient;
use crate::loader::UrlLoader;
use crate::store::bookmark::{BOOKMARKS_COLLECTION, BookmarkService};
use crate::store::report::{REPORTS_COLLECTION, ReportStore};
use crate::store::session::{SESSION_COLLECTION, SessionManager};
use crate::store::{FirestoreKv, KvStore, MemoryKv};

pub const ANNOTATED_IMAGE_TTL: Duration = Duration::from_secs(300);
pub const AUDIO_TTL: Duration = Duration::from_secs(300);
pub const SUMMARY_TTL: Duration = Duration::from_secs(600);
pub const VOICE_RESUME_TTL: Duration = Duration::from_secs(900);
pub const BATCH_JOBS_COLLECTION: &str = "batch_jobs";

/// 有效期限的記憶體暫存，寫入時順便清掉過期項目。
pub struct TtlStore<T: Clone> {
    items: DashMap<String, (T, Instant)>,
    ttl: Duration,
}

impl<T: Clone> TtlStore<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            items: DashMap::new(),
            ttl,
        }
    }

    /// 存入並回傳新的 uuid。
    pub fn insert(&self, value: T) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.insert_with_id(&id, value);
        id
    }

    pub fn insert_with_id(&self, id: &str, value: T) {
        let now = Instant::now();
        self.items
            .retain(|_, (_, created)| now.duration_since(*created) <= self.ttl);
        self.items.insert(id.to_string(), (value, now));
    }

    pub fn get(&self, id: &str) -> Option<T> {
        let expired = {
            let entry = self.items.get(id)?;
            if entry.1.elapsed() <= self.ttl {
                return Some(entry.0.clone());
            }
            true
        };
        if expired {
            self.items.remove(id);
        }
        None
    }

    pub fn remove(&self, id: &str) -> Option<T> {
        self.items.remove(id).map(|(_, (v, _))| v)
    }
}

pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
    pub line: LineClient,
    pub ai: Arc<Ai>,
    pub loader: Arc<UrlLoader>,
    pub orchestrator: Orchestrator,
    pub sessions: Arc<SessionManager>,
    pub bookmarks: BookmarkService,
    pub reports: ReportStore,
    pub batch: BatchService,
    /// 使用者剛傳的圖片，等 quick reply 選分析方式
    pub image_temp: DashMap<String, Vec<u8>>,
    /// 等待使用者輸入 Agentic Vision 指令
    pub pending_agentic_vision: DashMap<String, ()>,
    pub annotated_images: TtlStore<Vec<u8>>,
    pub audio: TtlStore<Vec<u8>>,
    pub summaries: TtlStore<String>,
    /// Gemini Live session resumption handle（user_id → handle）
    pub voice_resume_handles: TtlStore<String>,
    /// 從 webhook request 自動偵測的對外 base URL
    app_base_url: RwLock<String>,
}

/// 外部依賴的 endpoint，測試時換成 mock server。
pub struct Endpoints {
    pub line: Option<String>,
    pub gemini: Option<String>,
}

impl AppState {
    pub async fn new(config: Config) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .expect("reqwest client");
        let auth = GcpAuth::new();
        let gcp_available = auth.is_available().await;
        let kv = |collection: &str| -> Arc<dyn KvStore> {
            Arc::new(FirestoreKv::new(
                http.clone(),
                auth.clone(),
                &config.project_id,
                collection,
                gcp_available,
            ))
        };
        let vertex = GeminiClient::vertex(
            http.clone(),
            auth.clone(),
            &config.project_id,
            &config.location,
        );
        let developer = GeminiClient::developer(http.clone(), &config.google_ai_api_key);
        let line = LineClient::new(http.clone(), &config.channel_access_token);
        let stores = Stores {
            sessions: kv(SESSION_COLLECTION),
            bookmarks: kv(BOOKMARKS_COLLECTION),
            reports: kv(REPORTS_COLLECTION),
            batch_jobs: kv(BATCH_JOBS_COLLECTION),
        };
        let state = Self::assemble(config, http, line, vertex, developer, stores);
        state.sessions.spawn_cleanup_task(Duration::from_secs(300));
        state
    }

    /// 測試用：所有外部服務指向 mock server，持久化用記憶體。
    pub fn for_test(config: Config, endpoints: Endpoints) -> Arc<Self> {
        let http = reqwest::Client::new();
        let gemini_base = endpoints
            .gemini
            .unwrap_or_else(|| "http://127.0.0.1:9".into());
        let line_base = endpoints
            .line
            .unwrap_or_else(|| "http://127.0.0.1:9".into());
        let vertex = GeminiClient::with_base_url(http.clone(), &gemini_base, true);
        let developer = GeminiClient::with_base_url(http.clone(), &gemini_base, false);
        let line =
            LineClient::with_base_url(http.clone(), &config.channel_access_token, &line_base);
        let stores = Stores {
            sessions: Arc::new(MemoryKv::new()),
            bookmarks: Arc::new(MemoryKv::new()),
            reports: Arc::new(MemoryKv::new()),
            batch_jobs: Arc::new(MemoryKv::new()),
        };
        Self::assemble(config, http, line, vertex, developer, stores)
    }

    fn assemble(
        config: Config,
        http: reqwest::Client,
        line: LineClient,
        vertex: GeminiClient,
        developer: GeminiClient,
        stores: Stores,
    ) -> Arc<Self> {
        let ai = Arc::new(Ai::new(
            vertex.clone(),
            developer.clone(),
            http.clone(),
            config.github_token.clone(),
        ));
        let loader = Arc::new(
            UrlLoader::new(
                http.clone(),
                config.firecrawl_key.clone(),
                &config.singlefile_path,
                &config.chromium_path,
            )
            .with_video_summarizer(ai.clone()),
        );
        let sessions = Arc::new(SessionManager::new(
            stores.sessions,
            config.session_timeout_minutes,
            config.max_history_length,
        ));
        let chat = ChatAgent::new(
            vertex,
            sessions.clone(),
            ChatSettings {
                model: config.chat_model.clone(),
                temperature: config.temperature,
                max_output_tokens: config.max_output_tokens,
                enable_grounding: config.enable_grounding,
            },
        );
        let orchestrator = Orchestrator::new(
            chat,
            ai.clone(),
            loader.clone(),
            config.enable_maps_grounding,
        );
        Arc::new(Self {
            bookmarks: BookmarkService::new(stores.bookmarks),
            reports: ReportStore::new(stores.reports),
            batch: BatchService::new(developer, stores.batch_jobs),
            config,
            http,
            line,
            ai,
            loader,
            orchestrator,
            sessions,
            image_temp: DashMap::new(),
            pending_agentic_vision: DashMap::new(),
            annotated_images: TtlStore::new(ANNOTATED_IMAGE_TTL),
            audio: TtlStore::new(AUDIO_TTL),
            summaries: TtlStore::new(SUMMARY_TTL),
            voice_resume_handles: TtlStore::new(VOICE_RESUME_TTL),
            app_base_url: RwLock::new(String::new()),
        })
    }

    pub fn app_base_url(&self) -> Option<String> {
        let url = self.app_base_url.read().unwrap().clone();
        (!url.is_empty()).then_some(url)
    }

    /// 只在第一次偵測到時設定（與 Python 版相同）。
    pub fn detect_base_url(&self, proto: Option<&str>, host: Option<&str>) {
        if self.app_base_url().is_some() {
            return;
        }
        if let Some(host) = host.filter(|h| !h.is_empty()) {
            let url = format!("{}://{host}", proto.unwrap_or("https"));
            tracing::info!("App base URL detected: {url}");
            *self.app_base_url.write().unwrap() = url;
        }
    }

    /// Batch webhook 的網域：WEBHOOK_DOMAIN 優先，否則用偵測到的 base URL。
    pub fn webhook_domain(&self) -> Option<String> {
        self.config.webhook_domain.clone().or_else(|| {
            self.app_base_url()
                .and_then(|u| url::Url::parse(&u).ok())
                .and_then(|u| {
                    u.host_str().map(|h| match u.port() {
                        Some(p) => format!("{h}:{p}"),
                        None => h.to_string(),
                    })
                })
        })
    }
}

struct Stores {
    sessions: Arc<dyn KvStore>,
    bookmarks: Arc<dyn KvStore>,
    reports: Arc<dyn KvStore>,
    batch_jobs: Arc<dyn KvStore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_store_expires_entries() {
        let store = TtlStore::new(Duration::from_millis(50));
        let id = store.insert(1);
        assert_eq!(store.get(&id), Some(1));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(store.get(&id), None);
    }

    #[test]
    fn base_url_is_detected_once() {
        let state = AppState::for_test(
            Config::for_test(),
            Endpoints {
                line: None,
                gemini: None,
            },
        );
        assert_eq!(state.app_base_url(), None);
        state.detect_base_url(None, Some("bot.example.com"));
        state.detect_base_url(Some("http"), Some("other.example.com"));
        assert_eq!(
            state.app_base_url().as_deref(),
            Some("https://bot.example.com")
        );
        assert_eq!(state.webhook_domain().as_deref(), Some("bot.example.com"));
    }
}
