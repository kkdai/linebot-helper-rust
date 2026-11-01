//! 對話 session 管理：記憶體 + Firestore（`chat_sessions`）持久化、TTL、背景清理。
//!
//! 與 Python 版不同的是 session 不保存 SDK 的 chat 物件，ChatAgent 每次都從
//! history 重建 contents，因此「從 Firestore 還原」只需要還原 history。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use super::{KvStore, now_ts};

pub const SESSION_COLLECTION: &str = "chat_sessions";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    /// epoch 秒（與 Python 版 `_serialize_history` 相同）
    #[serde(default)]
    pub timestamp: f64,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: String,
    pub history: Vec<HistoryMessage>,
    pub created_at: f64,
    pub last_active: f64,
    /// 不持久化（與 Python 版一致），例如 last_location
    pub metadata: Map<String, Value>,
}

impl Session {
    fn new(user_id: &str) -> Self {
        let now = now_ts();
        Self {
            user_id: user_id.into(),
            history: Vec::new(),
            created_at: now,
            last_active: now,
            metadata: Map::new(),
        }
    }

    fn to_doc(&self) -> Value {
        json!({
            "user_id": self.user_id,
            "history": self.history,
            "created_at": self.created_at,
            "last_active": self.last_active,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub history_count: usize,
    pub created_at: f64,
    pub last_active: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionStats {
    pub active_sessions: usize,
    pub total_messages: usize,
    pub oldest_session_age_minutes: f64,
    pub cleanup_runs: u64,
    pub sessions_cleaned: u64,
}

pub struct SessionManager {
    timeout_secs: f64,
    max_history_length: usize,
    store: Arc<dyn KvStore>,
    sessions: Mutex<HashMap<String, Session>>,
    cleanup_runs: AtomicU64,
    sessions_cleaned: AtomicU64,
}

impl SessionManager {
    pub fn new(store: Arc<dyn KvStore>, timeout_minutes: i64, max_history_length: usize) -> Self {
        tracing::info!(
            "SessionManager initialized: timeout={timeout_minutes}min, max_history={max_history_length}"
        );
        Self {
            timeout_secs: timeout_minutes as f64 * 60.0,
            max_history_length,
            store,
            sessions: Mutex::default(),
            cleanup_runs: AtomicU64::new(0),
            sessions_cleaned: AtomicU64::new(0),
        }
    }

    fn is_expired(&self, session: &Session) -> bool {
        now_ts() - session.last_active >= self.timeout_secs
    }

    async fn persist(&self, session: &Session) {
        self.store.save(&session.user_id, &session.to_doc()).await;
    }

    /// 取得未過期的 session（不更新活動時間、不從 store 還原）。
    pub async fn get_session(&self, user_id: &str) -> Option<Session> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(user_id)
            .filter(|s| !self.is_expired(s))
            .cloned()
    }

    /// 從持久化 store 還原（instance 重啟後對話記憶不遺失）。
    async fn restore(&self, user_id: &str) -> Option<Session> {
        let stored = self.store.load(user_id).await?;
        let last_active = stored["last_active"].as_f64().unwrap_or(0.0);
        if now_ts() - last_active >= self.timeout_secs {
            self.store.delete(user_id).await;
            return None;
        }
        let history: Vec<HistoryMessage> =
            serde_json::from_value(stored["history"].clone()).unwrap_or_default();
        tracing::info!(
            "Restored session for user {user_id} from persistent store ({} messages)",
            history.len()
        );
        Some(Session {
            user_id: user_id.into(),
            history,
            created_at: stored["created_at"].as_f64().unwrap_or_else(now_ts),
            last_active: now_ts(),
            metadata: Map::new(),
        })
    }

    pub async fn get_or_create_session(&self, user_id: &str) -> Session {
        {
            let mut sessions = self.sessions.lock().await;
            if let Some(s) = sessions.get_mut(user_id) {
                if !self.is_expired(s) {
                    s.last_active = now_ts();
                    return s.clone();
                }
                tracing::info!("Session expired for user {user_id}");
                sessions.remove(user_id);
            }
        }

        let session = match self.restore(user_id).await {
            Some(s) => s,
            None => {
                tracing::info!("Creating new session for user {user_id}");
                Session::new(user_id)
            }
        };
        self.persist(&session).await;
        self.sessions
            .lock()
            .await
            .entry(user_id.to_string())
            .or_insert(session)
            .clone()
    }

    pub async fn add_to_history(&self, user_id: &str, role: &str, content: &str) -> bool {
        let snapshot = {
            let mut sessions = self.sessions.lock().await;
            let Some(s) = sessions.get_mut(user_id) else {
                return false;
            };
            s.history.push(HistoryMessage {
                role: role.into(),
                content: content.into(),
                timestamp: now_ts(),
            });
            if s.history.len() > self.max_history_length {
                let excess = s.history.len() - self.max_history_length;
                s.history.drain(..excess);
            }
            s.clone()
        };
        self.persist(&snapshot).await;
        true
    }

    pub async fn set_metadata(&self, user_id: &str, key: &str, value: Value) -> bool {
        let mut sessions = self.sessions.lock().await;
        match sessions.get_mut(user_id) {
            Some(s) => {
                s.metadata.insert(key.into(), value);
                true
            }
            None => false,
        }
    }

    pub async fn clear_session(&self, user_id: &str) -> bool {
        self.store.delete(user_id).await;
        let removed = self.sessions.lock().await.remove(user_id).is_some();
        if removed {
            tracing::info!("Cleared session for user {user_id}");
        }
        removed
    }

    pub async fn get_session_info(&self, user_id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.lock().await;
        sessions.get(user_id).map(|s| SessionInfo {
            history_count: s.history.len(),
            created_at: s.created_at,
            last_active: s.last_active,
        })
    }

    pub async fn cleanup_expired_sessions(&self) -> usize {
        let expired: Vec<String> = {
            let mut sessions = self.sessions.lock().await;
            let expired: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| self.is_expired(s))
                .map(|(k, _)| k.clone())
                .collect();
            for k in &expired {
                sessions.remove(k);
            }
            expired
        };
        for user_id in &expired {
            self.store.delete(user_id).await;
        }
        if !expired.is_empty() {
            tracing::info!("Cleaned up {} expired sessions", expired.len());
            self.sessions_cleaned
                .fetch_add(expired.len() as u64, Ordering::Relaxed);
        }
        self.cleanup_runs.fetch_add(1, Ordering::Relaxed);
        expired.len()
    }

    pub async fn get_stats(&self) -> SessionStats {
        let sessions = self.sessions.lock().await;
        let oldest = sessions
            .values()
            .map(|s| s.created_at)
            .fold(f64::INFINITY, f64::min);
        SessionStats {
            active_sessions: sessions.len(),
            total_messages: sessions.values().map(|s| s.history.len()).sum(),
            oldest_session_age_minutes: if sessions.is_empty() {
                0.0
            } else {
                (now_ts() - oldest) / 60.0
            },
            cleanup_runs: self.cleanup_runs.load(Ordering::Relaxed),
            sessions_cleaned: self.sessions_cleaned.load(Ordering::Relaxed),
        }
    }

    /// 背景清理任務，每 interval 跑一次。
    pub fn spawn_cleanup_task(self: &Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let cleaned = manager.cleanup_expired_sessions().await;
                tracing::debug!("Cleanup run completed: {cleaned} sessions removed");
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryKv;

    fn manager(store: Arc<MemoryKv>, max_history: usize) -> SessionManager {
        SessionManager::new(store, 30, max_history)
    }

    #[tokio::test]
    async fn history_is_trimmed_and_persisted() {
        let store = Arc::new(MemoryKv::new());
        let m = manager(store.clone(), 3);
        m.get_or_create_session("U1").await;
        for i in 0..5 {
            assert!(m.add_to_history("U1", "user", &format!("m{i}")).await);
        }
        let s = m.get_session("U1").await.unwrap();
        let contents: Vec<_> = s.history.iter().map(|h| h.content.as_str()).collect();
        assert_eq!(contents, vec!["m2", "m3", "m4"]);
        let doc = store.load("U1").await.unwrap();
        assert_eq!(doc["history"].as_array().unwrap().len(), 3);
        assert!(doc["created_at"].is_f64());
    }

    #[tokio::test]
    async fn session_restored_after_restart() {
        let store = Arc::new(MemoryKv::new());
        let first = manager(store.clone(), 20);
        first.get_or_create_session("U1").await;
        first.add_to_history("U1", "user", "你好").await;
        first.add_to_history("U1", "assistant", "嗨").await;

        // 模擬 instance 重啟：新的 manager、同一個 store
        let second = manager(store.clone(), 20);
        assert!(second.get_session("U1").await.is_none());
        let s = second.get_or_create_session("U1").await;
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[1].content, "嗨");
    }

    #[tokio::test]
    async fn expired_stored_session_is_not_restored() {
        let store = Arc::new(MemoryKv::new());
        store
            .save(
                "U1",
                &json!({"user_id": "U1", "history": [{"role": "user", "content": "old", "timestamp": 1.0}],
                        "created_at": 1.0, "last_active": now_ts() - 31.0 * 60.0}),
            )
            .await;
        let m = manager(store.clone(), 20);
        let s = m.get_or_create_session("U1").await;
        assert!(s.history.is_empty());
    }

    #[tokio::test]
    async fn restore_accepts_python_written_documents() {
        // Python 版寫入的 timestamp 可能是整數（Firestore integerValue）
        let store = Arc::new(MemoryKv::new());
        store
            .save(
                "U1",
                &json!({"user_id": "U1", "history": [{"role": "user", "content": "hi", "timestamp": 1700000000}],
                        "created_at": 1700000000, "last_active": now_ts()}),
            )
            .await;
        let s = manager(store, 20).get_or_create_session("U1").await;
        assert_eq!(s.history[0].timestamp, 1_700_000_000.0);
    }

    #[tokio::test]
    async fn clear_session_removes_memory_and_store() {
        let store = Arc::new(MemoryKv::new());
        let m = manager(store.clone(), 20);
        m.get_or_create_session("U1").await;
        assert!(m.clear_session("U1").await);
        assert!(!m.clear_session("U1").await);
        assert!(store.load("U1").await.is_none());
    }

    #[tokio::test]
    async fn metadata_and_stats() {
        let m = manager(Arc::new(MemoryKv::new()), 20);
        assert!(!m.set_metadata("U1", "last_location", json!({})).await);
        m.get_or_create_session("U1").await;
        assert!(
            m.set_metadata("U1", "last_location", json!({"latitude": 25.0}))
                .await
        );
        m.add_to_history("U1", "user", "x").await;
        let s = m.get_session("U1").await.unwrap();
        assert_eq!(s.metadata["last_location"]["latitude"], 25.0);
        let stats = m.get_stats().await;
        assert_eq!(stats.active_sessions, 1);
        assert_eq!(stats.total_messages, 1);
        assert_eq!(m.cleanup_expired_sessions().await, 0);
        assert_eq!(m.get_stats().await.cleanup_runs, 1);
    }
}
