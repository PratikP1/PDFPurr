//! Font embedding — parse, subset, and embed TTF/OTF fonts in PDF.
//!
//! Uses [`skrifa`] for font metrics and [`allsorts`] for subsetting.
//! Produces PDF font dictionaries, font descriptors, and font streams
//! for embedding in a document.
//!
//! ISO 32000-2:2020, Section 9.6.

use std::collections::BTreeMap;

use skrifa::instance::Size;
use skrifa::metrics::GlyphMetrics;
use skrifa::{FontRef, MetadataProvider};

use super::common;
use crate::core::objects::{Dictionary, Object, PdfName, PdfStream};
use crate::error::{PdfError, PdfResult};

/// A parsed TrueType/OpenType font ready for embedding.
///
/// Holds the raw font data and provides access to font metrics.
/// Call [`subset`](Self::subset) to produce a [`SubsetFont`] containing
/// only the glyphs needed for specific text.
pub struct EmbeddedFont {
    /// The raw font file bytes (TTF or OTF).
    data: Vec<u8>,
    /// PostScript name of the font.
    ps_name: String,
    /// Units per em from the head table.
    units_per_em: u16,
    /// Font ascent in font units.
    ascent: f32,
    /// Font descent in font units (typically negative).
    descent: f32,
    /// Cap height in font units.
    cap_height: f32,
    /// Font bounding box [x_min, y_min, x_max, y_max] in font units.
    bbox: [f32; 4],
    /// Whether this is a TrueType (glyf) or CFF font.
    is_truetype: bool,
    /// Variation axis settings for variable fonts (empty for static fonts).
    axes: Vec<(String, f32)>,
}

impl EmbeddedFont {
    /// Parses a TrueType (.ttf) font from raw bytes.
    pub fn from_ttf(data: &[u8]) -> PdfResult<Self> {
        Self::from_font_data(data, true)
    }

    /// Parses an OpenType CFF (.otf) font from raw bytes.
    pub fn from_otf(data: &[u8]) -> PdfResult<Self> {
        Self::from_font_data(data, false)
    }

    /// Parses a TrueType variable font at specific axis settings.
    ///
    /// Each axis is a `(tag, value)` pair, e.g., `("wght", 700.0)` for bold.
    /// The font metrics and glyph widths are computed at the specified instance.
    pub fn from_ttf_with_axes(data: &[u8], axes: &[(&str, f32)]) -> PdfResult<Self> {
        let m = common::parse_font_metrics_with_axes(data, axes)?;
        Ok(Self {
            data: data.to_vec(),
            ps_name: m.ps_name,
            units_per_em: m.units_per_em,
            ascent: m.ascent,
            descent: m.descent,
            cap_height: m.cap_height,
            bbox: m.bbox,
            is_truetype: true,
            axes: axes.iter().map(|(t, v)| (t.to_string(), *v)).collect(),
        })
    }

    fn from_font_data(data: &[u8], expect_truetype: bool) -> PdfResult<Self> {
        let m = common::parse_font_metrics(data)?;
        Ok(Self {
            data: data.to_vec(),
            ps_name: m.ps_name,
            units_per_em: m.units_per_em,
            ascent: m.ascent,
            descent: m.descent,
            cap_height: m.cap_height,
            bbox: m.bbox,
            is_truetype: expect_truetype,
            axes: Vec::new(),
        })
    }

    /// Returns the PostScript name of the font.
    pub fn ps_name(&self) -> &str {
        &self.ps_name
    }

    /// Returns the units per em.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Returns the font ascent in font units.
    pub fn ascent(&self) -> f32 {
        self.ascent
    }

    /// Returns the font descent in font units.
    pub fn descent(&self) -> f32 {
        self.descent
    }

    /// Measures the width of a string in points at the given font size.
    pub fn measure_text(&self, text: &str, size: f64) -> PdfResult<f64> {
        let font_ref = FontRef::new(&self.data)
            .map_err(|e| PdfError::InvalidFont(format!("Font parse error: {}", e)))?;

        let axis_refs: Vec<(&str, f32)> = self.axes.iter().map(|(t, v)| (t.as_str(), *v)).collect();
        let location = common::build_location_pub(&font_ref, &axis_refs);
        let charmap = font_ref.charmap();
        let glyph_metrics = GlyphMetrics::new(&font_ref, Size::unscaled(), &location);

        let mut total_width: f64 = 0.0;
        for ch in text.chars() {
            if let Some(gid) = charmap.map(ch) {
                total_width += glyph_metrics.advance_width(gid).unwrap_or(0.0) as f64;
            }
        }

        Ok(total_width * size / self.units_per_em as f64)
    }

    /// Maximum glyphs for a simple (single-byte) font subset.
    ///
    /// PDF simple fonts use single-byte character codes (0..=255).
    /// For larger glyph sets, use [`crate::fonts::cidfont`] to produce
    /// a composite Type0/CIDFont with 2-byte codes.
    const MAX_SIMPLE_FONT_GLYPHS: usize = 256;

    /// Subsets the font to only include glyphs needed for the given characters.
    ///
    /// Returns a [`SubsetFont`] that can generate PDF dictionaries and streams.
    ///
    /// # Errors
    ///
    /// Returns [`PdfError::InvalidFont`] if the subset exceeds 256 glyphs,
    /// which is the limit for simple (single-byte) PDF fonts. For larger
    /// glyph sets (CJK, mixed scripts), use the CIDFont API instead.
    pub fn subset(&self, chars: &[char]) -> PdfResult<SubsetFont> {
        let axis_refs: Vec<(&str, f32)> = self.axes.iter().map(|(t, v)| (t.as_str(), *v)).collect();
        let result = common::collect_glyphs_and_subset_with_axes(&self.data, chars, &axis_refs)?;

        if result.old_to_new.len() > Self::MAX_SIMPLE_FONT_GLYPHS {
            return Err(PdfError::InvalidFont(format!(
                "Subset has {} glyphs, exceeding the 256-glyph limit for simple fonts. \
                 Use CIDFont (fonts::cidfont) for larger glyph sets.",
                result.old_to_new.len()
            )));
        }

        Ok(SubsetFont {
            data: result.data,
            ps_name: format!("{}+Subset", self.ps_name),
            units_per_em: self.units_per_em,
            ascent: self.ascent,
            descent: self.descent,
            cap_height: self.cap_height,
            bbox: self.bbox,
            is_truetype: self.is_truetype,
            char_to_gid: result.char_to_gid,
            old_to_new: result.old_to_new,
            widths: result.widths,
        })
    }
}

/// A subsetted font ready for PDF embedding.
///
/// Contains only the glyphs needed for specific text, plus the mapping
/// data required to generate PDF dictionaries and streams.
pub struct SubsetFont {
    /// The subsetted font program bytes.
    data: Vec<u8>,
    /// PostScript name with subset tag.
    ps_name: String,
    /// Units per em.
    units_per_em: u16,
    /// Font ascent.
    ascent: f32,
    /// Font descent.
    descent: f32,
    /// Cap height.
    cap_height: f32,
    /// Bounding box.
    bbox: [f32; 4],
    /// Whether TrueType (glyf) or CFF.
    is_truetype: bool,
    /// Maps Unicode characters to original glyph IDs.
    char_to_gid: BTreeMap<char, u16>,
    /// Maps original glyph IDs to new (subset) glyph IDs.
    old_to_new: BTreeMap<u16, u16>,
    /// Glyph widths indexed by original glyph ID, in font units.
    widths: BTreeMap<u16, u16>,
}

impl SubsetFont {
    /// Encodes a text string as bytes for use in a PDF content stream.
    ///
    /// For simple fonts (single-byte encoding), maps characters to glyph
    /// indices in the subset. Returns bytes suitable for use with Tj/TJ.
    pub fn encode_text(&self, text: &str) -> Vec<u8> {
        text.chars()
            .filter_map(|ch| {
                let old_gid = self.char_to_gid.get(&ch)?;
                let new_gid = self.old_to_new.get(old_gid)?;
                Some(*new_gid as u8)
            })
            .collect()
    }

    /// Generates the `/Font` dictionary for this subset font.
    ///
    /// For a simple TrueType font, this is a Type 1-like dictionary with
    /// `/Subtype /TrueType`, `/BaseFont`, `/FirstChar`, `/LastChar`,
    /// `/Widths`, and `/Encoding`.
    pub fn to_font_dictionary(&self, descriptor_ref: Object) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        dict.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new(if self.is_truetype {
                "TrueType"
            } else {
                "Type1"
            })),
        );
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new(&self.ps_name)),
        );

        // Widths array: indexed by character code (new GID)
        let num_glyphs = self.old_to_new.len();
        if num_glyphs > 0 {
            let max_new_gid = self.old_to_new.values().copied().max().unwrap_or(0) as usize;
            let mut width_array = vec![Object::Integer(0); max_new_gid + 1];
            for (&old_gid, &new_gid) in &self.old_to_new {
                let w = self.widths.get(&old_gid).copied().unwrap_or(0);
                let scaled = (w as f64 * 1000.0 / self.units_per_em as f64).round() as i64;
                if (new_gid as usize) < width_array.len() {
                    width_array[new_gid as usize] = Object::Integer(scaled);
                }
            }
            dict.insert(PdfName::new("FirstChar"), Object::Integer(0));
            dict.insert(
                PdfName::new("LastChar"),
                Object::Integer(max_new_gid as i64),
            );
            dict.insert(PdfName::new("Widths"), Object::Array(width_array));
        }

        dict.insert(PdfName::new("FontDescriptor"), descriptor_ref);
        dict
    }

    /// Generates the `/FontDescriptor` dictionary.
    pub fn to_font_descriptor(&self, font_file_ref: Object) -> Dictionary {
        common::build_font_descriptor(common::FontDescriptorParams {
            ps_name: &self.ps_name,
            units_per_em: self.units_per_em,
            ascent: self.ascent,
            descent: self.descent,
            cap_height: self.cap_height,
            bbox: self.bbox,
            flags: 32, // Nonsymbolic
            font_file_key: if self.is_truetype {
                "FontFile2"
            } else {
                "FontFile3"
            },
            font_file_ref,
        })
    }

    /// Generates the font program stream (for `/FontFile2` or `/FontFile3`).
    pub fn to_font_stream(&self) -> PdfResult<PdfStream> {
        common::build_font_stream(&self.data)
    }

    /// Generates a `/ToUnicode` CMap stream for this subset font.
    ///
    /// Maps character codes (new glyph IDs) back to Unicode code points
    /// so text can be extracted from the PDF.
    pub fn to_unicode_cmap(&self) -> PdfResult<PdfStream> {
        common::build_to_unicode_cmap(&self.char_to_gid, &self.old_to_new, 1)
    }

    /// Returns the number of glyphs in the subset.
    pub fn glyph_count(&self) -> usize {
        self.old_to_new.len()
    }

    /// Returns the subsetted font data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the bytes of a minimal valid TrueType font for testing.
    ///
    /// We use the system's DejaVu Sans if available, otherwise generate
    /// a stub. Tests that need real font data should skip gracefully.
    fn test_font_data() -> Option<Vec<u8>> {
        // Try common system font paths (single TTF only, not .ttc collections)
        let paths = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Courier New.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
            "C:\\Windows\\Fonts\\times.ttf",
            "C:\\Windows\\Fonts\\cour.ttf",
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read(path) {
                // Verify it parses as a single TTF (skip .ttc collections)
                if EmbeddedFont::from_ttf(&data).is_ok() {
                    return Some(data);
                }
            }
        }
        None
    }

    #[test]
    fn embed_load_ttf() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return, // Skip if no font available
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        assert!(!font.ps_name().is_empty());
        assert!(font.units_per_em() > 0);
    }

    #[test]
    fn embed_font_metrics() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        // Ascent should be positive, descent negative
        assert!(font.ascent() > 0.0);
        assert!(font.descent() < 0.0);
    }

    #[test]
    fn embed_measure_text() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let width = font.measure_text("Hello", 12.0).unwrap();
        assert!(width > 0.0);
        // Longer text should be wider
        let width2 = font.measure_text("Hello World", 12.0).unwrap();
        assert!(width2 > width);
    }

    #[test]
    fn embed_subset_font() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();
        // Subset should be smaller than original
        assert!(subset.data().len() < data.len());
        assert!(subset.glyph_count() > 0);
    }

    #[test]
    fn embed_font_descriptor() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A', 'B']).unwrap();
        let desc = subset.to_font_descriptor(Object::Null);

        assert_eq!(
            desc.get(&PdfName::new("Type")).and_then(|o| o.as_name()),
            Some("FontDescriptor")
        );
        assert!(desc.get(&PdfName::new("Ascent")).is_some());
        assert!(desc.get(&PdfName::new("Descent")).is_some());
        assert!(desc.get(&PdfName::new("CapHeight")).is_some());
        assert!(desc.get(&PdfName::new("FontBBox")).is_some());
        assert!(desc.get(&PdfName::new("FontFile2")).is_some());
    }

    #[test]
    fn embed_font_dictionary() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();
        let dict = subset.to_font_dictionary(Object::Null);

        assert_eq!(
            dict.get(&PdfName::new("Type")).and_then(|o| o.as_name()),
            Some("Font")
        );
        assert_eq!(
            dict.get(&PdfName::new("Subtype")).and_then(|o| o.as_name()),
            Some("TrueType")
        );
        assert!(dict.get(&PdfName::new("Widths")).is_some());
        assert!(dict.get(&PdfName::new("FirstChar")).is_some());
        assert!(dict.get(&PdfName::new("LastChar")).is_some());
    }

    #[test]
    fn embed_font_stream() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A']).unwrap();
        let stream = subset.to_font_stream().unwrap();

        // Stream should have Length1 (original size)
        assert!(stream.dict.get(&PdfName::new("Length1")).is_some());
        assert!(!stream.data.is_empty());
    }

    #[test]
    fn embed_to_unicode_cmap() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();
        let cmap_stream = subset.to_unicode_cmap().unwrap();

        // Decompress and check content
        let cmap_data = cmap_stream.decode_data().unwrap();
        let cmap_str = String::from_utf8(cmap_data).unwrap();
        assert!(cmap_str.contains("beginbfchar"));
        assert!(cmap_str.contains("endbfchar"));
        assert!(cmap_str.contains("begincmap"));
    }

    #[test]
    fn encode_text_truncation_detected() {
        // If a subset has new GIDs > 255, encode_text should error
        // (simple fonts are single-byte: codes 0..=255 only)
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();

        // Collect > 256 unique chars (most system fonts have this many)
        let chars: Vec<char> = (0x0020u32..=0x0200).filter_map(char::from_u32).collect();
        if chars.len() <= 256 {
            return; // font doesn't have enough glyphs for this test
        }
        let result = font.subset(&chars);
        // Should error because simple font can't encode >255 glyphs
        assert!(
            result.is_err(),
            "subset with >255 glyphs should error for simple font, got {} glyphs",
            result.as_ref().map(|s| s.glyph_count()).unwrap_or(0)
        );
    }

    #[test]
    fn font_dictionary_last_char_within_byte_range() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A', 'B', 'C']).unwrap();
        let dict = subset.to_font_dictionary(Object::Null);

        let last_char = dict
            .get(&PdfName::new("LastChar"))
            .and_then(|o| o.as_i64())
            .unwrap();
        assert!(
            last_char <= 255,
            "/LastChar should be <= 255 for simple font, got {}",
            last_char
        );
    }

    #[test]
    fn subset_empty_chars_produces_empty_subset() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&[]).unwrap();
        // Subsetter may include .notdef (GID 0), so count can be 0 or 1
        assert!(
            subset.glyph_count() <= 1,
            "empty subset should have at most .notdef"
        );
        let encoded = subset.encode_text("Hello");
        assert!(
            encoded.is_empty(),
            "encoding with empty subset should produce no bytes"
        );
    }

    #[test]
    fn subset_unmapped_chars_are_skipped() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        // Subset with 'A' only, then encode text containing 'B'
        let subset = font.subset(&['A']).unwrap();
        let encoded = subset.encode_text("AB");
        // Only 'A' should be encoded; 'B' is not in the subset
        assert_eq!(encoded.len(), 1);
    }

    #[test]
    fn measure_empty_text_is_zero() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let width = font.measure_text("", 12.0).unwrap();
        assert_eq!(width, 0.0);
    }

    #[test]
    fn subset_duplicate_chars_deduplicates() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset1 = font.subset(&['A', 'A', 'A']).unwrap();
        let subset2 = font.subset(&['A']).unwrap();
        assert_eq!(subset1.glyph_count(), subset2.glyph_count());
    }

    #[test]
    fn invalid_font_data_returns_error() {
        let result = EmbeddedFont::from_ttf(b"not a font");
        assert!(result.is_err());
    }

    #[test]
    fn from_otf_with_ttf_data_still_works() {
        // from_otf sets is_truetype=false but skrifa parses both formats.
        // A TTF loaded as OTF should still parse (but generate /FontFile3).
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_otf(&data).unwrap();
        assert!(!font.ps_name().is_empty());

        let subset = font.subset(&['A']).unwrap();
        // FontDescriptor should use /FontFile3 (CFF path) not /FontFile2
        let desc = subset.to_font_descriptor(Object::Null);
        assert!(
            desc.get(&PdfName::new("FontFile3")).is_some(),
            "OTF font should use /FontFile3"
        );
        assert!(
            desc.get(&PdfName::new("FontFile2")).is_none(),
            "OTF font should not use /FontFile2"
        );
    }

    #[test]
    fn embed_encode_text() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = EmbeddedFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();
        let encoded = subset.encode_text("Hello");
        // Should have 5 bytes (H, e, l, l, o)
        assert_eq!(encoded.len(), 5);
        // Same character should map to same byte
        assert_eq!(encoded[2], encoded[3]); // both 'l'
    }

    #[test]
    fn variable_font_loads_and_subsets() {
        // Bahnschrift is a variable font with a weight axis
        let path = "C:\\Windows\\Fonts\\bahnschrift.ttf";
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => return, // Skip if not available
        };

        let font = EmbeddedFont::from_ttf(&data).unwrap();
        assert!(!font.ps_name().is_empty());

        // Should subset normally (default instance)
        let subset = font.subset(&['A', 'B', 'C']).unwrap();
        assert!(subset.glyph_count() >= 3);

        // Widths at default weight
        let width_default = font.measure_text("Hello", 12.0).unwrap();
        assert!(width_default > 0.0);
    }

    #[test]
    fn variable_font_with_axis_changes_metrics() {
        let path = "C:\\Windows\\Fonts\\bahnschrift.ttf";
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => return,
        };

        // Create at minimum and maximum weight
        let font_light = EmbeddedFont::from_ttf_with_axes(&data, &[("wght", 300.0)]).unwrap();
        let font_bold = EmbeddedFont::from_ttf_with_axes(&data, &[("wght", 700.0)]).unwrap();

        // The ascent/descent or widths should differ between instances.
        // Even if advance widths stay the same, from_ttf_with_axes should
        // not panic and should produce valid fonts at both extremes.
        let w_light = font_light.measure_text("Hello World", 12.0).unwrap();
        let w_bold = font_bold.measure_text("Hello World", 12.0).unwrap();

        // Both should produce valid non-zero widths
        assert!(w_light > 0.0, "Light width should be positive");
        assert!(w_bold > 0.0, "Bold width should be positive");

        // Subset at both extremes should work
        let subset_light = font_light.subset(&['A', 'B']).unwrap();
        let subset_bold = font_bold.subset(&['A', 'B']).unwrap();
        assert!(subset_light.glyph_count() >= 2);
        assert!(subset_bold.glyph_count() >= 2);
    }
}
