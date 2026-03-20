//! PDF stream filter (decompression/decoding) support.
//!
//! Implements the standard PDF stream filters defined in
//! ISO 32000-2:2020, Section 7.4. Filters transform stream data
//! between its encoded (stored) form and its decoded (usable) form.

use crate::core::objects::{DictExt, Dictionary, Object, PdfName};
use crate::error::{PdfError, PdfResult};
use crate::parser::lexer::hex_digit;

/// Decodes stream data by applying the filter pipeline specified in the
/// stream dictionary's `/Filter` entry.
///
/// If no `/Filter` is present, returns the data unchanged.
/// Supports single filters (Name) and filter chains (Array of Names).
pub fn decode_stream(data: &[u8], dict: &Dictionary) -> PdfResult<Vec<u8>> {
    let filter_obj = match dict.get(&PdfName::new("Filter")) {
        Some(obj) => obj,
        None => return Ok(data.to_vec()),
    };

    let filters = extract_filter_names(filter_obj)?;
    let decode_parms = extract_decode_parms(dict, filters.len());

    // Apply each filter, then its corresponding predictor (if any).
    let mut result = apply_filter(&filters[0], data)?;
    if let Some(parms) = decode_parms.first().and_then(|p| p.as_ref()) {
        result = apply_predictor(&result, parms)?;
    }

    for (i, filter_name) in filters[1..].iter().enumerate() {
        result = apply_filter(filter_name, &result)?;
        if let Some(parms) = decode_parms.get(i + 1).and_then(|p| p.as_ref()) {
            result = apply_predictor(&result, parms)?;
        }
    }

    Ok(result)
}

/// Extracts `/DecodeParms` entries corresponding to each filter.
///
/// Returns a Vec of `Option<&Dictionary>` aligned with the filter list.
/// A single `/DecodeParms` dict applies to the single filter; an array
/// of dicts/nulls applies to the corresponding filter by index.
fn extract_decode_parms(dict: &Dictionary, filter_count: usize) -> Vec<Option<&Dictionary>> {
    let parms_obj = match dict
        .get(&PdfName::new("DecodeParms"))
        .or_else(|| dict.get(&PdfName::new("DP")))
    {
        Some(obj) => obj,
        None => return vec![None; filter_count],
    };

    match parms_obj {
        Object::Dictionary(d) => {
            let mut result = vec![None; filter_count];
            if !result.is_empty() {
                result[0] = Some(d);
            }
            result
        }
        Object::Array(arr) => arr
            .iter()
            .map(|obj| obj.as_dict())
            .chain(std::iter::repeat(None))
            .take(filter_count)
            .collect(),
        _ => vec![None; filter_count],
    }
}

/// Applies PNG/TIFF predictor decoding to decompressed data.
///
/// ISO 32000-2:2020, Table 8. Predictor values:
/// - 1: No prediction (identity)
/// - 2: TIFF Predictor 2 (horizontal differencing)
/// - 10-15: PNG predictors (None, Sub, Up, Average, Paeth, Optimum)
fn apply_predictor(data: &[u8], parms: &Dictionary) -> PdfResult<Vec<u8>> {
    let predictor = parms.get_i64("Predictor").unwrap_or(1);
    if predictor == 1 {
        return Ok(data.to_vec());
    }

    // Clamp predictor parameters to sane ranges to prevent allocation bombs.
    // Real-world PDFs rarely exceed 10000 columns or 4 color components.
    let columns = parms.get_i64("Columns").unwrap_or(1).clamp(1, 100_000) as usize;
    let colors = parms.get_i64("Colors").unwrap_or(1).clamp(1, 64) as usize;
    let bits_per_component = parms.get_i64("BitsPerComponent").unwrap_or(8).clamp(1, 32) as usize;

    if predictor == 2 {
        return apply_tiff_predictor(data, columns, colors, bits_per_component);
    }

    if (10..=15).contains(&predictor) {
        return apply_png_predictor(data, columns, colors, bits_per_component);
    }

    // Unknown predictor — return data unchanged
    Ok(data.to_vec())
}

/// Applies TIFF Predictor 2 (horizontal differencing).
fn apply_tiff_predictor(
    data: &[u8],
    columns: usize,
    colors: usize,
    bpc: usize,
) -> PdfResult<Vec<u8>> {
    if bpc != 8 {
        // Only 8-bit supported for now; return unchanged for other bpc
        return Ok(data.to_vec());
    }
    let row_bytes = columns.checked_mul(colors).unwrap_or(0);
    if row_bytes == 0 {
        return Ok(data.to_vec());
    }

    let mut result = data.to_vec();
    for row_start in (0..result.len()).step_by(row_bytes) {
        let row_end = (row_start + row_bytes).min(result.len());
        for i in (row_start + colors)..row_end {
            result[i] = result[i].wrapping_add(result[i - colors]);
        }
    }
    Ok(result)
}

/// Applies PNG predictor decoding (predictors 10-15).
///
/// Each row is prefixed with a 1-byte filter type:
/// 0=None, 1=Sub, 2=Up, 3=Average, 4=Paeth.
fn apply_png_predictor(
    data: &[u8],
    columns: usize,
    colors: usize,
    bpc: usize,
) -> PdfResult<Vec<u8>> {
    let bytes_per_pixel = colors.saturating_mul(bpc).div_ceil(8);
    let row_data_bytes = columns
        .saturating_mul(colors)
        .saturating_mul(bpc)
        .div_ceil(8);
    // Each row has a 1-byte filter type prefix
    let row_total = 1 + row_data_bytes;

    // Guard against absurd row sizes from malicious parameters
    if row_total <= 1 || row_data_bytes > 16 * 1024 * 1024 {
        return Ok(data.to_vec());
    }

    let num_rows = data.len() / row_total;
    let alloc_size = num_rows
        .saturating_mul(row_data_bytes)
        .min(256 * 1024 * 1024);
    let mut result = Vec::with_capacity(alloc_size);
    let mut prev_row = vec![0u8; row_data_bytes];

    for row_idx in 0..num_rows {
        let row_start = row_idx * row_total;
        if row_start >= data.len() {
            break;
        }
        let filter_type = data[row_start];
        let row_data = &data[row_start + 1..(row_start + row_total).min(data.len())];

        let mut current_row = vec![0u8; row_data_bytes];
        let actual_len = row_data.len().min(row_data_bytes);

        for i in 0..actual_len {
            let raw = row_data[i];
            let left = if i >= bytes_per_pixel {
                current_row[i - bytes_per_pixel]
            } else {
                0
            };
            let up = prev_row[i];
            let up_left = if i >= bytes_per_pixel {
                prev_row[i - bytes_per_pixel]
            } else {
                0
            };

            current_row[i] = match filter_type {
                0 => raw,                                                     // None
                1 => raw.wrapping_add(left),                                  // Sub
                2 => raw.wrapping_add(up),                                    // Up
                3 => raw.wrapping_add(((left as u16 + up as u16) / 2) as u8), // Average
                4 => raw.wrapping_add(paeth_predictor(left, up, up_left)),    // Paeth
                _ => raw, // Unknown filter — treat as None
            };
        }

        result.extend_from_slice(&current_row);
        prev_row = current_row;
    }

    Ok(result)
}

/// Paeth predictor function (PNG specification).
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i16;
    let b = b as i16;
    let c = c as i16;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Extracts filter names from a `/Filter` value, which can be a single
/// Name or an Array of Names.
fn extract_filter_names(filter_obj: &Object) -> PdfResult<Vec<String>> {
    match filter_obj {
        Object::Name(name) => Ok(vec![name.as_str().to_string()]),
        Object::Array(arr) => {
            let mut names = Vec::with_capacity(arr.len());
            for obj in arr {
                match obj {
                    Object::Name(name) => names.push(name.as_str().to_string()),
                    _ => {
                        return Err(PdfError::TypeError {
                            expected: "Name".to_string(),
                            found: obj.type_name().to_string(),
                        });
                    }
                }
            }
            Ok(names)
        }
        _ => Err(PdfError::TypeError {
            expected: "Name or Array".to_string(),
            found: filter_obj.type_name().to_string(),
        }),
    }
}

/// Applies a single named filter to decode data.
fn apply_filter(name: &str, data: &[u8]) -> PdfResult<Vec<u8>> {
    match name {
        "FlateDecode" | "Fl" => decode_flate(data),
        "ASCIIHexDecode" | "AHx" => decode_ascii_hex(data),
        "ASCII85Decode" | "A85" => decode_ascii85(data),
        "LZWDecode" | "LZW" => decode_lzw(data),
        "RunLengthDecode" | "RL" => decode_run_length(data),
        "CCITTFaxDecode" | "CCF" => decode_ccitt_fax(data),
        // JPEG and JPEG2000 data is self-contained — passthrough is correct
        // for image extraction. Pixel decoding happens at a higher level.
        "DCTDecode" | "DCT" => Ok(data.to_vec()),
        "JPXDecode" => Ok(data.to_vec()),
        other => Err(PdfError::UnsupportedFeature(format!(
            "Stream filter: {}",
            other
        ))),
    }
}

/// Maximum decoded stream size (256 MB) to prevent zip bomb attacks.
const MAX_DECODED_SIZE: u64 = 256 * 1024 * 1024;

/// Decodes FlateDecode (zlib/deflate) compressed data.
///
/// Tries zlib-wrapped deflate first (correct per PDF spec), then falls
/// back to raw deflate without the 2-byte zlib header. Many real-world
/// PDFs have incorrect or missing zlib wrappers.
fn decode_flate(data: &[u8]) -> PdfResult<Vec<u8>> {
    use flate2::read::{DeflateDecoder, ZlibDecoder};
    use std::io::Read;

    // Try 1: zlib-wrapped deflate (PDF spec conformant)
    let mut decoder = ZlibDecoder::new(data).take(MAX_DECODED_SIZE);
    let mut decoded = Vec::new();
    if decoder.read_to_end(&mut decoded).is_ok() {
        return Ok(decoded);
    }

    // Try 2: raw deflate without zlib header (common in malformed PDFs)
    let mut raw_decoder = DeflateDecoder::new(data).take(MAX_DECODED_SIZE);
    let mut raw_decoded = Vec::new();
    if raw_decoder.read_to_end(&mut raw_decoded).is_ok() && !raw_decoded.is_empty() {
        tracing::debug!(
            "FlateDecode: zlib failed, raw deflate succeeded ({} bytes)",
            raw_decoded.len()
        );
        return Ok(raw_decoded);
    }

    Err(PdfError::CompressionError(
        "FlateDecode failed (tried both zlib and raw deflate)".to_string(),
    ))
}

/// Decodes ASCIIHexDecode data: hex pairs terminated by `>`.
fn decode_ascii_hex(data: &[u8]) -> PdfResult<Vec<u8>> {
    let mut result = Vec::with_capacity(data.len() / 2);
    let mut high_nibble: Option<u8> = None;

    for &b in data {
        if b == b'>' {
            // End-of-data marker
            if let Some(high) = high_nibble {
                // Odd number of digits: final nibble is padded with 0
                result.push(high << 4);
            }
            return Ok(result);
        }

        if b.is_ascii_whitespace() {
            continue;
        }

        let nibble = hex_digit(b).ok_or_else(|| {
            PdfError::CompressionError(format!("Invalid hex digit in ASCIIHexDecode: 0x{:02x}", b))
        })?;

        match high_nibble {
            None => high_nibble = Some(nibble),
            Some(high) => {
                result.push((high << 4) | nibble);
                high_nibble = None;
            }
        }
    }

    // No `>` found — treat remaining as complete
    if let Some(high) = high_nibble {
        result.push(high << 4);
    }

    Ok(result)
}

/// Decodes ASCII85Decode (base-85) data, delimited by `~>`.
fn decode_ascii85(data: &[u8]) -> PdfResult<Vec<u8>> {
    let mut result = Vec::new();
    let mut group: [u8; 5] = [0; 5];
    let mut count = 0;
    let mut i = 0;

    while i < data.len() {
        let b = data[i];
        i += 1;

        if b.is_ascii_whitespace() {
            continue;
        }

        // End-of-data marker
        if b == b'~' && i < data.len() && data[i] == b'>' {
            break;
        }

        if b == b'z' {
            if count != 0 {
                return Err(PdfError::CompressionError(
                    "ASCII85: 'z' in middle of group".to_string(),
                ));
            }
            result.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }

        if !(b'!'..=b'u').contains(&b) {
            return Err(PdfError::CompressionError(format!(
                "ASCII85: invalid byte 0x{:02x}",
                b
            )));
        }

        group[count] = b - b'!';
        count += 1;

        if count == 5 {
            let val = group.iter().fold(0u32, |acc, &d| acc * 85 + d as u32);
            result.extend_from_slice(&val.to_be_bytes());
            count = 0;
        }
    }

    // Handle partial final group
    if count > 1 {
        // Pad with 'u' (84) values
        for slot in group.iter_mut().skip(count) {
            *slot = 84; // 'u' - '!' = 84
        }
        let val = group.iter().fold(0u32, |acc, &d| acc * 85 + d as u32);
        let bytes = val.to_be_bytes();
        result.extend_from_slice(&bytes[..count - 1]);
    }

    Ok(result)
}

/// Decodes LZWDecode compressed data.
///
/// PDF uses the "early change" LZW variant with MSB-first bit ordering
/// and a minimum code size of 8.
fn decode_lzw(data: &[u8]) -> PdfResult<Vec<u8>> {
    use lzw::{DecoderEarlyChange, MsbReader};

    let mut decoder = DecoderEarlyChange::new(MsbReader::new(), 8);
    let mut result = Vec::new();
    let mut input = data;

    while !input.is_empty() {
        let (consumed, decoded) = decoder
            .decode_bytes(input)
            .map_err(|e| PdfError::CompressionError(format!("LZWDecode failed: {}", e)))?;
        if consumed == 0 && decoded.is_empty() {
            break;
        }
        result.extend_from_slice(decoded);
        if result.len() as u64 > MAX_DECODED_SIZE {
            return Err(PdfError::CompressionError(
                "LZWDecode output exceeds size limit".to_string(),
            ));
        }
        input = &input[consumed..];
    }

    Ok(result)
}

/// Decodes RunLengthDecode data per ISO 32000-2:2020, Section 7.4.5.
///
/// Format: length byte `n` followed by data:
/// - `0..=127`: copy next `n + 1` bytes literally
/// - `129..=255`: repeat next single byte `257 - n` times
/// - `128`: end-of-data marker
fn decode_run_length(data: &[u8]) -> PdfResult<Vec<u8>> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let n = data[i];
        i += 1;

        match n {
            0..=127 => {
                let count = n as usize + 1;
                if i + count > data.len() {
                    return Err(PdfError::CompressionError(
                        "RunLengthDecode: unexpected end of data in literal run".to_string(),
                    ));
                }
                result.extend_from_slice(&data[i..i + count]);
                i += count;
            }
            128 => break, // EOD
            _ => {
                // 129..=255: repeat next byte (257 - n) times
                if i >= data.len() {
                    return Err(PdfError::CompressionError(
                        "RunLengthDecode: unexpected end of data in repeat run".to_string(),
                    ));
                }
                let count = 257 - n as usize;
                let byte = data[i];
                i += 1;
                result.resize(result.len() + count, byte);
            }
        }

        if result.len() as u64 > MAX_DECODED_SIZE {
            return Err(PdfError::CompressionError(
                "RunLengthDecode output exceeds size limit".to_string(),
            ));
        }
    }

    Ok(result)
}

/// Decodes CCITTFaxDecode (Group 4) data.
///
/// **Limitation:** Uses the default fax page width of 1728 pixels.
/// PDF streams typically specify the actual width via `/DecodeParms`
/// `/Columns`, which should be passed to this function for correct
/// decoding of non-standard widths. Full `/DecodeParms` support
/// (`/K`, `/Columns`, `/Rows`, `/BlackIs1`) is planned.
fn decode_ccitt_fax(data: &[u8]) -> PdfResult<Vec<u8>> {
    let width: u16 = 1728; // Standard fax width
    let mut result = Vec::new();

    fax::decoder::decode_g4(data.iter().copied(), width, None, |transitions| {
        // Pack each line into bytes (1 bit per pixel, MSB first)
        let bytes_per_line = (width as usize).div_ceil(8);
        let mut line_bytes = vec![0u8; bytes_per_line];

        for (x, color) in fax::decoder::pels(transitions, width).enumerate() {
            if color == fax::Color::Black {
                line_bytes[x / 8] |= 0x80 >> (x % 8);
            }
        }

        result.extend_from_slice(&line_bytes);
    });

    Ok(result)
}

/// Compresses data using FlateDecode (zlib/deflate).
///
/// Returns the compressed data and the filter name. If compression
/// would increase the size, returns the original data with no filter.
pub fn encode_flate(data: &[u8]) -> PdfResult<(Vec<u8>, Option<&'static str>)> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| PdfError::CompressionError(format!("FlateDecode encode failed: {}", e)))?;
    let compressed = encoder
        .finish()
        .map_err(|e| PdfError::CompressionError(format!("FlateDecode encode finish: {}", e)))?;

    if compressed.len() < data.len() {
        Ok((compressed, Some("FlateDecode")))
    } else {
        Ok((data.to_vec(), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FlateDecode tests ---

    #[test]
    fn flate_decode_basic() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"Hello, PDFPurr! This is a test of FlateDecode.";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let decoded = decode_flate(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn flate_decode_empty() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;

        let encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        let compressed = encoder.finish().unwrap();

        let decoded = decode_flate(&compressed).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn flate_decode_invalid_data() {
        let result = decode_flate(b"not valid zlib data");
        assert!(result.is_err());
    }

    // --- ASCIIHexDecode tests ---

    #[test]
    fn ascii_hex_basic() {
        let decoded = decode_ascii_hex(b"48656C6C6F>").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn ascii_hex_lowercase() {
        let decoded = decode_ascii_hex(b"48656c6c6f>").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn ascii_hex_with_whitespace() {
        let decoded = decode_ascii_hex(b"48 65 6C 6C 6F>").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn ascii_hex_odd_digits() {
        // Odd final nibble is padded with 0
        let decoded = decode_ascii_hex(b"A>").unwrap();
        assert_eq!(decoded, vec![0xA0]);
    }

    #[test]
    fn ascii_hex_empty() {
        let decoded = decode_ascii_hex(b">").unwrap();
        assert!(decoded.is_empty());
    }

    // --- ASCII85Decode tests ---

    #[test]
    fn ascii85_basic() {
        // "Hello" in ASCII85 is "87cURD]j7BEbo80"
        // but let's test with a known encoding
        // "Man " = 9jqo^
        let decoded = decode_ascii85(b"9jqo^~>").unwrap();
        assert_eq!(decoded, b"Man ");
    }

    #[test]
    fn ascii85_z_shorthand() {
        // 'z' represents four zero bytes
        let decoded = decode_ascii85(b"z~>").unwrap();
        assert_eq!(decoded, vec![0, 0, 0, 0]);
    }

    #[test]
    fn ascii85_partial_group() {
        // Partial group: 2 chars encode 1 byte
        let decoded = decode_ascii85(b"/c~>").unwrap();
        // /c = (b'/' - b'!') * 85 + (b'c' - b'!') = 14 * 85 + 66 = 1256
        // Padded: 14 * 85^4 + 66 * 85^3 + 84 * 85^2 + 84 * 85 + 84
        // = 14*52200625 + 66*614125 + 84*7225 + 84*85 + 84
        // = 730808750 + 40532250 + 607020 + 7140 + 84
        // = 771955244
        // as big-endian bytes: [0x2E, 0x02, 0x81, 0x2C]
        // take first 1 byte: 0x2E = '.'
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn ascii85_with_whitespace() {
        let decoded = decode_ascii85(b"9jqo^ ~>").unwrap();
        assert_eq!(decoded, b"Man ");
    }

    // --- decode_stream tests ---

    #[test]
    fn decode_stream_no_filter() {
        let dict = Dictionary::new();
        let data = b"raw data";
        let decoded = decode_stream(data, &dict).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_stream_single_filter() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"Hello World";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("FlateDecode")),
        );

        let decoded = decode_stream(&compressed, &dict).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_stream_filter_chain() {
        // ASCII hex encode "Hello", then wrap in filter array
        let hex_data = b"48656C6C6F>";
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Array(vec![Object::Name(PdfName::new("ASCIIHexDecode"))]),
        );

        let decoded = decode_stream(hex_data, &dict).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn decode_stream_unsupported_filter() {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("JBIG2Decode")),
        );

        let result = decode_stream(b"data", &dict);
        assert!(result.is_err());
    }

    // --- LZWDecode tests ---

    #[test]
    fn lzw_decode_round_trip() {
        use lzw::{Encoder, MsbWriter};

        let original = b"TOBEORNOTTOBEORTOBEORNOT";
        let mut compressed = Vec::new();
        {
            let mut enc = Encoder::new(MsbWriter::new(&mut compressed), 8).unwrap();
            enc.encode_bytes(original).unwrap();
        }

        let decoded = decode_lzw(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn lzw_decode_repeated_data() {
        use lzw::{Encoder, MsbWriter};

        let original = vec![0xAA; 1000];
        let mut compressed = Vec::new();
        {
            let mut enc = Encoder::new(MsbWriter::new(&mut compressed), 8).unwrap();
            enc.encode_bytes(&original).unwrap();
        }

        let decoded = decode_lzw(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    // --- RunLengthDecode tests ---

    #[test]
    fn run_length_literal_run() {
        // n=4 means copy next 5 bytes literally
        let data = [4, b'H', b'e', b'l', b'l', b'o', 128];
        let decoded = decode_run_length(&data).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn run_length_repeat_run() {
        // n=253 means repeat next byte 257-253=4 times
        let data = [253, b'A', 128];
        let decoded = decode_run_length(&data).unwrap();
        assert_eq!(decoded, b"AAAA");
    }

    #[test]
    fn run_length_mixed() {
        // Literal "Hi" (n=1, 2 bytes) + repeat 'X' 3 times (n=254) + EOD
        let data = [1, b'H', b'i', 254, b'X', 128];
        let decoded = decode_run_length(&data).unwrap();
        assert_eq!(decoded, b"HiXXX");
    }

    #[test]
    fn run_length_empty_eod() {
        let data = [128];
        let decoded = decode_run_length(&data).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn run_length_truncated_literal() {
        // n=2 means 3 bytes but only 1 available
        let data = [2, b'A'];
        let result = decode_run_length(&data);
        assert!(result.is_err());
    }

    // --- DCTDecode / JPXDecode passthrough tests ---

    #[test]
    fn dct_decode_passthrough() {
        let jpeg_data = b"\xFF\xD8\xFF\xE0fake jpeg data\xFF\xD9";
        let decoded = apply_filter("DCTDecode", jpeg_data).unwrap();
        assert_eq!(decoded, jpeg_data);
    }

    #[test]
    fn jpx_decode_passthrough() {
        let jp2_data = b"\x00\x00\x00\x0Cjp2 fake data";
        let decoded = apply_filter("JPXDecode", jp2_data).unwrap();
        assert_eq!(decoded, jp2_data);
    }

    // --- CCITTFaxDecode tests ---

    #[test]
    fn ccitt_fax_decode_returns_ok() {
        // CCITTFaxDecode with empty/garbage data should still return Ok
        // (the fax decoder is lenient with short data)
        let result = decode_ccitt_fax(&[]);
        assert!(result.is_ok());
    }

    // --- apply_filter dispatch tests ---

    #[test]
    fn apply_filter_lzw_abbreviation() {
        // "LZW" is an abbreviation for "LZWDecode"
        use lzw::{Encoder, MsbWriter};
        let original = b"test";
        let mut compressed = Vec::new();
        {
            let mut enc = Encoder::new(MsbWriter::new(&mut compressed), 8).unwrap();
            enc.encode_bytes(original).unwrap();
        }
        let decoded = apply_filter("LZW", &compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn apply_filter_dct_abbreviation() {
        let data = b"jpeg";
        let decoded = apply_filter("DCT", data).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn apply_filter_run_length_abbreviation() {
        let data = [0, b'X', 128]; // literal 1 byte + EOD
        let decoded = apply_filter("RL", &data).unwrap();
        assert_eq!(decoded, b"X");
    }

    // --- extract_filter_names tests ---

    #[test]
    fn extract_single_filter() {
        let obj = Object::Name(PdfName::new("FlateDecode"));
        let names = extract_filter_names(&obj).unwrap();
        assert_eq!(names, vec!["FlateDecode"]);
    }

    #[test]
    fn extract_filter_array() {
        let obj = Object::Array(vec![
            Object::Name(PdfName::new("FlateDecode")),
            Object::Name(PdfName::new("ASCIIHexDecode")),
        ]);
        let names = extract_filter_names(&obj).unwrap();
        assert_eq!(names, vec!["FlateDecode", "ASCIIHexDecode"]);
    }

    #[test]
    fn extract_filter_invalid_type() {
        let obj = Object::Integer(42);
        assert!(extract_filter_names(&obj).is_err());
    }

    // --- encode_flate tests ---

    #[test]
    fn encode_flate_round_trip() {
        // Use highly compressible data to ensure compression helps
        let original: Vec<u8> = (0..500).map(|i| (i % 26 + b'A' as usize) as u8).collect();
        let (compressed, filter) = super::encode_flate(&original).unwrap();
        assert_eq!(filter, Some("FlateDecode"));
        assert!(compressed.len() < original.len());

        // Verify round trip
        let decoded = decode_flate(&compressed).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn encode_flate_tiny_data() {
        // Very small data — compression may not help
        let original = b"Hi";
        let (data, filter) = super::encode_flate(original).unwrap();
        if filter.is_none() {
            assert_eq!(data, original);
        }
    }
}
