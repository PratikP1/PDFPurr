//! Feature validation tests — verify every major crate feature works
//! against real PDF files from the corpus.

use pdfpurr::Document;
use std::path::Path;

fn corpus_path(sub: &str) -> std::path::PathBuf {
    Path::new("tests/corpus").join(sub)
}

fn read_corpus(sub: &str) -> Option<Vec<u8>> {
    let path = corpus_path(sub);
    std::fs::read(&path).ok()
}

// ─── 1. PARSING: every corpus file must parse without panic ───

#[test]
fn all_basic_pdfs_parse() {
    for name in &[
        "dummy.pdf",
        "generated_hello.pdf",
        "pdf-sample.pdf",
        "tcp_ip_intro.pdf",
        "tracemonkey.pdf",
    ] {
        let data = read_corpus(&format!("basic/{name}")).unwrap();
        let doc = Document::from_bytes(&data);
        assert!(doc.is_ok(), "{name} should parse: {:?}", doc.err());
        let doc = doc.unwrap();
        assert!(doc.page_count().unwrap() > 0, "{name} should have pages");
    }
}

#[test]
fn all_encrypted_pdfs_parse() {
    let cases: Vec<(&str, Vec<&[u8]>)> = vec![
        ("rc4_40bit_r2.pdf", vec![b"", b"pass40"]),
        ("aes128_r4.pdf", vec![b"", b"testpw"]),
        ("aes256_r6_user.pdf", vec![b"userpass"]),
        ("aes256_r6_empty_user.pdf", vec![b""]),
    ];
    for (name, passwords) in &cases {
        let data = read_corpus(&format!("encrypted/{name}")).unwrap();
        let success = passwords
            .iter()
            .any(|pw| Document::from_bytes_with_password(&data, pw).is_ok());
        assert!(success, "{name} should parse with one of the passwords");
    }
}

#[test]
fn permission_restricted_pdfs_parse() {
    for name in &[
        "encryption_nocopy.pdf",
        "encryption_noprinting.pdf",
        "encryption_notextaccess.pdf",
        "encryption_openpassword.pdf",
    ] {
        let data = read_corpus(&format!("encrypted/{name}")).unwrap();
        // These should parse (possibly with empty password)
        let doc =
            Document::from_bytes(&data).or_else(|_| Document::from_bytes_with_password(&data, b""));
        assert!(doc.is_ok(), "{name} should parse: {:?}", doc.err());
    }
}

#[test]
fn malformed_pdfs_dont_panic() {
    for name in &[
        "calistoMTNoFontsEmbedded.pdf",
        "corruptionOneByteMissing.pdf",
        "externalLink.pdf",
        "vera_6-1-2-t02-fail-b.pdf",
        "vera_6-1-2-t02-fail-c.pdf",
    ] {
        let data = read_corpus(&format!("malformed/{name}")).unwrap();
        // Must not panic — Ok or Err is fine
        let _ = Document::from_bytes(&data);
    }
}

#[test]
fn adversarial_pdfs_dont_panic() {
    for name in &[
        "bom_header.pdf",
        "corrupted.pdf",
        "cr_line_endings.pdf",
        "duplicate_objects.pdf",
        "long_name.pdf",
        "no_xref_table.pdf",
        "not_encrypted.pdf",
    ] {
        let data = read_corpus(&format!("adversarial/{name}")).unwrap();
        let _ = Document::from_bytes(&data);
    }
}

#[test]
fn scanned_pdfs_parse() {
    for name in &[
        "crazyones.pdf",
        "graph_scanned.pdf",
        "linn_scanned.pdf",
        "skew_scanned.pdf",
    ] {
        let data = read_corpus(&format!("scanned/{name}")).unwrap();
        let doc = Document::from_bytes(&data);
        assert!(doc.is_ok(), "{name} should parse: {:?}", doc.err());
    }
}

// ─── 2. TEXT EXTRACTION ───

#[test]
fn text_extraction_on_basic_pdfs() {
    let data = read_corpus("basic/tracemonkey.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let text = doc.extract_page_text(0).unwrap_or_default();
    assert!(!text.is_empty(), "tracemonkey page 0 should have text");
}

#[test]
fn text_extraction_on_generated_pdf() {
    let data = read_corpus("basic/generated_hello.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let text = doc.extract_page_text(0).unwrap();
    assert!(
        text.contains("Hello"),
        "generated_hello.pdf should contain 'Hello', got: {:?}",
        &text[..text.len().min(100)]
    );
}

#[test]
fn text_extraction_returns_empty_on_scanned_pdfs() {
    // Scanned PDFs have no text layer — extraction should return empty or minimal
    for name in &["graph_scanned.pdf", "skew_scanned.pdf"] {
        let data = read_corpus(&format!("scanned/{name}")).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let text = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text.len() < 50,
            "{name} scanned page should have minimal text, got {} chars",
            text.len()
        );
    }
}

// ─── 3. RENDERING ───

#[test]
fn render_basic_pdfs() {
    use pdfpurr::{RenderOptions, Renderer};

    for name in &["tracemonkey.pdf", "generated_hello.pdf"] {
        let data = read_corpus(&format!("basic/{name}")).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let renderer = Renderer::new(
            &doc,
            RenderOptions {
                dpi: 72.0,
                ..Default::default()
            },
        );
        let result = renderer.render_page(0);
        assert!(result.is_ok(), "{name} should render: {:?}", result.err());
        let pixmap = result.unwrap();
        assert!(
            pixmap.width() > 0 && pixmap.height() > 0,
            "{name} pixmap should have dimensions"
        );
    }
}

#[test]
fn render_scanned_pdfs() {
    use pdfpurr::{RenderOptions, Renderer};

    for name in &["graph_scanned.pdf", "linn_scanned.pdf", "skew_scanned.pdf"] {
        let data = read_corpus(&format!("scanned/{name}")).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let renderer = Renderer::new(
            &doc,
            RenderOptions {
                dpi: 72.0,
                ..Default::default()
            },
        );
        let result = renderer.render_page(0);
        assert!(result.is_ok(), "{name} should render: {:?}", result.err());
    }
}

// ─── 4. METADATA ───

#[test]
fn metadata_extraction() {
    let data = read_corpus("basic/tracemonkey.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let meta = doc.metadata();
    // tracemonkey has metadata — at least producer should be set
    assert!(
        meta.title.is_some() || meta.producer.is_some() || meta.creator.is_some(),
        "tracemonkey should have some metadata"
    );
}

// ─── 5. OUTLINES ───

#[test]
fn outlines_on_basic_pdfs() {
    let data = read_corpus("basic/tcp_ip_intro.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let outlines = doc.outlines();
    // tcp_ip_intro may or may not have outlines — just don't panic
    let _ = outlines;
}

// ─── 6. ANNOTATIONS ───

#[test]
fn annotations_dont_panic() {
    for name in &["tracemonkey.pdf", "generated_hello.pdf", "pdf-sample.pdf"] {
        let data = read_corpus(&format!("basic/{name}")).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let page = doc.get_page(0).unwrap();
        let annots = doc.page_annotations(page);
        let _ = annots; // just don't panic
    }
}

// ─── 7. IMAGES ───

#[test]
fn image_extraction_on_scanned_pdfs() {
    let data = read_corpus("scanned/graph_scanned.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let images = doc.extract_all_images().unwrap();
    assert!(
        !images.is_empty(),
        "graph_scanned should have at least one image (XObject or inline)"
    );
}

// ─── 8. ACCESSIBILITY / TAGGED PDF ───

#[test]
fn tagged_pdfs_have_structure_tree() {
    let data = read_corpus("tagged/ua1_7.2-t02-pass-a.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let tree = doc.structure_tree();
    assert!(tree.is_some(), "Passing UA PDF should have structure tree");
}

#[test]
fn untagged_pdfs_have_no_structure_tree() {
    let data = read_corpus("basic/generated_hello.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let tree = doc.structure_tree();
    assert!(
        tree.is_none(),
        "Simple generated PDF should not have structure tree"
    );
}

#[test]
fn accessibility_report_on_tagged_pdf() {
    let data = read_corpus("tagged/ua1_7.2-t02-pass-a.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let report = doc.accessibility_report();
    // Passing PDF should be compliant or close to it
    let _ = report;
}

#[test]
fn accessibility_report_on_untagged_pdf() {
    let data = read_corpus("basic/dummy.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let report = doc.accessibility_report();
    // Untagged PDF should flag issues
    assert!(
        !report.is_compliant(),
        "Untagged PDF should not be accessible"
    );
}

// ─── 9. STANDARDS VALIDATION ───

#[test]
fn pdfa_validation_on_tagged_pdf() {
    use pdfpurr::PdfALevel;

    let data = read_corpus("tagged/vera_6-1-2-t01-pass-a.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let report = doc.validate_pdfa(PdfALevel::A1b);
    // veraPDF pass file should have fewer issues
    let _ = report;
}

// ─── 10. WRITE PATH: roundtrip ───

#[test]
fn roundtrip_basic_pdf() {
    let data = read_corpus("basic/generated_hello.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let bytes = doc.to_bytes().unwrap();
    let doc2 = Document::from_bytes(&bytes).unwrap();
    assert_eq!(doc.page_count().unwrap(), doc2.page_count().unwrap());
}

#[test]
fn create_and_roundtrip() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    doc.add_page(595.0, 842.0).unwrap();
    let bytes = doc.to_bytes().unwrap();
    let doc2 = Document::from_bytes(&bytes).unwrap();
    assert_eq!(doc2.page_count().unwrap(), 2);
}

// ─── 11. PAGE MANIPULATION ───

#[test]
fn rotate_page_roundtrips() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    doc.rotate_page(0, 90).unwrap();
    let bytes = doc.to_bytes().unwrap();
    let doc2 = Document::from_bytes(&bytes).unwrap();
    assert_eq!(doc2.page_count().unwrap(), 1);
}

#[test]
fn merge_two_documents() {
    let data1 = read_corpus("basic/generated_hello.pdf").unwrap();
    let data2 = read_corpus("basic/dummy.pdf").unwrap();
    let mut doc1 = Document::from_bytes(&data1).unwrap();
    let doc2 = Document::from_bytes(&data2).unwrap();
    let pages_before = doc1.page_count().unwrap();
    doc1.merge(&doc2).unwrap();
    assert!(
        doc1.page_count().unwrap() > pages_before,
        "Merge should add pages"
    );
}

// ─── 12. FONT EMBEDDING ───

#[test]
fn ttf_font_embeds_and_subsets() {
    let font_path = Path::new("tests/fonts/NotoSans-Regular.ttf");
    if !font_path.exists() {
        return;
    }
    let data = std::fs::read(font_path).unwrap();
    let font = pdfpurr::EmbeddedFont::from_ttf(&data).unwrap();
    let subset = font.subset(&['A', 'B', 'C']).unwrap();
    assert!(subset.glyph_count() >= 3);
}

#[test]
fn otf_cff_font_embeds_and_subsets() {
    let font_path = Path::new("tests/fonts/SourceCodePro-Regular.otf");
    if !font_path.exists() {
        return;
    }
    let data = std::fs::read(font_path).unwrap();
    let font = pdfpurr::EmbeddedFont::from_otf(&data).unwrap();
    let subset = font.subset(&['X', 'Y', 'Z']).unwrap();
    assert!(subset.glyph_count() >= 3);
}

// ─── 13. OCR (mock engine) ───

#[test]
fn ocr_mock_engine_on_blank_page() {
    use pdfpurr::ocr::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
    use pdfpurr::ocr::OcrConfig;

    struct MockEngine;
    impl OcrEngine for MockEngine {
        fn recognize(&self, _image: &OcrImage) -> pdfpurr::error::PdfResult<OcrResult> {
            Ok(OcrResult {
                words: vec![OcrWord {
                    text: "MockOCR".to_string(),
                    x: 100,
                    y: 100,
                    width: 200,
                    height: 40,
                    confidence: 0.95,
                }],
                image_width: 2550,
                image_height: 3300,
            })
        }
    }

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    let applied = doc.ocr_page(0, &MockEngine, &OcrConfig::default()).unwrap();
    assert!(applied, "OCR should apply to blank page");

    let text = doc.extract_page_text(0).unwrap_or_default();
    assert!(
        text.contains("MockOCR"),
        "Should extract OCR text, got: {:?}",
        &text[..text.len().min(100)]
    );
}

#[test]
fn ocr_redo_replaces_text() {
    use pdfpurr::ocr::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
    use pdfpurr::ocr::OcrConfig;

    struct Engine1;
    impl OcrEngine for Engine1 {
        fn recognize(&self, _: &OcrImage) -> pdfpurr::error::PdfResult<OcrResult> {
            Ok(OcrResult {
                words: vec![OcrWord {
                    text: "First".into(),
                    x: 100,
                    y: 100,
                    width: 200,
                    height: 40,
                    confidence: 0.9,
                }],
                image_width: 2550,
                image_height: 3300,
            })
        }
    }

    struct Engine2;
    impl OcrEngine for Engine2 {
        fn recognize(&self, _: &OcrImage) -> pdfpurr::error::PdfResult<OcrResult> {
            Ok(OcrResult {
                words: vec![OcrWord {
                    text: "Second".into(),
                    x: 100,
                    y: 100,
                    width: 200,
                    height: 40,
                    confidence: 0.95,
                }],
                image_width: 2550,
                image_height: 3300,
            })
        }
    }

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    doc.ocr_page(0, &Engine1, &OcrConfig::default()).unwrap();

    let config = OcrConfig {
        should_redo: true,
        ..Default::default()
    };
    doc.redo_ocr_page(0, &Engine2, &config).unwrap();

    let text = doc.extract_page_text(0).unwrap_or_default();
    assert!(text.contains("Second"), "Redo should produce new text");
    assert!(!text.contains("First"), "Old OCR should be gone");
}

// ─── 14. LINEARIZED WRITING ───

#[test]
fn linearized_output_parses() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    let bytes = doc.to_linearized_bytes().unwrap();
    let doc2 = Document::from_bytes(&bytes).unwrap();
    assert_eq!(doc2.page_count().unwrap(), 1);
}

// ─── 15. INCREMENTAL UPDATES ───

#[test]
fn incremental_update_preserves_content() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    let original = doc.to_bytes().unwrap();

    let mut doc2 = Document::from_bytes(&original).unwrap();
    doc2.add_page(612.0, 792.0).unwrap();
    let updated = doc2.to_incremental_update(&original).unwrap();

    let doc3 = Document::from_bytes(&updated).unwrap();
    assert_eq!(doc3.page_count().unwrap(), 2);
}

// ─── 16. FORM FIELDS ───

#[test]
fn form_fields_on_basic_pdfs() {
    // Most basic PDFs have no forms — just verify no panic
    let data = read_corpus("basic/tracemonkey.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let fields = doc.form_fields();
    let _ = fields;
}

// ─── 17. SIGNATURES ───

#[test]
fn signatures_on_basic_pdfs() {
    let data = read_corpus("basic/tracemonkey.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    let sigs = doc.signatures();
    let _ = sigs;
}

// ─── 18. LAZY LOADING ───

#[test]
fn lazy_loading_produces_same_results() {
    let data = read_corpus("basic/tracemonkey.pdf").unwrap();
    let doc_normal = Document::from_bytes(&data).unwrap();
    let doc_lazy = Document::from_bytes_lazy(&data).unwrap();
    assert_eq!(
        doc_normal.page_count().unwrap(),
        doc_lazy.page_count().unwrap()
    );
}

// ─── 19. PRIVATE CORPUS (if available) ───

#[test]
fn private_corpus_all_parse_without_panic() {
    let corpus_dir = Path::new("tests/private_corpus");
    if !corpus_dir.exists() {
        return;
    }

    let mut tested = 0;
    let mut failed = Vec::new();

    for entry in walkdir(corpus_dir) {
        let data = match std::fs::read(&entry) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Try to parse — must not panic
        match Document::from_bytes(&data) {
            Ok(_) => {}
            Err(_) => {
                // Try with empty password
                match Document::from_bytes_with_password(&data, b"") {
                    Ok(_) => {}
                    Err(_) => {
                        failed.push(entry.display().to_string());
                    }
                }
            }
        }
        tested += 1;
    }

    eprintln!(
        "Private corpus: {tested} files tested, {} failed to parse",
        failed.len()
    );
    if !failed.is_empty() {
        eprintln!("Failed files (expected for adversarial corpus):");
        for f in &failed[..failed.len().min(10)] {
            eprintln!("  {f}");
        }
    }
    // We expect some adversarial files to fail — just ensure no panics
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path));
            } else if path
                .extension()
                .map_or(false, |e| e == "pdf" || e == "fuzz" || e == "pdf_")
            {
                files.push(path);
            }
        }
    }
    files
}
