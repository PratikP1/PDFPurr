//! ToUnicode CMap parser for PDF text extraction.
//!
//! Parses CMap streams that map character codes to Unicode strings.
//! These appear as `/ToUnicode` entries in font dictionaries and are
//! the most reliable way to extract Unicode text from PDFs.
//!
//! ISO 32000-2:2020, Section 9.10.3.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::core::objects::decode_utf16be;
use crate::error::{PdfError, PdfResult};
use crate::parser::lexer::hex_digit;

/// A parsed ToUnicode CMap that maps character codes to Unicode strings.
#[derive(Debug, Clone)]
pub struct ToUnicodeCMap {
    /// Single character mappings from `beginbfchar`/`endbfchar` sections.
    single_mappings: HashMap<Vec<u8>, String>,
    /// Range mappings from `beginbfrange`/`endbfrange` sections.
    range_mappings: Vec<CMapRange>,
}

/// A range mapping from a `bfrange` section.
#[derive(Debug, Clone)]
struct CMapRange {
    /// Start of the character code range (inclusive).
    start: Vec<u8>,
    /// End of the character code range (inclusive).
    end: Vec<u8>,
    /// What this range maps to.
    target: CMapTarget,
}

/// The target of a range mapping.
#[derive(Debug, Clone)]
enum CMapTarget {
    /// Base Unicode codepoint; each code in the range maps to base + offset.
    Base(Vec<u8>),
    /// Explicit array of Unicode strings, one per code in the range.
    Array(Vec<String>),
}

impl ToUnicodeCMap {
    /// Parses a ToUnicode CMap from raw stream data.
    pub fn parse(data: &[u8]) -> PdfResult<Self> {
        let tokens = tokenize_cmap(data);
        let mut cmap = ToUnicodeCMap {
            single_mappings: HashMap::new(),
            range_mappings: Vec::new(),
        };

        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "beginbfchar" => {
                    i += 1;
                    i = cmap.parse_bfchar(&tokens, i)?;
                }
                "beginbfrange" => {
                    i += 1;
                    i = cmap.parse_bfrange(&tokens, i)?;
                }
                _ => {
                    i += 1;
                }
            }
        }

        Ok(cmap)
    }

    /// Maps a character code (as bytes) to a Unicode string.
    ///
    /// Returns a borrowed string for single and array mappings, or an
    /// owned string for computed range mappings.
    pub fn map_code(&self, code: &[u8]) -> Option<Cow<'_, str>> {
        // Check single mappings first
        if let Some(s) = self.single_mappings.get(code) {
            return Some(Cow::Borrowed(s));
        }

        // Check range mappings
        for range in &self.range_mappings {
            if code.len() != range.start.len() {
                continue;
            }
            if code >= range.start.as_slice() && code <= range.end.as_slice() {
                let offset = code_offset(code, &range.start);
                match &range.target {
                    CMapTarget::Base(base) => {
                        let mut buf = [0u8; 4];
                        offset_hex_bytes_into(base, offset, &mut buf);
                        return Some(Cow::Owned(hex_bytes_to_unicode(&buf[..base.len()])));
                    }
                    CMapTarget::Array(arr) => {
                        if (offset as usize) < arr.len() {
                            return Some(Cow::Borrowed(&arr[offset as usize]));
                        }
                    }
                }
            }
        }

        None
    }

    /// Parses `bfchar` entries until `endbfchar`.
    fn parse_bfchar(&mut self, tokens: &[CMapToken], mut i: usize) -> PdfResult<usize> {
        while i + 1 < tokens.len() {
            if tokens[i].as_str() == "endbfchar" {
                return Ok(i + 1);
            }
            let src = parse_hex_token(&tokens[i])?;
            let dst_str = hex_bytes_to_unicode(&parse_hex_token(&tokens[i + 1])?);
            self.single_mappings.insert(src, dst_str);
            i += 2;
        }
        Ok(i)
    }

    /// Parses `bfrange` entries until `endbfrange`.
    fn parse_bfrange(&mut self, tokens: &[CMapToken], mut i: usize) -> PdfResult<usize> {
        while i + 2 < tokens.len() {
            if tokens[i].as_str() == "endbfrange" {
                return Ok(i + 1);
            }
            let start = parse_hex_token(&tokens[i])?;
            let end = parse_hex_token(&tokens[i + 1])?;

            let target = if tokens[i + 2].is_array() {
                // Array of Unicode strings
                let arr = parse_array_token(&tokens[i + 2])?;
                CMapTarget::Array(arr)
            } else {
                // Base Unicode value
                let base = parse_hex_token(&tokens[i + 2])?;
                CMapTarget::Base(base)
            };

            self.range_mappings.push(CMapRange { start, end, target });
            i += 3;
        }
        Ok(i)
    }
}

/// A token from the CMap tokenizer.
#[derive(Debug, Clone)]
enum CMapToken {
    /// A hex string: `<ABCD>`
    HexString(String),
    /// A keyword or other token
    Keyword(String),
    /// An array of hex strings: `[<0041> <0042>]`
    Array(Vec<String>),
}

impl CMapToken {
    fn as_str(&self) -> &str {
        match self {
            CMapToken::HexString(s) | CMapToken::Keyword(s) => s,
            CMapToken::Array(_) => "[]",
        }
    }

    fn is_array(&self) -> bool {
        matches!(self, CMapToken::Array(_))
    }
}

/// Tokenizes CMap stream data into a sequence of tokens.
fn tokenize_cmap(data: &[u8]) -> Vec<CMapToken> {
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < data.len() {
        // Skip whitespace
        if data[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Skip comments
        if data[i] == b'%' {
            while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                i += 1;
            }
            continue;
        }

        // Hex string
        if data[i] == b'<' {
            i += 1;
            let mut hex = String::new();
            while i < data.len() && data[i] != b'>' {
                if !data[i].is_ascii_whitespace() {
                    hex.push(data[i] as char);
                }
                i += 1;
            }
            if i < data.len() {
                i += 1; // skip '>'
            }
            tokens.push(CMapToken::HexString(hex));
            continue;
        }

        // Array of hex strings
        if data[i] == b'[' {
            i += 1;
            let mut arr = Vec::new();
            while i < data.len() && data[i] != b']' {
                if data[i] == b'<' {
                    i += 1;
                    let mut hex = String::new();
                    while i < data.len() && data[i] != b'>' {
                        if !data[i].is_ascii_whitespace() {
                            hex.push(data[i] as char);
                        }
                        i += 1;
                    }
                    if i < data.len() {
                        i += 1; // skip '>'
                    }
                    arr.push(hex);
                } else {
                    i += 1;
                }
            }
            if i < data.len() {
                i += 1; // skip ']'
            }
            tokens.push(CMapToken::Array(arr));
            continue;
        }

        // Keyword or other token
        let start = i;
        while i < data.len()
            && !data[i].is_ascii_whitespace()
            && data[i] != b'<'
            && data[i] != b'['
            && data[i] != b']'
        {
            i += 1;
        }
        if i > start {
            let word = String::from_utf8_lossy(&data[start..i]).to_string();
            tokens.push(CMapToken::Keyword(word));
        }
    }

    tokens
}

/// Parses a hex string token into bytes.
fn parse_hex_token(token: &CMapToken) -> PdfResult<Vec<u8>> {
    let hex = match token {
        CMapToken::HexString(h) => h,
        _ => {
            return Err(PdfError::EncodingError(
                "Expected hex string in CMap".to_string(),
            ))
        }
    };
    hex_string_to_bytes(hex)
}

/// Parses an array token into a vec of Unicode strings.
fn parse_array_token(token: &CMapToken) -> PdfResult<Vec<String>> {
    let arr = match token {
        CMapToken::Array(a) => a,
        _ => {
            return Err(PdfError::EncodingError(
                "Expected array in CMap".to_string(),
            ))
        }
    };
    arr.iter()
        .map(|hex| {
            let bytes = hex_string_to_bytes(hex)?;
            Ok(hex_bytes_to_unicode(&bytes))
        })
        .collect()
}

/// Converts a hex string (e.g., "0041") into bytes.
fn hex_string_to_bytes(hex: &str) -> PdfResult<Vec<u8>> {
    let raw = hex.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len() / 2);
    let mut i = 0;
    while i + 1 < raw.len() {
        let high = hex_digit(raw[i]).ok_or_else(|| {
            PdfError::EncodingError(format!("Invalid hex char in CMap: {}", raw[i] as char))
        })?;
        let low = hex_digit(raw[i + 1]).ok_or_else(|| {
            PdfError::EncodingError(format!("Invalid hex char in CMap: {}", raw[i + 1] as char))
        })?;
        bytes.push((high << 4) | low);
        i += 2;
    }
    // Handle odd-length hex string (pad with 0)
    if i < raw.len() {
        let high = hex_digit(raw[i]).ok_or_else(|| {
            PdfError::EncodingError(format!("Invalid hex char in CMap: {}", raw[i] as char))
        })?;
        bytes.push(high << 4);
    }
    Ok(bytes)
}

/// Converts hex bytes (big-endian UTF-16BE) to a Unicode string.
fn hex_bytes_to_unicode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if !bytes.len().is_multiple_of(2) {
        // Single byte — treat as direct Unicode codepoint
        return char::from_u32(bytes[0] as u32)
            .map(|c| c.to_string())
            .unwrap_or_default();
    }

    decode_utf16be(bytes).unwrap_or_default()
}

/// Computes the offset between two byte sequences of equal length,
/// treating them as big-endian unsigned integers.
fn code_offset(code: &[u8], start: &[u8]) -> u32 {
    let code_val = bytes_to_u32(code);
    let start_val = bytes_to_u32(start);
    code_val.wrapping_sub(start_val)
}

/// Converts a big-endian byte slice to a u32.
fn bytes_to_u32(bytes: &[u8]) -> u32 {
    let mut val: u32 = 0;
    for &b in bytes {
        val = (val << 8) | (b as u32);
    }
    val
}

/// Adds an offset to a hex byte sequence (big-endian addition) into a stack buffer.
///
/// CMap codes are at most 4 bytes, so a `[u8; 4]` suffices. The caller
/// must use `&buf[..base.len()]` to get the valid slice.
fn offset_hex_bytes_into(base: &[u8], offset: u32, buf: &mut [u8; 4]) {
    buf[..base.len()].copy_from_slice(base);
    let mut carry = offset;
    for byte in buf[..base.len()].iter_mut().rev() {
        let sum = *byte as u32 + carry;
        *byte = (sum & 0xFF) as u8;
        carry = sum >> 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bfchar_single() {
        let cmap_data = b"1 beginbfchar\n<0041> <0061>\nendbfchar";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x41]).as_deref(), Some("a"));
    }

    #[test]
    fn parse_bfchar_multiple() {
        let cmap_data = b"2 beginbfchar\n<0041> <0061>\n<0042> <0062>\nendbfchar";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x41]).as_deref(), Some("a"));
        assert_eq!(cmap.map_code(&[0x00, 0x42]).as_deref(), Some("b"));
    }

    #[test]
    fn parse_bfrange_with_base() {
        let cmap_data = b"1 beginbfrange\n<0041> <0043> <0061>\nendbfrange";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x41]).as_deref(), Some("a"));
        assert_eq!(cmap.map_code(&[0x00, 0x42]).as_deref(), Some("b"));
        assert_eq!(cmap.map_code(&[0x00, 0x43]).as_deref(), Some("c"));
    }

    #[test]
    fn parse_bfrange_with_array() {
        let cmap_data = b"1 beginbfrange\n<01> <03> [<0041> <0042> <0043>]\nendbfrange";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x01]).as_deref(), Some("A"));
        assert_eq!(cmap.map_code(&[0x02]).as_deref(), Some("B"));
        assert_eq!(cmap.map_code(&[0x03]).as_deref(), Some("C"));
    }

    #[test]
    fn map_code_not_found() {
        let cmap_data = b"1 beginbfchar\n<0041> <0061>\nendbfchar";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x99]).as_deref(), None);
    }

    #[test]
    fn parse_with_comments() {
        let cmap_data = b"% This is a comment\n1 beginbfchar\n<0041> <0061>\nendbfchar";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x41]).as_deref(), Some("a"));
    }

    #[test]
    fn multibyte_unicode_mapping() {
        let cmap_data = b"1 beginbfchar\n<0041> <00E9>\nendbfchar";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x00, 0x41]).as_deref(), Some("\u{00E9}"));
    }

    #[test]
    fn single_byte_codes() {
        let cmap_data = b"1 beginbfrange\n<20> <7E> <0020>\nendbfrange";
        let cmap = ToUnicodeCMap::parse(cmap_data).unwrap();
        assert_eq!(cmap.map_code(&[0x20]).as_deref(), Some(" "));
        assert_eq!(cmap.map_code(&[0x41]).as_deref(), Some("A"));
        assert_eq!(cmap.map_code(&[0x7E]).as_deref(), Some("~"));
    }

    #[test]
    fn hex_bytes_to_unicode_basic() {
        assert_eq!(hex_bytes_to_unicode(&[0x00, 0x41]), "A");
        assert_eq!(hex_bytes_to_unicode(&[0x00, 0xE9]), "\u{00E9}");
    }

    #[test]
    fn code_offset_basic() {
        assert_eq!(code_offset(&[0x00, 0x43], &[0x00, 0x41]), 2);
        assert_eq!(code_offset(&[0x01, 0x00], &[0x00, 0xFF]), 1);
    }
}
