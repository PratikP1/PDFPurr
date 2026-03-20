//! Font loading and character code to Unicode mapping.
//!
//! Parses PDF font dictionaries and provides character code → Unicode
//! translation using encoding tables and ToUnicode CMaps.
//!
//! ISO 32000-2:2020, Section 9.5–9.8.

use crate::core::objects::{decode_utf16be, DictExt, Dictionary, Object};
use crate::error::{PdfError, PdfResult};
use crate::fonts::cmap::ToUnicodeCMap;
use crate::fonts::encoding::Encoding;

/// PDF font subtypes (ISO 32000-2:2020, Table 109).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontSubtype {
    /// Type 1 font (PostScript).
    Type1,
    /// MMType1 (Multiple Master Type 1).
    MMType1,
    /// TrueType font.
    TrueType,
    /// Type 3 font (glyph descriptions as content streams).
    Type3,
    /// Type 0 composite font (references CIDFont descendants).
    Type0,
    /// CIDFontType0 (CID-keyed Type 1).
    CIDFontType0,
    /// CIDFontType2 (CID-keyed TrueType).
    CIDFontType2,
}

impl FontSubtype {
    /// Parses a font subtype from its PDF name.
    pub fn from_name(name: &str) -> PdfResult<Self> {
        match name {
            "Type1" => Ok(Self::Type1),
            "MMType1" => Ok(Self::MMType1),
            "TrueType" => Ok(Self::TrueType),
            "Type3" => Ok(Self::Type3),
            "Type0" => Ok(Self::Type0),
            "CIDFontType0" => Ok(Self::CIDFontType0),
            "CIDFontType2" => Ok(Self::CIDFontType2),
            _ => Err(PdfError::InvalidFont(format!(
                "Unknown font subtype: {}",
                name
            ))),
        }
    }
}

/// A PDF font that can map character codes to Unicode text.
#[derive(Debug, Clone)]
pub struct Font {
    /// The font's base name (from `/BaseFont` entry).
    pub(crate) name: String,
    /// The font subtype.
    pub(crate) subtype: FontSubtype,
    /// The encoding for single-byte character mapping.
    pub(crate) encoding: Encoding,
    /// Optional ToUnicode CMap for direct code → Unicode mapping.
    pub(crate) to_unicode: Option<ToUnicodeCMap>,
}

impl Font {
    /// Creates a fallback font with default WinAnsi encoding.
    ///
    /// Used when a font's dictionary is corrupt or unparseable.
    /// Text extraction will use WinAnsiEncoding, which is correct
    /// for most Western-language PDFs.
    pub(crate) fn default_fallback() -> Self {
        Self {
            name: "Unknown".to_string(),
            subtype: FontSubtype::Type1,
            encoding: Encoding::win_ansi(),
            to_unicode: None,
        }
    }

    /// Returns the font's base name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the font subtype.
    pub fn subtype(&self) -> &FontSubtype {
        &self.subtype
    }

    /// Returns whether this is a composite (Type 0) font with multi-byte codes.
    pub fn is_composite(&self) -> bool {
        self.subtype == FontSubtype::Type0
    }

    /// Creates a font for testing purposes.
    #[cfg(test)]
    pub(crate) fn for_test(name: &str, subtype: FontSubtype, encoding: Encoding) -> Self {
        Self {
            name: name.to_string(),
            subtype,
            encoding,
            to_unicode: None,
        }
    }

    /// Builds a Font from a font dictionary.
    ///
    /// The `resolve` closure is used to follow indirect references
    /// (e.g., for `/ToUnicode` stream objects and `/Encoding` dictionaries).
    pub fn from_dict<'d, R>(dict: &Dictionary, resolve: &R) -> PdfResult<Self>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        // Read /Subtype
        let subtype_name = dict.get_name("Subtype").unwrap_or("Type1");
        let subtype = FontSubtype::from_name(subtype_name)?;

        // Read /BaseFont
        let name = dict.get_name("BaseFont").unwrap_or("Unknown").to_string();

        // Read /ToUnicode CMap (if present)
        let to_unicode = Self::load_tounicode(dict, resolve)?;

        // Read /Encoding
        let encoding = Self::load_encoding(dict, &subtype)?;

        Ok(Font {
            name,
            subtype,
            encoding,
            to_unicode,
        })
    }

    /// Decodes a byte slice to Unicode text using this font's mappings.
    ///
    /// Resolution priority:
    /// 1. ToUnicode CMap (if present)
    /// 2. Encoding table
    /// 3. Latin-1 fallback (byte value = Unicode codepoint)
    pub fn decode_bytes(&self, bytes: &[u8]) -> String {
        let mut result = String::with_capacity(bytes.len());
        self.decode_bytes_into(bytes, &mut result);
        result
    }

    /// Decodes a byte slice and appends the result directly to `out`,
    /// avoiding an intermediate allocation.
    pub fn decode_bytes_into(&self, bytes: &[u8], out: &mut String) {
        // Check for UTF-16BE BOM
        if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
            if let Some(s) = decode_utf16be(&bytes[2..]) {
                out.push_str(&s);
            }
            return;
        }

        if self.is_composite() {
            self.decode_composite_into(bytes, out);
        } else {
            self.decode_simple_into(bytes, out);
        }
    }

    /// Decodes bytes for a simple (single-byte) font into `out`.
    fn decode_simple_into(&self, bytes: &[u8], out: &mut String) {
        for &b in bytes {
            // Try ToUnicode first
            if let Some(ref cmap) = self.to_unicode {
                if let Some(s) = cmap.map_code(&[b]) {
                    out.push_str(&s);
                    continue;
                }
            }
            // Try encoding
            if let Some(ch) = self.encoding.decode_byte(b) {
                out.push(ch);
            } else {
                // Latin-1 fallback
                out.push(b as char);
            }
        }
    }

    /// Decodes bytes for a composite (Type 0 / CID) font into `out`.
    ///
    /// Composite fonts use multi-byte character codes. With a ToUnicode
    /// CMap, we try 2-byte codes first. Without it, we fall back to
    /// single-byte processing.
    fn decode_composite_into(&self, bytes: &[u8], out: &mut String) {
        let mut i = 0;

        while i < bytes.len() {
            // Try 2-byte code with ToUnicode
            if let Some(ref cmap) = self.to_unicode {
                if i + 1 < bytes.len() {
                    if let Some(s) = cmap.map_code(&bytes[i..i + 2]) {
                        out.push_str(&s);
                        i += 2;
                        continue;
                    }
                }
                // Try 1-byte code
                if let Some(s) = cmap.map_code(&bytes[i..i + 1]) {
                    out.push_str(&s);
                    i += 1;
                    continue;
                }
            }
            // Fallback: treat as single byte
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    /// Loads the ToUnicode CMap from the font dictionary.
    fn load_tounicode<'d, R>(dict: &Dictionary, resolve: &R) -> PdfResult<Option<ToUnicodeCMap>>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        let tu_obj = match dict.get_str("ToUnicode") {
            Some(obj) => obj,
            None => return Ok(None),
        };

        // Resolve reference if needed
        let resolved = match tu_obj {
            Object::Reference(_) => match resolve(tu_obj) {
                Some(obj) => obj,
                None => return Ok(None),
            },
            _ => tu_obj,
        };

        // Decode the stream
        let stream = match resolved {
            Object::Stream(s) => s,
            _ => return Ok(None),
        };

        let data = stream.decode_data()?;
        let cmap = ToUnicodeCMap::parse(&data)?;
        Ok(Some(cmap))
    }

    /// Loads the encoding from the font dictionary.
    fn load_encoding(dict: &Dictionary, subtype: &FontSubtype) -> PdfResult<Encoding> {
        if let Some(enc_obj) = dict.get_str("Encoding") {
            Encoding::from_object(enc_obj)
        } else {
            // Default encoding depends on font subtype
            Ok(match subtype {
                FontSubtype::Type1 | FontSubtype::MMType1 => Encoding::standard(),
                FontSubtype::TrueType => Encoding::win_ansi(),
                _ => Encoding::latin1(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;

    fn make_simple_font_dict(encoding_name: &str) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type1")));
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Helvetica")),
        );
        dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new(encoding_name)),
        );
        dict
    }

    /// A resolve closure that never resolves (for tests without indirect references).
    macro_rules! no_resolve {
        () => {
            |_obj: &Object| -> Option<&Object> { None }
        };
    }

    #[test]
    fn load_type1_font_win_ansi() {
        let dict = make_simple_font_dict("WinAnsiEncoding");
        let font = Font::from_dict(&dict, &no_resolve!()).unwrap();

        assert_eq!(font.name(), "Helvetica");
        assert_eq!(*font.subtype(), FontSubtype::Type1);
        assert!(!font.is_composite());
    }

    #[test]
    fn decode_simple_ascii() {
        let dict = make_simple_font_dict("WinAnsiEncoding");
        let font = Font::from_dict(&dict, &no_resolve!()).unwrap();

        let text = font.decode_bytes(b"Hello");
        assert_eq!(text, "Hello");
    }

    #[test]
    fn decode_win_ansi_special() {
        let dict = make_simple_font_dict("WinAnsiEncoding");
        let font = Font::from_dict(&dict, &no_resolve!()).unwrap();

        // 0x93 = left double quotation mark, 0x94 = right double quotation mark
        let text = font.decode_bytes(&[0x93, 0x48, 0x69, 0x94]);
        assert_eq!(text, "\u{201C}Hi\u{201D}");
    }

    #[test]
    fn decode_utf16be_bom() {
        let dict = make_simple_font_dict("WinAnsiEncoding");
        let font = Font::from_dict(&dict, &no_resolve!()).unwrap();

        // UTF-16BE BOM + "Hi"
        let text = font.decode_bytes(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]);
        assert_eq!(text, "Hi");
    }

    #[test]
    fn font_subtype_parsing() {
        assert_eq!(FontSubtype::from_name("Type1").unwrap(), FontSubtype::Type1);
        assert_eq!(
            FontSubtype::from_name("TrueType").unwrap(),
            FontSubtype::TrueType
        );
        assert_eq!(FontSubtype::from_name("Type0").unwrap(), FontSubtype::Type0);
        assert!(FontSubtype::from_name("Unknown").is_err());
    }

    #[test]
    fn default_encoding_by_subtype() {
        // Type1 without /Encoding → StandardEncoding
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type1")));
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Times-Roman")),
        );
        let font = Font::from_dict(&dict, &no_resolve!()).unwrap();
        // StandardEncoding: 0xAE = fi ligature
        assert_eq!(font.encoding.decode_byte(0xAE), Some('\u{FB01}'));

        // TrueType without /Encoding → WinAnsiEncoding
        let mut dict2 = Dictionary::new();
        dict2.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("TrueType")),
        );
        dict2.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Arial")),
        );
        let font2 = Font::from_dict(&dict2, &no_resolve!()).unwrap();
        // WinAnsiEncoding: 0x80 = Euro sign
        assert_eq!(font2.encoding.decode_byte(0x80), Some('\u{20AC}'));
    }
}
