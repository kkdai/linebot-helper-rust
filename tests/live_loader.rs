//! `cargo test --test live_loader -- --ignored --nocapture`（需要網路）

use linebot_helper::loader::UrlLoader;

fn loader() -> UrlLoader {
    UrlLoader::new(
        reqwest::Client::new(),
        None,
        "single-file",
        "/usr/bin/chromium",
    )
}

#[tokio::test]
#[ignore]
async fn loads_real_web_page() {
    let text = loader().load("https://www.rust-lang.org/").await.unwrap();
    println!("{}", text.chars().take(300).collect::<String>());
    assert!(text.contains("Rust"));
}

#[tokio::test]
#[ignore]
async fn loads_real_pdf() {
    let text = loader()
        .load("https://arxiv.org/pdf/1706.03762")
        .await
        .unwrap();
    println!("{}", text.chars().take(300).collect::<String>());
    assert!(text.contains("Attention"));
}
