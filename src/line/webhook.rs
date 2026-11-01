//! LINE webhook：簽章驗證與事件解析。

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::gemini::types::B64;

/// `X-Line-Signature == base64(HMAC-SHA256(channel_secret, body))`，常數時間比較。
pub fn verify_signature(channel_secret: &str, body: &[u8], signature: &str) -> bool {
    let Ok(expected) = B64.decode(signature.trim()) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[derive(Debug, Deserialize)]
pub struct WebhookBody {
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Source {
    #[serde(rename_all = "camelCase")]
    User { user_id: String },
    #[serde(rename_all = "camelCase")]
    Group {
        group_id: String,
        #[serde(default)]
        user_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Room {
        room_id: String,
        #[serde(default)]
        user_id: Option<String>,
    },
}

impl Source {
    /// 只有 1:1 聊天回傳 user id（Python 版只處理 SourceUser）。
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Source::User { user_id } => Some(user_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    Text {
        id: String,
        text: String,
    },
    Image {
        id: String,
    },
    Audio {
        id: String,
        #[serde(default)]
        duration: Option<u64>,
    },
    Location {
        id: String,
        latitude: f64,
        longitude: f64,
        #[serde(default)]
        address: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageEvent {
    #[serde(default)]
    pub reply_token: String,
    pub source: Source,
    pub message: Message,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Postback {
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostbackEvent {
    #[serde(default)]
    pub reply_token: String,
    pub source: Source,
    pub postback: Postback,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Message(MessageEvent),
    Postback(PostbackEvent),
    Other(String),
}

/// 解析 webhook body；無法辨識的事件型別變成 `Event::Other`，不讓整包失敗。
pub fn parse_events(body: &[u8]) -> Result<Vec<Event>, serde_json::Error> {
    let body: WebhookBody = serde_json::from_slice(body)?;
    Ok(body
        .events
        .into_iter()
        .map(|raw| {
            let kind = raw["type"].as_str().unwrap_or_default().to_string();
            let parsed = match kind.as_str() {
                "message" => serde_json::from_value(raw).map(Event::Message),
                "postback" => serde_json::from_value(raw).map(Event::Postback),
                _ => return Event::Other(kind),
            };
            parsed.unwrap_or_else(|e| {
                tracing::warn!("failed to parse {kind} event: {e}");
                Event::Other(kind)
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_verification() {
        let secret = "testsecret";
        let body = br#"{"events":[]}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = B64.encode(mac.finalize().into_bytes());
        assert!(verify_signature(secret, body, &sig));
        assert!(!verify_signature(secret, b"tampered", &sig));
        assert!(!verify_signature("other", body, &sig));
        assert!(!verify_signature(secret, body, "not-base64!!"));
    }

    #[test]
    fn parses_message_and_postback_events() {
        let body = r#"{"destination":"x","events":[
          {"type":"message","replyToken":"r1","source":{"type":"user","userId":"U1"},
           "timestamp":1,"mode":"active","webhookEventId":"e",
           "message":{"type":"text","id":"m1","text":"hello","quoteToken":"q"}},
          {"type":"message","replyToken":"r2","source":{"type":"group","groupId":"G1","userId":"U1"},
           "message":{"type":"location","id":"m2","latitude":25.03,"longitude":121.56,"address":"台北"}},
          {"type":"message","replyToken":"r3","source":{"type":"user","userId":"U1"},
           "message":{"type":"sticker","id":"m3","packageId":"1","stickerId":"2"}},
          {"type":"postback","replyToken":"r4","source":{"type":"user","userId":"U1"},
           "postback":{"data":"{\"action\":\"read_aloud\"}"}},
          {"type":"follow","replyToken":"r5","source":{"type":"user","userId":"U1"}}
        ]}"#;
        let events = parse_events(body.as_bytes()).unwrap();
        assert_eq!(events.len(), 5);
        match &events[0] {
            Event::Message(m) => {
                assert_eq!(m.reply_token, "r1");
                assert_eq!(m.source.user_id(), Some("U1"));
                assert_eq!(
                    m.message,
                    Message::Text {
                        id: "m1".into(),
                        text: "hello".into()
                    }
                );
            }
            other => panic!("{other:?}"),
        }
        match &events[1] {
            Event::Message(m) => {
                assert_eq!(m.source.user_id(), None);
                assert!(
                    matches!(m.message, Message::Location { latitude, .. } if latitude == 25.03)
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&events[2], Event::Message(m) if m.message == Message::Other));
        assert!(matches!(&events[3], Event::Postback(p) if p.postback.data.contains("read_aloud")));
        assert_eq!(events[4], Event::Other("follow".into()));
    }
}
