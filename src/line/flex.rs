//! Flex Message 組裝與書籤指令解析（移植自 services/bookmark_flex.py 與 main.py 的社群 carousel）。
//! 純函式，只產生 JSON。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::store::bookmark::Bookmark;

const SUMMARY_SNIPPET_LENGTH: usize = 100;

static COMMAND_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^/(save|list|search)(?:\s+(.*))?$").unwrap());

/// "/save https://x" → ("save", "https://x")；"/list" → ("list", "")；非書籤指令回 None。
pub fn parse_bookmark_command(text: &str) -> Option<(String, String)> {
    let caps = COMMAND_PATTERN.captures(text.trim())?;
    Some((
        caps[1].to_string(),
        caps.get(2)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default(),
    ))
}

/// 社群 carousel 的第一顆 bubble：📌 摘要與分析 + 儲存書籤 / 研究報告按鈕。
/// doc_id 為 None 代表 Firestore 不可用，不放會失敗的按鈕。
pub fn build_summary_bubble(title: &str, summary: &str, url: &str, doc_id: Option<&str>) -> Value {
    let mut footer = vec![json!({
        "type": "button", "style": "link", "height": "sm",
        "action": { "type": "uri", "label": "🔗 開啟原文", "uri": url },
    })];
    if let Some(id) = doc_id {
        footer.insert(
            0,
            json!({
                "type": "button", "style": "secondary", "height": "sm",
                "action": {
                    "type": "postback", "label": "📄 詳細研究報告",
                    "data": json!({"action": "research_report", "id": id}).to_string(),
                    "displayText": "📄 產生詳細研究報告",
                },
            }),
        );
        footer.insert(
            0,
            json!({
                "type": "button", "style": "primary", "color": "#E67E22", "height": "sm",
                "action": {
                    "type": "postback", "label": "🔖 儲存書籤",
                    "data": json!({"action": "save_bookmark", "id": id}).to_string(),
                    "displayText": "🔖 儲存這篇文章",
                },
            }),
        );
    }
    json!({
        "type": "bubble",
        "size": "mega",
        "header": {
            "type": "box", "layout": "vertical",
            "contents": [{ "type": "text", "text": "📌 摘要與分析", "weight": "bold", "color": "#ffffff", "size": "lg" }],
            "backgroundColor": "#E67E22", "paddingAll": "md",
        },
        "body": {
            "type": "box", "layout": "vertical", "spacing": "md",
            "contents": [
                { "type": "text", "text": non_empty(title, "（無標題）"), "weight": "bold", "size": "md", "wrap": true, "color": "#111111" },
                { "type": "text", "text": non_empty(summary, "（無摘要）"), "wrap": true, "size": "sm", "color": "#333333" },
            ],
            "paddingAll": "lg",
        },
        "footer": { "type": "box", "layout": "vertical", "spacing": "sm", "contents": footer, "paddingAll": "md" },
    })
}

/// 單一平台文案 bubble（含一鍵複製按鈕）。
pub fn build_platform_bubble(header: &str, color: &str, text: &str, copy_label: &str) -> Value {
    json!({
        "type": "bubble",
        "size": "mega",
        "header": {
            "type": "box", "layout": "vertical",
            "contents": [{ "type": "text", "text": header, "weight": "bold", "color": "#ffffff", "size": "lg" }],
            "backgroundColor": color, "paddingAll": "md",
        },
        "body": {
            "type": "box", "layout": "vertical",
            "contents": [{ "type": "text", "text": text, "wrap": true, "size": "sm", "color": "#333333" }],
            "paddingAll": "lg",
        },
        "footer": {
            "type": "box", "layout": "vertical",
            "contents": [{
                "type": "button", "style": "primary", "color": color, "height": "sm",
                "action": { "type": "clipboard", "label": copy_label, "clipboardText": text },
            }],
            "paddingAll": "md",
        },
    })
}

fn non_empty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() { fallback } else { s }
}

fn snippet(text: &str) -> String {
    if text.chars().count() <= SUMMARY_SNIPPET_LENGTH {
        text.to_string()
    } else {
        format!(
            "{}…",
            text.chars()
                .take(SUMMARY_SNIPPET_LENGTH - 1)
                .collect::<String>()
        )
    }
}

pub fn build_bookmark_bubble(b: &Bookmark) -> Value {
    let date = if b.created_at > 0.0 {
        chrono::DateTime::from_timestamp(b.created_at as i64, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    json!({
        "type": "bubble",
        "size": "kilo",
        "body": {
            "type": "box", "layout": "vertical", "spacing": "sm",
            "contents": [
                { "type": "text", "text": non_empty(&b.title, "（無標題）"), "weight": "bold", "size": "md", "wrap": true, "color": "#111111" },
                { "type": "text", "text": snippet(&b.summary), "wrap": true, "size": "sm", "color": "#555555" },
                { "type": "text", "text": format!("🕐 {date}"), "size": "xs", "color": "#999999" },
            ],
            "paddingAll": "lg",
        },
        "footer": {
            "type": "box", "layout": "horizontal", "spacing": "sm",
            "contents": [
                {
                    "type": "button", "style": "primary", "color": "#2C6E49", "height": "sm", "flex": 2,
                    "action": { "type": "uri", "label": "🔗 開啟連結", "uri": b.url },
                },
                {
                    "type": "button", "style": "secondary", "height": "sm", "flex": 1,
                    "action": {
                        "type": "postback", "label": "🗑 刪除",
                        "data": json!({"action": "delete_bookmark", "id": b.doc_id}).to_string(),
                        "displayText": "🗑 刪除書籤",
                    },
                },
            ],
            "paddingAll": "md",
        },
    })
}

pub fn build_bookmark_carousel(bookmarks: &[Bookmark]) -> Value {
    json!({
        "type": "carousel",
        "contents": bookmarks.iter().map(build_bookmark_bubble).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bookmark_commands() {
        assert_eq!(
            parse_bookmark_command("/save https://x"),
            Some(("save".into(), "https://x".into()))
        );
        assert_eq!(
            parse_bookmark_command(" /list "),
            Some(("list".into(), "".into()))
        );
        assert_eq!(
            parse_bookmark_command("/search  python 教學"),
            Some(("search".into(), "python 教學".into()))
        );
        assert_eq!(parse_bookmark_command("/saved"), None);
        assert_eq!(parse_bookmark_command("hello /save"), None);
    }

    #[test]
    fn summary_bubble_buttons_depend_on_doc_id() {
        let with = build_summary_bubble("T", "S", "https://u", Some("abc"));
        let buttons = with["footer"]["contents"].as_array().unwrap();
        assert_eq!(buttons.len(), 3);
        assert_eq!(buttons[0]["action"]["label"], "🔖 儲存書籤");
        let data: Value =
            serde_json::from_str(buttons[0]["action"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(data, json!({"action": "save_bookmark", "id": "abc"}));
        assert_eq!(buttons[1]["action"]["label"], "📄 詳細研究報告");

        let without = build_summary_bubble("", "", "https://u", None);
        assert_eq!(without["footer"]["contents"].as_array().unwrap().len(), 1);
        assert_eq!(without["body"]["contents"][0]["text"], "（無標題）");
    }

    #[test]
    fn bookmark_bubble_snippet_and_delete_button() {
        let b = Bookmark {
            title: "標題".into(),
            summary: "字".repeat(150),
            url: "https://u".into(),
            created_at: 1_700_000_000.0,
            doc_id: "d1".into(),
            ..Default::default()
        };
        let bubble = build_bookmark_bubble(&b);
        let snippet = bubble["body"]["contents"][1]["text"].as_str().unwrap();
        assert_eq!(snippet.chars().count(), 100);
        assert!(snippet.ends_with('…'));
        assert_eq!(bubble["body"]["contents"][2]["text"], "🕐 2023-11-14");
        assert!(
            bubble["footer"]["contents"][1]["action"]["data"]
                .as_str()
                .unwrap()
                .contains("d1")
        );
        assert_eq!(
            build_bookmark_carousel(&[b])["contents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
