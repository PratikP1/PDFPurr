#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Primary fuzz target: Document::from_bytes must never panic on any input.
    // Errors are expected and fine — panics, stack overflows, and infinite
    // loops are the bugs we're hunting.
    if let Ok(doc) = pdfpurr::Document::from_bytes(data) {
        // If parsing succeeds, exercise the read path
        let _ = doc.page_count();
        let _ = doc.metadata();
        let _ = doc.outlines();
        if let Ok(pages) = doc.pages() {
            for (i, _) in pages.iter().enumerate().take(3) {
                let _ = doc.extract_page_text(i);
            }
        }
    }

    // Also try with password (exercises encryption path)
    let _ = pdfpurr::Document::from_bytes_with_password(data, b"");
});
