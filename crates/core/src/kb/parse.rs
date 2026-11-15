//! 把各种文档读成带位置的文字段落。扫描件和图片不在这里识别，由调用方交给看图模型。

use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Mutex;

use pdfium_render::prelude::*;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// 一段文字在原文档里的位置，用来给回答标出处。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Locator {
    None,
    Page(u32),
    Slide(u32),
    Sheet(String),
    Heading(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub locator: Locator,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Markdown,
    Html,
    Docx,
    Pptx,
    Sheet,
    Pdf,
    Image,
}

pub fn kind_of(ext: &str) -> Option<Kind> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "txt" | "log" | "csv" | "tsv" | "json" | "yaml" | "yml" | "xml" | "ini" | "toml" => Kind::Text,
        "md" | "markdown" => Kind::Markdown,
        "html" | "htm" => Kind::Html,
        "docx" => Kind::Docx,
        "pptx" => Kind::Pptx,
        "xlsx" | "xlsm" | "xls" | "ods" => Kind::Sheet,
        "pdf" => Kind::Pdf,
        "png" | "jpg" | "jpeg" | "webp" => Kind::Image,
        _ => return None,
    })
}

fn damaged(e: impl std::fmt::Display) -> Error {
    Error::Doc("doc_damaged", e.to_string())
}

/// 读取除 PDF、图片以外的文档。
pub fn parse(path: &Path, kind: Kind) -> Result<Vec<Section>> {
    let sections = match kind {
        Kind::Text => vec![Section { locator: Locator::None, text: decode(&read(path)?) }],
        Kind::Markdown => markdown(&decode(&read(path)?)),
        Kind::Html => {
            let text = html2text::from_read(Cursor::new(decode(&read(path)?).into_bytes()), 200).map_err(damaged)?;
            vec![Section { locator: Locator::None, text }]
        }
        Kind::Docx => docx(path)?,
        Kind::Pptx => pptx(path)?,
        Kind::Sheet => sheet(path)?,
        Kind::Pdf | Kind::Image => return Err(Error::Internal("pdf and images are handled separately".into())),
    };
    Ok(sections.into_iter().filter(|s| !s.text.trim().is_empty()).collect())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(damaged)
}

/// 文本文件不一定是 UTF-8（老的中文文档常是 GBK、Big5），先判断编码再解码。
pub fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode(&bytes[bom_len..]).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            det.feed(bytes, true);
            det.guess(None, chardetng::Utf8Detection::Allow).decode(bytes).0.into_owned()
        }
    }
}

fn markdown(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        let heading =
            (!in_code && line.starts_with('#')).then(|| line.trim_start_matches('#').trim()).filter(|h| !h.is_empty());
        match (heading, out.last_mut()) {
            (Some(h), _) => out.push(Section { locator: Locator::Heading(h.to_string()), text: format!("{h}\n") }),
            (None, Some(last)) => {
                last.text.push_str(line);
                last.text.push('\n');
            }
            (None, None) => out.push(Section { locator: Locator::None, text: format!("{line}\n") }),
        }
    }
    out
}

fn zip_entry(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
    /// 单个内部文件解压后的上限，防止恶意压缩包撑爆内存。
    const LIMIT: u64 = 256 * 1024 * 1024;
    let entry = archive.by_name(name).map_err(damaged)?;
    let mut out = Vec::new();
    entry.take(LIMIT + 1).read_to_end(&mut out).map_err(damaged)?;
    if out.len() as u64 > LIMIT {
        return Err(Error::Doc("doc_too_large", name.to_string()));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    zip::ZipArchive::new(std::fs::File::open(path).map_err(damaged)?).map_err(damaged)
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

fn attr(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key).map(|a| a.value.to_string())
}

fn is_heading_style(style: &str) -> bool {
    let s = style.to_ascii_lowercase();
    s.starts_with("heading") || s.contains("标题") || s == "title" || (s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
}

/// Word：正文和表格里的文字；按标题样式分段。
fn docx(path: &Path) -> Result<Vec<Section>> {
    let xml = zip_entry(&mut open_zip(path)?, "word/document.xml")?;
    let mut reader = Reader::from_str(&xml);
    let mut out = vec![Section { locator: Locator::None, text: String::new() }];
    let (mut para, mut style, mut in_text) = (String::new(), String::new(), false);
    loop {
        match reader.read_event().map_err(damaged)? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                "w:t" => in_text = true,
                "w:tab" => para.push('\t'),
                "w:br" | "w:cr" => para.push('\n'),
                "w:pStyle" => style = attr(&e, "w:val").unwrap_or_default(),
                _ => {}
            },
            Event::Text(t) if in_text => para.push_str(&t),
            Event::GeneralRef(r) if in_text => {
                if let Some(c) = entity(&r) {
                    para.push(c);
                }
            }
            Event::End(e) => match e.name().as_ref() {
                "w:t" => in_text = false,
                "w:p" => {
                    let text = para.trim();
                    if is_heading_style(&style) && !text.is_empty() && text.chars().count() <= 100 {
                        out.push(Section { locator: Locator::Heading(text.to_string()), text: String::new() });
                    }
                    let last = out.last_mut().expect("at least one section");
                    last.text.push_str(&para);
                    // 表格单元格里的段落先不换行，等这一行结束
                    last.text.push('\n');
                    para.clear();
                    style.clear();
                }
                "w:tc" => {
                    let last = out.last_mut().expect("at least one section");
                    while last.text.ends_with('\n') {
                        last.text.pop();
                    }
                    last.text.push_str(" | ");
                }
                "w:tr" => out.last_mut().expect("at least one section").text.push('\n'),
                _ => {}
            },
            _ => {}
        }
    }
    Ok(out)
}

/// PowerPoint：每张幻灯片一段。
fn pptx(path: &Path) -> Result<Vec<Section>> {
    let mut archive = open_zip(path)?;
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .flatten()
        .filter_map(|n| {
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    slides.sort();
    let mut out = Vec::new();
    for (num, name) in slides {
        let xml = zip_entry(&mut archive, &name)?;
        let mut reader = Reader::from_str(&xml);
        let (mut text, mut in_text) = (String::new(), false);
        loop {
            match reader.read_event().map_err(damaged)? {
                Event::Eof => break,
                Event::Start(e) if e.name().as_ref() == "a:t" => in_text = true,
                Event::Text(t) if in_text => text.push_str(&t),
                Event::GeneralRef(r) if in_text => {
                    if let Some(c) = entity(&r) {
                        text.push(c);
                    }
                }
                Event::End(e) => match e.name().as_ref() {
                    "a:t" => in_text = false,
                    "a:p" => text.push('\n'),
                    _ => {}
                },
                _ => {}
            }
        }
        out.push(Section { locator: Locator::Slide(num), text });
    }
    Ok(out)
}

/// 表格：每个工作表一段，每行一行，单元格用竖线隔开。
fn sheet(path: &Path) -> Result<Vec<Section>> {
    use calamine::{open_workbook_auto, Data, Reader as _};
    let mut book = open_workbook_auto(path).map_err(damaged)?;
    let mut out = Vec::new();
    for name in book.sheet_names().to_vec() {
        let Ok(range) = book.worksheet_range(&name) else { continue };
        let mut text = String::new();
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    // 整数不显示成 3.0
                    Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
                    other => other.to_string(),
                })
                .collect();
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            text.push_str(cells.join(" | ").trim_end_matches([' ', '|']));
            text.push('\n');
        }
        out.push(Section { locator: Locator::Sheet(name), text });
    }
    Ok(out)
}

// ---------- PDF ----------

/// pdfium 每个进程只能初始化一次，这里全局保留一个实例；调用排队进行。
static PDFIUM: Mutex<Option<Pdfium>> = Mutex::new(None);

fn with_pdf<T>(lib: &Path, path: &Path, f: impl FnOnce(&PdfDocument) -> Result<T>) -> Result<T> {
    if !lib.is_file() {
        return Err(Error::Doc("pdf_engine_missing", lib.display().to_string()));
    }
    let mut guard = PDFIUM.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        let bindings = Pdfium::bind_to_library(lib)
            .map_err(|e| Error::Doc("pdf_engine_missing", format!("{}: {e}", lib.display())))?;
        *guard = Some(Pdfium::new(bindings));
    }
    let pdfium = guard.as_ref().expect("initialised above");
    let doc = pdfium.load_pdf_from_file(path, None).map_err(|e| match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            Error::Doc("doc_encrypted", String::new())
        }
        other => damaged(other),
    })?;
    f(&doc)
}

/// 每一页的文字。扫描页得到的是空字符串。
pub fn pdf_pages(lib: &Path, path: &Path) -> Result<Vec<String>> {
    with_pdf(lib, path, |doc| {
        Ok(doc.pages().iter().map(|page| page.text().map(|t| t.all()).unwrap_or_default()).collect())
    })
}

/// 把一页画成图片（PNG），供看图模型识别。
pub fn pdf_render_page(lib: &Path, path: &Path, index: usize) -> Result<Vec<u8>> {
    with_pdf(lib, path, |doc| {
        let page = doc.pages().get(index as _).map_err(damaged)?;
        let config = PdfRenderConfig::new().set_target_width(1600).set_maximum_height(2400);
        let image = page.render_with_config(&config).map_err(damaged)?.as_image().map_err(damaged)?;
        encode_png(&image)
    })
}

fn encode_png(image: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image.to_rgb8().write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(damaged)?;
    Ok(out)
}

/// 读入图片并缩到适合识别的大小。
pub fn image_for_ocr(path: &Path) -> Result<Vec<u8>> {
    let img = image::ImageReader::open(path)
        .map_err(damaged)?
        .with_guessed_format()
        .map_err(damaged)?
        .decode()
        .map_err(damaged)?;
    let img = if img.width().max(img.height()) > 2000 {
        img.resize(2000, 2000, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    encode_png(&img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_gbk_and_utf8_with_bom() {
        let (gbk, _, _) = encoding_rs::GBK.encode("保修条款：整机保修十二个月，主要部件保修二十四个月。");
        assert!(decode(&gbk).contains("整机保修十二个月"));
        assert_eq!(decode("\u{feff}你好".as_bytes()), "你好");
    }

    #[test]
    fn markdown_splits_on_headings_but_not_inside_code() {
        let s = markdown("前言\n# 第一章\n内容一\n```\n# 不是标题\n```\n## 第二节\n内容二\n");
        let locs: Vec<_> = s.iter().map(|x| x.locator.clone()).collect();
        assert_eq!(locs, vec![Locator::None, Locator::Heading("第一章".into()), Locator::Heading("第二节".into())]);
        assert!(s[1].text.contains("# 不是标题"));
    }

    #[test]
    fn locator_serializes_compactly() {
        assert_eq!(serde_json::to_string(&Locator::Page(3)).unwrap(), r#"{"kind":"page","value":3}"#);
        assert_eq!(serde_json::to_string(&Locator::None).unwrap(), r#"{"kind":"none"}"#);
    }

    #[test]
    fn knows_supported_types() {
        assert_eq!(kind_of("PDF"), Some(Kind::Pdf));
        assert_eq!(kind_of("xlsx"), Some(Kind::Sheet));
        assert_eq!(kind_of("zip"), None);
        assert_eq!(kind_of("doc"), None);
    }
}
