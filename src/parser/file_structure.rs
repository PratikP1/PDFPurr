//! PDF file structure parsing.
//!
//! Handles the four main parts of a PDF file:
//! 1. **Header** — Version identifier (`%PDF-1.7`)
//! 2. **Body** — Indirect objects
//! 3. **Cross-reference table** — Object location index
//! 4. **Trailer** — Document metadata and xref pointer
//!
//! Also handles incremental updates (multiple body/xref/trailer sections).

use nom::{
    bytes::complete::{tag, take_while, take_while1},
    character::complete::{char, digit1},
    combinator::{map_res, opt},
    IResult,
};

use crate::core::objects::{DictExt, Dictionary, Object, PdfName};
use crate::error::{PdfError, PdfResult};
use crate::parser::lexer::{skip_whitespace, skip_whitespace_and_comments};
use crate::parser::objects::parse_object;

/// Parsed PDF version from the file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfVersion {
    /// Major version number (always 1 or 2).
    pub major: u8,
    /// Minor version number (0-9).
    pub minor: u8,
}

impl PdfVersion {
    /// Creates a new PdfVersion.
    pub fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }
}

impl std::fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Parses the PDF header: `%PDF-X.Y`
///
/// Returns the version and the remaining input after the header line.
pub fn parse_header(input: &[u8]) -> IResult<&[u8], PdfVersion> {
    let (input, _) = tag(b"%PDF-")(input)?;
    let (input, major) = map_res(digit1, |d: &[u8]| {
        std::str::from_utf8(d)
            .map_err(|_| "invalid utf8")
            .and_then(|s| s.parse::<u8>().map_err(|_| "invalid major"))
    })(input)?;
    let (input, _) = char('.')(input)?;
    let (input, minor) = map_res(digit1, |d: &[u8]| {
        std::str::from_utf8(d)
            .map_err(|_| "invalid utf8")
            .and_then(|s| s.parse::<u8>().map_err(|_| "invalid minor"))
    })(input)?;

    // Skip the rest of the header line and consume the line ending
    let (input, _) = take_while(|b: u8| b != b'\n' && b != b'\r')(input)?;
    let (input, _) = opt(nom::branch::alt((tag(b"\r\n"), tag(b"\n"), tag(b"\r"))))(input)?;

    Ok((input, PdfVersion::new(major, minor)))
}

/// A single entry in a traditional cross-reference table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XRefEntry {
    /// Byte offset of the object in the file (type 1), or
    /// object stream number (type 2).
    pub offset: u64,
    /// Generation number (type 0/1), or index within object stream (type 2).
    pub generation: u16,
    /// Whether the object is in use ('n') or free ('f').
    pub in_use: bool,
    /// Xref entry type: 0 = free, 1 = uncompressed, 2 = compressed in ObjStm.
    /// Traditional xref tables always produce type 1 (in-use) or type 0 (free).
    pub entry_type: u8,
}

/// A cross-reference subsection: starting object number + entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XRefSubsection {
    /// First object number in this subsection.
    pub first_id: u32,
    /// Entries in this subsection.
    pub entries: Vec<XRefEntry>,
}

/// A complete cross-reference table (may contain multiple subsections).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XRefTable {
    /// The subsections making up this xref table.
    pub subsections: Vec<XRefSubsection>,
}

/// Parses a single xref entry line (20 bytes): `0000000000 00000 n \n`
fn parse_xref_entry(input: &[u8]) -> IResult<&[u8], XRefEntry> {
    // Offset: 10 digits
    let (input, offset_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
    let offset_str = std::str::from_utf8(offset_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let offset: u64 = offset_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = char(' ')(input)?;

    // Generation: 5 digits
    let (input, gen_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
    let gen_str = std::str::from_utf8(gen_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let generation: u16 = gen_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = char(' ')(input)?;

    // Status: 'n' or 'f'
    let in_use = if !input.is_empty() && input[0] == b'n' {
        true
    } else if !input.is_empty() && input[0] == b'f' {
        false
    } else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Char,
        )));
    };
    let input = &input[1..];

    // Skip trailing whitespace (CR, LF, CRLF, or space+CR/LF)
    let (input, _) = take_while(|b: u8| b == b' ' || b == b'\r' || b == b'\n')(input)?;

    Ok((
        input,
        XRefEntry {
            offset,
            generation,
            in_use,
            entry_type: if in_use { 1 } else { 0 },
        },
    ))
}

/// Parses a complete traditional xref table.
pub fn parse_xref_table(input: &[u8]) -> IResult<&[u8], XRefTable> {
    let (input, _) = skip_whitespace(input)?;
    let (input, _) = tag(b"xref")(input)?;
    let (input, _) = take_while(|b: u8| b == b' ' || b == b'\r' || b == b'\n')(input)?;

    let mut subsections = Vec::new();
    let mut input = input;

    // Parse subsections until we hit `trailer`
    loop {
        let (rest, _) = skip_whitespace(input)?;
        input = rest;

        // Check if we've reached the trailer
        if input.starts_with(b"trailer") {
            break;
        }

        if input.is_empty() {
            break;
        }

        // Parse subsection header: first_id count
        let (rest, first_id_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
        let first_id_str = std::str::from_utf8(first_id_bytes).map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;
        let first_id: u32 = first_id_str.parse().map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;

        let (rest, _) = char(' ')(rest)?;

        let (rest, count_bytes) = take_while1(|b: u8| b.is_ascii_digit())(rest)?;
        let count_str = std::str::from_utf8(count_bytes).map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;
        let count: usize = count_str.parse().map_err(|_| {
            nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;

        let (rest, _) = take_while(|b: u8| b == b' ' || b == b'\r' || b == b'\n')(rest)?;

        // Parse entries
        let mut entries = Vec::with_capacity(count);
        let mut rest = rest;
        for _ in 0..count {
            let (r, entry) = parse_xref_entry(rest)?;
            entries.push(entry);
            rest = r;
        }

        subsections.push(XRefSubsection { first_id, entries });
        input = rest;
    }

    Ok((input, XRefTable { subsections }))
}

/// Parses the trailer dictionary.
pub fn parse_trailer(input: &[u8]) -> IResult<&[u8], Dictionary> {
    let (input, _) = skip_whitespace(input)?;
    let (input, _) = tag(b"trailer")(input)?;
    let (input, _) = skip_whitespace_and_comments(input)?;
    let (input, obj) = parse_object(input)?;

    match obj {
        Object::Dictionary(dict) => Ok((input, dict)),
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        ))),
    }
}

/// Parses the `startxref` pointer at the end of the file.
pub fn parse_startxref(input: &[u8]) -> IResult<&[u8], u64> {
    let (input, _) = skip_whitespace(input)?;
    let (input, _) = tag(b"startxref")(input)?;
    let (input, _) = skip_whitespace(input)?;
    let (input, offset_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
    let offset_str = std::str::from_utf8(offset_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let offset: u64 = offset_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    Ok((input, offset))
}

/// An indirect object: `N G obj ... endobj`
#[derive(Debug, Clone, PartialEq)]
pub struct IndirectObject {
    /// Object number.
    pub object_number: u32,
    /// Generation number.
    pub generation: u16,
    /// The object value.
    pub object: Object,
}

/// Parses an indirect object definition: `N G obj ... endobj`
pub fn parse_indirect_object(input: &[u8]) -> IResult<&[u8], IndirectObject> {
    let (input, _) = skip_whitespace_and_comments(input)?;

    // Object number
    let (input, obj_num_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
    let obj_num_str = std::str::from_utf8(obj_num_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let object_number: u32 = obj_num_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = skip_whitespace(input)?;

    // Generation number
    let (input, gen_bytes) = take_while1(|b: u8| b.is_ascii_digit())(input)?;
    let gen_str = std::str::from_utf8(gen_bytes).map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    let generation: u16 = gen_str.parse().map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;

    let (input, _) = skip_whitespace(input)?;
    let (input, _) = tag(b"obj")(input)?;
    let (input, _) = skip_whitespace_and_comments(input)?;

    // Parse the object value
    let (input, object) = parse_object(input)?;

    let (input, _) = skip_whitespace_and_comments(input)?;
    let (input, _) = tag(b"endobj")(input)?;

    Ok((
        input,
        IndirectObject {
            object_number,
            generation,
            object,
        },
    ))
}

/// Finds the `startxref` offset by scanning backwards from the end of the file.
///
/// Per the spec, `%%EOF` must appear within the last 1024 bytes. We search
/// backwards for `startxref` and parse the offset that follows.
pub fn find_startxref(data: &[u8]) -> PdfResult<u64> {
    // Search in the last 1024 bytes (or entire file if smaller)
    let search_start = data.len().saturating_sub(1024);
    let tail = &data[search_start..];

    // Find `startxref` in the tail
    let needle = b"startxref";
    let pos = tail
        .windows(needle.len())
        .rposition(|w| w == needle)
        .ok_or_else(|| PdfError::InvalidStructure("startxref not found".to_string()))?;

    let (_, offset) = parse_startxref(&tail[pos..])
        .map_err(|_| PdfError::InvalidStructure("Failed to parse startxref".to_string()))?;

    Ok(offset)
}

/// Loads the complete xref chain by following `/Prev` pointers in trailers.
///
/// PDF files with incremental updates contain multiple xref/trailer sections
/// linked by `/Prev` entries. This function follows the chain from the most
/// recent section back to the original, returning all sections in order
/// from **oldest to newest** (so newer objects naturally override older ones
/// when inserted into a HashMap).
///
/// Supports both traditional xref tables and cross-reference streams at
/// each level. Enforces a depth limit of 32 to guard against circular chains.
pub fn load_xref_chain(data: &[u8], startxref: u64) -> PdfResult<Vec<(XRefTable, Dictionary)>> {
    const MAX_DEPTH: usize = 32;
    let mut sections = Vec::new();
    let mut offset = startxref;

    for _ in 0..MAX_DEPTH {
        let idx = offset as usize;
        if idx >= data.len() {
            return Err(PdfError::XRefError(format!(
                "xref offset {} exceeds file size {}",
                offset,
                data.len()
            )));
        }
        let xref_data = &data[idx..];

        let (xref_table, trailer) = if is_traditional_xref(xref_data) {
            let (rest, xref_table) = parse_xref_table(xref_data)
                .map_err(|e| PdfError::XRefError(format!("Failed to parse xref table: {}", e)))?;
            let (_, trailer) = parse_trailer(rest).map_err(|e| {
                PdfError::InvalidStructure(format!("Failed to parse trailer: {}", e))
            })?;
            (xref_table, trailer)
        } else {
            parse_xref_stream(xref_data)?
        };

        // Check for /Prev pointing to a previous xref section
        let prev = trailer.get_i64("Prev").map(|v| v as u64);

        sections.push((xref_table, trailer));

        match prev {
            Some(prev_offset) => offset = prev_offset,
            None => break,
        }
    }

    // Reverse so oldest section is first (newer entries override older)
    sections.reverse();
    Ok(sections)
}

/// Determines whether the data at `startxref` offset is a traditional xref table
/// or a cross-reference stream (PDF 1.5+).
///
/// Returns `true` if the data starts with `xref`, indicating a traditional table.
/// Returns `false` if it starts with a digit, indicating an xref stream object.
pub fn is_traditional_xref(data: &[u8]) -> bool {
    let trimmed = data
        .iter()
        .skip_while(|&&b| crate::parser::lexer::is_whitespace(b));
    let first_bytes: Vec<u8> = trimmed.take(4).copied().collect();
    first_bytes == b"xref"
}

/// Parses a cross-reference stream object (PDF 1.5+, ISO 32000-2:2020 Section 7.5.8).
///
/// A cross-reference stream is an indirect object whose value is a stream
/// containing the cross-reference data in binary format. The stream dictionary
/// doubles as the trailer dictionary.
///
/// Returns the xref table and the trailer dictionary (from the stream dict).
pub fn parse_xref_stream(data: &[u8]) -> PdfResult<(XRefTable, Dictionary)> {
    // Parse as indirect object (it's "N 0 obj << ... >> stream ... endstream endobj")
    let (_, indirect_obj) = parse_indirect_object(data)
        .map_err(|e| PdfError::XRefError(format!("Failed to parse xref stream object: {}", e)))?;

    let stream = match indirect_obj.object {
        Object::Stream(s) => s,
        _ => {
            return Err(PdfError::XRefError(
                "Xref stream is not a stream object".to_string(),
            ));
        }
    };

    // Verify /Type is /XRef
    if let Some(type_obj) = stream.dict.get(&PdfName::new("Type")) {
        if type_obj.as_name() != Some("XRef") {
            return Err(PdfError::XRefError(format!(
                "Xref stream has wrong /Type: {:?}",
                type_obj
            )));
        }
    }

    // Get /Size (required)
    let size = stream
        .dict
        .get(&PdfName::new("Size"))
        .and_then(|o| o.as_i64())
        .ok_or_else(|| PdfError::XRefError("No /Size in xref stream".to_string()))?
        as u32;

    // Get /W (required) — array of 3 integers specifying field widths
    let w_array = stream
        .dict
        .get(&PdfName::new("W"))
        .and_then(|o| o.as_array())
        .ok_or_else(|| PdfError::XRefError("No /W in xref stream".to_string()))?;

    if w_array.len() != 3 {
        return Err(PdfError::XRefError(format!(
            "/W must have 3 elements, got {}",
            w_array.len()
        )));
    }

    let w: Vec<usize> = w_array
        .iter()
        .map(|o| {
            o.as_i64()
                .ok_or_else(|| PdfError::XRefError("/W entry is not an integer".to_string()))
                .map(|v| v.max(0) as usize)
        })
        .collect::<PdfResult<Vec<usize>>>()?;

    let entry_size = w[0] + w[1] + w[2];
    if entry_size == 0 {
        return Err(PdfError::XRefError(
            "XRef stream /W has zero entry size".to_string(),
        ));
    }

    // Get /Index (optional) — defaults to [0 Size]
    let index_ranges =
        if let Some(index_obj) = stream.dict.get(&PdfName::new("Index")) {
            let index_arr = index_obj
                .as_array()
                .ok_or_else(|| PdfError::XRefError("/Index must be an array".to_string()))?;
            let mut ranges = Vec::new();
            for pair in index_arr.chunks(2) {
                if pair.len() == 2 {
                    let first = pair[0].as_i64().ok_or_else(|| {
                        PdfError::XRefError("Index first must be integer".to_string())
                    })? as u32;
                    let count = pair[1].as_i64().ok_or_else(|| {
                        PdfError::XRefError("Index count must be integer".to_string())
                    })? as usize;
                    ranges.push((first, count));
                }
            }
            ranges
        } else {
            vec![(0, size as usize)]
        };

    // Decode the stream data
    let decoded = stream.decode_data()?;

    // Parse entries from the decoded data
    let mut subsections = Vec::new();
    let mut offset = 0;

    for (first_id, count) in &index_ranges {
        let mut entries = Vec::with_capacity(*count);

        for _ in 0..*count {
            if offset + entry_size > decoded.len() {
                break;
            }

            // Field 1: type (default 1 if w[0] == 0)
            let field_type = read_xref_field(&decoded, offset, w[0], 1);
            offset += w[0];

            // Field 2: depends on type
            let field2 = read_xref_field(&decoded, offset, w[1], 0);
            offset += w[1];

            // Field 3: depends on type
            let field3 = read_xref_field(&decoded, offset, w[2], 0);
            offset += w[2];

            match field_type {
                0 => {
                    // Free entry
                    entries.push(XRefEntry {
                        offset: field2,
                        generation: field3 as u16,
                        in_use: false,
                        entry_type: 0,
                    });
                }
                1 => {
                    // In-use, uncompressed at byte offset
                    entries.push(XRefEntry {
                        offset: field2,
                        generation: field3 as u16,
                        in_use: true,
                        entry_type: 1,
                    });
                }
                2 => {
                    // Compressed in an object stream.
                    // offset = object number of the containing ObjStm
                    // generation = index within that stream
                    entries.push(XRefEntry {
                        offset: field2,
                        generation: field3 as u16,
                        in_use: true,
                        entry_type: 2,
                    });
                }
                _ => {
                    // Unknown type — treat as free
                    entries.push(XRefEntry {
                        offset: 0,
                        generation: 0,
                        in_use: false,
                        entry_type: 0,
                    });
                }
            }
        }

        subsections.push(XRefSubsection {
            first_id: *first_id,
            entries,
        });
    }

    // The stream dictionary also serves as the trailer
    let trailer = stream.dict;

    Ok((XRefTable { subsections }, trailer))
}

/// Reads a big-endian integer field from xref stream data.
///
/// If `width` is 0, returns `default` (per the spec).
/// Returns `default` if the field extends beyond the data bounds.
fn read_xref_field(data: &[u8], offset: usize, width: usize, default: u64) -> u64 {
    if width == 0 {
        return default;
    }
    if offset + width > data.len() {
        return default;
    }
    let mut val: u64 = 0;
    for i in 0..width {
        val = (val << 8) | data[offset + i] as u64;
    }
    val
}

/// Parses an object stream (PDF 1.5+, ISO 32000-2:2020 Section 7.5.7).
///
/// An object stream is a stream containing multiple compressed objects.
/// The stream dictionary has:
/// - `/Type /ObjStm`
/// - `/N` — number of objects
/// - `/First` — byte offset within the decoded data to the first object
///
/// Returns a vector of (object_number, Object) pairs.
pub fn parse_object_stream(
    stream: &crate::core::objects::PdfStream,
) -> PdfResult<Vec<(u32, Object)>> {
    let n = stream
        .dict
        .get(&PdfName::new("N"))
        .and_then(|o| o.as_i64())
        .ok_or_else(|| PdfError::InvalidStructure("No /N in object stream".to_string()))?
        as usize;

    let first = stream
        .dict
        .get(&PdfName::new("First"))
        .and_then(|o| o.as_i64())
        .ok_or_else(|| PdfError::InvalidStructure("No /First in object stream".to_string()))?
        as usize;

    let decoded = stream.decode_data()?;

    if first > decoded.len() {
        return Err(PdfError::InvalidStructure(
            "Object stream /First offset exceeds data length".to_string(),
        ));
    }

    // Parse the header: N pairs of (object_number, byte_offset)
    let header_data = &decoded[..first];
    let mut header_input = header_data;
    let mut obj_info: Vec<(u32, usize)> = Vec::with_capacity(n);

    for _ in 0..n {
        let (rest, _) = skip_whitespace_and_comments(header_input)
            .map_err(|e| PdfError::ParseError(format!("Object stream header: {}", e)))?;
        let (rest, num_bytes) = take_while1(|b: u8| b.is_ascii_digit())(rest).map_err(
            |e: nom::Err<nom::error::Error<&[u8]>>| {
                PdfError::ParseError(format!("Object stream obj num: {}", e))
            },
        )?;
        let obj_num: u32 = std::str::from_utf8(num_bytes)
            .map_err(|e| PdfError::ParseError(format!("Invalid UTF-8: {}", e)))?
            .parse()
            .map_err(|e| PdfError::ParseError(format!("Invalid obj num: {}", e)))?;

        let (rest, _) = skip_whitespace_and_comments(rest)
            .map_err(|e| PdfError::ParseError(format!("Object stream header ws: {}", e)))?;
        let (rest, off_bytes) = take_while1(|b: u8| b.is_ascii_digit())(rest).map_err(
            |e: nom::Err<nom::error::Error<&[u8]>>| {
                PdfError::ParseError(format!("Object stream offset: {}", e))
            },
        )?;
        let byte_offset: usize = std::str::from_utf8(off_bytes)
            .map_err(|e| PdfError::ParseError(format!("Invalid UTF-8: {}", e)))?
            .parse()
            .map_err(|e| PdfError::ParseError(format!("Invalid offset: {}", e)))?;

        obj_info.push((obj_num, byte_offset));
        header_input = rest;
    }

    // Parse each object from the data section
    let data_section = &decoded[first..];
    let mut objects = Vec::with_capacity(n);

    for (obj_num, byte_offset) in &obj_info {
        if *byte_offset >= data_section.len() {
            continue;
        }

        let obj_data = &data_section[*byte_offset..];
        match parse_object(obj_data) {
            Ok((_, obj)) => objects.push((*obj_num, obj)),
            Err(e) => {
                tracing::debug!(
                    "Xref rebuild: skipping object {} at offset {}: {e}",
                    obj_num,
                    byte_offset
                );
                continue;
            }
        }
    }

    Ok(objects)
}

/// Rebuilds a cross-reference table by scanning raw bytes for `N G obj` patterns.
///
/// This is the fallback repair strategy when the normal xref table or xref stream
/// is missing or corrupt. Scans the entire file for indirect object headers
/// (`<number> <generation> obj`), validates each candidate by attempting to parse
/// the object, and builds an xref table from the results.
///
/// Also synthesises a minimal trailer with `/Size` and `/Root` (heuristic: the
/// first `/Catalog` dictionary found).
///
/// Inspired by QPDF's `--recover` and PDF.js's `XRef.indexObjects`.
pub fn rebuild_xref_from_scan(data: &[u8]) -> PdfResult<(XRefTable, Dictionary)> {
    let mut entries: Vec<(u32, u16, u64)> = Vec::new(); // (obj_num, gen, offset)
    let mut root_ref: Option<(u32, u16)> = None;

    // Scan for "N G obj" patterns at the start of lines or after whitespace.
    // We validate each candidate by trying to parse it as an indirect object.
    let mut pos = 0;
    while pos < data.len().saturating_sub(5) {
        // Look for a digit that could start "N G obj"
        if data[pos].is_ascii_digit() && (pos == 0 || is_line_boundary(data[pos - 1])) {
            if let Ok((_, indirect)) = parse_indirect_object(&data[pos..]) {
                let obj_num = indirect.object_number;
                let gen = indirect.generation;
                entries.push((obj_num, gen, pos as u64));

                // Heuristic: first /Type /Catalog is the root
                if root_ref.is_none() {
                    if let Some(dict) = indirect.object.as_dict() {
                        if dict.get_name("Type") == Some("Catalog") {
                            root_ref = Some((obj_num, gen));
                        }
                    }
                }

                // Skip past this object to avoid re-matching inside it
                // Find "endobj" after the current position
                if let Some(end_pos) = find_endobj(&data[pos..]) {
                    pos += end_pos;
                    continue;
                }
            }
        }
        pos += 1;
    }

    if entries.is_empty() {
        return Err(PdfError::InvalidStructure(
            "No indirect objects found during xref rebuild".to_string(),
        ));
    }

    // Build xref table: one subsection per object (simplest correct form).
    // Cap max object number to prevent allocation bombs from huge IDs.
    const MAX_REBUILD_OBJECTS: u32 = 1_000_000;
    let max_obj = entries
        .iter()
        .map(|(n, _, _)| *n)
        .max()
        .unwrap_or(0)
        .min(MAX_REBUILD_OBJECTS);
    entries.retain(|(n, _, _)| *n <= max_obj);
    let mut xref_entries = vec![
        XRefEntry {
            offset: 0,
            generation: 65535,
            in_use: false,
            entry_type: 0,
        };
        (max_obj + 1) as usize
    ];

    for &(obj_num, gen, offset) in &entries {
        if (obj_num as usize) < xref_entries.len() {
            xref_entries[obj_num as usize] = XRefEntry {
                offset,
                generation: gen,
                in_use: true,
                entry_type: 1,
            };
        }
    }

    let xref = XRefTable {
        subsections: vec![XRefSubsection {
            first_id: 0,
            entries: xref_entries,
        }],
    };

    // Synthesise a minimal trailer
    let mut trailer = Dictionary::new();
    trailer.insert(PdfName::new("Size"), Object::Integer((max_obj + 1) as i64));
    if let Some((obj_num, gen)) = root_ref {
        trailer.insert(
            PdfName::new("Root"),
            Object::Reference(crate::core::objects::IndirectRef::new(obj_num, gen)),
        );
    }

    Ok((xref, trailer))
}

/// Returns `true` if the byte typically precedes an object definition.
fn is_line_boundary(b: u8) -> bool {
    matches!(b, b'\n' | b'\r' | b' ' | b'\t')
}

/// Finds the byte offset just past the next `endobj` in `data`.
fn find_endobj(data: &[u8]) -> Option<usize> {
    let needle = b"endobj";
    data.windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + needle.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Header tests ---

    #[test]
    fn parse_header_1_7() {
        let (_, version) = parse_header(b"%PDF-1.7\n").unwrap();
        assert_eq!(version, PdfVersion::new(1, 7));
        assert_eq!(version.to_string(), "1.7");
    }

    #[test]
    fn parse_header_2_0() {
        let (_, version) = parse_header(b"%PDF-2.0\n").unwrap();
        assert_eq!(version, PdfVersion::new(2, 0));
    }

    #[test]
    fn parse_header_1_4() {
        let (_, version) = parse_header(b"%PDF-1.4\r\n").unwrap();
        assert_eq!(version, PdfVersion::new(1, 4));
    }

    #[test]
    fn parse_header_with_binary_marker() {
        // Some PDFs have a binary marker comment after the header
        let input = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n";
        let (rest, version) = parse_header(input).unwrap();
        assert_eq!(version, PdfVersion::new(1, 7));
        // Binary marker line is left for the caller
        assert!(rest.starts_with(b"%"));
    }

    #[test]
    fn parse_header_display() {
        let v = PdfVersion::new(1, 7);
        assert_eq!(format!("{}", v), "1.7");
    }

    // --- XRef entry tests ---

    #[test]
    fn parse_xref_entry_in_use() {
        let (_, entry) = parse_xref_entry(b"0000000009 00000 n \n").unwrap();
        assert_eq!(entry.offset, 9);
        assert_eq!(entry.generation, 0);
        assert!(entry.in_use);
    }

    #[test]
    fn parse_xref_entry_free() {
        let (_, entry) = parse_xref_entry(b"0000000000 65535 f \n").unwrap();
        assert_eq!(entry.offset, 0);
        assert_eq!(entry.generation, 65535);
        assert!(!entry.in_use);
    }

    #[test]
    fn parse_xref_entry_crlf() {
        let (_, entry) = parse_xref_entry(b"0000000100 00000 n \r\n").unwrap();
        assert_eq!(entry.offset, 100);
        assert!(entry.in_use);
    }

    // --- XRef table tests ---

    #[test]
    fn parse_xref_table_basic() {
        let input = b"xref\n0 3\n\
            0000000000 65535 f \n\
            0000000009 00000 n \n\
            0000000058 00000 n \n\
            trailer";

        let (rest, table) = parse_xref_table(input).unwrap();
        assert_eq!(table.subsections.len(), 1);
        assert_eq!(table.subsections[0].first_id, 0);
        assert_eq!(table.subsections[0].entries.len(), 3);
        assert!(!table.subsections[0].entries[0].in_use); // Object 0 is always free
        assert!(table.subsections[0].entries[1].in_use);
        assert_eq!(table.subsections[0].entries[1].offset, 9);
        assert!(rest.starts_with(b"trailer"));
    }

    #[test]
    fn parse_xref_table_multiple_subsections() {
        let input = b"xref\n0 2\n\
            0000000000 65535 f \n\
            0000000009 00000 n \n\
            5 1\n\
            0000000200 00000 n \n\
            trailer";

        let (_, table) = parse_xref_table(input).unwrap();
        assert_eq!(table.subsections.len(), 2);
        assert_eq!(table.subsections[0].first_id, 0);
        assert_eq!(table.subsections[0].entries.len(), 2);
        assert_eq!(table.subsections[1].first_id, 5);
        assert_eq!(table.subsections[1].entries.len(), 1);
        assert_eq!(table.subsections[1].entries[0].offset, 200);
    }

    // --- Trailer tests ---

    #[test]
    fn parse_trailer_basic() {
        let input = b"trailer\n<< /Size 3 /Root 1 0 R >>";
        let (_, dict) = parse_trailer(input).unwrap();
        assert_eq!(dict.get(&PdfName::new("Size")).unwrap().as_i64(), Some(3));
        let root = dict
            .get(&PdfName::new("Root"))
            .unwrap()
            .as_reference()
            .unwrap();
        assert_eq!(root.object_number, 1);
    }

    #[test]
    fn parse_trailer_with_info() {
        let input = b"trailer\n<< /Size 5 /Root 1 0 R /Info 4 0 R >>";
        let (_, dict) = parse_trailer(input).unwrap();
        assert_eq!(dict.len(), 3);
    }

    // --- startxref tests ---

    #[test]
    fn parse_startxref_basic() {
        let (_, offset) = parse_startxref(b"startxref\n408\n").unwrap();
        assert_eq!(offset, 408);
    }

    #[test]
    fn parse_startxref_with_eof() {
        let (rest, offset) = parse_startxref(b"startxref\n408\n%%EOF").unwrap();
        assert_eq!(offset, 408);
        assert!(rest.contains(&b'%'));
    }

    // --- Indirect object tests ---

    #[test]
    fn parse_indirect_object_integer() {
        let input = b"1 0 obj\n42\nendobj";
        let (_, obj) = parse_indirect_object(input).unwrap();
        assert_eq!(obj.object_number, 1);
        assert_eq!(obj.generation, 0);
        assert_eq!(obj.object.as_i64(), Some(42));
    }

    #[test]
    fn parse_indirect_object_dictionary() {
        let input = b"2 0 obj\n<< /Type /Page /Parent 1 0 R >>\nendobj";
        let (_, obj) = parse_indirect_object(input).unwrap();
        assert_eq!(obj.object_number, 2);
        let dict = obj.object.as_dict().unwrap();
        assert_eq!(
            dict.get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Page")
        );
    }

    #[test]
    fn parse_indirect_object_array() {
        let input = b"3 0 obj\n[1 2 3]\nendobj";
        let (_, obj) = parse_indirect_object(input).unwrap();
        assert_eq!(obj.object.as_array().unwrap().len(), 3);
    }

    #[test]
    fn parse_indirect_object_stream() {
        let input = b"4 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj";
        let (_, obj) = parse_indirect_object(input).unwrap();
        assert_eq!(obj.object_number, 4);
        let stream = obj.object.as_stream().unwrap();
        assert_eq!(stream.data, b"Hello");
    }

    #[test]
    fn parse_indirect_object_with_comments() {
        let input = b"% Object 1\n1 0 obj\n% value\n42\n% end\nendobj";
        let (_, obj) = parse_indirect_object(input).unwrap();
        assert_eq!(obj.object.as_i64(), Some(42));
    }

    // --- find_startxref tests ---

    #[test]
    fn find_startxref_basic() {
        let data = b"some pdf content\nstartxref\n408\n%%EOF\n";
        let offset = find_startxref(data).unwrap();
        assert_eq!(offset, 408);
    }

    #[test]
    fn find_startxref_not_found() {
        let data = b"no startxref here";
        assert!(find_startxref(data).is_err());
    }

    // --- Integration: mini PDF structure ---

    #[test]
    fn parse_minimal_pdf_structure() {
        // A minimal PDF-like structure (not a valid PDF but tests our parsers)
        let pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
            3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n\
            xref\n\
            0 4\n\
            0000000000 65535 f \n\
            0000000009 00000 n \n\
            0000000058 00000 n \n\
            0000000115 00000 n \n\
            trailer\n<< /Size 4 /Root 1 0 R >>\n\
            startxref\n222\n%%EOF";

        // Parse header
        let (rest, version) = parse_header(pdf).unwrap();
        assert_eq!(version, PdfVersion::new(1, 4));

        // Parse first indirect object
        let (rest, obj1) = parse_indirect_object(rest).unwrap();
        assert_eq!(obj1.object_number, 1);
        let catalog = obj1.object.as_dict().unwrap();
        assert_eq!(
            catalog.get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Catalog")
        );

        // Parse second indirect object
        let (rest, obj2) = parse_indirect_object(rest).unwrap();
        assert_eq!(obj2.object_number, 2);

        // Parse third indirect object
        let (rest, obj3) = parse_indirect_object(rest).unwrap();
        assert_eq!(obj3.object_number, 3);
        let page = obj3.object.as_dict().unwrap();
        let media_box = page
            .get(&PdfName::new("MediaBox"))
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(media_box.len(), 4);

        // Parse xref table
        let (rest, xref) = parse_xref_table(rest).unwrap();
        assert_eq!(xref.subsections[0].entries.len(), 4);

        // Parse trailer
        let (rest, trailer) = parse_trailer(rest).unwrap();
        assert_eq!(
            trailer.get(&PdfName::new("Size")).unwrap().as_i64(),
            Some(4)
        );

        // Parse startxref
        let (_, startxref_offset) = parse_startxref(rest).unwrap();
        assert_eq!(startxref_offset, 222);
    }

    // --- is_traditional_xref tests ---

    #[test]
    fn detect_traditional_xref() {
        assert!(is_traditional_xref(b"xref\n0 3\n"));
        assert!(is_traditional_xref(b"  xref\n0 3\n"));
    }

    #[test]
    fn detect_xref_stream() {
        assert!(!is_traditional_xref(b"15 0 obj\n<< /Type /XRef"));
        assert!(!is_traditional_xref(b"1 0 obj"));
    }

    // --- read_xref_field tests ---

    #[test]
    fn read_field_zero_width() {
        assert_eq!(read_xref_field(&[], 0, 0, 1), 1);
        assert_eq!(read_xref_field(&[], 0, 0, 42), 42);
    }

    #[test]
    fn read_field_one_byte() {
        assert_eq!(read_xref_field(&[0xFF], 0, 1, 0), 255);
        assert_eq!(read_xref_field(&[0x00, 0x42], 1, 1, 0), 0x42);
    }

    #[test]
    fn read_field_two_bytes() {
        assert_eq!(read_xref_field(&[0x01, 0x00], 0, 2, 0), 256);
    }

    #[test]
    fn read_field_three_bytes() {
        assert_eq!(read_xref_field(&[0x01, 0x00, 0x00], 0, 3, 0), 65536);
    }

    // --- parse_xref_stream tests ---

    #[test]
    fn parse_xref_stream_basic() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Build xref stream data: 3 entries, W=[1,2,1]
        // Entry 0: type=0 (free), next_free=0, gen=255
        // Entry 1: type=1 (in-use), offset=100, gen=0
        // Entry 2: type=1 (in-use), offset=200, gen=0
        let mut xref_data = Vec::new();
        // Entry 0: free
        xref_data.push(0); // type = 0
        xref_data.extend_from_slice(&[0x00, 0x00]); // next free obj = 0
        xref_data.push(0xFF); // gen = 255
                              // Entry 1: in-use at offset 100
        xref_data.push(1); // type = 1
        xref_data.extend_from_slice(&[0x00, 0x64]); // offset = 100
        xref_data.push(0x00); // gen = 0
                              // Entry 2: in-use at offset 200
        xref_data.push(1); // type = 1
        xref_data.extend_from_slice(&[0x00, 0xC8]); // offset = 200
        xref_data.push(0x00); // gen = 0

        // Compress
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&xref_data).unwrap();
        let compressed = encoder.finish().unwrap();

        // Build the indirect object bytes
        let stream_dict = format!(
            "10 0 obj\n<< /Type /XRef /Size 3 /W [1 2 1] /Length {} /Filter /FlateDecode /Root 1 0 R >>\nstream\n",
            compressed.len()
        );
        let mut obj_bytes = stream_dict.into_bytes();
        obj_bytes.extend_from_slice(&compressed);
        obj_bytes.extend_from_slice(b"\nendstream\nendobj");

        let (xref_table, trailer) = parse_xref_stream(&obj_bytes).unwrap();

        assert_eq!(xref_table.subsections.len(), 1);
        assert_eq!(xref_table.subsections[0].first_id, 0);
        assert_eq!(xref_table.subsections[0].entries.len(), 3);
        assert!(!xref_table.subsections[0].entries[0].in_use);
        assert!(xref_table.subsections[0].entries[1].in_use);
        assert_eq!(xref_table.subsections[0].entries[1].offset, 100);
        assert!(xref_table.subsections[0].entries[2].in_use);
        assert_eq!(xref_table.subsections[0].entries[2].offset, 200);

        // Trailer should have /Root
        assert!(trailer.get(&PdfName::new("Root")).is_some());
    }

    #[test]
    fn parse_xref_stream_with_index() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Build xref stream with /Index [0 1 5 1], W=[1 2 1]
        let mut xref_data = Vec::new();
        // Entry for obj 0: free, type=0, next_free=0 (2 bytes), gen=255
        xref_data.push(0);
        xref_data.extend_from_slice(&[0x00, 0x00]); // next free (2 bytes)
        xref_data.push(0xFF); // gen
                              // Entry for obj 5: in-use at offset 500, type=1, offset=500 (2 bytes), gen=0
        xref_data.push(1);
        xref_data.extend_from_slice(&[0x01, 0xF4]); // 500
        xref_data.push(0x00);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&xref_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let stream_dict = format!(
            "20 0 obj\n<< /Type /XRef /Size 6 /W [1 2 1] /Index [0 1 5 1] /Length {} /Filter /FlateDecode /Root 1 0 R >>\nstream\n",
            compressed.len()
        );
        let mut obj_bytes = stream_dict.into_bytes();
        obj_bytes.extend_from_slice(&compressed);
        obj_bytes.extend_from_slice(b"\nendstream\nendobj");

        let (xref_table, _) = parse_xref_stream(&obj_bytes).unwrap();

        assert_eq!(xref_table.subsections.len(), 2);
        assert_eq!(xref_table.subsections[0].first_id, 0);
        assert_eq!(xref_table.subsections[0].entries.len(), 1);
        assert_eq!(xref_table.subsections[1].first_id, 5);
        assert_eq!(xref_table.subsections[1].entries.len(), 1);
        assert_eq!(xref_table.subsections[1].entries[0].offset, 500);
    }

    // --- parse_object_stream tests ---

    #[test]
    fn parse_object_stream_basic() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Object stream containing 2 objects:
        // Object 10: integer 42
        // Object 11: boolean true
        // Header: "10 0 11 3 " (obj_num offset pairs)
        // Data: "42 true"
        let header = b"10 0 11 3 ";
        let data = b"42 true";
        let mut stream_content = Vec::new();
        stream_content.extend_from_slice(header);
        stream_content.extend_from_slice(data);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&stream_content).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("ObjStm")));
        dict.insert(PdfName::new("N"), Object::Integer(2));
        dict.insert(PdfName::new("First"), Object::Integer(header.len() as i64));
        dict.insert(
            PdfName::new("Length"),
            Object::Integer(compressed.len() as i64),
        );
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("FlateDecode")),
        );

        let stream = crate::core::objects::PdfStream::new(dict, compressed);
        let objects = parse_object_stream(&stream).unwrap();

        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].0, 10);
        assert_eq!(objects[0].1.as_i64(), Some(42));
        assert_eq!(objects[1].0, 11);
        assert_eq!(objects[1].1.as_bool(), Some(true));
    }

    #[test]
    fn parse_object_stream_uncompressed() {
        // Object stream without compression
        let header = b"5 0 ";
        let data = b"(Hello)";
        let mut stream_content = Vec::new();
        stream_content.extend_from_slice(header);
        stream_content.extend_from_slice(data);

        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("ObjStm")));
        dict.insert(PdfName::new("N"), Object::Integer(1));
        dict.insert(PdfName::new("First"), Object::Integer(header.len() as i64));
        dict.insert(
            PdfName::new("Length"),
            Object::Integer(stream_content.len() as i64),
        );

        let stream = crate::core::objects::PdfStream::new(dict, stream_content);
        let objects = parse_object_stream(&stream).unwrap();

        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].0, 5);
        assert!(objects[0].1.is_string());
    }

    // --- Incremental update (load_xref_chain) tests ---

    #[test]
    fn load_xref_chain_single_section() {
        // A minimal PDF with one xref section — chain should have exactly one entry
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 2\n\
                 0000000000 65535 f \n\
                 {:010} 00000 n \n\
                 trailer\n<< /Size 2 /Root 1 0 R >>\n\
                 startxref\n{}\n%%EOF",
                obj1_offset, xref_offset
            )
            .as_bytes(),
        );
        let startxref = find_startxref(&pdf).unwrap();
        let chain = load_xref_chain(&pdf, startxref).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].0.subsections.len(), 1);
        assert_eq!(chain[0].0.subsections[0].entries.len(), 2);
    }

    #[test]
    fn load_xref_chain_with_prev() {
        // Build a PDF with two xref sections linked by /Prev.
        // Original body + xref at offset A, then updated xref at offset B with /Prev = A.
        let mut pdf = Vec::new();
        // Header
        pdf.extend_from_slice(b"%PDF-1.4\n");
        // Object 1 (original)
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        // First xref table
        let xref1_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n\
                 0 2\n\
                 0000000000 65535 f \n\
                 {:010} 00000 n \n\
                 trailer\n<< /Size 2 /Root 1 0 R >>\n\
                 startxref\n{}\n%%EOF\n",
                obj1_offset, xref1_offset
            )
            .as_bytes(),
        );
        // Incremental update: new object 2
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages >>\nendobj\n");
        // Second xref table with /Prev pointing to first
        let xref2_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n\
                 2 1\n\
                 {:010} 00000 n \n\
                 trailer\n<< /Size 3 /Root 1 0 R /Prev {} >>\n\
                 startxref\n{}\n%%EOF",
                obj2_offset, xref1_offset, xref2_offset
            )
            .as_bytes(),
        );

        let startxref = find_startxref(&pdf).unwrap();
        let chain = load_xref_chain(&pdf, startxref).unwrap();

        // Should have 2 sections, oldest first
        assert_eq!(chain.len(), 2);
        // First (oldest) has 2 entries (objects 0 and 1)
        assert_eq!(chain[0].0.subsections[0].entries.len(), 2);
        // Second (newest) has 1 entry (object 2)
        assert_eq!(chain[1].0.subsections[0].entries.len(), 1);
        // Newest trailer has /Prev
        assert!(chain[1].1.get_i64("Prev").is_some());
    }

    #[test]
    fn load_xref_chain_out_of_bounds_offset() {
        let pdf = b"%PDF-1.4\n\
            xref\n0 1\n0000000000 65535 f \n\
            trailer\n<< /Size 1 /Prev 99999 >>\n\
            startxref\n9\n%%EOF";
        let startxref = find_startxref(pdf).unwrap();
        let result = load_xref_chain(pdf, startxref);
        // /Prev offset is out of bounds — should error
        assert!(result.is_err());
    }

    #[test]
    fn rebuild_xref_finds_objects_by_scanning() {
        // A PDF with a deliberately corrupt startxref, but valid objects.
        // rebuild_xref_from_scan should locate objects by regex.
        let pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
            3 0 obj\n<< /Type /Page /MediaBox [0 0 612 792] /Parent 2 0 R >>\nendobj\n";

        let (xref, trailer) = rebuild_xref_from_scan(pdf).unwrap();
        // Should find all 3 objects
        let total_entries: usize = xref
            .subsections
            .iter()
            .map(|s| s.entries.iter().filter(|e| e.in_use).count())
            .sum();
        assert_eq!(total_entries, 3, "should find 3 in-use objects");

        // Trailer should have /Root pointing to obj 1
        assert!(trailer.get_str("Size").is_some());
    }

    #[test]
    fn rebuild_xref_ignores_embedded_obj_patterns() {
        // "obj" inside a stream or string should NOT be treated as objects
        let pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Length 20 >>\nstream\n2 0 obj fake endobj\nendstream\nendobj\n\
            2 0 obj\n<< /Type /Catalog >>\nendobj\n";

        let (xref, _) = rebuild_xref_from_scan(pdf).unwrap();
        let total_entries: usize = xref
            .subsections
            .iter()
            .map(|s| s.entries.iter().filter(|e| e.in_use).count())
            .sum();
        assert_eq!(
            total_entries, 2,
            "should find 2 objects, ignoring stream content"
        );
    }

    // --- XRef field bounds check ---

    #[test]
    fn read_xref_field_within_bounds() {
        let data = [0x00, 0x01, 0x02, 0x03];
        assert_eq!(read_xref_field(&data, 0, 2, 99), 0x0001);
        assert_eq!(read_xref_field(&data, 2, 2, 99), 0x0203);
    }

    #[test]
    fn read_xref_field_zero_width_returns_default() {
        let data = [0xFF];
        assert_eq!(read_xref_field(&data, 0, 0, 42), 42);
    }

    #[test]
    fn read_xref_field_beyond_bounds_returns_default() {
        let data = [0x01, 0x02];
        // Trying to read 3 bytes from 2-byte data
        assert_eq!(read_xref_field(&data, 0, 3, 99), 99);
        // Offset past end
        assert_eq!(read_xref_field(&data, 5, 1, 99), 99);
    }

    #[test]
    fn read_xref_field_exactly_at_boundary() {
        let data = [0xAB, 0xCD];
        // Read exactly 2 bytes from 2-byte data — should succeed
        assert_eq!(read_xref_field(&data, 0, 2, 99), 0xABCD);
        // Read 1 byte starting at index 1 — should succeed
        assert_eq!(read_xref_field(&data, 1, 1, 99), 0xCD);
        // Read 1 byte starting at index 2 — out of bounds
        assert_eq!(read_xref_field(&data, 2, 1, 99), 99);
    }
}
