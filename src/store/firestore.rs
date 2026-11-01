//! Firestore key-value store（REST API）。
//!
//! Cloud Run instance 隨時可能被回收，任何需要跨請求存活的狀態都存這裡。
//! 文件格式與 Python 版（google-cloud-firestore）相同，新舊版可共用資料。
//!
//! 取不到 GCP 認證時 `is_available() == false`，所有操作變 no-op（與 Python 版一致），
//! 本機開發與 CI 不需要 Firestore 也能跑。

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::KvStore;
use crate::gcp::GcpAuth;

const FIRESTORE_BASE: &str = "https://firestore.googleapis.com/v1";

/// Firestore 文件 ID 不允許 '/'（例如 batch job 的 "batches/xxx"）。
pub fn sanitize_key(key: &str) -> String {
    key.replace('/', "__")
}

#[derive(Clone)]
pub struct FirestoreKv {
    http: reqwest::Client,
    auth: GcpAuth,
    collection_url: String,
    collection: String,
    available: bool,
}

impl FirestoreKv {
    pub fn new(
        http: reqwest::Client,
        auth: GcpAuth,
        project: &str,
        collection: &str,
        available: bool,
    ) -> Self {
        if available {
            tracing::info!("FirestoreKv ready: collection={collection}");
        } else {
            tracing::warn!(
                "Firestore unavailable, persistence disabled for collection={collection}"
            );
        }
        Self {
            http,
            auth,
            collection_url: format!(
                "{FIRESTORE_BASE}/projects/{project}/databases/(default)/documents/{collection}"
            ),
            collection: collection.to_string(),
            available,
        }
    }

    fn doc_url(&self, key: &str) -> String {
        format!("{}/{}", self.collection_url, sanitize_key(key))
    }

    async fn request(&self, req: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let token = self.auth.token().await?;
        Ok(req
            .bearer_auth(token)
            .timeout(Duration::from_secs(30))
            .send()
            .await?)
    }

    async fn try_save(&self, key: &str, doc: &Value) -> anyhow::Result<()> {
        // PATCH 不帶 updateMask = 整份文件覆寫（等同 Python 的 set()）
        let body = json!({ "fields": to_fields(doc) });
        let resp = self
            .request(self.http.patch(self.doc_url(key)).json(&body))
            .await?;
        ensure_ok(resp).await
    }

    async fn try_load(&self, key: &str) -> anyhow::Result<Option<Value>> {
        let resp = self.request(self.http.get(self.doc_url(key))).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = ensure_ok_resp(resp).await?;
        let doc: Value = resp.json().await?;
        Ok(Some(from_fields(doc.get("fields"))))
    }

    async fn try_delete(&self, key: &str) -> anyhow::Result<()> {
        let resp = self.request(self.http.delete(self.doc_url(key))).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        ensure_ok(resp).await
    }

    async fn try_load_all(&self) -> anyhow::Result<Vec<(String, Value)>> {
        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut req = self
                .http
                .get(&self.collection_url)
                .query(&[("pageSize", "300")]);
            if let Some(t) = &page_token {
                req = req.query(&[("pageToken", t)]);
            }
            let resp = ensure_ok_resp(self.request(req).await?).await?;
            let page: Value = resp.json().await?;
            for doc in page["documents"].as_array().into_iter().flatten() {
                let id = doc["name"]
                    .as_str()
                    .and_then(|n| n.rsplit('/').next())
                    .unwrap_or_default()
                    .to_string();
                out.push((id, from_fields(doc.get("fields"))));
            }
            match page["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => page_token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }
}

async fn ensure_ok_resp(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::bail!("Firestore HTTP {status}: {body:.300}")
}

async fn ensure_ok(resp: reqwest::Response) -> anyhow::Result<()> {
    ensure_ok_resp(resp).await.map(|_| ())
}

#[async_trait]
impl KvStore for FirestoreKv {
    fn is_available(&self) -> bool {
        self.available
    }

    async fn save(&self, key: &str, doc: &Value) {
        if !self.available {
            return;
        }
        if let Err(e) = self.try_save(key, doc).await {
            tracing::error!("Firestore save failed ({}/{key}): {e}", self.collection);
        }
    }

    async fn load(&self, key: &str) -> Option<Value> {
        if !self.available {
            return None;
        }
        self.try_load(key).await.unwrap_or_else(|e| {
            tracing::error!("Firestore load failed ({}/{key}): {e}", self.collection);
            None
        })
    }

    async fn delete(&self, key: &str) {
        if !self.available {
            return;
        }
        if let Err(e) = self.try_delete(key).await {
            tracing::error!("Firestore delete failed ({}/{key}): {e}", self.collection);
        }
    }

    async fn load_all(&self) -> Vec<(String, Value)> {
        if !self.available {
            return Vec::new();
        }
        self.try_load_all().await.unwrap_or_else(|e| {
            tracing::error!("Firestore load_all failed ({}): {e}", self.collection);
            Vec::new()
        })
    }
}

// ---------- JSON <-> Firestore Value ----------

fn to_value(v: &Value) -> Value {
    match v {
        Value::Null => json!({ "nullValue": null }),
        Value::Bool(b) => json!({ "booleanValue": b }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                // Firestore REST 的 int64 以字串表示
                json!({ "integerValue": i.to_string() })
            } else {
                json!({ "doubleValue": n.as_f64().unwrap_or(0.0) })
            }
        }
        Value::String(s) => json!({ "stringValue": s }),
        Value::Array(items) => {
            json!({ "arrayValue": { "values": items.iter().map(to_value).collect::<Vec<_>>() } })
        }
        Value::Object(_) => json!({ "mapValue": { "fields": to_fields(v) } }),
    }
}

pub fn to_fields(doc: &Value) -> Value {
    let fields: Map<String, Value> = doc
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), to_value(v))).collect())
        .unwrap_or_default();
    Value::Object(fields)
}

fn from_value(v: &Value) -> Value {
    let Some((kind, inner)) = v.as_object().and_then(|m| m.iter().next()) else {
        return Value::Null;
    };
    match kind.as_str() {
        "nullValue" => Value::Null,
        "booleanValue" => inner.clone(),
        "integerValue" => inner
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .map(Value::from)
            .unwrap_or_else(|| inner.clone()),
        "doubleValue" => inner.clone(),
        "stringValue" | "timestampValue" | "referenceValue" | "bytesValue" => inner.clone(),
        "arrayValue" => Value::Array(
            inner["values"]
                .as_array()
                .map(|vs| vs.iter().map(from_value).collect())
                .unwrap_or_default(),
        ),
        "mapValue" => from_fields(inner.get("fields")),
        "geoPointValue" => inner.clone(),
        _ => Value::Null,
    }
}

pub fn from_fields(fields: Option<&Value>) -> Value {
    let map: Map<String, Value> = fields
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), from_value(v))).collect())
        .unwrap_or_default();
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_slashes() {
        assert_eq!(sanitize_key("batches/abc"), "batches__abc");
    }

    #[test]
    fn value_roundtrip_preserves_types() {
        let doc = json!({
            "user_id": "U1",
            "created_at": 1730000000.5,
            "count": 3,
            "saved": true,
            "nothing": null,
            "history": [{"role": "user", "content": "hi", "timestamp": 1.25}],
            "meta": {"lat": 25.03}
        });
        let fields = to_fields(&doc);
        assert_eq!(fields["created_at"], json!({"doubleValue": 1730000000.5}));
        assert_eq!(fields["count"], json!({"integerValue": "3"}));
        assert_eq!(from_fields(Some(&fields)), doc);
    }
}
