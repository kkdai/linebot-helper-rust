//! 文件連結轉 Markdown（取代 Python 版的 firecrawl/anydoc + pypdf）。
//!
//! - PDF：`pdf-extract`
//! - docx/docm/pptx/pptm/odt/odp/epub：zip + XML 取文字
//! - xlsx/xlsm/xls/ods：`calamine` 轉 Markdown 表格
//! - csv：原文；rtf：剝除控制字
//! - doc/ppt（舊二進位格式）：不支援

use std::io::{Cursor, Read};

use calamine::{Data, Reader, open_workbook_auto_from_rs};
use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use super::html::html_to_markdown;

/// 支援的副檔名，即 format 名稱
pub const EXTENSION_FORMATS: &[&str] = &[
    "doc", "docx", "docm", "ppt", "pptx", "pptm", "xls", "xlsx", "xlsm", "odt", "ods", "odp",
    "rtf", "epub", "csv", "pdf",
];

pub fn format_from_content_type(content_type: &str) -> Option<&'static str> {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    Some(match ct.as_str() {
        "application/pdf" => "pdf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-word.document.macroenabled.12" => "docm",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.ms-powerpoint.presentation.macroenabled.12" => "pptm",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-excel.sheet.macroenabled.12" => "xlsm",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/rtf" | "text/rtf" => "rtf",
        "application/epub+zip" => "epub",
        "text/csv" => "csv",
        _ => return None,
    })
}

/// 依副檔名判斷（不發 request）。
pub fn format_from_url(url: &str) -> Option<&'static str> {
    let path = url::Url::parse(url).ok()?.path().to_string();
    let filename = path.rsplit('/').next()?;
    let (_, ext) = filename.rsplit_once('.')?;
    let ext = ext.to_lowercase();
    EXTENSION_FORMATS.iter().copied().find(|f| *f == ext)
}

/// 把文件 bytes 轉成 Markdown／純文字。CPU 密集，呼叫端應放進 `spawn_blocking`。
pub fn convert(bytes: &[u8], format: &str) -> anyhow::Result<String> {
    let text = match format {
        "pdf" => pdf_extract::extract_text_from_mem(bytes)?,
        "docx" | "docm" => xml_text_from_zip(bytes, &["word/document.xml"], &["p", "br", "tr"])?,
        "pptx" | "pptm" => pptx_text(bytes)?,
        "odt" | "odp" => xml_text_from_zip(bytes, &["content.xml"], &["p", "h", "line-break"])?,
        "xlsx" | "xlsm" | "xls" | "ods" => spreadsheet_markdown(bytes)?,
        "epub" => epub_markdown(bytes)?,
        "csv" => String::from_utf8_lossy(bytes).into_owned(),
        "rtf" => strip_rtf(&String::from_utf8_lossy(bytes)),
        other => anyhow::bail!("unsupported document format: {other}"),
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        anyhow::bail!("no text extracted from {format} document");
    }
    Ok(text)
}

fn read_zip_entry(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut s = String::new();
    file.read_to_string(&mut s).ok()?;
    Some(s)
}

/// 取 XML 內所有文字節點；遇到 `break_tags`（local name）的結束或空元素時換行。
fn xml_text(xml: &str, break_tags: &[&str]) -> String {
    let mut reader = XmlReader::from_str(xml);
    let mut out = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(t)) => out.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => out.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                } else {
                    out.push_str(match &*r {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Ok(Event::End(e)) => {
                if break_tags.contains(&local(e.local_name().as_ref())) {
                    out.push('\n');
                }
            }
            Ok(Event::Empty(e)) => {
                let local_name = e.local_name();
                let name = local(local_name.as_ref());
                if name == "tab" {
                    out.push('\t');
                } else if break_tags.contains(&name) {
                    out.push('\n');
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn local(name: &str) -> &str {
    name
}

fn xml_text_from_zip(
    bytes: &[u8],
    entries: &[&str],
    break_tags: &[&str],
) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut out = String::new();
    for entry in entries {
        if let Some(xml) = read_zip_entry(&mut archive, entry) {
            out.push_str(&xml_text(&xml, break_tags));
        }
    }
    Ok(out)
}

fn slide_number(name: &str) -> Option<u32> {
    name.strip_prefix("ppt/slides/slide")?
        .strip_suffix(".xml")?
        .parse()
        .ok()
}

fn pptx_text(bytes: &[u8]) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|n| n.ok())
        .filter_map(|n| slide_number(&n).map(|i| (i, n.into_owned())))
        .collect();
    slides.sort();
    let mut out = String::new();
    for (i, name) in slides {
        if let Some(xml) = read_zip_entry(&mut archive, &name) {
            out.push_str(&format!(
                "## Slide {i}\n\n{}\n\n",
                xml_text(&xml, &["p"]).trim()
            ));
        }
    }
    Ok(out)
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        other => other.to_string().replace('|', "\\|").replace('\n', " "),
    }
}

fn spreadsheet_markdown(bytes: &[u8]) -> anyhow::Result<String> {
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(bytes))?;
    let mut out = String::new();
    for name in workbook.sheet_names() {
        let Ok(range) = workbook.worksheet_range(&name) else {
            continue;
        };
        out.push_str(&format!("## {name}\n\n"));
        for (i, row) in range.rows().enumerate() {
            let cells: Vec<String> = row.iter().map(cell_to_string).collect();
            out.push_str(&format!("| {} |\n", cells.join(" | ")));
            if i == 0 {
                out.push_str(&format!("|{}\n", " --- |".repeat(cells.len())));
            }
        }
        out.push('\n');
    }
    Ok(out)
}

fn epub_markdown(bytes: &[u8]) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut names: Vec<String> = archive
        .file_names()
        .filter_map(|n| n.ok())
        .filter(|n| n.ends_with(".xhtml") || n.ends_with(".html") || n.ends_with(".htm"))
        .map(|n| n.into_owned())
        .collect();
    names.sort();
    let mut out = String::new();
    for name in names {
        if let Some(html) = read_zip_entry(&mut archive, &name) {
            out.push_str(&html_to_markdown(&html));
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

/// 很陽春的 RTF 剝除：略過 `{\\*...}` 與字型表等 destination 群組、控制字與大括號，
/// `\\par` / `\\line` 變換行。
fn strip_rtf(rtf: &str) -> String {
    const SKIP_DESTINATIONS: &[&str] = &[
        "fonttbl",
        "colortbl",
        "stylesheet",
        "info",
        "pict",
        "header",
        "footer",
        "generator",
    ];
    let mut out = String::new();
    let mut chars = rtf.chars().peekable();
    let mut depth = 0usize;
    let mut skip_depth: Option<usize> = None;
    let mut group_start = false;
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                depth += 1;
                group_start = true;
                continue;
            }
            '}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            '\\' => {
                let mut word = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.is_empty() {
                    match chars.next() {
                        Some('*') if group_start && skip_depth.is_none() => {
                            skip_depth = Some(depth)
                        }
                        Some(n @ ('{' | '}' | '\\')) if skip_depth.is_none() => out.push(n),
                        _ => {}
                    }
                    group_start = false;
                    continue;
                }
                while chars
                    .peek()
                    .is_some_and(|n| n.is_ascii_digit() || *n == '-')
                {
                    chars.next();
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                if group_start && skip_depth.is_none() && SKIP_DESTINATIONS.contains(&word.as_str())
                {
                    skip_depth = Some(depth);
                } else if skip_depth.is_none() && (word == "par" || word == "line") {
                    out.push('\n');
                }
            }
            '\r' | '\n' => {}
            _ if skip_depth.is_none() => out.push(c),
            _ => {}
        }
        group_start = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            for (name, content) in entries {
                zip.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn detects_format_from_extension_and_content_type() {
        assert_eq!(format_from_url("https://x.com/a/b.PDF?x=1"), Some("pdf"));
        assert_eq!(format_from_url("https://x.com/report.docx"), Some("docx"));
        assert_eq!(format_from_url("https://x.com/page.html"), None);
        assert_eq!(format_from_url("https://x.com/download.php"), None);
        assert_eq!(
            format_from_content_type("application/pdf; charset=binary"),
            Some("pdf")
        );
        assert_eq!(format_from_content_type("text/html"), None);
    }

    #[test]
    fn docx_text_keeps_paragraphs_and_entities() {
        let xml = r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
            <w:p><w:r><w:t>第一段 A&amp;B</w:t></w:r></w:p>
            <w:p><w:r><w:t>第二</w:t><w:tab/><w:t>段</w:t></w:r></w:p>
            </w:body></w:document>"#;
        let bytes = make_zip(&[("word/document.xml", xml)]);
        let text = convert(&bytes, "docx").unwrap();
        assert!(text.contains("第一段 A&B\n"), "{text:?}");
        assert!(text.contains("第二\t段"), "{text:?}");
    }

    #[test]
    fn pptx_slides_are_ordered_numerically() {
        let slide = |t: &str| {
            format!(
                r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#
            )
        };
        let (s1, s2, s10) = (slide("one"), slide("two"), slide("ten"));
        let bytes = make_zip(&[
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
        ]);
        let text = convert(&bytes, "pptx").unwrap();
        let (a, b, c) = (
            text.find("one").unwrap(),
            text.find("two").unwrap(),
            text.find("ten").unwrap(),
        );
        assert!(a < b && b < c, "{text}");
    }

    #[test]
    fn epub_html_is_converted() {
        let bytes = make_zip(&[(
            "OEBPS/ch1.xhtml",
            "<html><body><h1>Chapter</h1><p>Body</p></body></html>",
        )]);
        let text = convert(&bytes, "epub").unwrap();
        assert!(text.contains("Chapter") && text.contains("Body"));
    }

    #[test]
    fn rtf_is_stripped() {
        let rtf = r"{\rtf1\ansi{\fonttbl\f0\fswiss Helvetica;}{\*\generator x;}\f0\pard Hello \b world\b0\par Second line}";
        let text = strip_rtf(rtf);
        assert!(text.contains("Hello world"), "{text:?}");
        assert!(text.contains("\nSecond line"), "{text:?}");
        assert!(!text.contains("generator"));
    }

    #[test]
    fn legacy_and_empty_documents_error() {
        assert!(convert(b"anything", "doc").is_err());
        assert!(convert(b"   ", "csv").is_err());
    }
}
