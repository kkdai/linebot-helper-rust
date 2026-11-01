//! HTML → Markdown / 純文字。

use std::sync::LazyLock;

use htmd::HtmlToMarkdown;
use regex::Regex;
use scraper::Html;

static BASE64_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[.*?\]\(data:image/.*?;base64,.*?\)").unwrap());

static CONVERTER: LazyLock<HtmlToMarkdown> = LazyLock::new(|| {
    HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "noscript", "svg", "iframe", "template", "head",
        ])
        .build()
});

pub fn remove_base64_images(markdown: &str) -> String {
    BASE64_IMAGE.replace_all(markdown, "").into_owned()
}

/// 等同 Python 版 `parse_html(html, markdown=True)`：markdownify + 去掉 base64 圖片。
pub fn html_to_markdown(html: &str) -> String {
    let md = CONVERTER
        .convert(html)
        .unwrap_or_else(|_| html_to_text(html));
    remove_base64_images(&md)
}

/// 等同 BeautifulSoup `get_text(strip=True)`，但以換行分隔各段文字以保留可讀性。
pub fn html_to_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let skip = ["script", "style", "noscript", "template"];
    doc.root_element()
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            let parent_is_skipped = node
                .parent()
                .and_then(|p| p.value().as_element())
                .is_some_and(|e| skip.contains(&e.name()));
            (!parent_is_skipped).then(|| text.trim().to_string())
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_html_and_drops_scripts_and_base64_images() {
        let html = r#"<html><head><title>T</title><style>p{}</style></head><body>
            <h1>標題</h1><p>Hello <b>world</b></p>
            <script>alert(1)</script>
            <img src="data:image/png;base64,AAAA" alt="x">
            </body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("標題"));
        assert!(md.contains("**world**"));
        assert!(!md.contains("alert"));
        assert!(!md.contains("base64"));
    }

    #[test]
    fn extracts_plain_text() {
        let text = html_to_text("<div><p> a </p><script>x()</script><p>b</p></div>");
        assert_eq!(text, "a\nb");
    }

    #[test]
    fn removes_base64_markdown_images() {
        assert_eq!(
            remove_base64_images("a ![x](data:image/png;base64,QUJD) b"),
            "a  b"
        );
    }
}
