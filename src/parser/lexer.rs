//! PDF lexer: low-level tokenization of PDF byte streams.
//!
//! Handles whitespace, comments, delimiters, and the basic building blocks
//! that higher-level parsers consume.

use nom::{
    branch::alt,
    bytes::complete::{tag, take_while, take_while1},
    character::complete::{char, one_of},
    combinator::{opt, recognize, value},
    sequence::preceded,
    IResult,
};

/// Returns true if the byte is a PDF whitespace character (ISO 32000-2:2020, Table 1).
pub fn is_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// Returns true if the byte is a PDF delimiter character (ISO 32000-2:2020, Table 2).
pub fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Returns true if the byte is a regular character (not whitespace or delimiter).
pub fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

/// Consumes zero or more whitespace characters.
pub fn skip_whitespace(input: &[u8]) -> IResult<&[u8], &[u8]> {
    take_while(is_whitespace)(input)
}

/// Consumes a PDF comment: `%` followed by all bytes until end of line.
pub fn comment(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let (input, _) = char('%')(input)?;
    let (input, content) = take_while(|b: u8| b != b'\n' && b != b'\r')(input)?;
    // Consume the line ending
    let (input, _) = opt(alt((tag(b"\r\n"), tag(b"\r"), tag(b"\n"))))(input)?;
    Ok((input, content))
}

/// Consumes all whitespace and comments (which are treated as whitespace).
pub fn skip_whitespace_and_comments(mut input: &[u8]) -> IResult<&[u8], ()> {
    loop {
        let start_len = input.len();
        let (rest, _) = take_while(is_whitespace)(input)?;
        input = rest;
        if input.first() == Some(&b'%') {
            let (rest, _) = comment(input)?;
            input = rest;
        }
        if input.len() == start_len {
            break;
        }
    }
    Ok((input, ()))
}

/// Parses `true` or `false`.
pub fn boolean(input: &[u8]) -> IResult<&[u8], bool> {
    alt((value(true, tag(b"true")), value(false, tag(b"false"))))(input)
}

/// Parses a numeric token (integer or real) as a byte slice.
///
/// Matches: optional sign, digits, optional decimal part.
/// The caller determines whether this is an integer or real.
pub fn numeric_token(input: &[u8]) -> IResult<&[u8], &[u8]> {
    recognize(|input| {
        let (input, _) = opt(one_of("+-"))(input)?;
        // Must have at least one digit or a dot followed by digits
        let (input, _) = alt((
            // Digits optionally followed by .digits
            recognize(|input| {
                let (input, _) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
                let (input, _) =
                    opt(preceded(char('.'), take_while(|b: u8| b.is_ascii_digit())))(input)?;
                Ok((input, ()))
            }),
            // .digits (no leading digits)
            recognize(|input| {
                let (input, _) = char('.')(input)?;
                let (input, _) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
                Ok((input, ()))
            }),
        ))(input)?;
        Ok((input, ()))
    })(input)
}

/// Parses a PDF Name token (everything after the `/`).
///
/// Returns the raw bytes of the name (without the leading `/`).
/// Name hex escapes (e.g., `#20` for space) are NOT decoded here;
/// that is handled by the object-level parser.
pub fn name_token(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let (input, _) = char('/')(input)?;
    // A name consists of regular characters and `#` hex escapes.
    // It ends at whitespace, delimiter, or EOF.
    take_while(|b: u8| is_regular(b) || b == b'#')(input)
}

/// Decodes a single hexadecimal ASCII digit to its numeric value (0-15).
pub fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Whitespace tests ---

    #[test]
    fn skip_whitespace_empty() {
        let (rest, ws) = skip_whitespace(b"hello").unwrap();
        assert_eq!(ws, b"");
        assert_eq!(rest, b"hello");
    }

    #[test]
    fn skip_whitespace_spaces() {
        let (rest, _) = skip_whitespace(b"   hello").unwrap();
        assert_eq!(rest, b"hello");
    }

    #[test]
    fn skip_whitespace_mixed() {
        let (rest, _) = skip_whitespace(b" \t\r\n\x0Chello").unwrap();
        assert_eq!(rest, b"hello");
    }

    #[test]
    fn skip_whitespace_null_byte() {
        let (rest, _) = skip_whitespace(b"\x00hello").unwrap();
        assert_eq!(rest, b"hello");
    }

    // --- Comment tests ---

    #[test]
    fn comment_basic() {
        let (rest, content) = comment(b"% this is a comment\nnext").unwrap();
        assert_eq!(content, b" this is a comment");
        assert_eq!(rest, b"next");
    }

    #[test]
    fn comment_crlf() {
        let (rest, _) = comment(b"% comment\r\nnext").unwrap();
        assert_eq!(rest, b"next");
    }

    #[test]
    fn skip_whitespace_and_comments_mixed() {
        let (rest, _) = skip_whitespace_and_comments(b"  % comment\n  % another\n  hello").unwrap();
        assert_eq!(rest, b"hello");
    }

    // --- Boolean tests ---

    #[test]
    fn parse_true() {
        let (rest, val) = boolean(b"true rest").unwrap();
        assert!(val);
        assert_eq!(rest, b" rest");
    }

    #[test]
    fn parse_false() {
        let (rest, val) = boolean(b"false rest").unwrap();
        assert!(!val);
        assert_eq!(rest, b" rest");
    }

    #[test]
    fn parse_boolean_not_matched() {
        assert!(boolean(b"yes").is_err());
    }

    // --- Numeric token tests ---

    #[test]
    fn integer_positive() {
        let (rest, tok) = numeric_token(b"123 ").unwrap();
        assert_eq!(tok, b"123");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn integer_negative() {
        let (rest, tok) = numeric_token(b"-456 ").unwrap();
        assert_eq!(tok, b"-456");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn integer_with_plus() {
        let (rest, tok) = numeric_token(b"+789 ").unwrap();
        assert_eq!(tok, b"+789");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn real_with_dot() {
        let (rest, tok) = numeric_token(b"3.14 ").unwrap();
        assert_eq!(tok, b"3.14");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn real_leading_dot() {
        let (rest, tok) = numeric_token(b".5 ").unwrap();
        assert_eq!(tok, b".5");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn real_trailing_dot() {
        let (rest, tok) = numeric_token(b"10. ").unwrap();
        assert_eq!(tok, b"10.");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn negative_real() {
        let (rest, tok) = numeric_token(b"-3.14 ").unwrap();
        assert_eq!(tok, b"-3.14");
        assert_eq!(rest, b" ");
    }

    // --- Name token tests ---

    #[test]
    fn name_simple() {
        let (rest, name) = name_token(b"/Type ").unwrap();
        assert_eq!(name, b"Type");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn name_with_hex_escape() {
        let (rest, name) = name_token(b"/Name#20With#20Spaces ").unwrap();
        assert_eq!(name, b"Name#20With#20Spaces");
        assert_eq!(rest, b" ");
    }

    #[test]
    fn name_stops_at_delimiter() {
        let (rest, name) = name_token(b"/Type/Subtype").unwrap();
        assert_eq!(name, b"Type");
        assert_eq!(rest, b"/Subtype");
    }

    #[test]
    fn name_empty() {
        // An empty name is valid in PDF (just `/`)
        let (rest, name) = name_token(b"/ ").unwrap();
        assert_eq!(name, b"");
        assert_eq!(rest, b" ");
    }

    // --- Character classification tests ---

    #[test]
    fn whitespace_classification() {
        assert!(is_whitespace(b' '));
        assert!(is_whitespace(b'\t'));
        assert!(is_whitespace(b'\n'));
        assert!(is_whitespace(b'\r'));
        assert!(is_whitespace(0x0C)); // form feed
        assert!(is_whitespace(0x00)); // null
        assert!(!is_whitespace(b'A'));
    }

    #[test]
    fn delimiter_classification() {
        for &d in b"()<>[]{}/%".iter() {
            assert!(is_delimiter(d), "Expected {} to be a delimiter", d as char);
        }
        assert!(!is_delimiter(b'A'));
        assert!(!is_delimiter(b' '));
    }

    #[test]
    fn regular_classification() {
        assert!(is_regular(b'A'));
        assert!(is_regular(b'z'));
        assert!(is_regular(b'0'));
        assert!(!is_regular(b' '));
        assert!(!is_regular(b'('));
    }
}
