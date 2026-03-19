//! Tests against external PDF corpus files (veraPDF, qpdf fuzz cases).
//!
//! These files are stored in `tests/private_corpus/` which is gitignored.
//! Tests skip gracefully when corpus files are not present.

use pdfpurr::Document;
use std::path::Path;

const PRIVATE_CORPUS: &str = "tests/private_corpus";

/// Collects all PDF-like files recursively from a directory.
fn collect_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_files(&path));
            } else {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if ext == "pdf" || ext == "fuzz" {
                    files.push(path);
                }
            }
        }
    }
    files
}

// ---------------------------------------------------------------------------
// Parser robustness: every file must parse without panicking
// ---------------------------------------------------------------------------

#[test]
fn external_corpus_all_files_no_panic() {
    let dir = Path::new(PRIVATE_CORPUS);
    if !dir.exists() {
        return;
    }

    let files = collect_files(dir);
    if files.is_empty() {
        return;
    }

    let mut parsed_ok = 0;
    let mut parsed_err = 0;

    for path in &files {
        let data = std::fs::read(path).unwrap();
        let _name = path.file_name().unwrap().to_string_lossy();

        match Document::from_bytes(&data) {
            Ok(doc) => {
                parsed_ok += 1;
                // Exercise the API — none of these should panic
                let _ = doc.page_count();
                let _ = doc.metadata();
                let _ = doc.outlines();
                if let Ok(pages) = doc.pages() {
                    for (i, page) in pages.iter().enumerate().take(2) {
                        let _ = doc.extract_page_text(i);
                        let _ = doc.page_annotations(page);
                        let _ = doc.page_images(page);
                    }
                }
            }
            Err(_) => {
                parsed_err += 1;
            }
        }

        // Also try with empty password (exercises encryption path)
        let _ = Document::from_bytes_with_password(&data, b"");

        // And lazy loading
        let _ = Document::from_bytes_lazy(&data);
    }

    eprintln!(
        "External corpus: {} files, {} parsed OK, {} parse errors (expected for malformed files)",
        files.len(),
        parsed_ok,
        parsed_err
    );

    assert!(
        parsed_ok + parsed_err == files.len(),
        "All files should be accounted for"
    );
}

// ---------------------------------------------------------------------------
// Accessibility validation on all parseable files
// ---------------------------------------------------------------------------

#[test]
fn external_corpus_accessibility_audit() {
    let dir = Path::new(PRIVATE_CORPUS);
    if !dir.exists() {
        return;
    }

    let files = collect_files(dir);
    if files.is_empty() {
        return;
    }

    let mut audited = 0;
    let mut tagged_count = 0;
    let mut has_lang_count = 0;
    let mut has_alt_text_issues = 0;
    let mut total_checks_failed = 0;

    for path in &files {
        let data = std::fs::read(path).unwrap();

        let doc = match Document::from_bytes(&data) {
            Ok(d) => d,
            Err(_) => continue,
        };

        audited += 1;
        let report = doc.accessibility_report();

        // Count accessibility characteristics
        for check in &report.checks {
            if check.id == "tagged-pdf" && check.passed {
                tagged_count += 1;
            }
            if check.id == "language" && check.passed {
                has_lang_count += 1;
            }
            if check.id == "alt-text" && !check.passed {
                has_alt_text_issues += 1;
            }
            if !check.passed {
                total_checks_failed += 1;
            }
        }

        // Structure tree inspection (must not panic)
        let _ = doc.structure_tree();
    }

    eprintln!("Accessibility audit: {} files audited", audited);
    eprintln!("  Tagged PDFs: {}", tagged_count);
    eprintln!("  Has language: {}", has_lang_count);
    eprintln!("  Alt-text issues: {}", has_alt_text_issues);
    eprintln!("  Total check failures: {}", total_checks_failed);

    // We don't assert specific counts — these are external files.
    // The important thing is that the audit completed without panics.
    assert!(audited > 0, "Expected at least one auditable file");
}

// ---------------------------------------------------------------------------
// veraPDF specifically: parse all Isartor/veraPDF fail cases
// ---------------------------------------------------------------------------

#[test]
fn verapdf_fail_cases_parse_without_panic() {
    let dir = Path::new(PRIVATE_CORPUS).join("verapdf");
    if !dir.exists() {
        return;
    }

    let files = collect_files(&dir);
    let mut count = 0;

    for path in &files {
        let data = std::fs::read(path).unwrap();
        // PDF/A "fail" files have deliberate spec violations but should
        // still parse as valid PDFs (the violations are PDF/A-specific)
        match Document::from_bytes(&data) {
            Ok(doc) => {
                // Validate PDF/A — should report failures (these are fail cases)
                let report = doc.validate_pdfa(pdfpurr::PdfALevel::A1b);
                // We expect most to fail PDF/A validation
                let _ = report.is_compliant();
                count += 1;
            }
            Err(_) => {
                // Some files may have structural issues too
                count += 1;
            }
        }
    }

    if count > 0 {
        eprintln!("Tested {} veraPDF fail cases", count);
    }
}

// ---------------------------------------------------------------------------
// qpdf fuzz cases: these triggered crashes in qpdf — stress our parser
// ---------------------------------------------------------------------------

#[test]
fn qpdf_fuzz_cases_no_panic() {
    let dir = Path::new(PRIVATE_CORPUS).join("qpdf");
    if !dir.exists() {
        return;
    }

    let files = collect_files(&dir);
    let mut count = 0;

    for path in &files {
        let data = std::fs::read(path).unwrap();
        let _name = path.file_name().unwrap().to_string_lossy();

        // These files crashed qpdf — they must not crash us
        let result = Document::from_bytes(&data);

        if let Ok(doc) = &result {
            let _ = doc.page_count();
            let _ = doc.metadata();
            if let Ok(pages) = doc.pages() {
                for (i, _) in pages.iter().enumerate().take(1) {
                    let _ = doc.extract_page_text(i);
                }
            }
        }

        // Also test with password and lazy paths
        let _ = Document::from_bytes_with_password(&data, b"");
        let _ = Document::from_bytes_lazy(&data);

        count += 1;
    }

    if count > 0 {
        eprintln!("Tested {} qpdf fuzz cases without panic", count);
    }
}

// ---------------------------------------------------------------------------
// BFO PDF/A-2 test suite
// ---------------------------------------------------------------------------

#[test]
fn bfo_pdfa2_test_suite_no_panic() {
    let dir = Path::new(PRIVATE_CORPUS).join("bfo");
    if !dir.exists() {
        return;
    }

    let files = collect_files(&dir);
    let mut count = 0;

    for path in &files {
        let data = std::fs::read(path).unwrap();

        match Document::from_bytes(&data) {
            Ok(doc) => {
                let _ = doc.page_count();
                let _ = doc.metadata();
                let _ = doc.accessibility_report();
                let _ = doc.validate_pdfa(pdfpurr::PdfALevel::A2b);
            }
            Err(_) => {}
        }
        count += 1;
    }

    if count > 0 {
        eprintln!("Tested {} BFO PDF/A-2 files without panic", count);
    }
}
