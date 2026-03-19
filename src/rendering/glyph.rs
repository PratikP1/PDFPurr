//! Glyph outline rendering using embedded font programs.
//!
//! Converts embedded TrueType/CFF font data into tiny-skia paths using
//! skrifa's `OutlinePen` interface.

use std::collections::HashMap;

use skrifa::instance::{LocationRef, Size};
use skrifa::metrics::Metrics;
use skrifa::outline::OutlinePen;
use skrifa::raw::FontRef;
use skrifa::{GlyphId, MetadataProvider};
use tiny_skia::PathBuilder;

use crate::core::objects::{DictExt, Dictionary, Object};
use crate::fonts::encoding::Encoding;

use super::colors::obj_f64;

/// A pen that converts glyph outline commands to a [`tiny_skia::PathBuilder`].
struct SkiaPen {
    builder: PathBuilder,
}

impl SkiaPen {
    fn new() -> Self {
        Self {
            builder: PathBuilder::new(),
        }
    }

    fn finish(self) -> Option<tiny_skia::Path> {
        self.builder.finish()
    }
}

impl OutlinePen for SkiaPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

/// Cached font data for rendering glyphs from an embedded font program.
pub(crate) struct FontProgram {
    /// Raw font data (TTF/OTF bytes).
    data: Vec<u8>,
    /// Character code to glyph ID mapping, indexed by byte value (0–255).
    /// Built from the font's encoding or cmap table.
    code_to_gid: Vec<Option<GlyphId>>,
    /// Pre-cached advance widths in font units, indexed by byte value (0–255).
    advance_widths: Vec<Option<f32>>,
    /// Units per em for scaling.
    units_per_em: f32,
}

impl FontProgram {
    /// Loads a font program from raw TTF/OTF data with the specified encoding.
    ///
    /// Uses the encoding to map byte codes (0–255) to Unicode, then looks up
    /// glyph IDs in the font's cmap table.
    pub fn from_data_with_encoding(data: Vec<u8>, encoding: &Encoding) -> Option<Self> {
        let font_ref = FontRef::new(&data).ok()?;
        let charmap = font_ref.charmap();
        let metrics = Metrics::new(&font_ref, Size::unscaled(), LocationRef::default());
        let upem = metrics.units_per_em as f32;

        // Build code → GID mapping for single-byte codes (0-255).
        // Map byte to Unicode via the provided encoding, then look up in font's cmap.
        let code_to_gid: Vec<Option<GlyphId>> = (0u8..=255)
            .map(|code| encoding.decode_byte(code).and_then(|c| charmap.map(c)))
            .collect();

        // Pre-cache advance widths for all mapped glyphs
        let glyph_metrics =
            skrifa::metrics::GlyphMetrics::new(&font_ref, Size::unscaled(), LocationRef::default());
        let advance_widths: Vec<Option<f32>> = code_to_gid
            .iter()
            .map(|gid| gid.map(|g| glyph_metrics.advance_width(g).unwrap_or(0.0)))
            .collect();

        Some(FontProgram {
            data,
            code_to_gid,
            advance_widths,
            units_per_em: upem,
        })
    }

    /// Draws a glyph outline for the given character code, returning a path
    /// scaled to the given font size in points.
    pub fn glyph_path(&self, code: u8) -> Option<tiny_skia::Path> {
        let gid = self.code_to_gid.get(code as usize).copied().flatten()?;
        let font_ref = FontRef::new(&self.data).ok()?;
        let outlines = font_ref.outline_glyphs();
        let glyph = outlines.get(gid)?;

        let mut pen = SkiaPen::new();
        let settings =
            skrifa::outline::DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
        glyph.draw(settings, &mut pen).ok()?;
        pen.finish()
    }

    /// Returns the units-per-em value for scaling.
    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Returns the advance width of a glyph in font units.
    pub fn advance_width(&self, code: u8) -> Option<f32> {
        self.advance_widths.get(code as usize).copied().flatten()
    }
}

/// A Type 3 font whose glyphs are defined as PDF content streams.
///
/// Each glyph is a mini content stream in `/CharProcs` that draws using
/// standard PDF operators. The `/FontMatrix` transforms from glyph space
/// to text space (typically `[0.001 0 0 0.001 0 0]`).
pub(crate) struct Type3FontProgram {
    /// Glyph name → decoded content stream bytes.
    char_procs: HashMap<String, Vec<u8>>,
    /// Character code → glyph name mapping (from /Encoding /Differences).
    glyph_names: Vec<Option<String>>,
    /// Advance widths indexed by (code - first_char).
    widths: Vec<f64>,
    /// First character code with a width entry.
    first_char: u8,
    /// Font matrix transforming glyph space to text space.
    font_matrix: [f64; 6],
}

impl Type3FontProgram {
    /// Parses a Type 3 font from its PDF font dictionary.
    pub fn from_dict(font_dict: &Dictionary, doc: &crate::document::Document) -> Option<Self> {
        // FontMatrix (required, typically [0.001 0 0 0.001 0 0])
        let matrix_arr = font_dict.get_str("FontMatrix")?.as_array()?;
        if matrix_arr.len() != 6 {
            return None;
        }
        let font_matrix = [
            obj_f64(&matrix_arr[0])?,
            obj_f64(&matrix_arr[1])?,
            obj_f64(&matrix_arr[2])?,
            obj_f64(&matrix_arr[3])?,
            obj_f64(&matrix_arr[4])?,
            obj_f64(&matrix_arr[5])?,
        ];

        let first_char = match font_dict.get_str("FirstChar") {
            Some(Object::Integer(n)) => *n as u8,
            _ => 0,
        };

        // Widths array
        let widths: Vec<f64> = font_dict
            .get_str("Widths")
            .and_then(|o| o.as_array())
            .map(|arr| arr.iter().filter_map(obj_f64).collect())
            .unwrap_or_default();

        // Build glyph name table from /Encoding /Differences
        let mut glyph_names: Vec<Option<String>> = vec![None; 256];
        if let Some(Object::Dictionary(enc_dict)) =
            font_dict.get_str("Encoding").and_then(|o| doc.resolve(o))
        {
            if let Some(Object::Array(diff)) = enc_dict.get_str("Differences") {
                let mut code: Option<u8> = None;
                for obj in diff {
                    match obj {
                        Object::Integer(n) => code = Some(*n as u8),
                        Object::Name(name) => {
                            if let Some(c) = code {
                                glyph_names[c as usize] = Some(name.as_str().to_string());
                                code = Some(c.wrapping_add(1));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // CharProcs: glyph name → content stream
        let cp_dict = font_dict
            .get_str("CharProcs")
            .and_then(|o| doc.resolve(o))
            .and_then(|o| match o {
                Object::Dictionary(d) => Some(d),
                _ => None,
            })?;

        let mut char_procs = HashMap::new();
        for (name, val) in cp_dict.iter() {
            if let Some(Object::Stream(s)) = doc.resolve(val) {
                if let Ok(data) = s.decode_data() {
                    char_procs.insert(name.as_str().to_string(), data);
                }
            }
        }

        Some(Type3FontProgram {
            char_procs,
            glyph_names,
            widths,
            first_char,
            font_matrix,
        })
    }

    /// Returns the advance width for a character code in glyph space units.
    pub fn glyph_width(&self, code: u8) -> Option<f64> {
        let idx = (code as usize).checked_sub(self.first_char as usize)?;
        self.widths.get(idx).copied()
    }

    /// Returns the font matrix for transforming glyph space to text space.
    pub fn font_matrix(&self) -> [f64; 6] {
        self.font_matrix
    }

    /// Returns the content stream bytes for rendering a glyph.
    pub fn glyph_stream(&self, code: u8) -> Option<&[u8]> {
        let name = self.glyph_names.get(code as usize)?.as_ref()?;
        self.char_procs.get(name).map(|v| v.as_slice())
    }
}

/// Attempts to extract embedded font program data from a PDF font dictionary.
///
/// Follows the chain: font dict → `/FontDescriptor` → `/FontFile2` (TrueType)
/// or `/FontFile3` (CFF/OpenType).
pub(crate) fn extract_font_program(
    font_dict: &Dictionary,
    doc: &crate::document::Document,
) -> Option<FontProgram> {
    // Get font descriptor
    let descriptor = font_dict
        .get_str("FontDescriptor")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        })?;

    // Try FontFile2 (TrueType), FontFile3 (CFF/OpenType), then FontFile (Type 1 PFB)
    let (font_stream, is_type1) = if let Some(s) = descriptor
        .get_str("FontFile2")
        .or_else(|| descriptor.get_str("FontFile3"))
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Stream(s) => Some(s),
            _ => None,
        }) {
        (s, false)
    } else if let Some(s) = descriptor
        .get_str("FontFile")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Stream(s) => Some(s),
            _ => None,
        })
    {
        (s, true)
    } else {
        return None;
    };

    let raw_data = font_stream.decode_data().ok()?;
    let font_data = if is_type1 {
        strip_pfb_wrapper(raw_data)
    } else {
        raw_data
    };

    // Use the font dict's /Encoding if present, otherwise fall back to WinAnsi
    let encoding = font_dict
        .get_str("Encoding")
        .and_then(|obj| Encoding::from_object(obj).ok())
        .unwrap_or_else(Encoding::win_ansi);

    FontProgram::from_data_with_encoding(font_data, &encoding)
}

/// Cached font data for CID-keyed fonts (Type 0 composite fonts).
///
/// CID fonts use multi-byte character codes mapped via CMaps.
/// This handles CIDFontType2 (TrueType-based CID fonts) with Identity CID→GID mapping.
pub(crate) struct CidFontProgram {
    /// Raw font data (TTF/OTF bytes).
    data: Vec<u8>,
    /// Default glyph width in font units (/DW, default 1000).
    default_width: f32,
    /// Per-CID width overrides from /W array.
    widths: HashMap<u16, f32>,
    /// Units per em for scaling.
    units_per_em: f32,
}

impl CidFontProgram {
    /// Draws a glyph outline for the given GID, returning an unscaled path.
    pub fn glyph_path_by_gid(&self, gid: u16) -> Option<tiny_skia::Path> {
        let font_ref = FontRef::new(&self.data).ok()?;
        let outlines = font_ref.outline_glyphs();
        let glyph = outlines.get(GlyphId::new(gid as u32))?;

        let mut pen = SkiaPen::new();
        let settings =
            skrifa::outline::DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
        glyph.draw(settings, &mut pen).ok()?;
        pen.finish()
    }

    /// Returns the advance width for a CID in font units.
    pub fn advance_width(&self, cid: u16) -> f32 {
        self.widths.get(&cid).copied().unwrap_or(self.default_width)
    }

    /// Returns the units-per-em value for scaling.
    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }
}

/// Extracts a CID font program from a Type 0 composite font dictionary.
///
/// Follows: font dict → /DescendantFonts[0] → /FontDescriptor → /FontFile2.
/// Parses /DW (default width) and /W (per-CID width overrides).
pub(crate) fn extract_cid_font_program(
    font_dict: &Dictionary,
    doc: &crate::document::Document,
) -> Option<CidFontProgram> {
    // Get DescendantFonts array → first element (the CIDFont dict)
    let descendants = font_dict
        .get_str("DescendantFonts")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| o.as_array())?;

    let cid_font_dict = descendants
        .first()
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        })?;

    // Extract font program from FontDescriptor
    let descriptor = cid_font_dict
        .get_str("FontDescriptor")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        })?;

    let font_stream = descriptor
        .get_str("FontFile2")
        .or_else(|| descriptor.get_str("FontFile3"))
        .and_then(|o| doc.resolve(o))
        .and_then(|o| match o {
            Object::Stream(s) => Some(s),
            _ => None,
        })?;

    let font_data = font_stream.decode_data().ok()?;
    let font_ref = FontRef::new(&font_data).ok()?;
    let metrics = Metrics::new(&font_ref, Size::unscaled(), LocationRef::default());
    let units_per_em = metrics.units_per_em as f32;

    // Default width /DW (default 1000)
    let default_width = cid_font_dict.get_i64("DW").unwrap_or(1000) as f32;

    // Parse /W array: [CID [w1 w2 ...]] or [CIDfirst CIDlast w]
    let widths = parse_w_array(cid_font_dict.get_str("W").and_then(|o| doc.resolve(o)));

    Some(CidFontProgram {
        data: font_data,
        default_width,
        widths,
        units_per_em,
    })
}

/// Parses a CID font /W (widths) array.
///
/// Format: `[cid [w1 w2 ...] cid_first cid_last w ...]`
/// - `cid [w1 w2 ...]` assigns widths w1, w2, ... to CIDs cid, cid+1, ...
/// - `cid_first cid_last w` assigns width w to all CIDs in range
fn parse_w_array(obj: Option<&Object>) -> HashMap<u16, f32> {
    let mut widths = HashMap::new();
    let arr = match obj.and_then(|o| o.as_array()) {
        Some(a) => a,
        None => return widths,
    };

    let mut i = 0;
    while i < arr.len() {
        let cid = match &arr[i] {
            Object::Integer(n) => *n as u16,
            _ => break,
        };
        i += 1;
        if i >= arr.len() {
            break;
        }

        match &arr[i] {
            Object::Array(sub) => {
                // cid [w1 w2 ...] — consecutive CIDs starting at `cid`
                for (j, w_obj) in sub.iter().enumerate() {
                    if let Some(w) = obj_f64(w_obj) {
                        widths.insert(cid + j as u16, w as f32);
                    }
                }
                i += 1;
            }
            Object::Integer(_) | Object::Real(_) => {
                // cid_first cid_last w — range of CIDs with same width
                if i + 1 < arr.len() {
                    let cid_last = match &arr[i] {
                        Object::Integer(n) => *n as u16,
                        _ => break,
                    };
                    i += 1;
                    if let Some(w) = obj_f64(&arr[i]) {
                        for c in cid..=cid_last {
                            widths.insert(c, w as f32);
                        }
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    widths
}

/// Strips the PFB (Printer Font Binary) wrapper from Type 1 font data.
///
/// PFB format uses 0x80 header bytes followed by segment type and length.
/// This extracts the ASCII and binary segments, concatenating their payloads.
/// If the data doesn't have a PFB wrapper, returns it unchanged (zero-copy).
fn strip_pfb_wrapper(data: Vec<u8>) -> Vec<u8> {
    if data.first() != Some(&0x80) {
        // Not PFB-wrapped, return as-is (may be bare Type 1 PostScript)
        return data;
    }

    let mut result = Vec::with_capacity(data.len());
    let mut offset = 0;

    while offset + 6 <= data.len() && data[offset] == 0x80 {
        let segment_type = data[offset + 1];
        if segment_type == 3 {
            // EOF marker
            break;
        }

        // Length is 4 bytes little-endian
        let len = u32::from_le_bytes([
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
            data[offset + 5],
        ]) as usize;
        offset += 6;

        let end = (offset + len).min(data.len());
        result.extend_from_slice(&data[offset..end]);
        offset = end;
    }

    if result.is_empty() {
        data
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skia_pen_builds_path() {
        let mut pen = SkiaPen::new();
        pen.move_to(0.0, 0.0);
        pen.line_to(100.0, 0.0);
        pen.line_to(100.0, 100.0);
        pen.close();
        let path = pen.finish();
        assert!(path.is_some(), "SkiaPen should produce a valid path");
    }

    #[test]
    fn font_program_with_encoding() {
        // Load DejaVu Sans if available
        let data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let win =
            FontProgram::from_data_with_encoding(data.clone(), &Encoding::win_ansi()).unwrap();
        let mac = FontProgram::from_data_with_encoding(data, &Encoding::mac_roman()).unwrap();

        // Byte 0xC0: WinAnsi = U+00C0 (À), MacRoman = U+00BF (¿)
        // Both should resolve but to different glyphs
        let win_path = win.glyph_path(0xC0);
        let mac_path = mac.glyph_path(0xC0);
        assert!(win_path.is_some(), "WinAnsi should have glyph for 0xC0");
        assert!(mac_path.is_some(), "MacRoman should have glyph for 0xC0");
        // The paths should differ (different glyphs)
        let win_bounds = win_path.unwrap().bounds();
        let mac_bounds = mac_path.unwrap().bounds();
        assert_ne!(
            (win_bounds.width() as i32, win_bounds.height() as i32),
            (mac_bounds.width() as i32, mac_bounds.height() as i32),
            "WinAnsi and MacRoman should map 0xC0 to different glyphs"
        );
    }

    #[test]
    fn extract_font_program_uses_encoding() {
        use crate::core::objects::{IndirectRef, PdfName, PdfStream};
        use crate::document::Document;

        let ttf_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut doc = Document::new();

        let font_stream = PdfStream::new(Dictionary::new(), ttf_data.clone());
        let fs_id = doc.add_object(Object::Stream(font_stream));

        let mut descriptor = Dictionary::new();
        descriptor.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(fs_id.0, fs_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(descriptor));

        // Font dict with MacRomanEncoding
        let mut font_dict = Dictionary::new();
        font_dict.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("MacRomanEncoding")),
        );

        let fp = extract_font_program(&font_dict, &doc);
        assert!(
            fp.is_some(),
            "should extract font program with MacRoman encoding"
        );

        // Verify the encoding was applied: byte 0xCA should map to MacRoman's glyph
        let fp = fp.unwrap();
        let mac_ref =
            FontProgram::from_data_with_encoding(ttf_data, &Encoding::mac_roman()).unwrap();

        // Both should resolve 0xCA to the same glyph (MacRoman encoding)
        let extracted_path = fp.glyph_path(0xCA);
        let mac_path = mac_ref.glyph_path(0xCA);
        assert_eq!(
            extracted_path.is_some(),
            mac_path.is_some(),
            "Extracted font program should use MacRoman encoding"
        );
    }

    #[test]
    fn font_program_from_real_ttf() {
        // Load DejaVu Sans if available
        let data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return, // Skip if font not available
        };

        let fp = FontProgram::from_data_with_encoding(data, &Encoding::win_ansi())
            .expect("should parse DejaVu Sans");
        assert!(fp.units_per_em() > 0.0);

        // 'A' = code 65 should have a glyph
        let path = fp.glyph_path(b'A');
        assert!(path.is_some(), "glyph 'A' should produce a path");

        // advance width should be positive
        let aw = fp.advance_width(b'A');
        assert!(aw.is_some());
        assert!(aw.unwrap() > 0.0);
    }

    #[test]
    fn cidfont_program_from_dict() {
        use crate::core::objects::{Dictionary, IndirectRef, Object, PdfName, PdfStream};
        use crate::document::Document;

        // Load a real TTF for embedding
        let ttf_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut doc = Document::new();

        // Font descriptor with embedded TTF
        let font_stream = PdfStream::new(Dictionary::new(), ttf_data);
        let fs_id = doc.add_object(Object::Stream(font_stream));

        let mut descriptor = Dictionary::new();
        descriptor.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(fs_id.0, fs_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(descriptor));

        // CIDFont dict with /DW and /W
        let mut cid_font = Dictionary::new();
        cid_font.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("CIDFontType2")),
        );
        cid_font.insert(PdfName::new("DW"), Object::Integer(500));
        cid_font.insert(
            PdfName::new("W"),
            Object::Array(vec![
                // CID 36 has width 600
                Object::Integer(36),
                Object::Array(vec![Object::Integer(600)]),
            ]),
        );
        cid_font.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );
        let cid_id = doc.add_object(Object::Dictionary(cid_font));

        // Type 0 font dict
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type0")));
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("Identity-H")),
        );
        font_dict.insert(
            PdfName::new("DescendantFonts"),
            Object::Array(vec![Object::Reference(IndirectRef::new(
                cid_id.0, cid_id.1,
            ))]),
        );

        let cid_prog = extract_cid_font_program(&font_dict, &doc);
        assert!(cid_prog.is_some(), "should parse CIDFont from Type 0 dict");

        let cid_prog = cid_prog.unwrap();
        assert!(cid_prog.units_per_em() > 0.0);
        assert_eq!(cid_prog.advance_width(36), 600.0);
        assert_eq!(cid_prog.advance_width(99), 500.0); // falls back to DW
    }

    #[test]
    fn cidfont_glyph_path() {
        let ttf_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let cid_prog = CidFontProgram {
            data: ttf_data,
            default_width: 1000.0,
            widths: HashMap::new(),
            units_per_em: 2048.0,
        };

        // GID 36 in DejaVu Sans is typically '$' or similar — should have an outline
        let path = cid_prog.glyph_path_by_gid(36);
        assert!(path.is_some(), "GID 36 should produce a glyph path");
    }

    #[test]
    fn parse_w_array_formats() {
        // Test consecutive CID format: [cid [w1 w2 ...]]
        let arr = Object::Array(vec![
            Object::Integer(10),
            Object::Array(vec![Object::Integer(300), Object::Integer(400)]),
        ]);
        let widths = parse_w_array(Some(&arr));
        assert_eq!(widths.get(&10), Some(&300.0));
        assert_eq!(widths.get(&11), Some(&400.0));
        assert_eq!(widths.get(&12), None);

        // Test range format: [cid_first cid_last w]
        let arr = Object::Array(vec![
            Object::Integer(20),
            Object::Integer(22),
            Object::Integer(500),
        ]);
        let widths = parse_w_array(Some(&arr));
        assert_eq!(widths.get(&20), Some(&500.0));
        assert_eq!(widths.get(&21), Some(&500.0));
        assert_eq!(widths.get(&22), Some(&500.0));
        assert_eq!(widths.get(&23), None);
    }

    #[test]
    fn strip_pfb_wrapper_basic() {
        // Construct a minimal PFB with two segments
        let mut pfb = Vec::new();
        // Segment 1: ASCII (type 1), length 5
        pfb.push(0x80);
        pfb.push(1); // ASCII
        pfb.extend_from_slice(&5u32.to_le_bytes());
        pfb.extend_from_slice(b"Hello");
        // Segment 2: Binary (type 2), length 3
        pfb.push(0x80);
        pfb.push(2); // Binary
        pfb.extend_from_slice(&3u32.to_le_bytes());
        pfb.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        // EOF marker
        pfb.push(0x80);
        pfb.push(3);

        let result = strip_pfb_wrapper(pfb);
        assert_eq!(&result[..5], b"Hello");
        assert_eq!(&result[5..], &[0xDE, 0xAD, 0xBE]);
    }

    #[test]
    fn strip_pfb_wrapper_no_header() {
        // Non-PFB data should be returned unchanged
        let data = b"not a pfb file".to_vec();
        let expected = data.clone();
        let result = strip_pfb_wrapper(data);
        assert_eq!(result, expected);
    }

    #[test]
    fn extract_font_program_tries_fontfile() {
        use crate::core::objects::{IndirectRef, PdfName, PdfStream};
        use crate::document::Document;

        let mut doc = Document::new();

        // Create a dummy stream as /FontFile (Type 1)
        // skrifa won't parse Type 1 PostScript, so extract_font_program will
        // return None, but the code path should be exercised without panicking.
        let type1_data = b"%!PS-AdobeFont-1.0: TestFont";
        let font_stream = PdfStream::new(Dictionary::new(), type1_data.to_vec());
        let fs_id = doc.add_object(Object::Stream(font_stream));

        let mut descriptor = Dictionary::new();
        descriptor.insert(
            PdfName::new("FontFile"),
            Object::Reference(IndirectRef::new(fs_id.0, fs_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(descriptor));

        let mut font_dict = Dictionary::new();
        font_dict.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );

        // Should not panic, may return None since skrifa can't parse Type 1
        let _fp = extract_font_program(&font_dict, &doc);
    }

    #[test]
    fn extract_font_program_prefers_fontfile2() {
        use crate::core::objects::{IndirectRef, PdfName, PdfStream};
        use crate::document::Document;

        let ttf_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut doc = Document::new();

        // Add both /FontFile (dummy) and /FontFile2 (real TTF)
        let dummy = PdfStream::new(Dictionary::new(), b"dummy".to_vec());
        let dummy_id = doc.add_object(Object::Stream(dummy));

        let ttf_stream = PdfStream::new(Dictionary::new(), ttf_data);
        let ttf_id = doc.add_object(Object::Stream(ttf_stream));

        let mut descriptor = Dictionary::new();
        descriptor.insert(
            PdfName::new("FontFile"),
            Object::Reference(IndirectRef::new(dummy_id.0, dummy_id.1)),
        );
        descriptor.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(ttf_id.0, ttf_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(descriptor));

        let mut font_dict = Dictionary::new();
        font_dict.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );

        // Should prefer FontFile2 (TrueType) over FontFile (Type 1)
        let fp = extract_font_program(&font_dict, &doc);
        assert!(
            fp.is_some(),
            "Should extract from FontFile2 when both present"
        );
    }

    #[test]
    fn type3_font_program_from_dict() {
        use crate::core::objects::{Dictionary, IndirectRef, Object, PdfName, PdfStream};
        use crate::document::Document;

        let mut doc = Document::new();

        // CharProc: a simple filled rectangle "0 0 100 100 re f"
        let glyph_stream = PdfStream::new(Dictionary::new(), b"0 0 100 100 re f".to_vec());
        let glyph_id = doc.add_object(Object::Stream(glyph_stream));

        // CharProcs dict: /A → glyph stream
        let mut char_procs = Dictionary::new();
        char_procs.insert(
            PdfName::new("A"),
            Object::Reference(IndirectRef::new(glyph_id.0, glyph_id.1)),
        );
        let cp_id = doc.add_object(Object::Dictionary(char_procs));

        // Encoding with Differences: code 65 = /A
        let mut enc = Dictionary::new();
        enc.insert(PdfName::new("Type"), Object::Name(PdfName::new("Encoding")));
        enc.insert(
            PdfName::new("Differences"),
            Object::Array(vec![Object::Integer(65), Object::Name(PdfName::new("A"))]),
        );
        let enc_id = doc.add_object(Object::Dictionary(enc));

        // Font dict
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type3")));
        font_dict.insert(
            PdfName::new("FontMatrix"),
            Object::Array(vec![
                Object::Real(0.001),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(0.001),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        font_dict.insert(PdfName::new("FirstChar"), Object::Integer(65));
        font_dict.insert(PdfName::new("LastChar"), Object::Integer(65));
        font_dict.insert(
            PdfName::new("Widths"),
            Object::Array(vec![Object::Integer(500)]),
        );
        font_dict.insert(
            PdfName::new("CharProcs"),
            Object::Reference(IndirectRef::new(cp_id.0, cp_id.1)),
        );
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Reference(IndirectRef::new(enc_id.0, enc_id.1)),
        );

        let t3 = Type3FontProgram::from_dict(&font_dict, &doc);
        assert!(t3.is_some(), "should parse Type 3 font");

        let t3 = t3.unwrap();
        assert_eq!(t3.glyph_width(65), Some(500.0));
        assert!(t3.glyph_stream(65).is_some());
        assert_eq!(t3.glyph_stream(65).unwrap(), b"0 0 100 100 re f");
        assert!(t3.glyph_stream(66).is_none());
    }
}
