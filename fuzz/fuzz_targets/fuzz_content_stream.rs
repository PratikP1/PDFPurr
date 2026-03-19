#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz the content stream tokenizer — must never panic.
    let _ = pdfpurr::content::tokenize_content_stream(data);
});
