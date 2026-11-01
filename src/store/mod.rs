//! 持久化：Firestore KV store 與建在上面的各個 service。

pub mod bookmark;
pub mod firestore;
pub mod memory;
pub mod report;
pub mod session;

use async_trait::async_trait;
use serde_json::Value;

pub use firestore::{FirestoreKv, sanitize_key};
pub use memory::MemoryKv;

/// 以 collection 為單位的 key-value 儲存。失敗只記 log，不往外丟（與 Python 版一致）。
#[async_trait]
pub trait KvStore: Send + Sync {
    fn is_available(&self) -> bool;
    async fn save(&self, key: &str, doc: &Value);
    async fn load(&self, key: &str) -> Option<Value>;
    async fn delete(&self, key: &str);
    async fn load_all(&self) -> Vec<(String, Value)>;
}

pub fn now_ts() -> f64 {
    chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0
}
