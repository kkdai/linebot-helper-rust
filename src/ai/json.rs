//! 從 LLM 回應中寬鬆地抽出 JSON（移植自 maps_tool 的 regex 抽取）。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

static CODE_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)```(?:json)?\s*(.*?)\s*```").unwrap());
static ARRAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\[(.*?)\]").unwrap());

/// 先找 ```json ... ``` 區塊，再退而求其次取第一個 `{` 到最後一個 `}`。
pub fn extract_object(text: &str) -> anyhow::Result<Value> {
    let text = text.trim();
    let candidate = if let Some(c) = CODE_FENCE.captures(text) {
        c[1].trim().to_string()
    } else {
        match (text.find('{'), text.rfind('}')) {
            (Some(s), Some(e)) if e > s => text[s..=e].to_string(),
            _ => text.to_string(),
        }
    };
    serde_json::from_str(&candidate)
        .map_err(|e| anyhow::anyhow!("JSON parsing failed ({e}): {candidate:.200}"))
}

/// 解析字串陣列；JSON 失敗時取第一個 `[...]`，並容忍單引號（等同 Python 的 ast.literal_eval）。
pub fn extract_string_array(text: &str) -> Vec<String> {
    let text = text.trim();
    let parsed: Option<Vec<Value>> = serde_json::from_str(text).ok().or_else(|| {
        let inner = ARRAY.captures(text)?[1].to_string();
        serde_json::from_str(&format!("[{inner}]"))
            .ok()
            .or_else(|| serde_json::from_str(&format!("[{}]", inner.replace('\'', "\""))).ok())
    });
    parsed
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Null => None,
            other => Some(other.to_string()),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// JSON 值轉成顯示用字串（rating 可能是數字也可能是字串）。
pub fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_from_fence_or_braces() {
        assert_eq!(
            extract_object("前言\n```json\n{\"a\": 1}\n```\n後記").unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            extract_object("結果如下：{\"b\": [1]} 謝謝").unwrap(),
            json!({"b": [1]})
        );
        assert!(extract_object("沒有 JSON").is_err());
    }

    #[test]
    fn extracts_string_arrays_leniently() {
        assert_eq!(extract_string_array(r#"["A", "B"]"#), vec!["A", "B"]);
        assert_eq!(
            extract_string_array("餐廳有：['鼎泰豐', '欣葉'] 等"),
            vec!["鼎泰豐", "欣葉"]
        );
        assert!(extract_string_array("none").is_empty());
    }
}
