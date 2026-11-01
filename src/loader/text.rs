//! 使用者訊息的 URL 與摘要模式解析（移植自 `loader/text_utils.py`、`loader/utils.py`）。

use std::sync::LazyLock;

use regex::Regex;

static URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://[^\s]+").unwrap());

static MODE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)\[短\]|\(短\)|\[short\]|\(short\)").unwrap(),
            "short",
        ),
        (
            Regex::new(r"(?i)\[詳\]|\(詳\)|\[detailed\]|\(detailed\)|\[detail\]|\(detail\)")
                .unwrap(),
            "detailed",
        ),
        (
            Regex::new(r"(?i)\[標準\]|\(標準\)|\[normal\]|\(normal\)").unwrap(),
            "normal",
        ),
    ]
});

pub fn find_urls(text: &str) -> Vec<String> {
    URL_PATTERN
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// 解析 `[短]`、`[詳]`、`(short)` 等模式標記，回傳（去掉標記的文字, mode）。
pub fn parse_summary_mode(text: &str) -> (String, &'static str) {
    for (pattern, mode) in MODE_PATTERNS.iter() {
        if pattern.is_match(text) {
            return (pattern.replace_all(text, "").trim().to_string(), mode);
        }
    }
    (text.to_string(), "normal")
}

pub fn extract_url_and_mode(text: &str) -> (Vec<String>, &'static str) {
    let (cleaned, mode) = parse_summary_mode(text);
    (find_urls(&cleaned), mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_urls_and_modes() {
        assert_eq!(
            extract_url_and_mode("看看 https://a.com/x [短]"),
            (vec!["https://a.com/x".to_string()], "short")
        );
        assert_eq!(
            extract_url_and_mode("https://a.com (DETAILED) http://b.org"),
            (
                vec!["https://a.com".to_string(), "http://b.org".to_string()],
                "detailed"
            )
        );
        assert_eq!(extract_url_and_mode("沒有網址"), (vec![], "normal"));
    }
}
