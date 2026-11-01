pub mod agents;
pub mod ai;
pub mod app;
pub mod batch;
pub mod config;
pub mod gcp;
pub mod gemini;
pub mod handlers;
pub mod line;
pub mod loader;
pub mod report_page;
pub mod state;
pub mod store;
pub mod voice;

/// reqwest（aws-lc-rs）與 tokio-tungstenite（ring）都帶了 rustls 的 crypto provider，
/// 兩者並存時 rustls 無法自動選擇，必須在建立任何 TLS 連線前明確指定。
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
