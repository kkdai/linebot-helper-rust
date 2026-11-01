//! `cargo test --test live_firestore -- --ignored`（需 ADC 與 GOOGLE_CLOUD_PROJECT）

use linebot_helper::gcp::GcpAuth;
use linebot_helper::store::{FirestoreKv, KvStore};
use serde_json::json;

#[tokio::test]
#[ignore]
async fn firestore_roundtrip() {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT").unwrap();
    let auth = GcpAuth::new();
    assert!(auth.is_available().await);
    let kv = FirestoreKv::new(
        reqwest::Client::new(),
        auth,
        &project,
        "rust_port_selftest",
        true,
    );
    let doc = json!({"a": "中文", "n": 3, "f": 1.5, "arr": [true, null], "m": {"k": "v"}});
    kv.save("batches/x1", &doc).await;
    assert_eq!(kv.load("batches/x1").await, Some(doc));
    assert!(
        kv.load_all()
            .await
            .iter()
            .any(|(id, _)| id == "batches__x1")
    );
    kv.delete("batches/x1").await;
    assert_eq!(kv.load("batches/x1").await, None);
}
