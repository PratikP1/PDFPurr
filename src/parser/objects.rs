//! PDF object parser.
//!
//! Parses complete PDF objects (booleans, numbers, strings, names, arrays,
//! dictionaries, and indirect references) from byte streams.

use nom::{
    branch::alt,
    bytes::complete::{tag, take_while},
    character::complete::char,
    combinator::{map, value},
    IResult,
};

use crate::core::objects::{
    Dictionary, IndirectRef, Object, PdfName, PdfStream, PdfString, StringFormat,
};
use crate::parser::lexer::{
    boolean, hex_digit, is_whitespace, name_token, numeric_token, skip_whitespace_and_comments,
};

/// Parses a PDF boolean object.
fn parse_boolean(input: &[u8]) -> IResult<&[u8], Object> {
    map(boolean, Object::Boolean)(input)
}

/// Parses a numeric value (integer or real).
fn parse_number(input: &[u8]) -> IResult<&[u8], Object> {
    let (rest, token) = numeric_token(input)?;
    // numeric_token guarantees ASCII-only bytes ([0-9.+-]), so this never fails.
    let s = std::str::from_utf8(token).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Float))
    })?;

    if token.contains(&b'.') {
        let val: f64 = s.parse().map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Float))
        })?;
        Ok((rest, Object::Real(val)))
    } else {
        let val: i64 = s.parse().map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;
        Ok((rest, Object::Integer(val)))
    }
}

/// Parses a literal string: `(...)` with balanced parentheses and escape sequences.
fn parse_literal_string(input: &[u8]) -> IResult<&[u8], Object> {
    let (mut input, _) = char('(')(input)?;
    let mut bytes = Vec::new();
    let mut depth: u32 = 1;

    loop {
        if input.is_empty() {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Char,
            )));
        }

        match input[0] {
            b'(' => {
                depth += 1;
                bytes.push(b'(');
                input = &input[1..];
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    input = &input[1..];
                    break;
                }
                bytes.push(b')');
                input = &input[1..];
            }
            b'\\' => {
                if input.len() < 2 {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Char,
                    )));
                }
                match input[1] {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'b' => bytes.push(0x08),
                    b'f' => bytes.push(0x0C),
                    b'(' => bytes.push(b'('),
                    b')' => bytes.push(b')'),
                    b'\\' => bytes.push(b'\\'),
                    b'\r' => {
                        // Line continuation: backslash + CR or CR+LF
                        if input.len() > 2 && input[2] == b'\n' {
                            input = &input[3..];
                        } else {
                            input = &input[2..];
                        }
                        continue;
                    }
                    b'\n' => {
                        // Line continuation: backslash + LF
                        input = &input[2..];
                        continue;
                    }
                    c if (b'0'..=b'7').contains(&c) => {
                        // Octal escape: 1-3 octal digits. Use u16 to avoid
                        // overflow on values > 377 octal; truncate to u8 per
                        // ISO 32000-2:2020, Section 7.3.4.2.
                        let mut val: u16 = (c - b'0') as u16;
                        let mut consumed = 2;
                        for i in 2..=3 {
                            if consumed < input.len()
                                && input[i].is_ascii_digit()
                                && input[i] <= b'7'
                            {
                                val = val * 8 + (input[i] - b'0') as u16;
                                consumed = i + 1;
                            } else {
                                break;
                            }
                        }
                        bytes.push(val as u8);
                        input = &input[consumed..];
                        continue;
                    }
                    // Unknown escape: ignore the backslash per spec
                    other => bytes.push(other),
                }
                input = &input[2..];
            }
            b => {
                bytes.push(b);
                input = &input[1..];
            }
        }
    }

    Ok((
        input,
        Object::String(PdfString::from_bytes(bytes, StringFormat::Literal)),
    ))
}

/// Parses a hexadecimal string: `<hex digits>`.
fn parse_hex_string(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, _) = char('<')(input)?;
    // Make sure this is NOT a dictionary start `<<`
    if input.first() == Some(&b'<') {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Char,
        )));
    }

    let (input, hex_content) = take_while(|b: u8| b != b'>')(input)?;
    let (input, _) = char('>')(input)?;

    // Decode hex, skipping whitespace. If odd number of digits, append 0.
    let mut bytes = Vec::new();
    let mut high: Option<u8> = None;
    for &b in hex_content {
        if is_whitespace(b) {
            continue;
        }
        if let Some(digit) = hex_digit(b) {
            match high {
                None => high = Some(digit),
                Some(h) => {
                    bytes.push(h * 16 + digit);
                    high = None;
                }
            }
        }
        // Invalid hex chars are silently ignored per some implementations
    }
    // Odd number of hex digits: treat the last digit as the high nibble
    if let Some(h) = high {
        bytes.push(h * 16);
    }

    Ok((
        input,
        Object::String(PdfString::from_bytes(bytes, StringFormat::Hexadecimal)),
    ))
}

/// Decodes name hex escapes (e.g., `#20` -> space).
fn decode_name(raw: &[u8]) -> String {
    let mut result = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            if let (Some(h), Some(l)) = (hex_digit(raw[i + 1]), hex_digit(raw[i + 2])) {
                result.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        result.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

/// Parses a PDF Name object.
fn parse_name(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, raw) = name_token(input)?;
    Ok((input, Object::Name(PdfName::new(decode_name(raw)))))
}

/// Parses the `null` keyword.
fn parse_null(input: &[u8]) -> IResult<&[u8], Object> {
    value(Object::Null, tag(b"null"))(input)
}

/// Parses a PDF array: `[ obj1 obj2 ... ]`.
fn parse_array(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, _) = char('[')(input)?;
    let mut items = Vec::with_capacity(8);
    let mut input = input;

    loop {
        let (rest, _) = skip_whitespace_and_comments(input)?;
        input = rest;

        if input.first() == Some(&b']') {
            input = &input[1..];
            break;
        }

        if input.is_empty() {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Char,
            )));
        }

        let (rest, obj) = parse_object(input)?;
        items.push(obj);
        input = rest;
    }

    Ok((input, Object::Array(items)))
}

/// Parses a PDF dictionary: `<< /Key value ... >>`.
fn parse_dictionary_inner(input: &[u8]) -> IResult<&[u8], Dictionary> {
    let (input, _) = tag(b"<<")(input)?;
    let mut dict = Dictionary::new();
    let mut input = input;

    loop {
        let (rest, _) = skip_whitespace_and_comments(input)?;
        input = rest;

        // Check for end of dictionary
        if input.len() >= 2 && &input[..2] == b">>" {
            input = &input[2..];
            break;
        }

        if input.is_empty() {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Char,
            )));
        }

        // Parse key (must be a Name)
        let (rest, key_obj) = parse_name(input)?;
        let key = match key_obj {
            Object::Name(n) => n,
            _ => {
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        };

        // Parse value
        let (rest, _) = skip_whitespace_and_comments(rest)?;
        let (rest, val) = parse_object(rest)?;
        dict.insert(key, val);
        input = rest;
    }

    Ok((input, dict))
}

/// Parses a PDF dictionary object.
fn parse_dictionary(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, dict) = parse_dictionary_inner(input)?;

    // Check if this dictionary is followed by a stream
    let (check_input, _) = skip_whitespace_and_comments(input)?;
    if check_input.len() >= 6 && &check_input[..6] == b"stream" {
        // Parse stream
        let mut stream_start = &check_input[6..];

        // Stream keyword must be followed by a single EOL (CR, LF, or CRLF)
        if stream_start.starts_with(b"\r\n") {
            stream_start = &stream_start[2..];
        } else if stream_start.starts_with(b"\n") || stream_start.starts_with(b"\r") {
            stream_start = &stream_start[1..];
        }

        // Determine stream length from the dictionary.
        // /Length may be a direct integer or an indirect reference. If it's
        // an indirect reference, we can't resolve it during initial parsing,
        // so fall back to scanning for `endstream`.
        let length = dict
            .get(&PdfName::new("Length"))
            .and_then(|obj| obj.as_i64())
            .map(|l| l.max(0) as usize);

        let (data, mut rest) = if let Some(len) = length {
            if stream_start.len() >= len {
                (stream_start[..len].to_vec(), &stream_start[len..])
            } else {
                // /Length exceeds available data — fall back to endstream scan.
                // This recovers PDFs with incorrect /Length values.
                match find_endstream(stream_start) {
                    Some(actual_len) => (
                        stream_start[..actual_len].to_vec(),
                        &stream_start[actual_len..],
                    ),
                    None => {
                        return Err(nom::Err::Error(nom::error::Error::new(
                            stream_start,
                            nom::error::ErrorKind::Eof,
                        )));
                    }
                }
            }
        } else {
            // Indirect /Length — scan for endstream marker
            match find_endstream(stream_start) {
                Some(len) => (stream_start[..len].to_vec(), &stream_start[len..]),
                None => {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        stream_start,
                        nom::error::ErrorKind::Eof,
                    )));
                }
            }
        };

        // Skip optional whitespace before endstream
        while !rest.is_empty() && is_whitespace(rest[0]) {
            rest = &rest[1..];
        }

        // Consume `endstream`
        if rest.len() >= 9 && &rest[..9] == b"endstream" {
            rest = &rest[9..];
        }

        return Ok((rest, Object::Stream(PdfStream::new(dict, data))));
    }

    Ok((input, Object::Dictionary(dict)))
}

/// Scans for the `endstream` keyword to determine stream length when
/// `/Length` is an indirect reference that can't be resolved during parsing.
fn find_endstream(data: &[u8]) -> Option<usize> {
    // Search for "\nendstream" or "\r\nendstream" or "\rendstream"
    let marker = b"endstream";
    data.windows(marker.len())
        .position(|w| w == marker)
        .map(|pos| {
            // Trim trailing whitespace before endstream
            let mut end = pos;
            while end > 0 && (data[end - 1] == b'\n' || data[end - 1] == b'\r') {
                end -= 1;
            }
            end
        })
}

/// Parses an indirect reference: `10 0 R`.
///
/// This is tricky because `10 0` looks like two integers until we see the `R`.
/// We attempt to parse `integer whitespace integer whitespace R` and backtrack
/// if it fails.
fn parse_indirect_ref(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, obj_num_tok) = numeric_token(input)?;
    let obj_num_str = std::str::from_utf8(obj_num_tok).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    // Must be a non-negative integer
    if obj_num_str.contains('.') || obj_num_str.starts_with('-') {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Digit,
        )));
    }

    let obj_num: u32 = obj_num_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = skip_whitespace_and_comments(input)?;

    let (input, gen_tok) = numeric_token(input)?;
    let gen_str = std::str::from_utf8(gen_tok).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    if gen_str.contains('.') || gen_str.starts_with('-') {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Digit,
        )));
    }

    let gen: u16 = gen_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = skip_whitespace_and_comments(input)?;
    let (input, _) = char('R')(input)?;

    Ok((input, Object::Reference(IndirectRef::new(obj_num, gen))))
}

/// Parses any PDF object.
///
/// This is the main entry point for parsing a single PDF object from a byte stream.
/// It handles all PDF object types including indirect references.
pub fn parse_object(input: &[u8]) -> IResult<&[u8], Object> {
    let (input, _) = skip_whitespace_and_comments(input)?;

    alt((
        parse_boolean,
        parse_null,
        // Try indirect reference before number (both start with digits)
        parse_indirect_ref,
        parse_number,
        parse_literal_string,
        parse_hex_string,
        parse_name,
        parse_array,
        parse_dictionary,
    ))(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Boolean parsing ---

    #[test]
    fn parse_true() {
        let (_, obj) = parse_object(b"true").unwrap();
        assert_eq!(obj.as_bool(), Some(true));
    }

    #[test]
    fn parse_false() {
        let (_, obj) = parse_object(b"false").unwrap();
        assert_eq!(obj.as_bool(), Some(false));
    }

    // --- Null parsing ---

    #[test]
    fn parse_null_value() {
        let (_, obj) = parse_object(b"null").unwrap();
        assert!(obj.is_null());
    }

    // --- Integer parsing ---

    #[test]
    fn parse_positive_integer() {
        let (_, obj) = parse_object(b"42").unwrap();
        assert_eq!(obj.as_i64(), Some(42));
    }

    #[test]
    fn parse_negative_integer() {
        let (_, obj) = parse_object(b"-7").unwrap();
        assert_eq!(obj.as_i64(), Some(-7));
    }

    #[test]
    fn parse_zero() {
        let (_, obj) = parse_object(b"0").unwrap();
        assert_eq!(obj.as_i64(), Some(0));
    }

    // --- Real number parsing ---

    #[test]
    fn parse_real_basic() {
        let (_, obj) = parse_object(b"3.14").unwrap();
        assert_eq!(obj.as_f64(), Some(3.14));
        assert!(obj.is_real());
    }

    #[test]
    fn parse_real_leading_dot() {
        let (_, obj) = parse_object(b".5").unwrap();
        assert_eq!(obj.as_f64(), Some(0.5));
    }

    #[test]
    fn parse_real_negative() {
        let (_, obj) = parse_object(b"-2.5").unwrap();
        assert_eq!(obj.as_f64(), Some(-2.5));
    }

    #[test]
    fn parse_real_trailing_dot() {
        let (_, obj) = parse_object(b"10.").unwrap();
        assert_eq!(obj.as_f64(), Some(10.0));
        assert!(obj.is_real()); // Has a dot, so it's Real, not Integer
    }

    // --- Literal string parsing ---

    #[test]
    fn parse_simple_string() {
        let (_, obj) = parse_object(b"(Hello World)").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello World"));
        assert_eq!(s.format, StringFormat::Literal);
    }

    #[test]
    fn parse_string_with_escapes() {
        let (_, obj) = parse_object(b"(Hello\\nWorld)").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.bytes, b"Hello\nWorld");
    }

    #[test]
    fn parse_string_with_balanced_parens() {
        let (_, obj) = parse_object(b"(Hello (World))").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello (World)"));
    }

    #[test]
    fn parse_string_with_escaped_parens() {
        let (_, obj) = parse_object(b"(Hello \\(World\\))").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello (World)"));
    }

    #[test]
    fn parse_string_with_octal_escape() {
        let (_, obj) = parse_object(b"(\\110ello)").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello")); // \110 = 'H' in octal
    }

    #[test]
    fn parse_empty_string() {
        let (_, obj) = parse_object(b"()").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.bytes, b"");
    }

    #[test]
    fn parse_string_with_backslash_escape() {
        let (_, obj) = parse_object(b"(a\\\\b)").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("a\\b"));
    }

    // --- Hex string parsing ---

    #[test]
    fn parse_hex_string_basic() {
        let (_, obj) = parse_object(b"<48656C6C6F>").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello"));
        assert_eq!(s.format, StringFormat::Hexadecimal);
    }

    #[test]
    fn parse_hex_string_lowercase() {
        let (_, obj) = parse_object(b"<48656c6c6f>").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello"));
    }

    #[test]
    fn parse_hex_string_with_whitespace() {
        let (_, obj) = parse_object(b"<48 65 6C 6C 6F>").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.as_text(), Some("Hello"));
    }

    #[test]
    fn parse_hex_string_odd_digits() {
        // Odd number of hex digits: last digit is high nibble, pad with 0
        let (_, obj) = parse_object(b"<ABC>").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.bytes, vec![0xAB, 0xC0]);
    }

    #[test]
    fn parse_empty_hex_string() {
        let (_, obj) = parse_object(b"<>").unwrap();
        let s = obj.as_pdf_string().unwrap();
        assert_eq!(s.bytes, b"");
    }

    // --- Name parsing ---

    #[test]
    fn parse_simple_name() {
        let (_, obj) = parse_object(b"/Type").unwrap();
        assert_eq!(obj.as_name(), Some("Type"));
    }

    #[test]
    fn parse_name_with_hex_escape() {
        let (_, obj) = parse_object(b"/Name#20With#20Spaces").unwrap();
        assert_eq!(obj.as_name(), Some("Name With Spaces"));
    }

    #[test]
    fn parse_empty_name() {
        let (_, obj) = parse_object(b"/ ").unwrap();
        assert_eq!(obj.as_name(), Some(""));
    }

    // --- Array parsing ---

    #[test]
    fn parse_simple_array() {
        let (_, obj) = parse_object(b"[1 2 3]").unwrap();
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_i64(), Some(1));
        assert_eq!(arr[1].as_i64(), Some(2));
        assert_eq!(arr[2].as_i64(), Some(3));
    }

    #[test]
    fn parse_mixed_array() {
        let (_, obj) = parse_object(b"[1 3.14 (hello) /Name true null]").unwrap();
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 6);
        assert_eq!(arr[0].as_i64(), Some(1));
        assert!(arr[1].is_real());
        assert!(arr[2].is_string());
        assert!(arr[3].is_name());
        assert_eq!(arr[4].as_bool(), Some(true));
        assert!(arr[5].is_null());
    }

    #[test]
    fn parse_nested_array() {
        let (_, obj) = parse_object(b"[[1 2] [3 4]]").unwrap();
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn parse_empty_array() {
        let (_, obj) = parse_object(b"[]").unwrap();
        assert_eq!(obj.as_array().unwrap().len(), 0);
    }

    // --- Dictionary parsing ---

    #[test]
    fn parse_simple_dictionary() {
        let (_, obj) = parse_object(b"<< /Type /Page /Count 3 >>").unwrap();
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.len(), 2);
        assert_eq!(
            dict.get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Page")
        );
        assert_eq!(dict.get(&PdfName::new("Count")).unwrap().as_i64(), Some(3));
    }

    #[test]
    fn parse_nested_dictionary() {
        let (_, obj) = parse_object(b"<< /Font << /F1 /Helvetica >> >>").unwrap();
        let dict = obj.as_dict().unwrap();
        let font = dict.get(&PdfName::new("Font")).unwrap().as_dict().unwrap();
        assert_eq!(
            font.get(&PdfName::new("F1")).unwrap().as_name(),
            Some("Helvetica")
        );
    }

    #[test]
    fn parse_empty_dictionary() {
        let (_, obj) = parse_object(b"<< >>").unwrap();
        assert_eq!(obj.as_dict().unwrap().len(), 0);
    }

    #[test]
    fn parse_dictionary_with_array_value() {
        let (_, obj) = parse_object(b"<< /MediaBox [0 0 612 792] >>").unwrap();
        let dict = obj.as_dict().unwrap();
        let media_box = dict.get(&PdfName::new("MediaBox")).unwrap();
        assert_eq!(media_box.as_array().unwrap().len(), 4);
    }

    // --- Indirect reference parsing ---

    #[test]
    fn parse_indirect_ref() {
        let (_, obj) = parse_object(b"10 0 R").unwrap();
        let r = obj.as_reference().unwrap();
        assert_eq!(r.object_number, 10);
        assert_eq!(r.generation, 0);
    }

    #[test]
    fn parse_indirect_ref_nonzero_gen() {
        let (_, obj) = parse_object(b"5 2 R").unwrap();
        let r = obj.as_reference().unwrap();
        assert_eq!(r.object_number, 5);
        assert_eq!(r.generation, 2);
    }

    #[test]
    fn parse_dict_with_reference() {
        let (_, obj) = parse_object(b"<< /Pages 2 0 R >>").unwrap();
        let dict = obj.as_dict().unwrap();
        let pages = dict.get(&PdfName::new("Pages")).unwrap();
        let r = pages.as_reference().unwrap();
        assert_eq!(r.object_number, 2);
    }

    // --- Stream parsing ---

    #[test]
    fn parse_stream_basic() {
        let input = b"<< /Length 5 >>\nstream\nHello\nendstream";
        let (_, obj) = parse_object(input).unwrap();
        let stream = obj.as_stream().unwrap();
        assert_eq!(stream.data, b"Hello");
        assert_eq!(
            stream.dict.get(&PdfName::new("Length")).unwrap().as_i64(),
            Some(5)
        );
    }

    #[test]
    fn parse_stream_with_filter() {
        let data = vec![0u8; 10];
        let mut input = Vec::new();
        input.extend_from_slice(b"<< /Length 10 /Filter /FlateDecode >>\nstream\n");
        input.extend_from_slice(&data);
        input.extend_from_slice(b"\nendstream");

        let (_, obj) = parse_object(&input).unwrap();
        let stream = obj.as_stream().unwrap();
        assert_eq!(stream.data.len(), 10);
        assert_eq!(stream.filter().unwrap().as_name(), Some("FlateDecode"));
    }

    // --- Whitespace and comment handling ---

    #[test]
    fn parse_with_leading_whitespace() {
        let (_, obj) = parse_object(b"  \t\n  42").unwrap();
        assert_eq!(obj.as_i64(), Some(42));
    }

    #[test]
    fn parse_with_comments() {
        let (_, obj) = parse_object(b"% a comment\n42").unwrap();
        assert_eq!(obj.as_i64(), Some(42));
    }

    #[test]
    fn parse_array_with_comments() {
        let (_, obj) = parse_object(b"[1 % first\n2 % second\n3]").unwrap();
        let arr = obj.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    // --- Edge cases ---

    #[test]
    fn parse_large_integer() {
        let (_, obj) = parse_object(b"999999999").unwrap();
        assert_eq!(obj.as_i64(), Some(999999999));
    }

    #[test]
    fn parse_integer_not_confused_with_ref() {
        // "10 0" without "R" should parse as integer 10 (with "0" remaining)
        let (_rest, obj) = parse_object(b"10 ").unwrap();
        assert_eq!(obj.as_i64(), Some(10));
    }

    // --- Name hex decode tests ---

    #[test]
    fn decode_name_no_escapes() {
        assert_eq!(decode_name(b"Type"), "Type");
    }

    #[test]
    fn decode_name_with_escapes() {
        assert_eq!(decode_name(b"Name#20With#20Spaces"), "Name With Spaces");
    }

    #[test]
    fn decode_name_hash_at_end() {
        // Incomplete hex escape at end: keep as-is
        assert_eq!(decode_name(b"Name#2"), "Name#2");
    }

    #[test]
    fn stream_wrong_length_recovers_via_endstream_scan() {
        // /Length says 100 but actual stream data is only 5 bytes.
        // Should recover by scanning for endstream.
        let input = b"<< /Length 100 >>\nstream\nHello\nendstream";
        let (_, obj) = parse_object(input).unwrap();
        let stream = obj.as_stream().expect("should parse as stream");
        assert_eq!(stream.data, b"Hello");
    }

    #[test]
    fn stream_length_too_short_still_parses() {
        // /Length says 3 but actual data is 5 bytes before endstream.
        // The /Length wins here (spec says /Length is authoritative),
        // so we get truncated data. This is correct per spec.
        let input = b"<< /Length 3 >>\nstream\nHello\nendstream";
        let (_, obj) = parse_object(input).unwrap();
        let stream = obj.as_stream().expect("should parse as stream");
        assert_eq!(stream.data, b"Hel");
    }
}
