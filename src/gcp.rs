//! Google Cloud ADC 認證（Vertex AI 與 Firestore 共用）。
//!
//! provider 延遲到第一次需要 token 時才建立：本機沒有 ADC 也能啟動，
//! 只有實際呼叫 Google API 時才會失敗。

use std::sync::Arc;

use gcp_auth::TokenProvider;
use tokio::sync::OnceCell;

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Clone, Default)]
pub struct GcpAuth {
    provider: Arc<OnceCell<Arc<dyn TokenProvider>>>,
}

impl GcpAuth {
    pub fn new() -> Self {
        Self::default()
    }

    async fn provider(&self) -> anyhow::Result<&Arc<dyn TokenProvider>> {
        self.provider
            .get_or_try_init(|| async { gcp_auth::provider().await })
            .await
            .map_err(|e| anyhow::anyhow!("GCP credentials unavailable: {e}"))
    }

    pub async fn token(&self) -> anyhow::Result<String> {
        let token = self
            .provider()
            .await?
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|e| anyhow::anyhow!("failed to fetch GCP token: {e}"))?;
        Ok(token.as_str().to_string())
    }

    /// 能否取得認證（用來決定 Firestore 是否降級成 no-op）。
    pub async fn is_available(&self) -> bool {
        self.provider().await.is_ok()
    }
}
