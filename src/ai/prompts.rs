//! Prompt 範本：用 ast 從 Python 原始碼原封不動抽出（見 `prompts/*.txt`），
//! f-string 的變數變成 `{name}` 佔位符，由 [`fill`] 代入。

pub const SUMMARIZE_SHORT: &str = include_str!("prompts/summarize_short.txt");
pub const SUMMARIZE_NORMAL: &str = include_str!("prompts/summarize_normal.txt");
pub const SUMMARIZE_DETAILED: &str = include_str!("prompts/summarize_detailed.txt");
pub const YOUTUBE_NORMAL: &str = include_str!("prompts/youtube_normal.txt");
pub const YOUTUBE_DETAIL: &str = include_str!("prompts/youtube_detail.txt");
pub const YOUTUBE_TWITTER: &str = include_str!("prompts/youtube_twitter.txt");
pub const HUMANIZE_GUIDELINES: &str = include_str!("prompts/humanize_guidelines.txt");
pub const SOCIAL_MEDIA: &str = include_str!("prompts/social_media.txt");
pub const RESEARCH_REPORT: &str = include_str!("prompts/research_report.txt");
pub const BOOKMARK_SUMMARY: &str = include_str!("prompts/bookmark_summary.txt");
pub const CHAT_INSTRUCTION: &str = include_str!("prompts/chat_instruction.txt");
pub const VISION_DEFAULT: &str = include_str!("prompts/vision_default.txt");
pub const GITHUB_SUMMARY: &str = include_str!("prompts/github_summary.txt");
pub const RESTAURANT_BATCH: &str = include_str!("prompts/restaurant_batch.txt");
pub const NEARBY_BATCH: &str = include_str!("prompts/nearby_batch.txt");
pub const SPECIFIC_RESTAURANT: &str = include_str!("prompts/specific_restaurant.txt");
pub const RESTAURANT_REVIEWS: &str = include_str!("prompts/restaurant_reviews.txt");
pub const RESTAURANT_NAMES: &str = include_str!("prompts/restaurant_names.txt");
pub const VOICE_SYSTEM: &str = include_str!("prompts/voice_system.txt");
pub const HELP: &str = include_str!("prompts/help.txt");

/// 依序代入 `{key}` 佔位符。使用者內容（如 `text`）請放在最後，避免內容裡剛好有佔位符字樣。
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    vars.iter().fold(template.to_string(), |acc, (k, v)| {
        acc.replace(&format!("{{{k}}}"), v)
    })
}

pub fn summarize(mode: &str) -> &'static str {
    match mode {
        "short" => SUMMARIZE_SHORT,
        "detailed" => SUMMARIZE_DETAILED,
        _ => SUMMARIZE_NORMAL,
    }
}

pub fn youtube(mode: &str) -> &'static str {
    match mode {
        "detail" => YOUTUBE_DETAIL,
        "twitter" => YOUTUBE_TWITTER,
        _ => YOUTUBE_NORMAL,
    }
}

/// 社群爆款文案 prompt（人性化守則放在寫作指南之前）。
pub fn social_media(text: &str) -> String {
    fill(
        SOCIAL_MEDIA,
        &[("HUMANIZE_GUIDELINES", HUMANIZE_GUIDELINES), ("text", text)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "這是一篇關於 AI 的測試文章 {text}";

    #[test]
    fn social_prompt_matches_python_tests() {
        let prompt = social_media(SAMPLE);
        assert!(prompt.contains(HUMANIZE_GUIDELINES));
        assert!(prompt.contains(SAMPLE));
        assert!(prompt.find("人性化守則").unwrap() < prompt.find("# 寫作指南").unwrap());
        for needle in [
            "先保事實",
            "AI 開場定型句",
            "具體 ＞ 抽象",
            "視頻→影片",
            "質量→品質",
            "全形標點",
            "台灣語氣詞",
            "禁止假掰誇張詞",
            "buzzword 空堆",
            "人性化守則權重高",
            "嚴禁 AI 正式腔套語",
            "人性化守則權重最高",
            "爆款",
            "Hashtag",
        ] {
            assert!(prompt.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn templates_have_expected_placeholders() {
        for t in [
            SUMMARIZE_SHORT,
            SUMMARIZE_NORMAL,
            SUMMARIZE_DETAILED,
            BOOKMARK_SUMMARY,
        ] {
            assert!(t.contains("{text}"));
        }
        assert!(RESEARCH_REPORT.contains("{url}") && RESEARCH_REPORT.contains("{text}"));
        assert!(VOICE_SYSTEM.contains("{location_info}"));
        let filled = fill(RESTAURANT_BATCH, &[("name", "阿宗麵線")]);
        assert!(filled.contains("阿宗麵線") && !filled.contains("{name}"));
    }

    #[test]
    fn mode_selection_defaults_to_normal() {
        assert_eq!(summarize("unknown"), SUMMARIZE_NORMAL);
        assert_eq!(youtube("detail"), YOUTUBE_DETAIL);
    }
}
