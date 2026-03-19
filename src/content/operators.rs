//! Content stream tokenizer.
//!
//! Parses PDF content streams into a sequence of tokens (operands and operators).
//! Content streams use postfix notation: operands appear before the operator
//! that consumes them.
//!
//! ISO 32000-2:2020, Section 7.8.2.

use crate::core::objects::{Dictionary, Object};
use crate::error::{PdfError, PdfResult};
use crate::parser::lexer::{is_whitespace, skip_whitespace_and_comments};
use crate::parser::objects::parse_object;

/// A token from a content stream: either an operand (PDF object) or
/// an operator name.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentToken {
    /// An operand value (number, string, name, array, dictionary).
    Operand(Object),
    /// An operator name (e.g., "BT", "Tj", "Tf", "cm").
    Operator(String),
    /// An inline image (BI dict ID data EI).
    InlineImage {
        /// The image dictionary (key-value pairs between BI and ID).
        dict: Dictionary,
        /// The raw image data (between ID and EI).
        data: Vec<u8>,
    },
}

/// Tokenizes an entire content stream into a sequence of operands and operators.
///
/// Content streams use postfix notation: zero or more operands followed by
/// an operator. This function returns all tokens in order; the caller
/// reconstructs the operand-operator groupings.
pub fn tokenize_content_stream(data: &[u8]) -> PdfResult<Vec<ContentToken>> {
    let mut tokens = Vec::with_capacity(data.len() / 10 + 16);
    let mut input = data;

    loop {
        // Skip whitespace and comments
        let (rest, _) = skip_whitespace_and_comments(input)
            .map_err(|e| PdfError::ParseError(format!("Content stream whitespace: {}", e)))?;
        input = rest;

        if input.is_empty() {
            break;
        }

        // Try parsing as an inline image first (BI ... ID ... EI)
        if input.starts_with(b"BI") && input.len() > 2 && is_whitespace(input[2]) {
            let (rest, bi_token) = parse_inline_image(input)?;
            tokens.push(bi_token);
            input = rest;
            continue;
        }

        // Try to parse as a PDF object (operand)
        match parse_object(input) {
            Ok((rest, obj)) => {
                // Check if this was actually an operator keyword that looks like
                // a name or boolean. PDF operators are bare keywords (not prefixed
                // with /). parse_object would parse "true"/"false" as booleans
                // and "/Name" as names, both of which are valid operands.
                tokens.push(ContentToken::Operand(obj));
                input = rest;
            }
            Err(_) => {
                // Not a PDF object — must be an operator keyword.
                let (rest, op_name) = parse_operator_name(input)?;
                tokens.push(ContentToken::Operator(op_name));
                input = rest;
            }
        }
    }

    Ok(tokens)
}

/// Parses an operator name (a sequence of regular bytes that isn't a PDF object).
fn parse_operator_name(input: &[u8]) -> PdfResult<(&[u8], String)> {
    if input.is_empty() {
        return Err(PdfError::ParseError(
            "Expected operator name, got EOF".to_string(),
        ));
    }

    let mut end = 0;
    while end < input.len() && !is_whitespace(input[end]) && !is_delimiter(input[end]) {
        end += 1;
    }

    if end == 0 {
        return Err(PdfError::ParseError(format!(
            "Unexpected byte in content stream: 0x{:02x}",
            input[0]
        )));
    }

    let name = std::str::from_utf8(&input[..end])
        .map_err(|e| PdfError::ParseError(format!("Invalid operator name: {}", e)))?
        .to_string();

    Ok((&input[end..], name))
}

/// Returns true if the byte is a PDF delimiter.
fn is_delimiter(b: u8) -> bool {
    crate::parser::lexer::is_delimiter(b)
}

/// Parses an inline image (BI dict ID data EI).
///
/// Captures the image dictionary and raw data bytes.
fn parse_inline_image(input: &[u8]) -> PdfResult<(&[u8], ContentToken)> {
    // Skip "BI"
    let mut pos = 2;

    // Parse key-value pairs between BI and ID into a dictionary
    let mut dict = Dictionary::new();
    loop {
        // Skip whitespace
        while pos < input.len() && is_whitespace(input[pos]) {
            pos += 1;
        }
        // Check for "ID" marker
        if pos + 1 < input.len() && input[pos] == b'I' && input[pos + 1] == b'D' {
            break;
        }
        if pos >= input.len() {
            return Err(PdfError::ParseError(
                "Inline image: ID marker not found".to_string(),
            ));
        }
        // Parse key-value pair
        let (rest, key) = parse_object(&input[pos..])
            .map_err(|e| PdfError::ParseError(format!("Inline image dict key: {}", e)))?;
        pos = input.len() - rest.len();
        let (rest, value) = parse_object(&input[pos..])
            .map_err(|e| PdfError::ParseError(format!("Inline image dict value: {}", e)))?;
        pos = input.len() - rest.len();

        if let Object::Name(name) = key {
            dict.insert(name, value);
        }
    }

    // Skip "ID" + single whitespace byte
    pos += 2;
    if pos < input.len() && is_whitespace(input[pos]) {
        pos += 1;
    }

    // Find "EI" marker — must be preceded by whitespace
    let data_start = pos;
    while pos + 2 < input.len() {
        if is_whitespace(input[pos])
            && input[pos + 1] == b'E'
            && input[pos + 2] == b'I'
            && (pos + 3 >= input.len() || is_whitespace(input[pos + 3]))
        {
            let image_data = input[data_start..pos].to_vec();
            let rest = &input[pos + 3..];

            return Ok((
                rest,
                ContentToken::InlineImage {
                    dict,
                    data: image_data,
                },
            ));
        }
        pos += 1;
    }

    Err(PdfError::ParseError(
        "Inline image: EI marker not found".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;

    #[test]
    fn tokenize_simple_text_showing() {
        let content = b"BT /F1 12 Tf (Hello World) Tj ET";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens.len(), 7);
        assert_eq!(tokens[0], ContentToken::Operator("BT".to_string()));
        assert_eq!(
            tokens[1],
            ContentToken::Operand(Object::Name(PdfName::new("F1")))
        );
        assert_eq!(tokens[2], ContentToken::Operand(Object::Integer(12)));
        assert_eq!(tokens[3], ContentToken::Operator("Tf".to_string()));
        assert!(matches!(
            tokens[4],
            ContentToken::Operand(Object::String(_))
        ));
        assert_eq!(tokens[5], ContentToken::Operator("Tj".to_string()));
        assert_eq!(tokens[6], ContentToken::Operator("ET".to_string()));
    }

    #[test]
    fn tokenize_text_with_positioning() {
        let content = b"BT 100 700 Td (Hello) Tj ET";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens.len(), 7);
        assert_eq!(tokens[0], ContentToken::Operator("BT".to_string()));
        assert_eq!(tokens[1], ContentToken::Operand(Object::Integer(100)));
        assert_eq!(tokens[2], ContentToken::Operand(Object::Integer(700)));
        assert_eq!(tokens[3], ContentToken::Operator("Td".to_string()));
    }

    #[test]
    fn tokenize_graphics_state() {
        let content = b"q 1 0 0 1 50 50 cm Q";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens[0], ContentToken::Operator("q".to_string()));
        assert_eq!(tokens[7], ContentToken::Operator("cm".to_string()));
        assert_eq!(tokens[8], ContentToken::Operator("Q".to_string()));
    }

    #[test]
    fn tokenize_with_comments() {
        let content = b"BT % begin text\n/F1 12 Tf\nET";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0], ContentToken::Operator("BT".to_string()));
    }

    #[test]
    fn tokenize_empty_stream() {
        let tokens = tokenize_content_stream(b"").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_whitespace_only() {
        let tokens = tokenize_content_stream(b"   \n\t  ").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_tj_array() {
        // TJ operator with array of strings and positioning
        let content = b"BT [(Hello ) -100 (World)] TJ ET";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens[0], ContentToken::Operator("BT".to_string()));
        assert!(matches!(tokens[1], ContentToken::Operand(Object::Array(_))));
        assert_eq!(tokens[2], ContentToken::Operator("TJ".to_string()));
        assert_eq!(tokens[3], ContentToken::Operator("ET".to_string()));
    }

    #[test]
    fn tokenize_hex_string_operand() {
        let content = b"BT <48656C6C6F> Tj ET";
        let tokens = tokenize_content_stream(content).unwrap();

        assert!(matches!(
            tokens[1],
            ContentToken::Operand(Object::String(_))
        ));
        if let ContentToken::Operand(Object::String(s)) = &tokens[1] {
            assert_eq!(s.as_text(), Some("Hello"));
        }
    }

    #[test]
    fn tokenize_inline_image() {
        // BI /W 2 /H 2 /BPC 8 /CS /RGB ID <4 bytes of data> EI
        let mut content = Vec::new();
        content.extend_from_slice(b"q BI /W 2 /H 2 ID ");
        content.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        content.extend_from_slice(b" EI Q");

        let tokens = tokenize_content_stream(&content).unwrap();
        // q, InlineImage, Q
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0], ContentToken::Operator("q".to_string()));
        match &tokens[1] {
            ContentToken::InlineImage { dict, data } => {
                // /W and /H keys should be in the dict
                assert_eq!(
                    dict.get(&PdfName::new("W")).and_then(|o| o.as_i64()),
                    Some(2)
                );
                assert_eq!(
                    dict.get(&PdfName::new("H")).and_then(|o| o.as_i64()),
                    Some(2)
                );
                assert_eq!(data, &[0xAA, 0xBB, 0xCC, 0xDD]);
            }
            _ => panic!("Expected InlineImage token"),
        }
        assert_eq!(tokens[2], ContentToken::Operator("Q".to_string()));
    }

    #[test]
    fn tokenize_real_number_operands() {
        let content = b"0.5 0 0 0.5 0 0 cm";
        let tokens = tokenize_content_stream(content).unwrap();

        assert_eq!(tokens.len(), 7);
        assert_eq!(tokens[6], ContentToken::Operator("cm".to_string()));
    }

    #[test]
    fn fuzz_crash_nested_parens_with_octal_escape() {
        // Crash found by libFuzzer: nested parens with octal \511 (>255)
        // and null byte. Must not panic.
        let input: &[u8] = &[40, 40, 92, 53, 49, 49, 45, 49, 0, 48, 49, 53, 47, 172];
        let _ = tokenize_content_stream(input);
    }
}
