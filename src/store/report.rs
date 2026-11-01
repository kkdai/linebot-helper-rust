//! 研究報告的永久保存（Firestore `reports` collection）。

use std::sync::Arc;

use serde_json::json;

use super::{KvStore, now_ts};

pub const REPORTS_COLLECTION: &str = "reports";

#[derive(Clone)]
pub struct ReportStore {
    store: Arc<dyn KvStore>,
}

impl ReportStore {
    pub fn new(store: Arc<dyn KvStore>) -> Self {
        Self { store }
    }

    pub async fn put(&self, html: &str) -> String {
        let report_id = uuid::Uuid::new_v4().simple().to_string();
        self.store
            .save(&report_id, &json!({ "html": html, "created_at": now_ts() }))
            .await;
        report_id
    }

    pub async fn get(&self, report_id: &str) -> Option<String> {
        self.store
            .load(report_id)
            .await?
            .get("html")?
            .as_str()
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryKv;

    #[tokio::test]
    async fn put_then_get_returns_html() {
        let store = ReportStore::new(Arc::new(MemoryKv::new()));
        let id = store.put("<h1>hi</h1>").await;
        assert_eq!(id.len(), 32);
        assert_eq!(store.get(&id).await.as_deref(), Some("<h1>hi</h1>"));
    }

    #[tokio::test]
    async fn missing_report_returns_none() {
        let store = ReportStore::new(Arc::new(MemoryKv::new()));
        assert_eq!(store.get("nope").await, None);
    }

    #[tokio::test]
    async fn unavailable_store_degrades() {
        let store = ReportStore::new(Arc::new(MemoryKv::unavailable()));
        let id = store.put("<p>x</p>").await;
        assert_eq!(store.get(&id).await, None);
    }
}
