//! Gemini Batch API（Developer API）：美食深度分析的批次工作。
//!
//! JSONL → Files API（resumable upload）→ `models/{model}:batchGenerateContent`
//! （附 dynamic webhook）→ 完成後下載結果 JSONL。job → user 對應存 Firestore
//! （Cloud Run instance 回收後 batch job 還在跑）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ai::{FLASH_LITE, Restaurant, prompts};
use crate::gemini::GeminiClient;
use crate::gemini::client::check_status;
use crate::store::{KvStore, now_ts};

pub const DISPLAY_NAME_PREFIX: &str = "FoodieAnalysis_";

#[derive(Clone)]
pub struct BatchService {
    pub client: GeminiClient,
    store: Arc<dyn KvStore>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchJob {
    pub name: String,
    pub display_name: String,
    /// 正規化成 `JOB_STATE_*`（API 回 `BATCH_STATE_*`）
    pub state: String,
    pub responses_file: Option<String>,
}

impl BatchJob {
    fn from_operation(op: &Value) -> Self {
        let meta = &op["metadata"];
        let state = meta["state"]
            .as_str()
            .unwrap_or_default()
            .replace("BATCH_STATE_", "JOB_STATE_");
        Self {
            name: op["name"].as_str().unwrap_or_default().into(),
            display_name: meta["displayName"].as_str().unwrap_or_default().into(),
            state,
            responses_file: meta["output"]["responsesFile"].as_str().map(str::to_string),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.state == "JOB_STATE_SUCCEEDED"
    }

    pub fn failed(&self) -> bool {
        self.state == "JOB_STATE_FAILED"
    }

    pub fn finished(&self) -> bool {
        self.succeeded() || self.failed()
    }

    /// display name `FoodieAnalysis_{user_id}_{timestamp}` 裡的 user id。
    pub fn user_id(&self) -> Option<&str> {
        let rest = self.display_name.strip_prefix(DISPLAY_NAME_PREFIX)?;
        let (user, ts) = rest.rsplit_once('_')?;
        (!user.is_empty() && !ts.is_empty()).then_some(user)
    }

    /// 結果檔；API 沒回時沿用 Python 版的推測規則。
    pub fn output_file(&self) -> String {
        self.responses_file.clone().unwrap_or_else(|| {
            format!(
                "files/batch-{}",
                self.name.rsplit('/').next().unwrap_or_default()
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RestaurantAnalysis {
    pub index: usize,
    pub name: String,
    pub analysis: String,
}

/// 每家餐廳一行 request。
pub fn generate_restaurant_jsonl(restaurants: &[Restaurant]) -> String {
    restaurants
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let name = if r.name.is_empty() {
                "未知餐廳"
            } else {
                &r.name
            };
            let address = if r.address.is_empty() {
                "未知地址"
            } else {
                &r.address
            };
            let rating = if r.rating.is_empty() {
                "未知評分"
            } else {
                &r.rating
            };
            let reviews_text = r
                .reviews
                .iter()
                .take(15)
                .map(|rev| format!("- {rev}"))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = prompts::fill(
                prompts::RESTAURANT_BATCH,
                &[
                    ("name", name),
                    ("address", address),
                    ("rating", rating),
                    ("reviews_text", &reviews_text),
                ],
            );
            json!({
                "key": format!("{idx}:{name}"),
                "request": {
                    "contents": [{ "parts": [{ "text": prompt }] }],
                    "generation_config": { "temperature": 0.5, "max_output_tokens": 1024 },
                },
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析結果 JSONL（每行 `{"key": "0:店名", "response": {...}}`），依原始順序排列。
pub fn parse_batch_results(content: &str) -> Vec<RestaurantAnalysis> {
    let mut results: Vec<RestaurantAnalysis> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(obj) => Some(obj),
            Err(e) => {
                tracing::error!("Failed to parse line in batch result: {e}");
                None
            }
        })
        .map(|obj| {
            let key = obj["key"].as_str().unwrap_or_default();
            let (index, name) = match key.split_once(':') {
                Some((i, n)) if i.chars().all(|c| c.is_ascii_digit()) && !i.is_empty() => {
                    (i.parse().unwrap_or(0), n.to_string())
                }
                _ => (0, key.to_string()),
            };
            let analysis: String = obj["response"]["candidates"][0]["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["text"].as_str())
                .collect();
            RestaurantAnalysis {
                index,
                name,
                analysis: if analysis.is_empty() {
                    "（分析解析失敗）".into()
                } else {
                    analysis
                },
            }
        })
        .collect();
    results.sort_by_key(|r| r.index);
    results
}

impl BatchService {
    pub fn new(client: GeminiClient, store: Arc<dyn KvStore>) -> Self {
        Self { client, store }
    }

    pub async fn save_job_mapping(&self, batch_job_id: &str, user_id: &str, metadata: Value) {
        self.store
            .save(
                batch_job_id,
                &json!({ "user_id": user_id, "metadata": metadata, "created_at": now_ts() }),
            )
            .await;
        tracing::info!("Saved mapping for batch job {batch_job_id} -> user {user_id}");
    }

    pub async fn job_user(&self, batch_job_id: &str) -> Option<String> {
        self.store
            .load(batch_job_id)
            .await?
            .get("user_id")?
            .as_str()
            .map(str::to_string)
    }

    /// Files API resumable upload，回傳 `files/...`。
    pub async fn upload_jsonl(&self, content: &str, display_name: &str) -> anyhow::Result<String> {
        let root = self.client.developer_root();
        let start = self
            .client
            .authorize(
                self.client
                    .http()
                    .post(format!("{root}/upload/v1beta/files"))
                    .header("X-Goog-Upload-Protocol", "resumable")
                    .header("X-Goog-Upload-Command", "start")
                    .header("X-Goog-Upload-Header-Content-Length", content.len())
                    .header("X-Goog-Upload-Header-Content-Type", "application/jsonl")
                    .json(&json!({ "file": { "displayName": display_name, "mimeType": "application/jsonl" } })),
            )
            .await?;
        let resp = check_status(start.timeout(Duration::from_secs(60)).send().await?).await?;
        let upload_url = resp
            .headers()
            .get("x-goog-upload-url")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("Files API did not return an upload URL"))?
            .to_string();

        let upload = self
            .client
            .authorize(
                self.client
                    .http()
                    .post(upload_url)
                    .header("X-Goog-Upload-Offset", "0")
                    .header("X-Goog-Upload-Command", "upload, finalize")
                    .body(content.to_string()),
            )
            .await?;
        let resp = check_status(upload.timeout(Duration::from_secs(120)).send().await?).await?;
        let body: Value = resp.json().await?;
        body["file"]["name"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Files API upload response missing file name"))
    }

    /// 建立 JSONL、上傳並送出 batch job。`webhook_domain` 有值時設定 dynamic webhook。
    pub async fn submit_restaurant_job(
        &self,
        user_id: &str,
        latitude: f64,
        longitude: f64,
        restaurants: &[Restaurant],
        webhook_domain: Option<&str>,
    ) -> anyhow::Result<String> {
        if restaurants.is_empty() {
            anyhow::bail!("沒有提供餐廳資料");
        }
        let ts = chrono::Utc::now().timestamp();
        let jsonl = generate_restaurant_jsonl(restaurants);
        let file_name = self
            .upload_jsonl(&jsonl, &format!("restaurant_batch_{user_id}_{ts}"))
            .await?;
        tracing::info!("Uploaded JSONL to File API: {file_name}");

        let mut batch = json!({
            "displayName": format!("{DISPLAY_NAME_PREFIX}{user_id}_{ts}"),
            "inputConfig": { "fileName": file_name },
        });
        if let Some(domain) = webhook_domain {
            let uri = format!("https://{domain}/api/gemini-callback/dynamic");
            tracing::info!("Configuring dynamic webhook calling to {uri}");
            batch["webhookConfig"] = json!({
                "uris": [uri],
                "userMetadata": { "user_id": user_id, "lat": latitude, "lng": longitude },
            });
        }
        let url = format!(
            "{}/models/{FLASH_LITE}:batchGenerateContent",
            self.client.base()
        );
        let op: Value = self
            .client
            .post_json(&url, &json!({ "batch": batch }))
            .await?;
        let job_name = op["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("batch create response missing name"))?
            .to_string();

        self.save_job_mapping(
            &job_name,
            user_id,
            json!({
                "restaurants": restaurants.iter().map(|r| json!({"name": r.name, "address": r.address})).collect::<Vec<_>>(),
                "coordinates": { "latitude": latitude, "longitude": longitude },
                "file_resource": file_name,
            }),
        )
        .await;
        Ok(job_name)
    }

    pub async fn list_jobs(&self) -> anyhow::Result<Vec<BatchJob>> {
        let mut jobs = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!("{}/batches?pageSize=100", self.client.base());
            if let Some(t) = &page_token {
                url.push_str(&format!("&pageToken={t}"));
            }
            let page: Value = self.client.get_json(&url).await?;
            jobs.extend(
                page["operations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(BatchJob::from_operation),
            );
            match page["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => page_token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(jobs)
    }

    pub async fn get_job(&self, name: &str) -> anyhow::Result<BatchJob> {
        let op: Value = self
            .client
            .get_json(&format!("{}/{name}", self.client.base()))
            .await?;
        Ok(BatchJob::from_operation(&op))
    }

    pub async fn delete_job(&self, name: &str) -> anyhow::Result<()> {
        Ok(self
            .client
            .delete(&format!("{}/{name}", self.client.base()))
            .await?)
    }

    /// 該使用者是否有尚未結束的分析工作（避免重複提交）。
    pub async fn has_active_job(&self, user_id: &str) -> anyhow::Result<bool> {
        let prefix = format!("{DISPLAY_NAME_PREFIX}{user_id}_");
        Ok(self
            .list_jobs()
            .await?
            .iter()
            .any(|j| j.display_name.starts_with(&prefix) && !j.finished()))
    }

    pub async fn download_results(&self, file: &str) -> anyhow::Result<Vec<RestaurantAnalysis>> {
        tracing::info!("Downloading batch results from: {file}");
        let url = format!(
            "{}/download/v1beta/{file}:download?alt=media",
            self.client.developer_root()
        );
        let req = self.client.authorize(self.client.http().get(url)).await?;
        let resp = check_status(req.timeout(Duration::from_secs(120)).send().await?).await?;
        Ok(parse_batch_results(&resp.text().await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryKv;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn jsonl_has_one_request_per_restaurant() {
        let restaurants = vec![
            Restaurant {
                name: "鼎泰豐".into(),
                address: "台北".into(),
                rating: "4.6".into(),
                reviews: vec!["小籠包好吃".into()],
            },
            Restaurant::default(),
        ];
        let jsonl = generate_restaurant_jsonl(&restaurants);
        let lines: Vec<Value> = jsonl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["key"], "0:鼎泰豐");
        assert_eq!(lines[1]["key"], "1:未知餐廳");
        let prompt = lines[0]["request"]["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(prompt.contains("鼎泰豐") && prompt.contains("- 小籠包好吃"));
        assert_eq!(
            lines[0]["request"]["generation_config"]["max_output_tokens"],
            1024
        );
    }

    #[test]
    fn parses_results_in_index_order() {
        let content = [
            r#"{"key": "1:欣葉", "response": {"candidates": [{"content": {"parts": [{"text": "B"}]}}]}}"#,
            "not json",
            r#"{"key": "0:鼎泰豐", "response": {"candidates": [{"content": {"parts": [{"text": "A1"}, {"text": "A2"}]}}]}}"#,
            r#"{"key": "2:空", "response": {}}"#,
        ]
        .join("\n");
        let results = parse_batch_results(&content);
        assert_eq!(
            results,
            vec![
                RestaurantAnalysis {
                    index: 0,
                    name: "鼎泰豐".into(),
                    analysis: "A1A2".into()
                },
                RestaurantAnalysis {
                    index: 1,
                    name: "欣葉".into(),
                    analysis: "B".into()
                },
                RestaurantAnalysis {
                    index: 2,
                    name: "空".into(),
                    analysis: "（分析解析失敗）".into()
                },
            ]
        );
    }

    #[test]
    fn job_state_and_user_from_operation() {
        let job = BatchJob::from_operation(&json!({
            "name": "batches/123",
            "metadata": {"displayName": "FoodieAnalysis_Uabc_1700000000", "state": "BATCH_STATE_SUCCEEDED",
                         "output": {"responsesFile": "files/out"}}
        }));
        assert!(job.succeeded() && job.finished());
        assert_eq!(job.user_id(), Some("Uabc"));
        assert_eq!(job.output_file(), "files/out");
        let running = BatchJob::from_operation(
            &json!({"name": "batches/9", "metadata": {"state": "BATCH_STATE_RUNNING"}}),
        );
        assert!(!running.finished());
        assert_eq!(running.user_id(), None);
        assert_eq!(running.output_file(), "files/batch-9");
    }

    #[tokio::test]
    async fn submit_uploads_file_creates_job_and_saves_mapping() {
        let server = MockServer::start().await;
        let upload_url = format!("{}/resumable-upload", server.uri());
        Mock::given(method("POST"))
            .and(path("/upload/v1beta/files"))
            .and(header("x-goog-upload-command", "start"))
            .respond_with(
                ResponseTemplate::new(200).insert_header("x-goog-upload-url", upload_url.as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/resumable-upload"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"file": {"name": "files/f1"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1beta/models/{FLASH_LITE}:batchGenerateContent"
            )))
            .and(body_partial_json(json!({"batch": {
                "inputConfig": {"fileName": "files/f1"},
                "webhookConfig": {"uris": ["https://bot.example/api/gemini-callback/dynamic"],
                                  "userMetadata": {"user_id": "U1"}}
            }})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "batches/b1"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = GeminiClient::with_base_url(
            reqwest::Client::new(),
            &format!("{}/v1beta", server.uri()),
            false,
        );
        let svc = BatchService::new(client, Arc::new(MemoryKv::new()));
        let restaurants = vec![Restaurant {
            name: "A".into(),
            ..Default::default()
        }];
        let job = svc
            .submit_restaurant_job("U1", 25.0, 121.0, &restaurants, Some("bot.example"))
            .await
            .unwrap();
        assert_eq!(job, "batches/b1");
        assert_eq!(svc.job_user("batches/b1").await.as_deref(), Some("U1"));
    }
}
