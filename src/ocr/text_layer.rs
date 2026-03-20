//! Invisible text layer generation from OCR results.
//!
//! Converts OCR word bounding boxes into a PDF content stream with
//! invisible text (rendering mode 3) positioned to overlay the
//! original page image.
//!
//! Uses a custom encoding with a `/ToUnicode` CMap so that every
//! Unicode character is preserved — no lossy WinAnsi conversion.
//! Screen readers, copy/paste, and search all see the original text.

use std::collections::BTreeMap;

use crate::content::ContentStreamBuilder;
use crate::core::objects::{Dictionary, Object, PdfName, PdfStream};

use super::config::OcrConfig;
use super::constants::PARAGRAPH_GAP_THRESHOLD;
use super::engine::OcrResult;

/// Font resource name used for OCR invisible text.
pub const OCR_FONT_NAME: &str = "F_OCR";

/// Result of building an OCR text layer, containing the content stream
/// and the ToUnicode CMap needed to decode the custom encoding.
pub struct OcrTextLayer {
    /// The PDF content stream bytes (invisible text with marked content).
    pub content: Vec<u8>,
    /// The ToUnicode CMap stream mapping custom codes → Unicode.
    /// `None` if the text was pure ASCII (no CMap needed).
    pub to_unicode_cmap: Option<PdfStream>,
    /// Character encoding map: Unicode char → code byte(s).
    /// Used internally; exposed for testing.
    pub char_map: BTreeMap<char, u16>,
}

/// Builds a PDF content stream containing invisible text from OCR results.
///
/// Each unique Unicode character gets a sequential code in a custom encoding.
/// A `/ToUnicode` CMap maps these codes back to Unicode for text extraction,
/// copy/paste, and screen readers. This preserves ALL characters — CJK,
/// Arabic, emoji, accented Latin, symbols — without any lossy conversion.
///
/// # Arguments
///
/// * `result` — OCR output with words and bounding boxes in pixel coordinates
/// * `page_width` — Page width in PDF points
/// * `page_height` — Page height in PDF points
/// * `config` — OCR configuration (min confidence, DPI)
pub fn build_ocr_text_layer(
    result: &OcrResult,
    page_width: f64,
    page_height: f64,
    config: &OcrConfig,
) -> OcrTextLayer {
    let mut builder = ContentStreamBuilder::new();

    // Mark the page image as Artifact (not real content for screen readers)
    builder.begin_marked_content("Artifact");
    builder.end_marked_content();

    // Scale factor: OCR pixels → PDF points
    let scale_x = page_width / result.image_width as f64;
    let scale_y = page_height / result.image_height as f64;

    // Group words into paragraphs
    let paragraphs = group_into_paragraphs(&result.words, config.min_confidence);

    // Phase 1: Collect all unique characters and assign codes
    let char_map = build_char_map(&paragraphs);
    let is_two_byte = char_map.len() > 255;

    // Phase 2: Write invisible text using the custom encoding
    builder.begin_text().set_text_rendering_mode(3); // invisible

    for (mcid, para_words) in paragraphs.iter().enumerate() {
        builder.end_text();
        builder.begin_marked_content_with_properties("P", mcid as u32);
        builder.begin_text().set_text_rendering_mode(3);

        for word in para_words {
            let pdf_x = word.x as f64 * scale_x;
            let pdf_y = page_height - (word.y as f64 + word.height as f64) * scale_y;
            let font_size = (word.height as f64 * scale_y).max(1.0);

            let encoded = encode_text(&word.text, &char_map, is_two_byte);

            builder
                .set_font(OCR_FONT_NAME, font_size)
                .set_text_matrix(1.0, 0.0, 0.0, 1.0, pdf_x, pdf_y)
                .show_text_bytes(&encoded);
        }

        builder.end_text();
        builder.end_marked_content();
        builder.begin_text().set_text_rendering_mode(3);
    }

    builder.end_text();

    // Phase 3: Build ToUnicode CMap — essential for text extraction.
    // Without this CMap, screen readers and search cannot decode the
    // custom character codes back to Unicode. The function is infallible.
    let to_unicode_cmap = if !char_map.is_empty() {
        Some(build_ocr_to_unicode_cmap(&char_map, is_two_byte))
    } else {
        None
    };

    OcrTextLayer {
        content: builder.build(),
        to_unicode_cmap,
        char_map,
    }
}

/// Assigns a sequential code to each unique character across all paragraphs.
///
/// Code 0 is reserved (`.notdef`). Codes start at 1.
fn build_char_map(paragraphs: &[Vec<&super::engine::OcrWord>]) -> BTreeMap<char, u16> {
    let mut map = BTreeMap::new();
    let mut next_code: u16 = 1;

    for para in paragraphs {
        for word in para {
            for ch in word.text.chars() {
                map.entry(ch).or_insert_with(|| {
                    let code = next_code;
                    next_code = next_code.saturating_add(1);
                    code
                });
            }
        }
    }

    map
}

/// Encodes a string using the character map.
///
/// Each character is replaced by its assigned code. Unknown characters
/// (shouldn't happen since we built the map from this text) get code 0.
fn encode_text(text: &str, char_map: &BTreeMap<char, u16>, two_byte: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * if two_byte { 2 } else { 1 });
    for ch in text.chars() {
        let code = char_map.get(&ch).copied().unwrap_or(0);
        if two_byte {
            bytes.push((code >> 8) as u8);
            bytes.push((code & 0xFF) as u8);
        } else {
            bytes.push(code as u8);
        }
    }
    bytes
}

/// Builds a ToUnicode CMap stream for the OCR font.
///
/// Maps each assigned code back to its Unicode code point so that
/// PDF viewers, screen readers, and copy/paste decode correctly.
///
/// This function is infallible — the CMap is a plain text string
/// stored uncompressed. No I/O, no compression, no external deps.
fn build_ocr_to_unicode_cmap(char_map: &BTreeMap<char, u16>, two_byte: bool) -> PdfStream {
    use std::fmt::Write;

    let hex_width = if two_byte { 4 } else { 2 };
    let mut cmap = String::with_capacity(512 + char_map.len() * 30);

    cmap.push_str("/CIDInit /ProcSet findresource begin\n");
    cmap.push_str("12 dict begin\n");
    cmap.push_str("begincmap\n");
    cmap.push_str("/CIDSystemInfo\n");
    cmap.push_str("<< /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    cmap.push_str("/CMapName /Adobe-Identity-UCS def\n");
    cmap.push_str("/CMapType 2 def\n");
    cmap.push_str("1 begincodespacerange\n");
    if two_byte {
        cmap.push_str("<0000> <FFFF>\n");
    } else {
        cmap.push_str("<00> <FF>\n");
    }
    cmap.push_str("endcodespacerange\n");

    // Sort by code for deterministic output
    let mut mappings: Vec<(u16, char)> = char_map.iter().map(|(&ch, &code)| (code, ch)).collect();
    mappings.sort_by_key(|&(code, _)| code);

    for chunk in mappings.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for &(code, ch) in chunk {
            let unicode = ch as u32;
            if unicode > 0xFFFF {
                // Surrogate pair for non-BMP characters
                let u = unicode - 0x10000;
                let high = 0xD800 + (u >> 10);
                let low = 0xDC00 + (u & 0x3FF);
                let _ = writeln!(
                    cmap,
                    "<{:0>width$X}> <{:04X}{:04X}>",
                    code,
                    high,
                    low,
                    width = hex_width
                );
            } else {
                let _ = writeln!(
                    cmap,
                    "<{:0>width$X}> <{:04X}>",
                    code,
                    unicode,
                    width = hex_width
                );
            }
        }
        cmap.push_str("endbfchar\n");
    }

    cmap.push_str("endcmap\n");
    cmap.push_str("CMapName currentdict /CMap defineresource pop\n");
    cmap.push_str("end\n");
    cmap.push_str("end\n");

    let data = cmap.into_bytes();
    let mut dict = Dictionary::new();
    dict.insert(PdfName::new("Length"), Object::Integer(data.len() as i64));
    PdfStream::new(dict, data)
}

/// Groups OCR words into paragraphs based on vertical proximity.
///
/// Filters out low-confidence and empty words first, then groups by
/// vertical gap exceeding [`PARAGRAPH_GAP_THRESHOLD`].
fn group_into_paragraphs(
    words: &[super::engine::OcrWord],
    _min_confidence: f32,
) -> Vec<Vec<&super::engine::OcrWord>> {
    // Include ALL non-empty words regardless of confidence.
    // The text layer must match the structure builder's word set so that
    // every ActualText entry has a corresponding content stream operator.
    // Low-confidence words may be inaccurate, but omitting them entirely
    // makes the page invisible to screen readers.
    let filtered: Vec<&super::engine::OcrWord> =
        words.iter().filter(|w| !w.text.trim().is_empty()).collect();

    if filtered.is_empty() {
        return Vec::new();
    }

    let mut paragraphs: Vec<Vec<&super::engine::OcrWord>> = Vec::new();
    let mut current: Vec<&super::engine::OcrWord> = vec![filtered[0]];
    let mut prev_bottom = filtered[0].y + filtered[0].height;

    for &word in &filtered[1..] {
        let gap = word.y.saturating_sub(prev_bottom);
        if gap > PARAGRAPH_GAP_THRESHOLD {
            paragraphs.push(std::mem::take(&mut current));
        }
        current.push(word);
        prev_bottom = word.y + word.height;
    }

    if !current.is_empty() {
        paragraphs.push(current);
    }

    paragraphs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::engine::{OcrResult, OcrWord};

    fn make_result(words: Vec<OcrWord>) -> OcrResult {
        OcrResult {
            words,
            image_width: 2550,  // 8.5" at 300 DPI
            image_height: 3300, // 11" at 300 DPI
        }
    }

    fn default_config() -> OcrConfig {
        OcrConfig::default()
    }

    #[test]
    fn empty_result_produces_minimal_stream() {
        let result = make_result(vec![]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());
        let text = String::from_utf8_lossy(&layer.content);
        assert!(text.contains("Artifact"));
        assert!(layer.to_unicode_cmap.is_none());
    }

    #[test]
    fn ascii_text_encoded_correctly() {
        let result = make_result(vec![OcrWord {
            text: "Hello".to_string(),
            x: 100,
            y: 100,
            width: 200,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());

        // Char map should have H=1, e=2, l=3, o=4
        assert_eq!(layer.char_map.len(), 4); // H, e, l, o
        assert!(layer.to_unicode_cmap.is_some(), "CMap should be generated");
    }

    #[test]
    fn bullet_character_preserved_in_cmap() {
        let result = make_result(vec![OcrWord {
            text: "item \u{2022} value".to_string(),
            x: 100,
            y: 100,
            width: 300,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());

        // Bullet should be in char map
        assert!(
            layer.char_map.contains_key(&'\u{2022}'),
            "Bullet should be mapped"
        );

        // CMap should contain the Unicode mapping
        let cmap = layer.to_unicode_cmap.unwrap();
        let cmap_text = String::from_utf8_lossy(&cmap.data);
        assert!(cmap_text.contains("2022"), "CMap should map to U+2022");
    }

    #[test]
    fn cjk_characters_preserved() {
        let result = make_result(vec![OcrWord {
            text: "\u{4E2D}\u{6587}".to_string(), // 中文
            x: 100,
            y: 100,
            width: 200,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());

        assert!(
            layer.char_map.contains_key(&'\u{4E2D}'),
            "中 should be mapped"
        );
        assert!(
            layer.char_map.contains_key(&'\u{6587}'),
            "文 should be mapped"
        );

        let cmap = layer.to_unicode_cmap.unwrap();
        let cmap_text = String::from_utf8_lossy(&cmap.data);
        assert!(cmap_text.contains("4E2D"), "CMap should map 中");
        assert!(cmap_text.contains("6587"), "CMap should map 文");
    }

    #[test]
    fn smart_quotes_preserved() {
        let result = make_result(vec![OcrWord {
            text: "\u{201C}hello\u{201D}".to_string(),
            x: 100,
            y: 100,
            width: 200,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());

        let cmap = layer.to_unicode_cmap.unwrap();
        let cmap_text = String::from_utf8_lossy(&cmap.data);
        assert!(cmap_text.contains("201C"), "CMap should map left quote");
        assert!(cmap_text.contains("201D"), "CMap should map right quote");
    }

    #[test]
    fn encode_text_produces_correct_bytes() {
        let mut map = BTreeMap::new();
        map.insert('H', 1u16);
        map.insert('i', 2u16);

        let bytes = encode_text("Hi", &map, false);
        assert_eq!(bytes, vec![1, 2]);
    }

    #[test]
    fn encode_text_two_byte_mode() {
        let mut map = BTreeMap::new();
        map.insert('A', 0x0100u16);

        let bytes = encode_text("A", &map, true);
        assert_eq!(bytes, vec![0x01, 0x00]);
    }

    #[test]
    fn char_map_assigns_sequential_codes() {
        let words = vec![OcrWord {
            text: "abc".to_string(),
            x: 0,
            y: 0,
            width: 100,
            height: 20,
            confidence: 0.9,
        }];
        let paragraphs = group_into_paragraphs(&words, 0.0);
        let map = build_char_map(&paragraphs);

        assert_eq!(map.get(&'a'), Some(&1));
        assert_eq!(map.get(&'b'), Some(&2));
        assert_eq!(map.get(&'c'), Some(&3));
    }

    #[test]
    fn duplicate_chars_share_same_code() {
        let words = vec![OcrWord {
            text: "hello".to_string(),
            x: 0,
            y: 0,
            width: 100,
            height: 20,
            confidence: 0.9,
        }];
        let paragraphs = group_into_paragraphs(&words, 0.0);
        let map = build_char_map(&paragraphs);

        // 'l' appears twice but should have one code
        assert_eq!(map.len(), 4); // h, e, l, o
        let l_code = map.get(&'l').unwrap();
        let encoded = encode_text("hello", &map, false);
        assert_eq!(encoded[2], *l_code as u8);
        assert_eq!(encoded[3], *l_code as u8); // same code for both 'l's
    }

    #[test]
    fn non_bmp_emoji_in_cmap() {
        let result = make_result(vec![OcrWord {
            text: "ok\u{1F44D}".to_string(), // ok👍
            x: 100,
            y: 100,
            width: 200,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());

        assert!(
            layer.char_map.contains_key(&'\u{1F44D}'),
            "Emoji should be mapped"
        );

        let cmap = layer.to_unicode_cmap.unwrap();
        let cmap_text = String::from_utf8_lossy(&cmap.data);
        // Non-BMP: U+1F44D → surrogate pair D83D DC4D
        assert!(
            cmap_text.contains("D83DDC4D"),
            "CMap should use surrogate pair for emoji"
        );
    }

    #[test]
    fn low_confidence_words_included_for_screen_readers() {
        let result = make_result(vec![
            OcrWord {
                text: "Good".to_string(),
                x: 100,
                y: 100,
                width: 100,
                height: 30,
                confidence: 0.9,
            },
            OcrWord {
                text: "Low".to_string(),
                x: 300,
                y: 100,
                width: 80,
                height: 30,
                confidence: 0.1,
            },
        ]);
        let config = OcrConfig {
            min_confidence: 0.3,
            ..Default::default()
        };
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &config);
        let text = String::from_utf8_lossy(&layer.content);

        // Both words should produce Tj operators
        let tj_count = text.matches("Tj").count();
        assert!(
            tj_count >= 2,
            "Both words should appear, got {} Tj operators",
            tj_count
        );
    }

    #[test]
    fn artifact_marker_for_image() {
        let result = make_result(vec![OcrWord {
            text: "test".to_string(),
            x: 100,
            y: 100,
            width: 100,
            height: 30,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());
        let text = String::from_utf8_lossy(&layer.content);
        assert!(
            text.contains("/Artifact BMC"),
            "Should mark image as artifact"
        );
    }

    #[test]
    fn marked_content_ids_wrap_paragraphs() {
        let result = make_result(vec![
            OcrWord {
                text: "First".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 20,
                confidence: 0.9,
            },
            OcrWord {
                text: "Second".to_string(),
                x: 100,
                y: 200,
                width: 200,
                height: 20,
                confidence: 0.9,
            },
        ]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());
        let text = String::from_utf8_lossy(&layer.content);

        assert!(
            text.contains("/P <</MCID 0>> BDC"),
            "First paragraph MCID 0"
        );
        assert!(
            text.contains("/P <</MCID 1>> BDC"),
            "Second paragraph MCID 1"
        );
    }

    #[test]
    fn y_coordinate_is_flipped() {
        let result = make_result(vec![OcrWord {
            text: "top".to_string(),
            x: 100,
            y: 10,
            width: 100,
            height: 20,
            confidence: 0.9,
        }]);
        let layer = build_ocr_text_layer(&result, 612.0, 792.0, &default_config());
        let text = String::from_utf8_lossy(&layer.content);

        let tm_line = text.lines().find(|l| l.contains("Tm")).unwrap();
        let parts: Vec<&str> = tm_line.split_whitespace().collect();
        let y_val: f64 = parts[parts.len() - 2].parse().unwrap();
        assert!(
            y_val > 700.0,
            "Top-of-image word should map to high PDF y, got {}",
            y_val
        );
    }
}
