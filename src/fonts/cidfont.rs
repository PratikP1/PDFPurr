//! CIDFont support for CJK and complex script font embedding.
//!
//! Generates Type 0 (composite) fonts with CIDFont Type 2 descendants
//! for TrueType-based CID fonts. This enables full Unicode coverage
//! including CJK characters, Arabic, Hebrew, Devanagari, etc.
//!
//! ISO 32000-2:2020, Section 9.7.

use std::collections::BTreeMap;

use super::common;
use crate::core::objects::{Dictionary, Object, PdfName, PdfStream};
use crate::error::PdfResult;

/// A CID-keyed font built from a TrueType font.
///
/// Produces a Type 0 composite font with a CIDFont Type 2 descendant.
/// Text is encoded as 2-byte big-endian glyph IDs, giving access to
/// the font's full glyph repertoire (up to 65535 glyphs).
pub struct CidFont {
    /// Raw font file bytes.
    data: Vec<u8>,
    /// PostScript name.
    ps_name: String,
    /// Units per em.
    units_per_em: u16,
    /// Ascent.
    ascent: f32,
    /// Descent.
    descent: f32,
    /// Cap height.
    cap_height: f32,
    /// Bounding box.
    bbox: [f32; 4],
}

impl CidFont {
    /// Creates a CID font from TrueType font data.
    pub fn from_ttf(data: &[u8]) -> PdfResult<Self> {
        let m = common::parse_font_metrics(data)?;
        Ok(Self {
            data: data.to_vec(),
            ps_name: m.ps_name,
            units_per_em: m.units_per_em,
            ascent: m.ascent,
            descent: m.descent,
            cap_height: m.cap_height,
            bbox: m.bbox,
        })
    }

    /// Returns the PostScript name.
    pub fn ps_name(&self) -> &str {
        &self.ps_name
    }

    /// Subsets the font to only include glyphs needed for the given characters.
    pub fn subset(&self, chars: &[char]) -> PdfResult<SubsetCidFont> {
        let result = common::collect_glyphs_and_subset(&self.data, chars)?;

        Ok(SubsetCidFont {
            data: result.data,
            ps_name: format!("{}+CIDSubset", self.ps_name),
            units_per_em: self.units_per_em,
            ascent: self.ascent,
            descent: self.descent,
            cap_height: self.cap_height,
            bbox: self.bbox,
            char_to_gid: result.char_to_gid,
            old_to_new: result.old_to_new,
            widths: result.widths,
        })
    }
}

/// A subsetted CID font ready for PDF embedding as a Type 0 composite font.
pub struct SubsetCidFont {
    /// Subsetted font data.
    data: Vec<u8>,
    /// PostScript name with subset tag.
    ps_name: String,
    /// Units per em.
    units_per_em: u16,
    /// Ascent.
    ascent: f32,
    /// Descent.
    descent: f32,
    /// Cap height.
    cap_height: f32,
    /// Bounding box.
    bbox: [f32; 4],
    /// Unicode char → original glyph ID.
    char_to_gid: BTreeMap<char, u16>,
    /// Original glyph ID → subset glyph ID.
    old_to_new: BTreeMap<u16, u16>,
    /// Glyph widths by original glyph ID.
    widths: BTreeMap<u16, u16>,
}

impl SubsetCidFont {
    /// Encodes text as 2-byte big-endian glyph IDs for use in content streams.
    pub fn encode_text(&self, text: &str) -> Vec<u8> {
        let mut result = Vec::with_capacity(text.len() * 2);
        for ch in text.chars() {
            if let Some(&old_gid) = self.char_to_gid.get(&ch) {
                if let Some(&new_gid) = self.old_to_new.get(&old_gid) {
                    result.extend_from_slice(&new_gid.to_be_bytes());
                }
            }
        }
        result
    }

    /// Generates the top-level Type 0 font dictionary.
    ///
    /// Requires a reference to the CIDFont descendant and ToUnicode CMap.
    pub fn to_type0_dictionary(
        &self,
        descendant_ref: Object,
        to_unicode_ref: Object,
    ) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type0")));
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new(&self.ps_name)),
        );
        dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("Identity-H")),
        );
        dict.insert(
            PdfName::new("DescendantFonts"),
            Object::Array(vec![descendant_ref]),
        );
        dict.insert(PdfName::new("ToUnicode"), to_unicode_ref);
        dict
    }

    /// Generates the CIDFont Type 2 descendant dictionary.
    pub fn to_cidfont_dictionary(&self, descriptor_ref: Object) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        dict.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("CIDFontType2")),
        );
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new(&self.ps_name)),
        );

        // CIDSystemInfo
        let mut sys_info = Dictionary::new();
        sys_info.insert(
            PdfName::new("Registry"),
            Object::String(crate::core::objects::PdfString::from_literal("Adobe")),
        );
        sys_info.insert(
            PdfName::new("Ordering"),
            Object::String(crate::core::objects::PdfString::from_literal("Identity")),
        );
        sys_info.insert(PdfName::new("Supplement"), Object::Integer(0));
        dict.insert(PdfName::new("CIDSystemInfo"), Object::Dictionary(sys_info));

        // /W array: CID-to-width mappings
        let scale = 1000.0 / self.units_per_em as f64;
        let mut w_array: Vec<Object> = Vec::new();
        for (&old_gid, &new_gid) in &self.old_to_new {
            let w = self.widths.get(&old_gid).copied().unwrap_or(0);
            let scaled = (w as f64 * scale).round() as i64;
            w_array.push(Object::Integer(new_gid as i64));
            w_array.push(Object::Array(vec![Object::Integer(scaled)]));
        }
        if !w_array.is_empty() {
            dict.insert(PdfName::new("W"), Object::Array(w_array));
        }

        // Default width (for .notdef)
        let default_w = self
            .widths
            .get(&0)
            .map(|&w| (w as f64 * scale).round() as i64)
            .unwrap_or(1000);
        dict.insert(PdfName::new("DW"), Object::Integer(default_w));

        dict.insert(PdfName::new("FontDescriptor"), descriptor_ref);

        // CIDToGIDMap — Identity mapping since we use subset glyph IDs directly
        dict.insert(
            PdfName::new("CIDToGIDMap"),
            Object::Name(PdfName::new("Identity")),
        );

        dict
    }

    /// Generates the font descriptor dictionary.
    pub fn to_font_descriptor(&self, font_file_ref: Object) -> Dictionary {
        common::build_font_descriptor(common::FontDescriptorParams {
            ps_name: &self.ps_name,
            units_per_em: self.units_per_em,
            ascent: self.ascent,
            descent: self.descent,
            cap_height: self.cap_height,
            bbox: self.bbox,
            flags: 4, // Symbolic
            font_file_key: "FontFile2",
            font_file_ref,
        })
    }

    /// Generates the font program stream.
    pub fn to_font_stream(&self) -> PdfResult<PdfStream> {
        common::build_font_stream(&self.data)
    }

    /// Generates a ToUnicode CMap for 2-byte CID encoding.
    pub fn to_unicode_cmap(&self) -> PdfResult<PdfStream> {
        common::build_to_unicode_cmap(&self.char_to_gid, &self.old_to_new, 2)
    }

    /// Returns the subsetted font data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns the number of glyphs in the subset.
    pub fn glyph_count(&self) -> usize {
        self.old_to_new.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font_data() -> Option<Vec<u8>> {
        let paths = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read(path) {
                return Some(data);
            }
        }
        None
    }

    #[test]
    fn cidfont_from_ttf() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        assert!(!font.ps_name().is_empty());
    }

    #[test]
    fn cidfont_dictionary() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A', 'B', 'C']).unwrap();

        let type0 = subset.to_type0_dictionary(Object::Null, Object::Null);
        assert_eq!(
            type0
                .get(&PdfName::new("Subtype"))
                .and_then(|o| o.as_name()),
            Some("Type0")
        );
        assert_eq!(
            type0
                .get(&PdfName::new("Encoding"))
                .and_then(|o| o.as_name()),
            Some("Identity-H")
        );
        assert!(type0.get(&PdfName::new("DescendantFonts")).is_some());
    }

    #[test]
    fn cidfont_cid_widths() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A', 'B']).unwrap();

        let cid_dict = subset.to_cidfont_dictionary(Object::Null);
        assert_eq!(
            cid_dict
                .get(&PdfName::new("Subtype"))
                .and_then(|o| o.as_name()),
            Some("CIDFontType2")
        );
        // Should have /W array with width entries
        assert!(cid_dict.get(&PdfName::new("W")).is_some());
        // Should have /DW default width
        assert!(cid_dict.get(&PdfName::new("DW")).is_some());
    }

    #[test]
    fn cidfont_to_unicode() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();

        let cmap_stream = subset.to_unicode_cmap().unwrap();
        let cmap_data = cmap_stream.decode_data().unwrap();
        let cmap_str = String::from_utf8(cmap_data).unwrap();
        // 2-byte codespace for CID
        assert!(cmap_str.contains("<0000> <FFFF>"));
        assert!(cmap_str.contains("beginbfchar"));
    }

    #[test]
    fn cidfont_encode_text() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['H', 'i']).unwrap();

        let encoded = subset.encode_text("Hi");
        // 2 characters × 2 bytes each = 4 bytes
        assert_eq!(encoded.len(), 4);
        // Both bytes should be big-endian glyph IDs
        let gid1 = u16::from_be_bytes([encoded[0], encoded[1]]);
        let gid2 = u16::from_be_bytes([encoded[2], encoded[3]]);
        assert_ne!(gid1, gid2); // H and i have different glyph IDs
    }

    #[test]
    fn cidfont_subset() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A', 'B', 'C']).unwrap();
        assert!(subset.data().len() < data.len());
        assert!(subset.glyph_count() > 0);
    }

    #[test]
    fn cidfont_font_descriptor() {
        let data = match test_font_data() {
            Some(d) => d,
            None => return,
        };
        let font = CidFont::from_ttf(&data).unwrap();
        let subset = font.subset(&['A']).unwrap();

        let desc = subset.to_font_descriptor(Object::Null);
        assert_eq!(
            desc.get(&PdfName::new("Type")).and_then(|o| o.as_name()),
            Some("FontDescriptor")
        );
        // CID fonts use Symbolic flag (4)
        assert_eq!(
            desc.get(&PdfName::new("Flags")).and_then(|o| o.as_i64()),
            Some(4)
        );
    }
}
