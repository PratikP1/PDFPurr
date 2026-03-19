//! Integration tests for PDFPurr.
//!
//! These tests exercise the full pipeline: build a PDF in memory,
//! parse it back, and verify the result. No external fixture files needed.

use pdfpurr::rendering::{RenderOptions, Renderer};
use pdfpurr::Document;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a minimal valid PDF byte stream with optional page content.
fn build_pdf(content: &[u8]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let obj1_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let obj2_offset = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let obj3_offset = pdf.len();
    if content.is_empty() {
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
    } else {
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        let obj4_offset = pdf.len();
        let header = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len());
        pdf.extend_from_slice(header.as_bytes());
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n");
        pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_offset).as_bytes());
        pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n");
        pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        pdf.extend_from_slice(b"%%EOF\n");
        return pdf;
    }

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
    pdf.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
    pdf.extend_from_slice(b"%%EOF\n");
    pdf
}

/// Builds a PDF with Info dictionary metadata.
fn build_pdf_with_info(title: &str, author: &str) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let obj1_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let obj2_offset = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let obj3_offset = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    let obj4_offset = pdf.len();
    let info = format!(
        "4 0 obj\n<< /Title ({}) /Author ({}) >>\nendobj\n",
        title, author
    );
    pdf.extend_from_slice(info.as_bytes());

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n");
    pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_offset).as_bytes());
    pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R /Info 4 0 R >>\nstartxref\n");
    pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
    pdf.extend_from_slice(b"%%EOF\n");
    pdf
}

// ---------------------------------------------------------------------------
// Document round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn parse_minimal_pdf() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    assert_eq!(doc.version.major, 1);
    assert_eq!(doc.version.minor, 4);
}

#[test]
fn parse_pdf_page_count() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(pages.len(), 1);
}

#[test]
fn parse_pdf_with_content_stream() {
    let content = b"q 1 0 0 1 0 0 cm Q";
    let pdf = build_pdf(content);
    let doc = Document::from_bytes(&pdf).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(pages.len(), 1);
}

#[test]
fn metadata_round_trip() {
    let pdf = build_pdf_with_info("Test Document", "PDFPurr");
    let doc = Document::from_bytes(&pdf).unwrap();
    let meta = doc.metadata();
    assert_eq!(meta.title.as_deref(), Some("Test Document"));
    assert_eq!(meta.author.as_deref(), Some("PDFPurr"));
}

#[test]
fn render_blank_page_produces_correct_size() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    let pixmap = renderer.render_page(0).unwrap();
    // 612×792 at 72 DPI = 612×792 pixels
    assert_eq!(pixmap.width(), 612);
    assert_eq!(pixmap.height(), 792);
}

#[test]
fn render_blank_page_is_white() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    let pixmap = renderer.render_page(0).unwrap();
    // Background is opaque white
    assert!(pixmap
        .pixels()
        .iter()
        .all(|p| p.red() == 255 && p.alpha() == 255));
}

#[test]
fn render_with_custom_dpi() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions {
        dpi: 144.0,
        ..Default::default()
    };
    let renderer = Renderer::new(&doc, opts);
    let pixmap = renderer.render_page(0).unwrap();
    // 612×792 at 144 DPI = 1224×1584 pixels
    assert_eq!(pixmap.width(), 1224);
    assert_eq!(pixmap.height(), 1584);
}

#[test]
fn render_colored_rect_changes_pixels() {
    // Draw a red filled rectangle
    let content = b"q 1 0 0 rg 100 100 200 200 re f Q";
    let pdf = build_pdf(content);
    let doc = Document::from_bytes(&pdf).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    let pixmap = renderer.render_page(0).unwrap();

    // Should have some red pixels (non-white)
    let has_red = pixmap
        .pixels()
        .iter()
        .any(|p| p.red() == 255 && p.green() == 0 && p.blue() == 0);
    assert!(has_red, "Expected red pixels from filled rectangle");
}

#[test]
fn render_stroked_line_changes_pixels() {
    // Draw a blue stroked line
    let content = b"q 0 0 1 RG 2 w 72 72 m 540 720 l S Q";
    let pdf = build_pdf(content);
    let doc = Document::from_bytes(&pdf).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    let pixmap = renderer.render_page(0).unwrap();

    // Should have some non-white pixels (stroked line changes the output)
    let has_non_white = pixmap
        .pixels()
        .iter()
        .any(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255);
    assert!(has_non_white, "Expected non-white pixels from stroked line");
}

#[test]
fn render_out_of_bounds_page_returns_error() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    assert!(renderer.render_page(999).is_err());
}

#[test]
fn render_with_custom_background() {
    let pdf = build_pdf(b"");
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions {
        dpi: 72.0,
        background: [0, 0, 0, 255], // black background
    };
    let renderer = Renderer::new(&doc, opts);
    let pixmap = renderer.render_page(0).unwrap();
    // All pixels should be black
    assert!(pixmap
        .pixels()
        .iter()
        .all(|p| p.red() == 0 && p.green() == 0 && p.blue() == 0 && p.alpha() == 255));
}

// ---------------------------------------------------------------------------
// Document write / save round-trip
// ---------------------------------------------------------------------------

#[test]
fn new_document_serialize_roundtrip() {
    let doc = Document::new();
    let buf = doc.to_bytes().unwrap();
    // Parse it back
    let doc2 = Document::from_bytes(&buf).unwrap();
    assert_eq!(doc2.pages().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Write → read roundtrip tests
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_add_page_preserves_dimensions() {
    let mut doc = Document::new();
    doc.add_page(595.0, 842.0).unwrap(); // A4
    doc.add_page(612.0, 792.0).unwrap(); // US Letter

    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.page_count().unwrap(), 2);

    let page0 = parsed.get_page(0).unwrap();
    let mb0 = parsed.page_media_box(page0).unwrap();
    assert_eq!(mb0, [0.0, 0.0, 595.0, 842.0]);

    let page1 = parsed.get_page(1).unwrap();
    let mb1 = parsed.page_media_box(page1).unwrap();
    assert_eq!(mb1, [0.0, 0.0, 612.0, 792.0]);
}

#[test]
fn roundtrip_merge_preserves_pages() {
    let mut doc1 = Document::new();
    doc1.add_page(612.0, 792.0).unwrap();
    doc1.add_page(612.0, 792.0).unwrap();

    let mut doc2 = Document::new();
    doc2.add_page(595.0, 842.0).unwrap();

    doc1.merge(&doc2).unwrap();

    let bytes = doc1.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.page_count().unwrap(), 3);
}

#[test]
fn roundtrip_form_field_preserves_value() {
    use pdfpurr::core::objects::*;

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();

    // Create a text field with a value
    let mut field = Dictionary::new();
    field.insert(PdfName::new("FT"), Object::Name(PdfName::new("Tx")));
    field.insert(
        PdfName::new("T"),
        Object::String(PdfString {
            bytes: b"username".to_vec(),
            format: StringFormat::Literal,
        }),
    );
    field.insert(
        PdfName::new("V"),
        Object::String(PdfString {
            bytes: b"alice".to_vec(),
            format: StringFormat::Literal,
        }),
    );
    let field_id = doc.add_object(Object::Dictionary(field));

    // Wire AcroForm into catalog
    if let Some(Object::Dictionary(catalog)) = doc.get_object_mut((1, 0)) {
        let mut acro = Dictionary::new();
        acro.insert(
            PdfName::new("Fields"),
            Object::Array(vec![Object::Reference(IndirectRef::new(
                field_id.0, field_id.1,
            ))]),
        );
        catalog.insert(PdfName::new("AcroForm"), Object::Dictionary(acro));
    }

    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    let fields = parsed.form_fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "username");
    assert_eq!(fields[0].value, Some("alice".to_string()));
}

#[test]
fn roundtrip_multiple_serialize_cycles() {
    // Serialize → parse → serialize → parse should be stable
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    doc.add_page(595.0, 842.0).unwrap();

    let bytes1 = doc.to_bytes().unwrap();
    let doc2 = Document::from_bytes(&bytes1).unwrap();
    let bytes2 = doc2.to_bytes().unwrap();
    let doc3 = Document::from_bytes(&bytes2).unwrap();

    assert_eq!(doc3.page_count().unwrap(), 2);
}

// ---------------------------------------------------------------------------
// Invalid PDF resilience
// ---------------------------------------------------------------------------

#[test]
fn empty_bytes_returns_error() {
    assert!(Document::from_bytes(b"").is_err());
}

#[test]
fn garbage_bytes_returns_error() {
    assert!(Document::from_bytes(b"not a pdf at all").is_err());
}

#[test]
fn truncated_pdf_returns_error() {
    assert!(Document::from_bytes(b"%PDF-1.4\n").is_err());
}

// ---------------------------------------------------------------------------
// CJK font integration
// ---------------------------------------------------------------------------

#[test]
fn cidfont_subset_korean_chars() {
    use pdfpurr::fonts::cidfont::CidFont;

    let font_path = "C:\\Windows\\Fonts\\malgun.ttf";
    let data = match std::fs::read(font_path) {
        Ok(d) => d,
        Err(_) => return, // Skip if font not available
    };

    let font = CidFont::from_ttf(&data).unwrap();
    // Korean text: "안녕하세요" (hello)
    let chars: Vec<char> = "안녕하세요".chars().collect();
    let subset = font.subset(&chars).unwrap();

    // Subset should contain the Korean glyphs
    assert!(
        subset.glyph_count() >= chars.len(),
        "subset should have at least {} glyphs for Korean text, got {}",
        chars.len(),
        subset.glyph_count()
    );

    // Encoding should produce 2 bytes per character (CID = big-endian u16)
    let encoded = subset.encode_text("안녕하세요");
    assert_eq!(
        encoded.len(),
        chars.len() * 2,
        "CID encoding should be 2 bytes per char"
    );

    // ToUnicode CMap should be valid
    let cmap = subset.to_unicode_cmap().unwrap();
    let cmap_data = cmap.decode_data().unwrap();
    let cmap_str = String::from_utf8(cmap_data).unwrap();
    assert!(cmap_str.contains("begincmap"));
}

#[test]
fn cidfont_mixed_latin_and_cjk() {
    use pdfpurr::fonts::cidfont::CidFont;

    let font_path = "C:\\Windows\\Fonts\\malgun.ttf";
    let data = match std::fs::read(font_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let font = CidFont::from_ttf(&data).unwrap();
    // Mixed: Latin + Korean
    let chars: Vec<char> = "Hello 안녕".chars().collect();
    let subset = font.subset(&chars).unwrap();

    let encoded = subset.encode_text("Hello 안녕");
    // 7 visible chars + 1 space = 8 chars × 2 bytes = 16 bytes
    assert_eq!(encoded.len(), 16);
}

// ---------------------------------------------------------------------------
// Generated corpus: diverse PDFs via the library itself
// ---------------------------------------------------------------------------

#[test]
fn generated_multi_page_document() {
    let mut doc = Document::new();
    for _ in 0..10 {
        doc.add_page(612.0, 792.0).unwrap();
    }

    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.page_count().unwrap(), 10);

    // Linearized version too
    let lin = doc.to_linearized_bytes().unwrap();
    let parsed_lin = Document::from_bytes(&lin).unwrap();
    assert_eq!(parsed_lin.page_count().unwrap(), 10);
}

#[test]
fn generated_mixed_page_sizes() {
    let mut doc = Document::new();
    let sizes = [
        (612.0, 792.0),  // US Letter
        (595.0, 842.0),  // A4
        (842.0, 1190.0), // A3
        (297.0, 420.0),  // A5
        (1224.0, 792.0), // Tabloid landscape
    ];
    for (w, h) in &sizes {
        doc.add_page(*w, *h).unwrap();
    }

    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.page_count().unwrap(), 5);

    for (i, (w, h)) in sizes.iter().enumerate() {
        let page = parsed.get_page(i).unwrap();
        let mb = parsed.page_media_box(page).unwrap();
        assert_eq!(mb[2], *w);
        assert_eq!(mb[3], *h);
    }
}

#[test]
fn generated_form_fields_roundtrip() {
    use pdfpurr::core::objects::*;

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();

    // Create multiple fields of different types
    let fields: Vec<(&str, &str, &str)> = vec![
        ("Tx", "name", "Alice"),
        ("Tx", "email", "alice@example.com"),
        ("Tx", "phone", "555-1234"),
    ];

    let mut field_refs = Vec::new();
    for (ft, name, value) in &fields {
        let mut field = Dictionary::new();
        field.insert(PdfName::new("FT"), Object::Name(PdfName::new(*ft)));
        field.insert(
            PdfName::new("T"),
            Object::String(PdfString {
                bytes: name.as_bytes().to_vec(),
                format: StringFormat::Literal,
            }),
        );
        field.insert(
            PdfName::new("V"),
            Object::String(PdfString {
                bytes: value.as_bytes().to_vec(),
                format: StringFormat::Literal,
            }),
        );
        let id = doc.add_object(Object::Dictionary(field));
        field_refs.push(Object::Reference(IndirectRef::new(id.0, id.1)));
    }

    if let Some(Object::Dictionary(catalog)) = doc.get_object_mut((1, 0)) {
        let mut acro = Dictionary::new();
        acro.insert(PdfName::new("Fields"), Object::Array(field_refs));
        catalog.insert(PdfName::new("AcroForm"), Object::Dictionary(acro));
    }

    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    let parsed_fields = parsed.form_fields();
    assert_eq!(parsed_fields.len(), 3);
    assert_eq!(parsed_fields[0].name, "name");
    assert_eq!(parsed_fields[0].value, Some("Alice".to_string()));
    assert_eq!(parsed_fields[2].name, "phone");
}

#[test]
fn generated_incremental_update_roundtrip() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    let original = doc.to_bytes().unwrap();

    // First update: add a page
    let mut doc2 = Document::from_bytes(&original).unwrap();
    doc2.add_page(595.0, 842.0).unwrap();
    let update1 = doc2.to_incremental_update(&original).unwrap();

    // Second update on top of first
    let mut doc3 = Document::from_bytes(&update1).unwrap();
    doc3.add_page(842.0, 1190.0).unwrap();
    let update2 = doc3.to_incremental_update(&update1).unwrap();

    // Final document should have 3 pages
    let final_doc = Document::from_bytes(&update2).unwrap();
    assert_eq!(final_doc.page_count().unwrap(), 3);

    // Original bytes should be preserved at the start
    assert!(update2.starts_with(&original));
}

// ---------------------------------------------------------------------------
// OCR → Tagged PDF roundtrip
// ---------------------------------------------------------------------------

/// Mock OCR engine that returns predictable words for roundtrip testing.
struct MockOcrEngine;

impl pdfpurr::ocr::OcrEngine for MockOcrEngine {
    fn recognize(
        &self,
        image: &pdfpurr::ocr::OcrImage,
    ) -> pdfpurr::error::PdfResult<pdfpurr::ocr::OcrResult> {
        Ok(pdfpurr::ocr::OcrResult {
            words: vec![
                // Heading (tall)
                pdfpurr::ocr::OcrWord {
                    text: "Chapter".into(),
                    x: 200,
                    y: 100,
                    width: 400,
                    height: 60,
                    confidence: 0.95,
                },
                pdfpurr::ocr::OcrWord {
                    text: "One".into(),
                    x: 650,
                    y: 100,
                    width: 200,
                    height: 60,
                    confidence: 0.95,
                },
                // Body text (normal)
                pdfpurr::ocr::OcrWord {
                    text: "This".into(),
                    x: 200,
                    y: 300,
                    width: 100,
                    height: 20,
                    confidence: 0.90,
                },
                pdfpurr::ocr::OcrWord {
                    text: "is".into(),
                    x: 350,
                    y: 300,
                    width: 50,
                    height: 20,
                    confidence: 0.90,
                },
                pdfpurr::ocr::OcrWord {
                    text: "body".into(),
                    x: 450,
                    y: 300,
                    width: 100,
                    height: 20,
                    confidence: 0.88,
                },
                pdfpurr::ocr::OcrWord {
                    text: "text.".into(),
                    x: 600,
                    y: 300,
                    width: 100,
                    height: 20,
                    confidence: 0.92,
                },
            ],
            image_width: image.width,
            image_height: image.height,
        })
    }
}

#[test]
fn ocr_roundtrip_produces_tagged_pdf() {
    use pdfpurr::ocr::OcrConfig;

    // Step 1: Create a blank PDF
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();

    // Step 2: Run OCR with mock engine
    let engine = MockOcrEngine;
    let config = OcrConfig::default();
    let count = doc.ocr_all_pages(&engine, &config).unwrap();
    assert_eq!(count, 1, "Should OCR 1 page");

    // Step 3: Write to bytes
    let bytes = doc.to_bytes().unwrap();

    // Step 4: Parse back
    let doc2 = Document::from_bytes(&bytes).unwrap();

    // Step 5: Verify structure tree exists
    let tree = doc2.structure_tree();
    assert!(tree.is_some(), "Parsed PDF should have structure tree");
    let tree = tree.unwrap();
    assert!(tree.lang.is_some(), "Should have document language");

    // Step 6: Verify text is extractable
    let text = doc2.extract_page_text(0).unwrap();
    assert!(
        text.contains("Chapter") || text.contains("body"),
        "OCR text should be extractable from parsed PDF, got: {}",
        text
    );
}

#[test]
fn ocr_roundtrip_structure_has_heading_and_paragraph() {
    use pdfpurr::ocr::OcrConfig;

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();

    let engine = MockOcrEngine;
    let config = OcrConfig::default();
    doc.ocr_all_pages(&engine, &config).unwrap();

    let tree = doc.structure_tree().unwrap();
    let doc_elem = &tree.children[0];
    assert_eq!(doc_elem.struct_type, "Document");

    // Should have both heading and paragraph children
    let types: Vec<&str> = doc_elem
        .children
        .iter()
        .map(|c| c.struct_type.as_str())
        .collect();

    assert!(
        types.iter().any(|t| t.starts_with('H')),
        "Should detect heading from tall text, got types: {:?}",
        types
    );
    assert!(
        types.contains(&"P"),
        "Should have paragraph element, got types: {:?}",
        types
    );
}

#[test]
fn ocr_roundtrip_accessibility_report() {
    use pdfpurr::ocr::OcrConfig;

    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();

    let engine = MockOcrEngine;
    let config = OcrConfig::default();
    doc.ocr_all_pages(&engine, &config).unwrap();

    // Run accessibility validation
    let report = doc.accessibility_report();

    // Tagged PDF check should pass
    let tagged = report.checks.iter().find(|c| c.id == "tagged-pdf").unwrap();
    assert!(tagged.passed, "OCR'd PDF should be tagged");

    // Language check should pass
    let lang = report
        .checks
        .iter()
        .find(|c| c.id == "document-language")
        .unwrap();
    assert!(lang.passed, "OCR'd PDF should have language set");
}
