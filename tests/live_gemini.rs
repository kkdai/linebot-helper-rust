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

fn ai() -> linebot_helper::ai::Ai {
    let http = reqwest::Client::new();
    let key = std::env::var("GOOGLE_AI_API_KEY").unwrap_or_default();
    linebot_helper::ai::Ai::new(
        vertex(),
        GeminiClient::developer(http.clone(), &key),
        http,
        None,
    )
}

#[tokio::test]
#[ignore]
async fn social_posts_structured_output() {
    let posts = ai()
        .generate_social_media_posts("Rust 1.99 發布，改善了編譯速度與 async 支援，社群反應熱烈。")
        .await;
    println!("{posts:#?}");
    assert_ne!(posts.title, "摘要生成失敗");
    assert!(!posts.threads.is_empty());
}

#[tokio::test]
#[ignore]
async fn maps_grounding_nearby_restaurants() {
    let r = ai()
        .search_nearby_places(25.0330, 121.5654, "restaurant", None)
        .await
        .unwrap();
    println!(
        "{}\n{:?}",
        r.places.chars().take(200).collect::<String>(),
        r.restaurant_names
    );
    assert!(r.places.starts_with("🍴"));
}

#[tokio::test]
#[ignore]
async fn youtube_summary() {
    let s = ai()
        .summarize_youtube("https://www.youtube.com/watch?v=jNQXAC9IVRw", "normal")
        .await
        .unwrap();
    println!("{s}");
    assert!(!s.is_empty());
}

#[tokio::test]
#[ignore]
async fn tts_produces_audio() {
    let (pcm, rate) = ai().synthesize_pcm("你好，這是測試。").await.unwrap();
    let ms = linebot_helper::ai::tts::pcm_duration_ms(pcm.len(), rate);
    println!("{} bytes PCM @ {rate} Hz, {ms} ms", pcm.len());
    assert!(ms > 300);
    // ffmpeg 轉檔只在有安裝 ffmpeg 時驗證（Docker image 內有）
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
    {
        let m4a = linebot_helper::ai::tts::pcm_to_m4a(&pcm, rate)
            .await
            .unwrap();
        assert_eq!(&m4a[4..8], b"ftyp");
    }
}
