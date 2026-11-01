//! 研究報告網頁：Markdown（Gemini 產出）→ HTML，套進行動裝置優先的閱讀版型。
//! inline CSS、無外部資源。

use pulldown_cmark::{Options, Parser, html};

use crate::gemini::WebSource;

const PAGE_CSS: &str = r#"
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI",
      "Noto Sans TC", "PingFang TC", "Microsoft JhengHei", sans-serif;
    line-height: 1.75; color: #1a1a1a; background: #f5f5f4;
  }
  .container { max-width: 720px; margin: 0 auto; padding: 24px 20px 48px; }
  .card { background: #ffffff; border-radius: 12px; padding: 28px 24px;
          box-shadow: 0 1px 4px rgba(0,0,0,.08); }
  h1 { font-size: 1.5rem; line-height: 1.4; margin: 0 0 4px; }
  h2 { font-size: 1.2rem; margin-top: 2em; padding-bottom: .3em;
       border-bottom: 2px solid #E67E22; }
  h3 { font-size: 1.05rem; margin-top: 1.6em; }
  a { color: #C0611A; word-break: break-all; }
  blockquote { margin: 1em 0; padding: .5em 1em; border-left: 4px solid #E67E22;
               background: #faf3ec; color: #555; }
  code { background: #f0f0ef; padding: .15em .4em; border-radius: 4px;
         font-size: .9em; }
  table { border-collapse: collapse; width: 100%; display: block;
          overflow-x: auto; }
  th, td { border: 1px solid #ddd; padding: 8px 10px; text-align: left; }
  .meta { color: #888; font-size: .85rem; margin-bottom: 24px; }
  .sources { margin-top: 32px; font-size: .9rem; }
  .sources li { margin-bottom: .5em; }
  @media (prefers-color-scheme: dark) {
    body { background: #191918; color: #e8e8e6; }
    .card { background: #242422; box-shadow: none; }
    blockquote { background: #2e2a24; color: #bbb; }
    code { background: #333; }
    th, td { border-color: #444; }
    a { color: #e89a55; }
  }
"#;

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn markdown_to_html(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_DEFINITION_LIST;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(markdown, options));
    out
}

pub fn render_report_page(title: &str, markdown: &str, url: &str, sources: &[WebSource]) -> String {
    let title = escape_html(if title.is_empty() {
        "研究報告"
    } else {
        title
    });
    let url = escape_html(url);
    let body_html = markdown_to_html(markdown);
    let generated_at = chrono::Utc::now().format("%Y-%m-%d %H:%M");

    let items: String = sources
        .iter()
        .filter(|s| !s.uri.is_empty())
        .map(|s| {
            let label = if s.title.is_empty() { &s.uri } else { &s.title };
            format!(
                r#"<li><a href="{}" target="_blank" rel="noopener">{}</a></li>"#,
                escape_html(&s.uri),
                escape_html(label)
            )
        })
        .collect();
    let sources_html = if items.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="sources"><h2>📚 參考來源</h2><ol>{items}</ol></div>"#)
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{title}</title>
<style>{PAGE_CSS}</style>
</head>
<body>
<div class="container">
  <div class="card">
    <h1>🔬 {title}</h1>
    <div class="meta">
      產生時間：{generated_at}<br>
      原文：<a href="{url}" target="_blank" rel="noopener">{url}</a>
    </div>
    {body_html}
    {sources_html}
  </div>
</div>
</body>
</html>"#
    )
}

pub fn render_expired_page() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>找不到報告</title>
<style>{PAGE_CSS}</style>
</head>
<body>
<div class="container">
  <div class="card">
    <h1>🔍 找不到這份報告</h1>
    <p>這個連結對應的研究報告不存在，可能是網址有誤。</p>
    <p>回到 LINE 重新點「📄 詳細研究報告」即可再產生一份。</p>
  </div>
</div>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_markdown_tables_and_sources() {
        let md = "## 執行摘要\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let sources = vec![WebSource {
            uri: "https://s.example".into(),
            title: "".into(),
        }];
        let html = render_report_page("標題 <x>", md, "https://a.example?q=1&b=2", &sources);
        assert!(html.contains("<h2>執行摘要</h2>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("🔬 標題 &lt;x&gt;"));
        assert!(html.contains("https://a.example?q=1&amp;b=2"));
        assert!(html.contains(
            r#"<a href="https://s.example" target="_blank" rel="noopener">https://s.example</a>"#
        ));
    }

    #[test]
    fn no_sources_section_when_empty() {
        let html = render_report_page("", "text", "u", &[]);
        assert!(!html.contains("參考來源"));
        assert!(html.contains("<title>研究報告</title>"));
        assert!(render_expired_page().contains("找不到這份報告"));
    }
}
