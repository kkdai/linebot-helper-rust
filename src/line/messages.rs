//! LINE 訊息 JSON builder。直接產生 `serde_json::Value`，
//! Flex 的 `clipboard` 這類新 action 也能原封不動送出。

use serde_json::{Value, json};

pub fn text(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

pub fn text_with_quick_reply(text: impl Into<String>, items: Vec<Value>) -> Value {
    let mut msg = self::text(text);
    if !items.is_empty() {
        msg["quickReply"] = json!({ "items": items });
    }
    msg
}

/// Quick reply 的 postback 按鈕。label 上限 20 字。
pub fn postback_item(label: &str, data: &str, display_text: &str) -> Value {
    json!({
        "type": "action",
        "action": {
            "type": "postback",
            "label": truncate_chars(label, 20),
            "data": data,
            "displayText": display_text,
        }
    })
}

pub fn image(url: &str) -> Value {
    json!({ "type": "image", "originalContentUrl": url, "previewImageUrl": url })
}

pub fn audio(url: &str, duration_ms: u64) -> Value {
    json!({ "type": "audio", "originalContentUrl": url, "duration": duration_ms })
}

pub fn flex(alt_text: &str, contents: Value) -> Value {
    json!({ "type": "flex", "altText": alt_text, "contents": contents })
}

/// 依字元（非 byte）截斷。
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Python 版回覆長文時的截斷規則：超過 4500 字就保留前 4400 字並加註。
pub fn truncate_reply(s: &str) -> String {
    if s.chars().count() > 4500 {
        format!("{}\n\n... (訊息過長，已截斷)", truncate_chars(s, 4400))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_reply_is_attached_only_when_items_exist() {
        assert!(
            text_with_quick_reply("hi", vec![])
                .get("quickReply")
                .is_none()
        );
        let msg = text_with_quick_reply("hi", vec![postback_item("a", "{}", "a")]);
        assert_eq!(msg["quickReply"]["items"][0]["action"]["type"], "postback");
    }

    #[test]
    fn labels_are_truncated_by_chars() {
        let item = postback_item("一二三四五六七八九十一二三四五六七八九十多", "d", "t");
        assert_eq!(
            item["action"]["label"].as_str().unwrap().chars().count(),
            20
        );
    }

    #[test]
    fn truncate_reply_counts_chars() {
        let long = "字".repeat(4501);
        let out = truncate_reply(&long);
        assert!(out.starts_with(&"字".repeat(4400)));
        assert!(out.ends_with("(訊息過長，已截斷)"));
        assert_eq!(truncate_reply("short"), "short");
    }
}
