//! 書籤／稍後讀服務（Firestore `bookmarks` collection）。
//!
//! 兩種寫入路徑：
//! - `/save` 指令 → `save_direct`（saved=true）
//! - 社群貼文 carousel 產生時 → `save_candidate`（saved=false），
//!   使用者按「儲存書籤」postback → `confirm_save` 翻成 saved=true
//!
//! doc id = sha1("{user_id}:{url}") 前 20 碼：同使用者同網址天然去重，
//! 且夠短可放進 postback data（上限 300 字元）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{KvStore, now_ts};

pub const BOOKMARKS_COLLECTION: &str = "bookmarks";
const DOC_ID_LENGTH: usize = 20;
const STALE_CANDIDATE_DAYS: f64 = 7.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Bookmark {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub saved: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_generated_at: Option<f64>,
    /// 不存進 Firestore，讀出時補上
    #[serde(skip)]
    pub doc_id: String,
}

#[derive(Clone)]
pub struct BookmarkService {
    store: Arc<dyn KvStore>,
}

impl BookmarkService {
    pub fn new(store: Arc<dyn KvStore>) -> Self {
        Self { store }
    }

    pub fn available(&self) -> bool {
        self.store.is_available()
    }

    pub fn make_doc_id(user_id: &str, url: &str) -> String {
        let digest = Sha1::digest(format!("{user_id}:{url}").as_bytes());
        hex::encode(digest)[..DOC_ID_LENGTH].to_string()
    }

    async fn load(&self, doc_id: &str) -> Option<Bookmark> {
        let doc = self.store.load(doc_id).await?;
        let mut bookmark: Bookmark = serde_json::from_value(doc).ok()?;
        bookmark.doc_id = doc_id.to_string();
        Some(bookmark)
    }

    async fn save(&self, bookmark: &Bookmark) {
        if let Ok(doc) = serde_json::to_value(bookmark) {
            self.store.save(&bookmark.doc_id, &doc).await;
        }
    }

    async fn write(
        &self,
        user_id: &str,
        url: &str,
        title: &str,
        summary: &str,
        saved: bool,
        source: &str,
    ) -> String {
        let bookmark = Bookmark {
            user_id: user_id.into(),
            url: url.into(),
            title: title.into(),
            summary: summary.into(),
            created_at: now_ts(),
            saved,
            source: source.into(),
            doc_id: Self::make_doc_id(user_id, url),
            ..Default::default()
        };
        self.save(&bookmark).await;
        bookmark.doc_id
    }

    /// `/save` 指令：直接存成已儲存書籤。
    pub async fn save_direct(
        &self,
        user_id: &str,
        url: &str,
        title: &str,
        summary: &str,
    ) -> String {
        self.write(user_id, url, title, summary, true, "command")
            .await
    }

    /// 社群貼文 carousel 產生時預寫的候選，等使用者按鈕確認。
    pub async fn save_candidate(
        &self,
        user_id: &str,
        url: &str,
        title: &str,
        summary: &str,
    ) -> String {
        self.cleanup_stale_candidates(user_id, STALE_CANDIDATE_DAYS)
            .await;
        self.write(user_id, url, title, summary, false, "button")
            .await
    }

    /// 把候選翻成已儲存。doc 不屬於該使用者時回 None。
    pub async fn confirm_save(&self, user_id: &str, doc_id: &str) -> Option<Bookmark> {
        let mut bookmark = self.get_bookmark(user_id, doc_id).await?;
        bookmark.saved = true;
        self.save(&bookmark).await;
        Some(bookmark)
    }

    /// 該使用者的書籤，created_at 新→舊。個人規模直接 load_all 後在記憶體過濾。
    async fn user_bookmarks(&self, user_id: &str, saved_only: bool) -> Vec<Bookmark> {
        let mut items: Vec<Bookmark> = self
            .store
            .load_all()
            .await
            .into_iter()
            .filter_map(|(id, doc)| {
                let mut b: Bookmark = serde_json::from_value(doc).ok()?;
                b.doc_id = id;
                Some(b)
            })
            .filter(|b| b.user_id == user_id && (!saved_only || b.saved))
            .collect();
        items.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
        items
    }

    pub async fn list_saved(&self, user_id: &str, limit: usize) -> Vec<Bookmark> {
        let mut items = self.user_bookmarks(user_id, true).await;
        items.truncate(limit);
        items
    }

    /// 對 title + summary 做不分大小寫子字串比對。
    pub async fn search(&self, user_id: &str, keyword: &str, limit: usize) -> Vec<Bookmark> {
        let needle = keyword.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        self.user_bookmarks(user_id, true)
            .await
            .into_iter()
            .filter(|b| {
                b.title.to_lowercase().contains(&needle)
                    || b.summary.to_lowercase().contains(&needle)
            })
            .take(limit)
            .collect()
    }

    /// 取單一書籤（含候選），驗證屬於該使用者。
    pub async fn get_bookmark(&self, user_id: &str, doc_id: &str) -> Option<Bookmark> {
        self.load(doc_id).await.filter(|b| b.user_id == user_id)
    }

    /// 記下最新研究報告 id 與產生時間；呼叫端已驗證過擁有者。
    pub async fn record_report(&self, doc_id: &str, report_id: &str) {
        if let Some(mut bookmark) = self.load(doc_id).await {
            bookmark.report_id = Some(report_id.into());
            bookmark.report_generated_at = Some(now_ts());
            self.save(&bookmark).await;
        }
    }

    pub async fn delete(&self, user_id: &str, doc_id: &str) -> bool {
        if self.get_bookmark(user_id, doc_id).await.is_none() {
            return false;
        }
        self.store.delete(doc_id).await;
        true
    }

    /// 刪除該使用者超過 max_age_days 未確認的候選（saved=false）。
    pub async fn cleanup_stale_candidates(&self, user_id: &str, max_age_days: f64) -> usize {
        let cutoff = now_ts() - max_age_days * 86400.0;
        let mut removed = 0;
        for b in self.user_bookmarks(user_id, false).await {
            if !b.saved && b.created_at <= cutoff {
                self.store.delete(&b.doc_id).await;
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!("Cleaned {removed} stale bookmark candidates for {user_id}");
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryKv;
    use serde_json::json;

    const UID: &str = "U_me";
    const OTHER: &str = "U_other";

    fn svc() -> (BookmarkService, Arc<MemoryKv>) {
        let store = Arc::new(MemoryKv::new());
        (BookmarkService::new(store.clone()), store)
    }

    #[tokio::test]
    async fn save_direct_creates_saved_bookmark() {
        let (svc, _) = svc();
        let id = svc
            .save_direct(UID, "https://example.com/a", "標題A", "摘要")
            .await;
        let items = svc.list_saved(UID, 10).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "標題A");
        assert!(items[0].saved);
        assert_eq!(items[0].source, "command");
        assert_eq!(items[0].doc_id, id);
    }

    #[tokio::test]
    async fn same_url_same_user_is_deduplicated() {
        let (svc, _) = svc();
        let a = svc.save_direct(UID, "https://x", "舊標題", "s").await;
        let b = svc.save_direct(UID, "https://x", "新標題", "s").await;
        assert_eq!(a, b);
        let items = svc.list_saved(UID, 10).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "新標題");
    }

    #[test]
    fn doc_id_matches_python_and_is_short() {
        let id = BookmarkService::make_doc_id(UID, "https://x");
        assert_eq!(id.len(), 20);
        assert_ne!(id, BookmarkService::make_doc_id(OTHER, "https://x"));
        // 與 Python hashlib.sha1("a:b").hexdigest()[:20] 相同
        assert_eq!(
            BookmarkService::make_doc_id("a", "b"),
            "dcea6d9ccd3d20ba1549"
        );
    }

    #[tokio::test]
    async fn candidate_not_listed_until_confirmed() {
        let (svc, _) = svc();
        let id = svc.save_candidate(UID, "https://b", "標題B", "s").await;
        assert!(svc.list_saved(UID, 10).await.is_empty());
        let confirmed = svc.confirm_save(UID, &id).await.unwrap();
        assert_eq!(confirmed.title, "標題B");
        let items = svc.list_saved(UID, 10).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, "button");
    }

    #[tokio::test]
    async fn confirm_rejects_other_users_and_unknown_ids() {
        let (svc, _) = svc();
        let id = svc.save_candidate(UID, "https://b", "t", "s").await;
        assert!(svc.confirm_save(OTHER, &id).await.is_none());
        assert!(svc.confirm_save(UID, "nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn stale_candidates_cleaned_but_saved_kept() {
        let (svc, store) = svc();
        let old_ts = now_ts() - 8.0 * 86400.0;
        for (id, saved) in [("old_candidate", false), ("old_saved", true)] {
            store
                .save(
                    id,
                    &json!({"user_id": UID, "url": id, "title": "t", "summary": "s",
                            "created_at": old_ts, "saved": saved, "source": "button"}),
                )
                .await;
        }
        svc.save_candidate(UID, "https://new", "t", "s").await;
        assert!(store.load("old_candidate").await.is_none());
        assert!(store.load("old_saved").await.is_some());
    }

    #[tokio::test]
    async fn list_orders_newest_first_and_limits() {
        let (svc, store) = svc();
        for i in 0..15 {
            store
                .save(
                    &format!("id{i}"),
                    &json!({"user_id": UID, "url": format!("u{i}"), "title": format!("標題{i}"),
                            "summary": "", "created_at": 1000.0 + i as f64, "saved": true,
                            "source": "command"}),
                )
                .await;
        }
        let items = svc.list_saved(UID, 10).await;
        assert_eq!(items.len(), 10);
        assert_eq!(items[0].title, "標題14");
        assert_eq!(items[9].title, "標題5");
    }

    #[tokio::test]
    async fn search_matches_title_and_summary_case_insensitive() {
        let (svc, _) = svc();
        svc.save_direct(UID, "https://example.com/1", "Python 教學", "x")
            .await;
        svc.save_direct(UID, "https://example.com/2", "其他", "學 PYTHON")
            .await;
        svc.save_direct(UID, "https://example.com/3", "Rust", "y")
            .await;
        svc.save_direct(OTHER, "https://example.com/4", "python", "z")
            .await;
        let mut urls: Vec<_> = svc
            .search(UID, "python", 10)
            .await
            .into_iter()
            .map(|b| b.url)
            .collect();
        urls.sort();
        assert_eq!(urls, vec!["https://example.com/1", "https://example.com/2"]);
        assert!(svc.search(UID, "不存在的詞", 10).await.is_empty());
    }

    #[tokio::test]
    async fn delete_only_own_bookmark() {
        let (svc, _) = svc();
        let id = svc.save_direct(UID, "https://d", "t", "s").await;
        assert!(!svc.delete(OTHER, &id).await);
        assert_eq!(svc.list_saved(UID, 10).await.len(), 1);
        assert!(svc.delete(UID, &id).await);
        assert!(svc.list_saved(UID, 10).await.is_empty());
    }

    #[tokio::test]
    async fn get_bookmark_checks_owner() {
        let (svc, _) = svc();
        let id = svc.save_candidate(UID, "https://r", "t", "s").await;
        assert_eq!(svc.get_bookmark(UID, &id).await.unwrap().doc_id, id);
        assert!(svc.get_bookmark(OTHER, &id).await.is_none());
        assert!(svc.get_bookmark(UID, "nope").await.is_none());
    }

    #[tokio::test]
    async fn record_report_overwrites_previous() {
        let (svc, _) = svc();
        let before = now_ts();
        let id = svc.save_candidate(UID, "https://r", "t", "s").await;
        svc.record_report(&id, "report-abc").await;
        svc.record_report(&id, "report-def").await;
        let b = svc.get_bookmark(UID, &id).await.unwrap();
        assert_eq!(b.report_id.as_deref(), Some("report-def"));
        assert!(b.report_generated_at.unwrap() >= before);
    }

    #[test]
    fn unavailable_store_reports_not_available() {
        assert!(!BookmarkService::new(Arc::new(MemoryKv::unavailable())).available());
    }
}
