//! Gemini Batch API 完成通知 webhooks。
//!
//! - static：Standard Webhooks HMAC-SHA256（`WEBHOOK_SIGNING_SECRET`，`whsec_` base64 key）
//! - dynamic：RS256 JWT，以 Google 的 JWKS 驗證，payload 就在 JWT 裡

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{Value, json};
use sha2::Sha256;

use super::{AppStateRef, detail, foodie};
use crate::gemini::types::B64;
use crate::state::AppState;

pub const JWKS_URI: &str = "https://generativelanguage.googleapis.com/.well-known/jwks.json";
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);

static JWKS_CACHE: Mutex<Option<(JwkSet, Instant)>> = Mutex::new(None);

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Standard Webhooks：`base64(HMAC(key, "{id}.{timestamp}.{payload}"))`，
/// signature header 可含多組以空白分隔的 `v1,<sig>`。
pub fn verify_static_webhook(
    payload: &str,
    webhook_id: &str,
    timestamp: &str,
    signature_header: &str,
    secret: &str,
) -> bool {
    let secret = secret.strip_prefix("whsec_").unwrap_or(secret);
    let Ok(key) = B64.decode(secret) else {
        tracing::error!("Failed to base64 decode signing secret");
        return false;
    };
    let to_sign = format!("{webhook_id}.{timestamp}.{payload}");
    signature_header.split(' ').any(|item| {
        let Some(("v1", sig)) = item.split_once(',') else {
            return false;
        };
        let Ok(sig) = B64.decode(sig) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&key) else {
            return false;
        };
        mac.update(to_sign.as_bytes());
        mac.verify_slice(&sig).is_ok()
    })
}

fn spawn_event(state: &Arc<AppState>, payload: &Value, user_id: Option<String>) {
    let event_type = payload["type"].as_str().unwrap_or_default().to_string();
    let data = &payload["data"];
    let batch_id = data["id"]
        .as_str()
        .or_else(|| data["name"].as_str())
        .map(str::to_string);
    tracing::info!("Received webhook event: {event_type}");
    let state = state.clone();
    match (event_type.as_str(), batch_id) {
        ("batch.succeeded", Some(id)) => {
            let Some(output) = data["output_file_uri"].as_str().map(str::to_string) else {
                return;
            };
            tokio::spawn(async move {
                foodie::process_batch_completed(&state, &id, &output, user_id).await;
            });
        }
        ("batch.failed", Some(id)) => {
            tracing::error!("Batch job failed: {id}");
            tokio::spawn(async move {
                foodie::process_batch_failed(&state, &id).await;
            });
        }
        _ => {}
    }
}

/// `POST /api/gemini-callback/static`
pub async fn static_webhook(
    State(state): State<AppStateRef>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = String::from_utf8_lossy(&body);
    match &state.config.webhook_signing_secret {
        Some(secret) => {
            let ok = match (
                header(&headers, "webhook-id"),
                header(&headers, "webhook-timestamp"),
                header(&headers, "webhook-signature"),
            ) {
                (Some(id), Some(ts), Some(sig)) => {
                    verify_static_webhook(&body, id, ts, sig, secret)
                }
                _ => false,
            };
            if !ok {
                tracing::error!("Static Webhook signature verification failed.");
                return detail(StatusCode::BAD_REQUEST, "Invalid signature");
            }
        }
        None => tracing::warn!(
            "WEBHOOK_SIGNING_SECRET is not configured. Signature validation skipped."
        ),
    }
    let Ok(payload) = serde_json::from_str::<Value>(&body) else {
        return detail(StatusCode::BAD_REQUEST, "Bad Request");
    };
    spawn_event(&state, &payload, None);
    Json(json!({ "status": "received" })).into_response()
}

async fn fetch_jwks(http: &reqwest::Client, force: bool) -> anyhow::Result<JwkSet> {
    if !force
        && let Some((set, at)) = JWKS_CACHE.lock().unwrap().as_ref()
        && at.elapsed() < JWKS_CACHE_TTL
    {
        return Ok(set.clone());
    }
    let set: JwkSet = http
        .get(JWKS_URI)
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    *JWKS_CACHE.lock().unwrap() = Some((set.clone(), Instant::now()));
    Ok(set)
}

pub fn verify_jwt_with_jwks(token: &str, jwks: &JwkSet) -> anyhow::Result<Value> {
    let header = jsonwebtoken::decode_header(token)?;
    let kid = header
        .kid
        .ok_or_else(|| anyhow::anyhow!("No kid found in JWT header"))?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| anyhow::anyhow!("JWK matching kid {kid} not found in JWKS"))?;
    let key = DecodingKey::from_jwk(jwk)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    Ok(jsonwebtoken::decode::<Value>(token, &key, &validation)?.claims)
}

async fn verify_dynamic_webhook(http: &reqwest::Client, token: &str) -> anyhow::Result<Value> {
    let jwks = fetch_jwks(http, false).await?;
    match verify_jwt_with_jwks(token, &jwks) {
        // 金鑰輪替：kid 找不到時強制更新一次 JWKS 再試
        Err(e) if e.to_string().contains("not found in JWKS") => {
            verify_jwt_with_jwks(token, &fetch_jwks(http, true).await?)
        }
        other => other,
    }
}

/// `POST /api/gemini-callback/dynamic`
pub async fn dynamic_webhook(State(state): State<AppStateRef>, headers: HeaderMap) -> Response {
    let Some(token) = header(&headers, "webhook-signature") else {
        tracing::error!("Missing Webhook-Signature header in dynamic webhook callback");
        return detail(StatusCode::BAD_REQUEST, "Missing signature header");
    };
    let payload = match verify_dynamic_webhook(&state.http, token).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Dynamic Webhook JWT verification failed: {e:#}");
            return detail(StatusCode::BAD_REQUEST, "Invalid JWT signature");
        }
    };
    let user_id = payload["data"]["user_metadata"]["user_id"]
        .as_str()
        .or_else(|| payload["user_metadata"]["user_id"].as_str())
        .map(str::to_string);
    spawn_event(&state, &payload, user_id);
    Json(json!({ "status": "received" })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_webhooks_test_vector() {
        // https://github.com/standard-webhooks/standard-webhooks 規格的範例
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        let id = "msg_p5jXN8AQM9LWM0D4loKWxJek";
        let ts = "1614265330";
        let payload = r#"{"test": 2432232314}"#;
        let sig = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";
        assert!(verify_static_webhook(payload, id, ts, sig, secret));
        assert!(verify_static_webhook(
            payload,
            id,
            ts,
            &format!("v1,bad {sig}"),
            secret
        ));
        assert!(!verify_static_webhook(
            payload,
            id,
            "1614265331",
            sig,
            secret
        ));
        assert!(!verify_static_webhook(payload, id, ts, "v2,abc", secret));
    }

    #[test]
    fn jwt_without_matching_kid_is_rejected() {
        let jwks: JwkSet = serde_json::from_value(json!({"keys": []})).unwrap();
        // header {"alg":"RS256","kid":"k1"} . payload {} . 假簽章
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImsxIn0.e30.c2ln";
        let err = verify_jwt_with_jwks(token, &jwks).unwrap_err();
        assert!(err.to_string().contains("not found in JWKS"));
    }
}
