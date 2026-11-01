//! 把技術錯誤轉成給使用者看的繁中訊息。

/// 移植自 `LineService.format_error_message`。
pub fn format_error_message(error: &str, context: &str) -> String {
    let e = error.to_lowercase();
    if e.contains("quota") || e.contains("rate limit") || e.contains("429") {
        "⏳ 系統目前繁忙，請稍後再試。\n\n💡 建議等待 1-2 分鐘後重試。".into()
    } else if e.contains("timeout") || e.contains("timed out") {
        "⏱️ 處理時間過長，請稍後再試。\n\n💡 如果問題持續，請嘗試簡化您的問題。".into()
    } else if e.contains("not found") || e.contains("404") {
        "🔍 找不到相關資訊。\n\n💡 請嘗試用不同的方式描述您的問題。".into()
    } else if e.contains("empty response") {
        "⚠️ 無法生成回應，可能是內容被過濾。\n\n💡 請嘗試用不同的問法。".into()
    } else {
        let base = if context.is_empty() {
            "❌ 處理時發生錯誤。".to_string()
        } else {
            format!("❌ {context}時發生錯誤。")
        };
        format!("{base}\n\n請稍後再試，或使用 /clear 清除對話記憶後重新開始。")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_errors() {
        assert!(format_error_message("HTTP 429 quota", "").starts_with("⏳"));
        assert!(format_error_message("operation timed out", "").starts_with("⏱️"));
        assert!(format_error_message("404 not found", "").starts_with("🔍"));
        assert_eq!(
            format_error_message("boom", "處理網址"),
            "❌ 處理網址時發生錯誤。\n\n請稍後再試，或使用 /clear 清除對話記憶後重新開始。"
        );
    }
}
