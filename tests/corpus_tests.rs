//! Integration tests against real-world PDF files.
//!
//! These tests exercise the library against diverse PDFs from public sources.
//! Each test validates that parsing, text extraction, and rendering work
//! without panicking on real documents.

use pdfpurr::rendering::{RenderOptions, Renderer};
use pdfpurr::Document;
use std::path::Path;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const CORPUS: &str = "tests/corpus";

/// Reads a corpus PDF file, returning None if missing (CI without corpus).
fn read_corpus(subdir: &str, name: &str) -> Option<Vec<u8>> {
    let path = Path::new(CORPUS).join(subdir).join(name);
    std::fs::read(&path).ok()
}

/// Parses a PDF and asserts it has at least one page.
fn assert_parses_with_pages(data: &[u8], name: &str) {
    let doc =
        Document::from_bytes(data).unwrap_or_else(|e| panic!("{}: parse failed: {}", name, e));
    let count = doc.page_count().unwrap_or(0);
    assert!(
        count > 0,
        "{}: expected at least 1 page, got {}",
        name,
        count
    );
}

/// Extracts text from page 0, propagating any extraction error.
fn extract_first_page_text(data: &[u8], name: &str) -> String {
    let doc = Document::from_bytes(data).unwrap();
    doc.extract_page_text(0)
        .unwrap_or_else(|e| panic!("{}: text extraction failed: {}", name, e))
}

/// Renders page 0 at 72 DPI and asserts the pixmap is non-empty.
fn assert_renders(data: &[u8], name: &str) {
    let doc = Document::from_bytes(data).unwrap();
    let renderer = Renderer::new(&doc, RenderOptions::default());
    let pixmap = renderer
        .render_page(0)
        .unwrap_or_else(|e| panic!("{}: render failed: {}", name, e));
    assert!(
        pixmap.width() > 0 && pixmap.height() > 0,
        "{}: empty pixmap",
        name
    );
}

// ---------------------------------------------------------------------------
// Basic PDFs — parse, extract text, render
// ---------------------------------------------------------------------------

#[test]
fn corpus_basic_generated_hello_parses() {
    let data = match read_corpus("basic", "generated_hello.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "generated_hello");
}

#[test]
fn corpus_basic_dummy_parses_and_extracts_text() {
    let data = match read_corpus("basic", "dummy.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "dummy");
    let text = extract_first_page_text(&data, "dummy");
    assert!(!text.is_empty(), "dummy.pdf should have extractable text");
}

#[test]
fn corpus_basic_dummy_renders() {
    let data = match read_corpus("basic", "dummy.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_renders(&data, "dummy");
}

#[test]
fn corpus_basic_pdf_sample_parses() {
    let data = match read_corpus("basic", "pdf-sample.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "pdf-sample");
}

#[test]
fn corpus_basic_tracemonkey_parses() {
    let data = match read_corpus("basic", "tracemonkey.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "tracemonkey");
    let doc = Document::from_bytes(&data).unwrap();
    assert!(
        doc.page_count().unwrap() > 1,
        "tracemonkey should be multi-page"
    );
}

#[test]
fn corpus_basic_tracemonkey_extracts_text() {
    let data = match read_corpus("basic", "tracemonkey.pdf") {
        Some(d) => d,
        None => return,
    };
    let text = extract_first_page_text(&data, "tracemonkey");
    assert!(!text.is_empty(), "tracemonkey.pdf page 0 should have text");
}

#[test]
fn corpus_basic_tcp_ip_parses() {
    let data = match read_corpus("basic", "tcp_ip_intro.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "tcp_ip_intro");
}

// ---------------------------------------------------------------------------
// Encrypted PDFs — parse with password
// ---------------------------------------------------------------------------

#[test]
fn corpus_encrypted_aes256_r6_with_user_password() {
    let data = match read_corpus("encrypted", "aes256_r6_user.pdf") {
        Some(d) => d,
        None => return,
    };
    // Should fail without password
    assert!(
        Document::from_bytes(&data).is_err()
            || Document::from_bytes(&data)
                .unwrap()
                .extract_page_text(0)
                .is_err(),
        "aes256_r6 should require password"
    );
    // Should succeed with correct password
    let doc = Document::from_bytes_with_password(&data, b"userpass")
        .unwrap_or_else(|e| panic!("aes256_r6_user with password: {}", e));
    assert!(doc.page_count().unwrap() > 0);
}

#[test]
fn corpus_encrypted_aes256_r6_empty_user_password() {
    let data = match read_corpus("encrypted", "aes256_r6_empty_user.pdf") {
        Some(d) => d,
        None => return,
    };
    // Empty user password — should open with empty password
    let doc = Document::from_bytes_with_password(&data, b"")
        .unwrap_or_else(|e| panic!("aes256_r6_empty_user with empty pw: {}", e));
    assert!(doc.page_count().unwrap() > 0);
}

#[test]
fn corpus_encrypted_aes256_r6_wrong_password_fails() {
    let data = match read_corpus("encrypted", "aes256_r6_user.pdf") {
        Some(d) => d,
        None => return,
    };
    let result = Document::from_bytes_with_password(&data, b"wrongpassword");
    assert!(result.is_err(), "Wrong password should fail for R6");
}

#[test]
fn corpus_encrypted_openpassword_rejects_empty_password() {
    let data = match read_corpus("encrypted", "encryption_openpassword.pdf") {
        Some(d) => d,
        None => return,
    };
    let result = Document::from_bytes_with_password(&data, b"");
    assert!(
        result.is_err(),
        "encryption_openpassword.pdf should reject empty password"
    );
}

#[test]
fn corpus_encrypted_aes128_r4() {
    let data = match read_corpus("encrypted", "aes128_r4.pdf") {
        Some(d) => d,
        None => return,
    };
    // Empty user password should work (qpdf sets separate user/owner)
    let doc = Document::from_bytes_with_password(&data, b"")
        .or_else(|_| Document::from_bytes_with_password(&data, b"testpw"))
        .unwrap_or_else(|e| panic!("aes128_r4: {}", e));
    assert!(doc.page_count().unwrap() > 0);
}

#[test]
fn corpus_encrypted_rc4_40bit_r2() {
    let data = match read_corpus("encrypted", "rc4_40bit_r2.pdf") {
        Some(d) => d,
        None => return,
    };
    let doc = Document::from_bytes_with_password(&data, b"")
        .or_else(|_| Document::from_bytes_with_password(&data, b"pass40"))
        .unwrap_or_else(|e| panic!("rc4_40bit_r2: {}", e));
    assert!(doc.page_count().unwrap() > 0);
}

#[test]
fn corpus_encrypted_permission_restricted_opens_and_resolves_pages() {
    // Linearized encrypted PDFs with permission restrictions must fully
    // parse, decrypt, and resolve their page tree — just like any other PDF.
    for name in &[
        "encryption_nocopy.pdf",
        "encryption_noprinting.pdf",
        "encryption_notextaccess.pdf",
    ] {
        let data = match read_corpus("encrypted", name) {
            Some(d) => d,
            None => continue,
        };
        let doc = Document::from_bytes_with_password(&data, b"")
            .unwrap_or_else(|e| panic!("{}: open failed: {}", name, e));
        let count = doc
            .page_count()
            .unwrap_or_else(|e| panic!("{}: page_count failed: {}", name, e));
        assert!(count > 0, "{}: expected pages, got 0", name);

        // Text extraction should work after decryption + ObjStm expansion
        let text = doc
            .extract_page_text(0)
            .unwrap_or_else(|e| panic!("{}: text extraction failed: {}", name, e));
        assert!(!text.is_empty(), "{}: expected extractable text", name);
    }
}

// ---------------------------------------------------------------------------
// Tagged / PDF/UA / PDF/A PDFs
// ---------------------------------------------------------------------------

#[test]
fn corpus_tagged_pdfa_pass_parses() {
    for name in &["vera_6-1-2-t01-pass-a.pdf", "vera_6-1-2-t02-pass-a.pdf"] {
        let data = match read_corpus("tagged", name) {
            Some(d) => d,
            None => continue,
        };
        assert_parses_with_pages(&data, name);
    }
}

#[test]
fn corpus_tagged_pdfa_fail_still_parses() {
    // PDF/A "fail" files have deliberate spec violations (e.g., leading
    // whitespace before %PDF header). They should still parse as valid PDFs
    // since the violations are PDF/A-specific, not structural.
    for name in &["vera_6-1-2-t01-fail-a.pdf", "vera_6-1-2-t02-fail-a.pdf"] {
        let data = match read_corpus("tagged", name) {
            Some(d) => d,
            None => continue,
        };
        assert_parses_with_pages(&data, name);
    }
}

#[test]
fn corpus_tagged_pdfua_pass_parses() {
    for name in &["ua1_7.2-t02-pass-a.pdf", "ua1_7.2-t03-pass-a.pdf"] {
        let data = match read_corpus("tagged", name) {
            Some(d) => d,
            None => continue,
        };
        assert_parses_with_pages(&data, name);
    }
}

#[test]
fn corpus_tagged_pdfua_fail_parses() {
    for name in &["ua1_7.2-t02-fail-a.pdf", "ua1_7.2-t03-fail-a.pdf"] {
        let data = match read_corpus("tagged", name) {
            Some(d) => d,
            None => continue,
        };
        assert_parses_with_pages(&data, name);
    }
}

// ---------------------------------------------------------------------------
// Malformed PDFs — should not panic
// ---------------------------------------------------------------------------

#[test]
fn corpus_malformed_no_panic() {
    let names = [
        "corruptionOneByteMissing.pdf",
        "calistoMTNoFontsEmbedded.pdf",
        "externalLink.pdf",
        "vera_6-1-2-t02-fail-b.pdf",
        "vera_6-1-2-t02-fail-c.pdf",
    ];
    for name in &names {
        let data = match read_corpus("malformed", name) {
            Some(d) => d,
            None => continue,
        };
        // These may or may not parse, but they must not panic
        let _ = Document::from_bytes(&data);
    }
}

#[test]
fn corpus_malformed_missing_fonts_parses() {
    let data = match read_corpus("malformed", "calistoMTNoFontsEmbedded.pdf") {
        Some(d) => d,
        None => return,
    };
    // Should parse even without embedded fonts
    let doc = Document::from_bytes(&data);
    assert!(doc.is_ok(), "Missing fonts PDF should still parse");
}

#[test]
fn corpus_malformed_external_link_parses() {
    let data = match read_corpus("malformed", "externalLink.pdf") {
        Some(d) => d,
        None => return,
    };
    assert_parses_with_pages(&data, "externalLink");
}

// ---------------------------------------------------------------------------
// Stress: parse all corpus files without panic
// ---------------------------------------------------------------------------

#[test]
fn corpus_all_files_no_panic() {
    let corpus_dir = Path::new(CORPUS);
    if !corpus_dir.exists() {
        return;
    }
    let mut count = 0;
    let mut parsed = 0;
    for entry in walkdir(corpus_dir) {
        if entry.extension().map_or(false, |e| e == "pdf") {
            let name = entry.file_name().unwrap().to_string_lossy().to_string();
            let data = std::fs::read(&entry).unwrap();
            // Must never panic regardless of file content
            if let Ok(doc) = Document::from_bytes(&data) {
                // Non-encrypted PDFs that parse should resolve their page tree
                let _ = doc.page_count();
                parsed += 1;
            }
            count += 1;
            // Catch accidental empty corpus: assert we aren't skipping everything
            assert!(
                !name.is_empty(),
                "walkdir returned entry with empty filename"
            );
        }
    }
    assert!(count > 0, "Expected at least one PDF in corpus");
    assert!(
        parsed > 0,
        "Expected at least one PDF to parse successfully"
    );
}

/// Simple recursive directory walker (avoids external dependency).
fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}
