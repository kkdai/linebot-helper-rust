//! 需要真實憑證的整合測試：`cargo test --test live_gemini -- --ignored`
//! （本機需有 gcloud ADC，並設定 GOOGLE_CLOUD_PROJECT）

use linebot_helper::gcp::GcpAuth;
use linebot_helper::gemini::{GeminiClient, GenerateContentRequest, tools};

fn vertex() -> GeminiClient {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT").expect("GOOGLE_CLOUD_PROJECT");
    GeminiClient::vertex(reqwest::Client::new(), GcpAuth::new(), &project, "global")
}

#[tokio::test]
#[ignore]
async fn vertex_generate_text() {
    let resp = vertex()
        .generate_content(
            "gemini-3.1-flash-lite",
            GenerateContentRequest::text("回答一個字：好"),
        )
        .await
        .unwrap();
    assert!(resp.text().is_some(), "{resp:?}");
}

#[tokio::test]
#[ignore]
async fn vertex_search_grounding_returns_sources() {
    let resp = vertex()
        .generate_content(
            "gemini-3.1-flash-lite",
            GenerateContentRequest::text("今天台北天氣如何？")
                .with_tools(vec![tools::google_search()]),
        )
        .await
        .unwrap();
    println!("{:?}", resp.grounding_sources());
    assert!(resp.text().is_some());
}
