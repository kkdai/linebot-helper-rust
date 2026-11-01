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

/// 移植自 `FriendlyErrorMessage.get_message`：依 HTTP status／逾時／連線錯誤分類。
pub fn friendly_error_message(error: &anyhow::Error, url: &str) -> String {
    if let Some(e) = error
        .chain()
        .find_map(|e| e.downcast_ref::<reqwest::Error>())
    {
        if let Some(status) = e.status() {
            let code = status.as_u16();
            return match code {
                403 => format!(
                    "❌ 抱歉，無法存取這個網站（被拒絕存取）。\n{url}\n\n可能原因：網站有防爬蟲保護，請稍後再試。"
                ),
                404 => format!("❌ 找不到這個頁面（404）。\n{url}\n\n請確認網址是否正確。"),
                429 => format!("❌ 請求過於頻繁（429）。\n{url}\n\n請稍後再試（約 1-2 分鐘）。"),
                500 => {
                    format!("❌ 網站伺服器錯誤（500）。\n{url}\n\n這是對方網站的問題，請稍後再試。")
                }
                502 | 503 => format!("❌ 網站暫時無法使用（{code}）。\n{url}\n\n請稍後再試。"),
                _ => {
                    format!("❌ 網站回應錯誤（HTTP {code}）。\n{url}\n\n請稍後再試或嘗試其他來源。")
                }
            };
        }
        if e.is_timeout() {
            return format!(
                "⏱️ 讀取超時。\n{url}\n\n網站回應太慢，已自動重試但仍失敗。請稍後再試。"
            );
        }
        if e.is_connect() {
            return format!("🔌 無法連線到網站。\n{url}\n\n請檢查網址是否正確，或稍後再試。");
        }
    }
    let mut msg = error.to_string();
    if msg.chars().count() > 200 {
        msg = format!("{}...", msg.chars().take(200).collect::<String>());
    }
    format!("❌ 處理時發生錯誤。\n\n技術訊息：{msg}\n\n已通知開發者，請稍後再試。")
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

    #[test]
    fn friendly_message_for_generic_error() {
        let msg = friendly_error_message(&anyhow::anyhow!("boom"), "https://x");
        assert!(msg.contains("技術訊息：boom"));
    }
}
