//! 用真实格式的小样本验证各类文档的读取。PDF 需要 pdfium：先运行 scripts/fetch-vendor.sh。

use std::path::{Path, PathBuf};

use commonbot_core::kb::ingest::find_pdfium;
use commonbot_core::kb::parse::{self, Kind, Locator};
use commonbot_core::Error;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn pdfium() -> PathBuf {
    let vendor = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium");
    let dirs: Vec<PathBuf> = std::fs::read_dir(&vendor)
        .map(|d| d.flatten().flat_map(|e| [e.path().join("lib"), e.path().join("bin")]).collect())
        .unwrap_or_default();
    let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    find_pdfium(&refs).expect("pdfium not found — run scripts/fetch-vendor.sh first")
}

#[test]
fn word_keeps_headings_tables_and_entities() {
    let s = parse::parse(&fixture("sample.docx"), Kind::Docx).unwrap();
    let locs: Vec<_> = s.iter().map(|x| x.locator.clone()).collect();
    assert_eq!(
        locs,
        vec![Locator::None, Locator::Heading("一、报修流程".into()), Locator::Heading("二、收费标准".into())]
    );
    assert!(s[1].text.contains("购买凭证 & 设备序列号"), "{}", s[1].text);
    assert!(s[1].text.contains("第一行\n第二行"));
    assert!(s[2].text.contains("项目 | 价格"), "{}", s[2].text);
    assert!(s[2].text.contains("换屏 | 600 元"), "{}", s[2].text);
}

#[test]
fn slides_come_in_numeric_order() {
    let s = parse::parse(&fixture("sample.pptx"), Kind::Pptx).unwrap();
    let locs: Vec<_> = s.iter().map(|x| x.locator.clone()).collect();
    assert_eq!(locs, vec![Locator::Slide(1), Locator::Slide(2), Locator::Slide(10)]);
    assert_eq!(s[0].text.trim(), "第一页\n年度总结");
}

#[test]
fn spreadsheet_rows_by_sheet() {
    let s = parse::parse(&fixture("sample.xlsx"), Kind::Sheet).unwrap();
    assert_eq!(s[0].locator, Locator::Sheet("中标公告".into()));
    assert_eq!(s[0].text, "项目 | 中标金额 | 年份\n政务云一期 | 1280000 | 2024\n智慧园区 | 356000.5 | 2025\n");
    assert_eq!(s[1].locator, Locator::Sheet("备注".into()));
}

#[test]
fn legacy_chinese_text_is_decoded() {
    let s = parse::parse(&fixture("gbk.txt"), Kind::Text).unwrap();
    assert!(s[0].text.contains("延保服务可在保修期结束前三十天内购买"), "{}", s[0].text);
}

#[test]
fn pdf_text_by_page_and_scanned_pages_are_empty() {
    let lib = pdfium();
    let pages = parse::pdf_pages(&lib, &fixture("text.pdf")).unwrap();
    assert_eq!(pages.len(), 2);
    assert!(pages[0].contains("屏幕属于主要部件，保修二十四个月"), "{}", pages[0]);
    assert!(pages[1].contains("人为摔坏"), "{}", pages[1]);

    let scan = parse::pdf_pages(&lib, &fixture("scan.pdf")).unwrap();
    assert_eq!(scan.len(), 1);
    assert!(scan[0].trim().is_empty(), "scanned page should have no text: {:?}", scan[0]);

    let mixed = parse::pdf_pages(&lib, &fixture("mixed.pdf")).unwrap();
    assert!(mixed[0].contains("投标截止时间") && mixed[1].trim().is_empty());
}

#[test]
fn pdf_page_renders_to_a_real_image() {
    let png = parse::pdf_render_page(&pdfium(), &fixture("scan.pdf"), 0).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!(img.width(), 1600);
    // 页面上有字：不是一张纯白的图
    let rgb = img.to_rgb8();
    let dark = rgb.pixels().filter(|p| p.0[0] < 128).count();
    assert!(dark > 500, "only {dark} dark pixels");
    assert!(parse::pdf_render_page(&pdfium(), &fixture("scan.pdf"), 5).is_err());
}

#[test]
fn damaged_files_are_reported_not_crashed() {
    assert!(matches!(parse::pdf_pages(&pdfium(), &fixture("broken.pdf")), Err(Error::Doc("doc_damaged", _))));
    assert!(matches!(parse::parse(&fixture("gbk.txt"), Kind::Docx), Err(Error::Doc("doc_damaged", _))));
    assert!(matches!(
        parse::pdf_pages(Path::new("/nonexistent/libpdfium.so"), &fixture("text.pdf")),
        Err(Error::Doc("pdf_engine_missing", _))
    ));
}

#[test]
fn image_is_prepared_for_recognition() {
    let png = parse::image_for_ocr(&fixture("scan.png")).unwrap();
    assert_eq!(image::load_from_memory(&png).unwrap().width(), 800);
}
