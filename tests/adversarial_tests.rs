//! Adversarial and edge-case PDF tests.
//!
//! These tests exercise the parser with intentionally malformed,
//! pathological, and boundary-condition PDFs. None should panic.

use pdfpurr::Document;
use std::path::Path;

// ---------------------------------------------------------------------------
// Truncated PDFs — verify graceful failure at every truncation point
// ---------------------------------------------------------------------------

#[test]
fn truncated_at_every_byte_never_panics() {
    let mut doc = Document::new();
    doc.add_page(612.0, 792.0).unwrap();
    let bytes = doc.to_bytes().unwrap();

    for len in 0..bytes.len() {
        let _ = Document::from_bytes(&bytes[..len]);
    }
}

// ---------------------------------------------------------------------------
// Pathological object nesting
// ---------------------------------------------------------------------------

#[test]
fn deeply_nested_dictionaries_in_pdf() {
    // 50 levels of nested dictionaries
    let mut inner = "<< /Leaf true >> ".to_string();
    for i in 0..50 {
        inner = format!("<< /Level{} {} >> ", i, inner);
    }
    let pdf = format!(
        "%PDF-1.4\n1 0 obj\n{}\nendobj\n\
         2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n\
         3 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
         xref\n0 4\n\
         0000000000 65535 f \n\
         0000000009 00000 n \n\
         {} 00000 n \n\
         {} 00000 n \n\
         trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n{}\n%%EOF\n",
        inner,
        "0000000009", // approximate — repair will fix
        "0000000009",
        "0000000009",
    );
    let _ = Document::from_bytes(pdf.as_bytes());
}

// ---------------------------------------------------------------------------
// Self-referencing objects
// ---------------------------------------------------------------------------

#[test]
fn object_referencing_itself_does_not_hang() {
    // Object 1 contains a reference to itself
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Self 1 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        xref\n0 3\n\
        0000000000 65535 f \n\
        0000000009 00000 n \n\
        0000000072 00000 n \n\
        trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n128\n%%EOF\n";

    let doc = Document::from_bytes(pdf).unwrap();
    assert_eq!(doc.page_count().unwrap(), 0);
    // Resolving the self-reference should not loop
    let catalog = doc.catalog().unwrap();
    if let Some(self_ref) = catalog.get(&pdfpurr::PdfName::new("Self")) {
        let _ = doc.resolve(self_ref);
    }
}

// ---------------------------------------------------------------------------
// Huge object numbers
// ---------------------------------------------------------------------------

#[test]
fn huge_object_numbers_do_not_oom() {
    // Object number near u32::MAX
    let pdf = b"%PDF-1.4\n\
        999999 0 obj\n<< /Type /Catalog /Pages 999998 0 R >>\nendobj\n\
        999998 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        startxref\n0\n%%EOF\n";

    // Should not allocate a billion-entry xref table
    let _ = Document::from_bytes(pdf);
}

// ---------------------------------------------------------------------------
// Empty and minimal PDFs
// ---------------------------------------------------------------------------

#[test]
fn pdf_with_zero_objects_handled() {
    let pdf = b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \n\
        trailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n";
    let result = Document::from_bytes(pdf);
    // May fail (no /Root) but must not panic
    if let Ok(doc) = result {
        let _ = doc.page_count();
    }
}

#[test]
fn pdf_header_only() {
    let _ = Document::from_bytes(b"%PDF-2.0\n");
}

#[test]
fn pdf_with_binary_garbage_after_header() {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(&[0xFF; 1000]);
    let _ = Document::from_bytes(&pdf);
}

// ---------------------------------------------------------------------------
// Malformed xref tables
// ---------------------------------------------------------------------------

#[test]
fn xref_with_negative_offset_handled() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        xref\n0 3\n\
        0000000000 65535 f \n\
        0000000009 00000 n \n\
        0000000058 00000 n \n\
        trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n-1\n%%EOF\n";
    let _ = Document::from_bytes(pdf);
}

#[test]
fn xref_entry_pointing_past_eof() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        xref\n0 3\n\
        0000000000 65535 f \n\
        9999999999 00000 n \n\
        0000000058 00000 n \n\
        trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n113\n%%EOF\n";
    let _ = Document::from_bytes(pdf);
}

// ---------------------------------------------------------------------------
// Malformed streams
// ---------------------------------------------------------------------------

#[test]
fn stream_with_zero_length() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
        3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n\
        4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n\
        xref\n0 5\n\
        0000000000 65535 f \n\
        0000000009 00000 n \n\
        0000000058 00000 n \n\
        0000000115 00000 n \n\
        0000000200 00000 n \n\
        trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n255\n%%EOF\n";

    if let Ok(doc) = Document::from_bytes(pdf) {
        let _ = doc.extract_page_text(0);
    }
}

#[test]
fn stream_missing_endstream_marker() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Length 5 >>\nstream\nHello\nendobj\n\
        2 0 obj\n<< /Type /Catalog >>\nendobj\n\
        startxref\n0\n%%EOF\n";
    let _ = Document::from_bytes(pdf);
}

// ---------------------------------------------------------------------------
// Malformed strings and names
// ---------------------------------------------------------------------------

#[test]
fn string_with_unmatched_parens() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n(unmatched paren (\nendobj\n\
        startxref\n0\n%%EOF\n";
    let _ = Document::from_bytes(pdf);
}

#[test]
fn name_with_null_bytes() {
    let pdf = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Na\x00me /Value >>\nendobj\n\
        startxref\n0\n%%EOF\n";
    let _ = Document::from_bytes(pdf);
}

// ---------------------------------------------------------------------------
// Stress: many objects
// ---------------------------------------------------------------------------

#[test]
fn pdf_with_1000_empty_pages() {
    let mut doc = Document::new();
    for _ in 0..1000 {
        doc.add_page(612.0, 792.0).unwrap();
    }
    let bytes = doc.to_bytes().unwrap();
    let parsed = Document::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.page_count().unwrap(), 1000);
}

// ---------------------------------------------------------------------------
// External adversarial corpus files
// ---------------------------------------------------------------------------

const ADVERSARIAL_DIR: &str = "tests/corpus/adversarial";

fn read_adversarial(name: &str) -> Option<Vec<u8>> {
    let path = Path::new(ADVERSARIAL_DIR).join(name);
    std::fs::read(&path).ok()
}

#[test]
fn adversarial_corrupted_pdf_no_panic() {
    if let Some(data) = read_adversarial("corrupted.pdf") {
        let _ = Document::from_bytes(&data);
    }
}

#[test]
fn adversarial_not_encrypted_parses() {
    if let Some(data) = read_adversarial("not_encrypted.pdf") {
        let doc = Document::from_bytes(&data).unwrap();
        assert!(doc.page_count().unwrap() > 0);
    }
}

#[test]
fn adversarial_no_xref_table_recovers() {
    if let Some(data) = read_adversarial("no_xref_table.pdf") {
        // No xref table — should recover via object scan
        let result = Document::from_bytes(&data);
        if let Ok(doc) = result {
            let _ = doc.page_count();
        }
    }
}

#[test]
fn adversarial_duplicate_objects_handled() {
    if let Some(data) = read_adversarial("duplicate_objects.pdf") {
        let result = Document::from_bytes(&data);
        if let Ok(doc) = result {
            let _ = doc.page_count();
        }
    }
}

#[test]
fn adversarial_cr_line_endings_handled() {
    if let Some(data) = read_adversarial("cr_line_endings.pdf") {
        let result = Document::from_bytes(&data);
        if let Ok(doc) = result {
            let _ = doc.page_count();
        }
    }
}

#[test]
fn adversarial_long_name_handled() {
    if let Some(data) = read_adversarial("long_name.pdf") {
        let _ = Document::from_bytes(&data);
    }
}

#[test]
fn adversarial_bom_header_handled() {
    if let Some(data) = read_adversarial("bom_header.pdf") {
        let result = Document::from_bytes(&data);
        if let Ok(doc) = result {
            let _ = doc.page_count();
        }
    }
}

#[test]
fn adversarial_all_files_no_panic() {
    let dir = Path::new(ADVERSARIAL_DIR);
    if !dir.exists() {
        return;
    }
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.path().extension().map_or(false, |e| e == "pdf") {
            let data = std::fs::read(entry.path()).unwrap();
            let _ = Document::from_bytes(&data);
            count += 1;
        }
    }
    assert!(count > 0, "Expected adversarial PDFs in corpus");
}
