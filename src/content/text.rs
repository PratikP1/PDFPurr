//! Text extraction from PDF content streams.
//!
//! Extracts text by processing text-showing operators (Tj, TJ, ', ").
//! Supports both basic extraction (Latin-1/UTF-16BE fallback) and
//! font-aware extraction using encoding tables and ToUnicode CMaps.
//!
//! ISO 32000-2:2020, Section 9.4.

use std::collections::HashMap;

use crate::content::operators::{tokenize_content_stream, ContentToken};
use crate::core::objects::{decode_utf16be, Object};
use crate::error::PdfResult;
use crate::fonts::graphics_state::GraphicsStateStack;
use crate::fonts::Font;

/// Extracts text from a content stream's raw (decoded) bytes.
///
/// This performs basic text extraction by processing text-showing operators.
/// It assumes standard encodings (Latin-1/WinAnsi) for string operands.
/// For more accurate extraction with CJK or custom-encoded fonts,
/// use [`extract_text_with_fonts`] with font encoding information.
pub fn extract_text_from_content(data: &[u8]) -> PdfResult<String> {
    extract_text_with_fonts(data, &HashMap::new())
}

/// Finds the string operand immediately before an operator at `op_index`.
fn find_string_operand(tokens: &[ContentToken], op_index: usize) -> Option<&[u8]> {
    if op_index == 0 {
        return None;
    }
    // Walk backwards past any non-string operands (e.g., for " operator
    // which takes: aw ac string)
    for j in (0..op_index).rev() {
        match &tokens[j] {
            ContentToken::Operand(Object::String(s)) => return Some(&s.bytes),
            ContentToken::Operator(_) => return None,
            _ => continue,
        }
    }
    None
}

/// Finds the array operand immediately before an operator at `op_index`.
fn find_array_operand(tokens: &[ContentToken], op_index: usize) -> Option<&[Object]> {
    if op_index == 0 {
        return None;
    }
    match &tokens[op_index - 1] {
        ContentToken::Operand(Object::Array(arr)) => Some(arr),
        _ => None,
    }
}

/// Finds the numeric operand at a given position relative to the operator.
/// `offset` 0 = immediately before operator, 1 = two before, etc.
fn find_numeric_operand(tokens: &[ContentToken], op_index: usize, offset: usize) -> Option<f64> {
    let target = op_index.checked_sub(1 + offset)?;
    match &tokens[target] {
        ContentToken::Operand(obj) => obj.as_f64(),
        _ => None,
    }
}

/// Extracts text from a content stream using font encoding information.
///
/// This is the font-aware version of `extract_text_from_content`. It tracks
/// the current font via graphics state and uses font encoding/ToUnicode
/// mappings to produce accurate Unicode text.
pub fn extract_text_with_fonts(data: &[u8], fonts: &HashMap<String, Font>) -> PdfResult<String> {
    let tokens = tokenize_content_stream(data)?;
    let mut text = String::new();
    let mut in_text_block = false;
    let mut gs = GraphicsStateStack::new();

    let mut i = 0;
    while i < tokens.len() {
        if let ContentToken::Operator(op) = &tokens[i] {
            match op.as_str() {
                "BT" => {
                    in_text_block = true;
                }
                "ET" => {
                    in_text_block = false;
                }
                "q" => {
                    gs.save();
                }
                "Q" => {
                    gs.restore();
                }
                "Tf" if in_text_block => {
                    // /FontName fontSize Tf
                    if let Some(font_name) = find_name_operand(&tokens, i) {
                        gs.current_mut().text_state.font_name = Some(font_name.to_string());
                    }
                    if let Some(size) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.font_size = size;
                    }
                }
                "Tc" if in_text_block => {
                    if let Some(v) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.character_spacing = v;
                    }
                }
                "Tw" if in_text_block => {
                    if let Some(v) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.word_spacing = v;
                    }
                }
                "TL" if in_text_block => {
                    if let Some(v) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.leading = v;
                    }
                }
                "Tz" if in_text_block => {
                    if let Some(v) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.horizontal_scaling = v;
                    }
                }
                "Ts" if in_text_block => {
                    if let Some(v) = find_numeric_operand(&tokens, i, 0) {
                        gs.current_mut().text_state.rise = v;
                    }
                }
                "Tj" | "'" | "\"" if in_text_block => {
                    if let Some(s) = find_string_operand(&tokens, i) {
                        let font = current_font(&gs, fonts);
                        append_decoded_string(&mut text, s, font);
                    }
                    if op == "'" || op == "\"" {
                        text.push('\n');
                    }
                }
                "TJ" if in_text_block => {
                    if let Some(arr) = find_array_operand(&tokens, i) {
                        let font = current_font(&gs, fonts);
                        for item in arr {
                            match item {
                                Object::String(s) => {
                                    append_decoded_string(&mut text, &s.bytes, font);
                                }
                                Object::Integer(n) if *n <= -100 => {
                                    text.push(' ');
                                }
                                Object::Real(n) if *n <= -100.0 => {
                                    text.push(' ');
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "Td" | "TD" if in_text_block => {
                    if let Some(ty) = find_numeric_operand(&tokens, i, 0) {
                        if ty.abs() > 0.5 && !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                    }
                }
                "T*" if in_text_block && !text.is_empty() && !text.ends_with('\n') => {
                    text.push('\n');
                }
                "Tm" if in_text_block
                    && !text.is_empty()
                    && !text.ends_with('\n')
                    && !text.ends_with(' ') =>
                {
                    text.push(' ');
                }
                _ => {}
            }
        }
        i += 1;
    }

    Ok(text.trim().to_string())
}

/// Returns the current font from the graphics state, if one is set and exists in the font map.
fn current_font<'a>(gs: &GraphicsStateStack, fonts: &'a HashMap<String, Font>) -> Option<&'a Font> {
    gs.current()
        .text_state
        .font_name
        .as_deref()
        .and_then(|name| fonts.get(name))
}

/// Appends decoded string bytes to text using a font's encoding if available.
fn append_decoded_string(text: &mut String, bytes: &[u8], font: Option<&Font>) {
    if let Some(font) = font {
        font.decode_bytes_into(bytes, text);
    } else {
        append_string_bytes(text, bytes);
    }
}

/// Finds the name operand before an operator (e.g., /F1 for Tf).
fn find_name_operand(tokens: &[ContentToken], op_index: usize) -> Option<&str> {
    if op_index < 2 {
        return None;
    }
    // For Tf, the name is two tokens before: /Name size Tf
    for j in (0..op_index).rev() {
        match &tokens[j] {
            ContentToken::Operand(Object::Name(name)) => return Some(name.as_str()),
            ContentToken::Operator(_) => return None,
            _ => continue,
        }
    }
    None
}

/// PDFDocEncoding lookup table for bytes 0x80–0xAD.
///
/// ISO 32000-2:2020, Table D.2. Entries are Unicode code points.
/// `0` means the byte is undefined and maps to U+FFFD.
const PDFDOC_ENCODING: [u16; 46] = [
    0x2022, // 0x80 → BULLET
    0x2020, // 0x81 → DAGGER
    0x2021, // 0x82 → DOUBLE DAGGER
    0x2026, // 0x83 → HORIZONTAL ELLIPSIS
    0x2014, // 0x84 → EM DASH
    0x2013, // 0x85 → EN DASH
    0x0192, // 0x86 → LATIN SMALL LETTER F WITH HOOK
    0x2044, // 0x87 → FRACTION SLASH
    0x2039, // 0x88 → SINGLE LEFT-POINTING ANGLE QUOTATION MARK
    0x203A, // 0x89 → SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
    0x2212, // 0x8A → MINUS SIGN
    0x2030, // 0x8B → PER MILLE SIGN
    0x201E, // 0x8C → DOUBLE LOW-9 QUOTATION MARK
    0x201C, // 0x8D → LEFT DOUBLE QUOTATION MARK
    0x201D, // 0x8E → RIGHT DOUBLE QUOTATION MARK
    0x2018, // 0x8F → LEFT SINGLE QUOTATION MARK
    0x2019, // 0x90 → RIGHT SINGLE QUOTATION MARK
    0x201A, // 0x91 → SINGLE LOW-9 QUOTATION MARK
    0x2122, // 0x92 → TRADE MARK SIGN
    0xFB01, // 0x93 → LATIN SMALL LIGATURE FI
    0xFB02, // 0x94 → LATIN SMALL LIGATURE FL
    0x0141, // 0x95 → LATIN CAPITAL LETTER L WITH STROKE
    0x0152, // 0x96 → LATIN CAPITAL LIGATURE OE
    0x0160, // 0x97 → LATIN CAPITAL LETTER S WITH CARON
    0x0178, // 0x98 → LATIN CAPITAL LETTER Y WITH DIAERESIS
    0x017D, // 0x99 → LATIN CAPITAL LETTER Z WITH CARON
    0x0131, // 0x9A → LATIN SMALL LETTER DOTLESS I
    0x0142, // 0x9B → LATIN SMALL LETTER L WITH STROKE
    0x0153, // 0x9C → LATIN SMALL LIGATURE OE
    0x0161, // 0x9D → LATIN SMALL LETTER S WITH CARON
    0x017E, // 0x9E → LATIN SMALL LETTER Z WITH CARON
    0xFFFD, // 0x9F → UNDEFINED
    0x20AC, // 0xA0 → EURO SIGN
    0x00A1, // 0xA1 → INVERTED EXCLAMATION MARK (same as Latin-1)
    0x00A2, // 0xA2 → CENT SIGN
    0x00A3, // 0xA3 → POUND SIGN
    0x00A4, // 0xA4 → CURRENCY SIGN
    0x00A5, // 0xA5 → YEN SIGN
    0x00A6, // 0xA6 → BROKEN BAR
    0x00A7, // 0xA7 → SECTION SIGN
    0x00A8, // 0xA8 → DIAERESIS
    0x00A9, // 0xA9 → COPYRIGHT SIGN
    0x00AA, // 0xAA → FEMININE ORDINAL INDICATOR
    0x00AB, // 0xAB → LEFT-POINTING DOUBLE ANGLE QUOTATION MARK
    0x00AC, // 0xAC → NOT SIGN
    0x00AD, // 0xAD → SOFT HYPHEN
];

/// Decodes a single byte using PDFDocEncoding (ISO 32000-2, Table D.2).
fn pdfdoc_decode(b: u8) -> char {
    match b {
        0x00..=0x07 | 0x11..=0x1F | 0x7F => '\u{FFFD}', // undefined
        0x08 => '\u{02D8}',                             // BREVE
        0x09 => '\u{02C7}',                             // CARON
        0x0A => '\u{02C6}',                             // MODIFIER LETTER CIRCUMFLEX ACCENT
        0x0B => '\u{02D9}',                             // DOT ABOVE
        0x0C => '\u{02DD}',                             // DOUBLE ACUTE ACCENT
        0x0D => '\u{02DB}',                             // OGONEK
        0x0E => '\u{02DA}',                             // RING ABOVE
        0x0F => '\u{02DC}',                             // SMALL TILDE
        0x10 => '\u{2003}',                             // EM SPACE
        0x80..=0xAD => {
            let cp = PDFDOC_ENCODING[(b - 0x80) as usize];
            char::from_u32(cp as u32).unwrap_or('\u{FFFD}')
        }
        0xAE..=0xFF => char::from_u32(b as u32).unwrap_or('\u{FFFD}'),
        0x20..=0x7E => b as char, // ASCII identity
    }
}

/// Appends bytes as text, detecting encoding.
fn append_string_bytes(text: &mut String, bytes: &[u8]) {
    // Check for UTF-16BE BOM
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        if let Some(s) = decode_utf16be(&bytes[2..]) {
            text.push_str(&s);
        }
    } else {
        // PDFDocEncoding (ISO 32000-2:2020, Annex D)
        for &b in bytes {
            text.push(pdfdoc_decode(b));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_text() {
        let content = b"BT /F1 12 Tf (Hello World) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extract_multiple_tj() {
        let content = b"BT /F1 12 Tf (Hello ) Tj (World) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extract_tj_array() {
        let content = b"BT /F1 12 Tf [(Hello ) -200 (World)] TJ ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "Hello  World");
    }

    #[test]
    fn extract_text_with_newline() {
        let content = b"BT /F1 12 Tf 0 -14 Td (Line 1) Tj 0 -14 Td (Line 2) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert!(text.contains("Line 1"));
        assert!(text.contains("Line 2"));
    }

    #[test]
    fn extract_text_t_star() {
        let content = b"BT /F1 12 Tf (Line 1) Tj T* (Line 2) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert!(text.contains("Line 1"));
        assert!(text.contains("Line 2"));
    }

    #[test]
    fn extract_empty_content() {
        let text = extract_text_from_content(b"").unwrap();
        assert!(text.is_empty());
    }

    #[test]
    fn extract_no_text_operators() {
        let content = b"q 1 0 0 1 50 50 cm Q";
        let text = extract_text_from_content(content).unwrap();
        assert!(text.is_empty());
    }

    #[test]
    fn extract_hex_string_text() {
        let content = b"BT /F1 12 Tf <48656C6C6F> Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_multiple_text_blocks() {
        let content = b"BT (First) Tj ET BT (Second) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert!(text.contains("First"));
        assert!(text.contains("Second"));
    }

    #[test]
    fn pdfdocencoding_0x80_to_0x9f_decoded_correctly() {
        // PDFDocEncoding (ISO 32000-2, Table D.2):
        // 0x80 = U+2022 BULLET
        // 0x84 = U+2014 EM DASH
        // 0x85 = U+2013 EN DASH
        // 0x8D = U+201C LEFT DOUBLE QUOTATION MARK
        // 0x8E = U+201D RIGHT DOUBLE QUOTATION MARK
        // 0x92 = U+2122 TRADE MARK SIGN
        // 0xA0 = U+20AC EURO SIGN
        let content = b"BT /F1 12 Tf <80848592A0> Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert!(
            text.contains('\u{2022}'),
            "0x80 should be BULLET, got: {:?}",
            text
        );
        assert!(
            text.contains('\u{2014}'),
            "0x84 should be EM DASH, got: {:?}",
            text
        );
        assert!(
            text.contains('\u{2013}'),
            "0x85 should be EN DASH, got: {:?}",
            text
        );
        assert!(
            text.contains('\u{2122}'),
            "0x92 should be TRADE MARK, got: {:?}",
            text
        );
        assert!(
            text.contains('\u{20AC}'),
            "0xA0 should be EURO SIGN, got: {:?}",
            text
        );
    }

    #[test]
    fn pdfdocencoding_ascii_range_unchanged() {
        // ASCII range (0x20-0x7E) should map identically
        let content = b"BT /F1 12 Tf (ABC xyz 123) Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "ABC xyz 123");
    }

    #[test]
    fn pdfdocencoding_undefined_bytes_replaced() {
        // 0x7F (DELETE), 0x80-area undefined bytes should be handled
        // 0xAD = U+00AD SOFT HYPHEN (identity in PDFDocEncoding)
        let content = b"BT /F1 12 Tf <AD> Tj ET";
        let text = extract_text_from_content(content).unwrap();
        assert!(
            text.contains('\u{00AD}'),
            "0xAD should be SOFT HYPHEN, got: {:?}",
            text
        );
    }

    #[test]
    fn tj_array_small_adjustment_no_space() {
        // Small adjustments (< 100) should not insert spaces
        let content = b"BT [(H) -10 (ello)] TJ ET";
        let text = extract_text_from_content(content).unwrap();
        assert_eq!(text, "Hello");
    }

    // --- Font-aware extraction tests ---

    use crate::fonts::encoding::Encoding;
    use crate::fonts::font::FontSubtype;

    fn make_test_font(encoding: Encoding) -> Font {
        Font::for_test("TestFont", FontSubtype::Type1, encoding)
    }

    fn font_map(entries: &[(&str, Font)]) -> HashMap<String, Font> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn extract_with_fonts_basic() {
        let fonts = font_map(&[("F1", make_test_font(Encoding::win_ansi()))]);
        let content = b"BT /F1 12 Tf (Hello) Tj ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_with_fonts_win_ansi_special() {
        let fonts = font_map(&[("F1", make_test_font(Encoding::win_ansi()))]);
        // 0x93 = left double quotation mark in WinAnsi, 0x94 = right
        let content = b"BT /F1 12 Tf <9348659494> Tj ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert!(text.contains('\u{201C}')); // left double quote
        assert!(text.contains('\u{201D}')); // right double quote
    }

    #[test]
    fn extract_with_fonts_fallback_no_font() {
        let fonts = HashMap::new();
        let content = b"BT /F1 12 Tf (Hello) Tj ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_with_fonts_graphics_state() {
        let fonts = font_map(&[
            ("F1", make_test_font(Encoding::win_ansi())),
            ("F2", make_test_font(Encoding::standard())),
        ]);
        // Switch fonts mid-stream
        let content = b"BT /F1 12 Tf (Hello ) Tj /F2 12 Tf (World) Tj ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extract_with_fonts_q_q_state() {
        let fonts = font_map(&[("F1", make_test_font(Encoding::win_ansi()))]);
        // q/Q should save/restore font state
        let content = b"BT /F1 12 Tf q (Inside) Tj Q (Outside) Tj ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert!(text.contains("Inside"));
        assert!(text.contains("Outside"));
    }

    #[test]
    fn extract_with_fonts_tj_array() {
        let fonts = font_map(&[("F1", make_test_font(Encoding::win_ansi()))]);
        let content = b"BT /F1 12 Tf [(Hello ) -200 (World)] TJ ET";
        let text = extract_text_with_fonts(content, &fonts).unwrap();
        assert_eq!(text, "Hello  World");
    }
}
