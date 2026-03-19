//! Property-based tests for PDFPurr using `proptest`.
//!
//! These tests generate random inputs and verify invariants:
//! - Parser never panics on arbitrary bytes
//! - Serialized objects round-trip through parse → serialize → parse
//! - Content tokenizer never panics on arbitrary content streams

use proptest::prelude::*;

use pdfpurr::content::tokenize_content_stream;
use pdfpurr::core::objects::{IndirectRef, Object, PdfName, PdfString};
use pdfpurr::parser::objects::parse_object;

// ---------------------------------------------------------------------------
// Strategies for generating random PDF objects
// ---------------------------------------------------------------------------

/// Generates a valid PDF Name string (ASCII alphanumeric, no special chars).
fn arb_name() -> impl Strategy<Value = String> {
    "[A-Za-z][A-Za-z0-9_]{0,20}"
}

/// Generates a leaf PDF object (no nesting).
fn arb_leaf_object() -> impl Strategy<Value = Object> {
    prop_oneof![
        any::<bool>().prop_map(Object::Boolean),
        // Include boundary values alongside the normal range
        prop_oneof![
            (-1_000_000_000i64..1_000_000_000i64),
            Just(0i64),
            Just(i64::MIN),
            Just(i64::MAX),
        ]
        .prop_map(Object::Integer),
        (-1e6f64..1e6f64)
            .prop_filter("finite", |v| v.is_finite() && *v != 0.0 && v.abs() > 1e-10)
            .prop_map(Object::Real),
        Just(Object::Null),
        arb_name().prop_map(|n| Object::Name(PdfName::new(n))),
        // Include parens and backslashes to exercise escape handling
        "[A-Za-z0-9 ,.!?()\\\\]{0,50}".prop_map(|s| Object::String(PdfString::from_literal(s))),
        prop::collection::vec(any::<u8>(), 0..20)
            .prop_map(|bytes| Object::String(PdfString::from_hex(bytes))),
        (1u32..10000, 0u16..5).prop_map(|(n, g)| Object::Reference(IndirectRef::new(n, g))),
    ]
}

/// Generates a PDF object with up to one level of nesting.
fn arb_object() -> impl Strategy<Value = Object> {
    arb_leaf_object().prop_recursive(
        2,  // max depth
        32, // max nodes
        8,  // items per collection
        |inner| {
            prop_oneof![
                // Array of objects
                prop::collection::vec(inner.clone(), 0..6).prop_map(Object::Array),
                // Dictionary
                prop::collection::vec((arb_name(), inner), 0..4).prop_map(|entries| {
                    let mut dict = pdfpurr::core::objects::Dictionary::new();
                    for (k, v) in entries {
                        dict.insert(PdfName::new(k), v);
                    }
                    Object::Dictionary(dict)
                }),
            ]
        },
    )
}

// ---------------------------------------------------------------------------
// Round-trip: serialize → parse → compare
// ---------------------------------------------------------------------------

/// Serializes an Object to PDF bytes.
fn serialize_object(obj: &Object) -> Vec<u8> {
    let mut buf = Vec::new();
    obj.write_pdf(&mut buf).unwrap();
    // Add trailing whitespace so parser has a delimiter
    buf.push(b' ');
    buf
}

/// Compares two objects, treating Real comparisons with tolerance.
fn objects_equal(a: &Object, b: &Object) -> bool {
    match (a, b) {
        (Object::Boolean(x), Object::Boolean(y)) => x == y,
        (Object::Integer(x), Object::Integer(y)) => x == y,
        (Object::Real(x), Object::Real(y)) => (x - y).abs() < 1e-6,
        // Reals may parse back as integers if they have no fractional part
        (Object::Real(x), Object::Integer(y)) => (*x - *y as f64).abs() < 1e-6,
        (Object::Integer(x), Object::Real(y)) => (*x as f64 - *y).abs() < 1e-6,
        (Object::String(x), Object::String(y)) => x.bytes == y.bytes,
        (Object::Name(x), Object::Name(y)) => x == y,
        (Object::Null, Object::Null) => true,
        (Object::Reference(x), Object::Reference(y)) => x == y,
        (Object::Array(x), Object::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| objects_equal(a, b))
        }
        (Object::Dictionary(x), Object::Dictionary(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && objects_equal(v1, v2))
        }
        _ => false,
    }
}

proptest! {
    // ------------------------------------------------------------------
    // Parser robustness: never panic on arbitrary bytes
    // ------------------------------------------------------------------

    #[test]
    fn parser_never_panics_on_random_bytes(data in prop::collection::vec(any::<u8>(), 0..256)) {
        // Should return Ok or Err, never panic
        let _ = parse_object(&data);
    }

    #[test]
    fn content_tokenizer_never_panics_on_random_bytes(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = tokenize_content_stream(&data);
    }

    #[test]
    fn document_parser_never_panics_on_random_bytes(data in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = pdfpurr::Document::from_bytes(&data);
    }

    // ------------------------------------------------------------------
    // Round-trip: object → serialize → parse → compare
    // ------------------------------------------------------------------

    #[test]
    fn object_round_trip(obj in arb_object()) {
        // Skip streams (can't trivially round-trip due to /Length)
        if matches!(obj, Object::Stream(_)) {
            return Ok(());
        }
        // Skip references (they parse as "N G R" which requires context)
        if contains_reference(&obj) {
            return Ok(());
        }

        let serialized = serialize_object(&obj);
        let parse_result = parse_object(&serialized);

        prop_assert!(
            parse_result.is_ok(),
            "Failed to parse serialized object: {:?}\nSerialized as: {:?}",
            obj,
            String::from_utf8_lossy(&serialized)
        );

        let (_, parsed) = parse_result.unwrap();
        prop_assert!(
            objects_equal(&obj, &parsed),
            "Round-trip mismatch:\n  original:  {:?}\n  parsed:    {:?}\n  serialized: {:?}",
            obj,
            parsed,
            String::from_utf8_lossy(&serialized)
        );
    }

    // ------------------------------------------------------------------
    // Integer parse round-trip
    // ------------------------------------------------------------------

    #[test]
    fn integer_round_trip(n in -999_999_999i64..999_999_999i64) {
        let obj = Object::Integer(n);
        let serialized = serialize_object(&obj);
        let (_, parsed) = parse_object(&serialized).unwrap();
        prop_assert_eq!(parsed, obj);
    }

    // ------------------------------------------------------------------
    // Real number parse round-trip (with tolerance)
    // ------------------------------------------------------------------

    #[test]
    fn real_round_trip(
        n in (-1e6f64..1e6f64).prop_filter("finite nonzero", |v| v.is_finite() && v.abs() > 1e-10)
    ) {
        let obj = Object::Real(n);
        let serialized = serialize_object(&obj);
        let (_, parsed) = parse_object(&serialized).unwrap();
        match parsed {
            Object::Real(v) => prop_assert!((v - n).abs() < 1e-4, "Real mismatch: {} vs {}", n, v),
            Object::Integer(v) => prop_assert!((v as f64 - n).abs() < 1e-4),
            other => prop_assert!(false, "Expected Real, got {:?}", other),
        }
    }

    // ------------------------------------------------------------------
    // Name round-trip
    // ------------------------------------------------------------------

    #[test]
    fn name_round_trip(name in "[A-Za-z][A-Za-z0-9]{0,30}") {
        let obj = Object::Name(PdfName::new(&name));
        let serialized = serialize_object(&obj);
        let (_, parsed) = parse_object(&serialized).unwrap();
        prop_assert_eq!(parsed, obj);
    }

    // ------------------------------------------------------------------
    // Literal string round-trip
    // ------------------------------------------------------------------

    #[test]
    fn literal_string_round_trip(text in "[A-Za-z0-9 ,.!?]{0,100}") {
        let obj = Object::String(PdfString::from_literal(&text));
        let serialized = serialize_object(&obj);
        let (_, parsed) = parse_object(&serialized).unwrap();
        if let Object::String(s) = &parsed {
            prop_assert_eq!(&s.bytes, text.as_bytes());
        } else {
            prop_assert!(false, "Expected String, got {:?}", parsed);
        }
    }

    // ------------------------------------------------------------------
    // Hex string round-trip
    // ------------------------------------------------------------------

    #[test]
    fn hex_string_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..50)) {
        let obj = Object::String(PdfString::from_hex(bytes.clone()));
        let serialized = serialize_object(&obj);
        let (_, parsed) = parse_object(&serialized).unwrap();
        if let Object::String(s) = &parsed {
            prop_assert_eq!(&s.bytes, &bytes);
        } else {
            prop_assert!(false, "Expected String, got {:?}", parsed);
        }
    }

    // ------------------------------------------------------------------
    // Content tokenizer: valid streams always tokenize
    // ------------------------------------------------------------------

    #[test]
    fn valid_content_stream_tokenizes(
        ops in prop::collection::vec(
            prop_oneof![
                Just("q".to_string()),
                Just("Q".to_string()),
                Just("BT".to_string()),
                Just("ET".to_string()),
                Just("f".to_string()),
                Just("S".to_string()),
                Just("n".to_string()),
            ],
            1..10
        )
    ) {
        let stream = ops.join(" ");
        let result = tokenize_content_stream(stream.as_bytes());
        prop_assert!(result.is_ok(), "Failed to tokenize: {}", stream);
        let tokens = result.unwrap();
        // Every token should be an operator (no operands in this stream)
        for token in &tokens {
            prop_assert!(
                matches!(token, pdfpurr::content::ContentToken::Operator(_)),
                "Expected operator, got {:?}",
                token
            );
        }
    }

    // ------------------------------------------------------------------
    // Content tokenizer with operands
    // ------------------------------------------------------------------

    #[test]
    fn content_stream_with_numbers_tokenizes(
        nums in prop::collection::vec(-1000i32..1000i32, 1..7)
    ) {
        // Build "n1 n2 ... cm" style content
        let mut stream = String::new();
        for n in &nums {
            stream.push_str(&format!("{} ", n));
        }
        stream.push_str("cm");
        let result = tokenize_content_stream(stream.as_bytes());
        prop_assert!(result.is_ok(), "Failed to tokenize: {}", stream);
    }

    // ------------------------------------------------------------------
    // Document parser with password: never panics
    // ------------------------------------------------------------------

    #[test]
    fn document_parser_with_password_never_panics(
        data in prop::collection::vec(any::<u8>(), 0..512),
        pw in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let _ = pdfpurr::Document::from_bytes_with_password(&data, &pw);
    }

    // ------------------------------------------------------------------
    // Mutated valid PDF: corrupt random bytes and verify no panic
    // ------------------------------------------------------------------

    #[test]
    fn mutated_pdf_never_panics(
        mutations in prop::collection::vec((0usize..200, any::<u8>()), 1..10)
    ) {
        // Start with a valid minimal PDF
        let mut pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
            3 0 obj\n<< /Type /Page /MediaBox [0 0 612 792] /Parent 2 0 R >>\nendobj\n\
            xref\n0 4\n\
            0000000000 65535 f \n\
            0000000009 00000 n \n\
            0000000058 00000 n \n\
            0000000115 00000 n \n\
            trailer\n<< /Size 4 /Root 1 0 R >>\n\
            startxref\n183\n%%EOF".to_vec();

        // Apply random mutations
        for (pos, byte) in &mutations {
            let idx = pos % pdf.len();
            pdf[idx] = *byte;
        }

        // Must not panic — errors are expected and fine
        let _ = pdfpurr::Document::from_bytes(&pdf);
    }

    // ------------------------------------------------------------------
    // Large random bytes: stress the repair path
    // ------------------------------------------------------------------

    #[test]
    fn large_random_input_never_panics(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = pdfpurr::Document::from_bytes(&data);
    }

    // ------------------------------------------------------------------
    // Targeted: deeply nested arrays/dicts
    // ------------------------------------------------------------------

    #[test]
    fn deeply_nested_objects_never_panic(depth in 1usize..200) {
        // Build deeply nested arrays: [[[...]]]
        let mut input = Vec::new();
        for _ in 0..depth {
            input.push(b'[');
        }
        input.extend_from_slice(b"1 ");
        for _ in 0..depth {
            input.push(b']');
        }
        input.push(b' ');
        let _ = parse_object(&input);
    }

    // ------------------------------------------------------------------
    // Targeted: string escape sequences
    // ------------------------------------------------------------------

    #[test]
    fn string_with_random_escapes_never_panics(
        body in prop::collection::vec(
            prop_oneof![
                Just(b'\\'),
                Just(b'('),
                Just(b')'),
                any::<u8>(),
            ],
            0..100
        )
    ) {
        let mut input = vec![b'('];
        input.extend_from_slice(&body);
        input.push(b')');
        input.push(b' ');
        let _ = parse_object(&input);
    }

    // ------------------------------------------------------------------
    // Targeted: PDF with stream and random content
    // ------------------------------------------------------------------

    #[test]
    fn pdf_with_random_stream_content_never_panics(
        stream_data in prop::collection::vec(any::<u8>(), 0..200)
    ) {
        let header = format!(
            "%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
             2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
             3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n\
             4 0 obj\n<< /Length {} >>\nstream\n",
            stream_data.len()
        );
        let mut pdf = header.into_bytes();
        pdf.extend_from_slice(&stream_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000200 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n300\n%%EOF\n");
        // Parse and try text extraction — must never panic
        if let Ok(doc) = pdfpurr::Document::from_bytes(&pdf) {
            let _ = doc.extract_page_text(0);
        }
    }

    // ------------------------------------------------------------------
    // Targeted: hex strings with odd lengths and non-hex chars
    // ------------------------------------------------------------------

    #[test]
    fn malformed_hex_strings_never_panic(
        body in prop::collection::vec(any::<u8>(), 0..50)
    ) {
        let mut input = vec![b'<'];
        input.extend_from_slice(&body);
        input.push(b'>');
        input.push(b' ');
        let _ = parse_object(&input);
    }
}

/// Returns true if the object tree contains any Reference nodes.
fn contains_reference(obj: &Object) -> bool {
    match obj {
        Object::Reference(_) => true,
        Object::Array(arr) => arr.iter().any(contains_reference),
        Object::Dictionary(dict) => dict.values().any(contains_reference),
        _ => false,
    }
}
