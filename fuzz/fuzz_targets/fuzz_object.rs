#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz the low-level object parser — must never panic.
    let _ = pdfpurr::parser::parse_object(data);
});
