//! PDF character encoding tables and glyph name mapping.
//!
//! Implements the standard PDF encodings defined in ISO 32000-2:2020, Annex D,
//! and the Adobe Glyph List mapping from glyph names to Unicode codepoints.
//!
//! PDFs reference characters by byte codes (0–255). The encoding maps each
//! code to a Unicode character. The `/Encoding` dictionary entry in a font
//! selects a base encoding and optionally patches it with `/Differences`.

use crate::core::objects::{DictExt, Dictionary, Object};
use crate::error::{PdfError, PdfResult};

/// A character encoding that maps byte codes (0–255) to Unicode characters.
#[derive(Debug, Clone)]
pub struct Encoding {
    /// Encoding table: code → char. `None` means the code is undefined.
    /// Includes both the base encoding and any `/Differences` overrides
    /// (applied at parse time via Adobe Glyph List resolution).
    table: [Option<char>; 256],
}

impl Encoding {
    /// Creates an encoding from a raw lookup table.
    fn from_table(table: [Option<char>; 256]) -> Self {
        Self { table }
    }

    /// Returns the WinAnsiEncoding (most common in modern PDFs).
    pub fn win_ansi() -> Self {
        Self::from_table(WIN_ANSI_ENCODING)
    }

    /// Returns the MacRomanEncoding.
    pub fn mac_roman() -> Self {
        Self::from_table(MAC_ROMAN_ENCODING)
    }

    /// Returns the StandardEncoding (Type 1 default).
    pub fn standard() -> Self {
        Self::from_table(STANDARD_ENCODING)
    }

    /// Returns the MacExpertEncoding.
    pub fn mac_expert() -> Self {
        Self::from_table(MAC_EXPERT_ENCODING)
    }

    /// Returns a Latin-1 identity encoding (byte value = Unicode codepoint).
    /// Used as fallback when no encoding is specified.
    pub fn latin1() -> Self {
        let mut table = [None; 256];
        for i in 0u16..256 {
            table[i as usize] = char::from_u32(i as u32);
        }
        Self::from_table(table)
    }

    /// Builds an encoding from a font's `/Encoding` dictionary entry.
    ///
    /// Handles:
    /// - Name value: `/WinAnsiEncoding`, `/MacRomanEncoding`, `/StandardEncoding`, `/MacExpertEncoding`
    /// - Dictionary with `/BaseEncoding` and/or `/Differences`
    pub fn from_object(obj: &Object) -> PdfResult<Self> {
        match obj {
            Object::Name(name) => Self::from_name(name.as_str()),
            Object::Dictionary(dict) => Self::from_dict(dict),
            _ => Err(PdfError::TypeError {
                expected: "Name or Dictionary".to_string(),
                found: obj.type_name().to_string(),
            }),
        }
    }

    /// Builds an encoding from a named encoding.
    fn from_name(name: &str) -> PdfResult<Self> {
        match name {
            "WinAnsiEncoding" => Ok(Self::win_ansi()),
            "MacRomanEncoding" => Ok(Self::mac_roman()),
            "StandardEncoding" => Ok(Self::standard()),
            "MacExpertEncoding" => Ok(Self::mac_expert()),
            _ => Err(PdfError::UnsupportedFeature(format!(
                "Unknown encoding: {}",
                name
            ))),
        }
    }

    /// Builds an encoding from an encoding dictionary with
    /// optional `/BaseEncoding` and `/Differences`.
    fn from_dict(dict: &Dictionary) -> PdfResult<Self> {
        // Start with base encoding
        let mut encoding = if let Some(base_name) = dict.get_name("BaseEncoding") {
            Self::from_name(base_name)?
        } else {
            Self::standard()
        };

        // Apply /Differences array
        if let Some(diff_obj) = dict.get_str("Differences") {
            let arr = diff_obj.as_array().ok_or_else(|| PdfError::TypeError {
                expected: "Array".to_string(),
                found: diff_obj.type_name().to_string(),
            })?;
            encoding.apply_differences(arr)?;
        }

        Ok(encoding)
    }

    /// Applies a `/Differences` array to this encoding.
    ///
    /// Format: `[code1 name1 name2 ... code2 name3 ...]`
    /// Each integer sets the current code, each name maps that code
    /// and increments it.
    fn apply_differences(&mut self, arr: &[Object]) -> PdfResult<()> {
        let mut current_code: Option<u8> = None;

        for obj in arr {
            match obj {
                Object::Integer(n) => {
                    if *n < 0 || *n > 255 {
                        return Err(PdfError::InvalidFont(format!(
                            "Differences code out of range: {}",
                            n
                        )));
                    }
                    current_code = Some(*n as u8);
                }
                Object::Name(name) => {
                    let code = current_code.ok_or_else(|| {
                        PdfError::InvalidFont(
                            "Differences: name without preceding code".to_string(),
                        )
                    })?;
                    if let Some(ch) = glyph_name_to_unicode(name.as_str()) {
                        self.table[code as usize] = Some(ch);
                    }
                    current_code = Some(code.wrapping_add(1));
                }
                _ => {
                    // Ignore unexpected types
                }
            }
        }

        Ok(())
    }

    /// Decodes a single byte code to a Unicode character.
    pub fn decode_byte(&self, code: u8) -> Option<char> {
        self.table[code as usize]
    }

    /// Decodes a byte slice to a String using this encoding.
    pub fn decode_bytes(&self, bytes: &[u8]) -> String {
        let mut result = String::with_capacity(bytes.len());
        for &b in bytes {
            if let Some(ch) = self.decode_byte(b) {
                result.push(ch);
            }
        }
        result
    }
}

/// Looks up a glyph name in the Adobe Glyph List and returns its Unicode codepoint.
///
/// Covers the ~470 most common glyph names used in PDF fonts.
/// Also handles `uniXXXX` and `uXXXX` naming conventions.
pub fn glyph_name_to_unicode(name: &str) -> Option<char> {
    // Handle uniXXXX convention (exactly 4 hex digits after "uni")
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    // Handle uXXXX or uXXXXX convention
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    ADOBE_GLYPH_LIST.get(name).copied()
}

// =============================================================================
// Encoding tables
// =============================================================================

/// WinAnsiEncoding — the most common encoding in modern PDFs.
/// Maps codes 0x00–0xFF per ISO 32000-2:2020 Annex D, Table D.1.
const WIN_ANSI_ENCODING: [Option<char>; 256] = {
    let mut t = [None; 256];
    // 0x00–0x1F: control codes (undefined in PDF)
    // 0x20–0x7E: ASCII
    let mut i = 0x20u16;
    while i <= 0x7E {
        t[i as usize] = Some(i as u8 as char);
        i += 1;
    }
    // 0x80–0x9F: Windows-1252 special characters
    t[0x80] = Some('\u{20AC}'); // Euro sign
                                // 0x81 undefined
    t[0x82] = Some('\u{201A}'); // single low-9 quotation mark
    t[0x83] = Some('\u{0192}'); // Latin small f with hook
    t[0x84] = Some('\u{201E}'); // double low-9 quotation mark
    t[0x85] = Some('\u{2026}'); // horizontal ellipsis
    t[0x86] = Some('\u{2020}'); // dagger
    t[0x87] = Some('\u{2021}'); // double dagger
    t[0x88] = Some('\u{02C6}'); // modifier letter circumflex accent
    t[0x89] = Some('\u{2030}'); // per mille sign
    t[0x8A] = Some('\u{0160}'); // Latin capital S with caron
    t[0x8B] = Some('\u{2039}'); // single left-pointing angle quotation mark
    t[0x8C] = Some('\u{0152}'); // Latin capital ligature OE
                                // 0x8D undefined
    t[0x8E] = Some('\u{017D}'); // Latin capital Z with caron
                                // 0x8F undefined
                                // 0x90 undefined
    t[0x91] = Some('\u{2018}'); // left single quotation mark
    t[0x92] = Some('\u{2019}'); // right single quotation mark
    t[0x93] = Some('\u{201C}'); // left double quotation mark
    t[0x94] = Some('\u{201D}'); // right double quotation mark
    t[0x95] = Some('\u{2022}'); // bullet
    t[0x96] = Some('\u{2013}'); // en dash
    t[0x97] = Some('\u{2014}'); // em dash
    t[0x98] = Some('\u{02DC}'); // small tilde
    t[0x99] = Some('\u{2122}'); // trade mark sign
    t[0x9A] = Some('\u{0161}'); // Latin small s with caron
    t[0x9B] = Some('\u{203A}'); // single right-pointing angle quotation mark
    t[0x9C] = Some('\u{0153}'); // Latin small ligature oe
                                // 0x9D undefined
    t[0x9E] = Some('\u{017E}'); // Latin small z with caron
    t[0x9F] = Some('\u{0178}'); // Latin capital Y with diaeresis
                                // 0xA0–0xFF: Latin-1 supplement (same as Unicode)
    i = 0xA0;
    while i <= 0xFF {
        // SAFETY: 0xA0..=0xFF are all valid Unicode scalar values (Latin-1 Supplement)
        t[i as usize] = Some(char::from_u32(i as u32).unwrap());
        i += 1;
    }
    t
};

/// MacRomanEncoding — used by Mac-originated PDFs.
const MAC_ROMAN_ENCODING: [Option<char>; 256] = {
    let mut t = [None; 256];
    // 0x20–0x7E: ASCII (same as all encodings)
    let mut i = 0x20u16;
    while i <= 0x7E {
        t[i as usize] = Some(i as u8 as char);
        i += 1;
    }
    // High byte mappings for MacRoman
    t[0x80] = Some('\u{00C4}'); // Adieresis
    t[0x81] = Some('\u{00C5}'); // Aring
    t[0x82] = Some('\u{00C7}'); // Ccedilla
    t[0x83] = Some('\u{00C9}'); // Eacute
    t[0x84] = Some('\u{00D1}'); // Ntilde
    t[0x85] = Some('\u{00D6}'); // Odieresis
    t[0x86] = Some('\u{00DC}'); // Udieresis
    t[0x87] = Some('\u{00E1}'); // aacute
    t[0x88] = Some('\u{00E0}'); // agrave
    t[0x89] = Some('\u{00E2}'); // acircumflex
    t[0x8A] = Some('\u{00E4}'); // adieresis
    t[0x8B] = Some('\u{00E3}'); // atilde
    t[0x8C] = Some('\u{00E5}'); // aring
    t[0x8D] = Some('\u{00E7}'); // ccedilla
    t[0x8E] = Some('\u{00E9}'); // eacute
    t[0x8F] = Some('\u{00E8}'); // egrave
    t[0x90] = Some('\u{00EA}'); // ecircumflex
    t[0x91] = Some('\u{00EB}'); // edieresis
    t[0x92] = Some('\u{00ED}'); // iacute
    t[0x93] = Some('\u{00EC}'); // igrave
    t[0x94] = Some('\u{00EE}'); // icircumflex
    t[0x95] = Some('\u{00EF}'); // idieresis
    t[0x96] = Some('\u{00F1}'); // ntilde
    t[0x97] = Some('\u{00F3}'); // oacute
    t[0x98] = Some('\u{00F2}'); // ograve
    t[0x99] = Some('\u{00F4}'); // ocircumflex
    t[0x9A] = Some('\u{00F6}'); // odieresis
    t[0x9B] = Some('\u{00F5}'); // otilde
    t[0x9C] = Some('\u{00FA}'); // uacute
    t[0x9D] = Some('\u{00F9}'); // ugrave
    t[0x9E] = Some('\u{00FB}'); // ucircumflex
    t[0x9F] = Some('\u{00FC}'); // udieresis
    t[0xA0] = Some('\u{2020}'); // dagger
    t[0xA1] = Some('\u{00B0}'); // degree
    t[0xA2] = Some('\u{00A2}'); // cent
    t[0xA3] = Some('\u{00A3}'); // sterling
    t[0xA4] = Some('\u{00A7}'); // section
    t[0xA5] = Some('\u{2022}'); // bullet
    t[0xA6] = Some('\u{00B6}'); // paragraph
    t[0xA7] = Some('\u{00DF}'); // germandbls
    t[0xA8] = Some('\u{00AE}'); // registered
    t[0xA9] = Some('\u{00A9}'); // copyright
    t[0xAA] = Some('\u{2122}'); // trademark
    t[0xAB] = Some('\u{00B4}'); // acute
    t[0xAC] = Some('\u{00A8}'); // dieresis
    t[0xAD] = Some('\u{2260}'); // notequal
    t[0xAE] = Some('\u{00C6}'); // AE
    t[0xAF] = Some('\u{00D8}'); // Oslash
    t[0xB0] = Some('\u{221E}'); // infinity
    t[0xB1] = Some('\u{00B1}'); // plusminus
    t[0xB2] = Some('\u{2264}'); // lessequal
    t[0xB3] = Some('\u{2265}'); // greaterequal
    t[0xB4] = Some('\u{00A5}'); // yen
    t[0xB5] = Some('\u{00B5}'); // mu
    t[0xB6] = Some('\u{2202}'); // partialdiff
    t[0xB7] = Some('\u{2211}'); // summation
    t[0xB8] = Some('\u{220F}'); // product
    t[0xB9] = Some('\u{03C0}'); // pi
    t[0xBA] = Some('\u{222B}'); // integral
    t[0xBB] = Some('\u{00AA}'); // ordfeminine
    t[0xBC] = Some('\u{00BA}'); // ordmasculine
    t[0xBD] = Some('\u{2126}'); // Omega
    t[0xBE] = Some('\u{00E6}'); // ae
    t[0xBF] = Some('\u{00F8}'); // oslash
    t[0xC0] = Some('\u{00BF}'); // questiondown
    t[0xC1] = Some('\u{00A1}'); // exclamdown
    t[0xC2] = Some('\u{00AC}'); // logicalnot
    t[0xC3] = Some('\u{221A}'); // radical
    t[0xC4] = Some('\u{0192}'); // florin
    t[0xC5] = Some('\u{2248}'); // approxequal
    t[0xC6] = Some('\u{2206}'); // Delta
    t[0xC7] = Some('\u{00AB}'); // guillemotleft
    t[0xC8] = Some('\u{00BB}'); // guillemotright
    t[0xC9] = Some('\u{2026}'); // ellipsis
    t[0xCA] = Some('\u{00A0}'); // nbspace
    t[0xCB] = Some('\u{00C0}'); // Agrave
    t[0xCC] = Some('\u{00C3}'); // Atilde
    t[0xCD] = Some('\u{00D5}'); // Otilde
    t[0xCE] = Some('\u{0152}'); // OE
    t[0xCF] = Some('\u{0153}'); // oe
    t[0xD0] = Some('\u{2013}'); // endash
    t[0xD1] = Some('\u{2014}'); // emdash
    t[0xD2] = Some('\u{201C}'); // quotedblleft
    t[0xD3] = Some('\u{201D}'); // quotedblright
    t[0xD4] = Some('\u{2018}'); // quoteleft
    t[0xD5] = Some('\u{2019}'); // quoteright
    t[0xD6] = Some('\u{00F7}'); // divide
    t[0xD7] = Some('\u{25CA}'); // lozenge
    t[0xD8] = Some('\u{00FF}'); // ydieresis
    t[0xD9] = Some('\u{0178}'); // Ydieresis
    t[0xDA] = Some('\u{2044}'); // fraction
    t[0xDB] = Some('\u{20AC}'); // Euro
    t[0xDC] = Some('\u{2039}'); // guilsinglleft
    t[0xDD] = Some('\u{203A}'); // guilsinglright
    t[0xDE] = Some('\u{FB01}'); // fi
    t[0xDF] = Some('\u{FB02}'); // fl
    t[0xE0] = Some('\u{2021}'); // daggerdbl
    t[0xE1] = Some('\u{00B7}'); // periodcentered
    t[0xE2] = Some('\u{201A}'); // quotesinglbase
    t[0xE3] = Some('\u{201E}'); // quotedblbase
    t[0xE4] = Some('\u{2030}'); // perthousand
    t[0xE5] = Some('\u{00C2}'); // Acircumflex
    t[0xE6] = Some('\u{00CA}'); // Ecircumflex
    t[0xE7] = Some('\u{00C1}'); // Aacute
    t[0xE8] = Some('\u{00CB}'); // Edieresis
    t[0xE9] = Some('\u{00C8}'); // Egrave
    t[0xEA] = Some('\u{00CD}'); // Iacute
    t[0xEB] = Some('\u{00CE}'); // Icircumflex
    t[0xEC] = Some('\u{00CF}'); // Idieresis
    t[0xED] = Some('\u{00CC}'); // Igrave
    t[0xEE] = Some('\u{00D3}'); // Oacute
    t[0xEF] = Some('\u{00D4}'); // Ocircumflex
                                // 0xF0 = Apple logo (no standard Unicode)
    t[0xF1] = Some('\u{00D2}'); // Ograve
    t[0xF2] = Some('\u{00DA}'); // Uacute
    t[0xF3] = Some('\u{00DB}'); // Ucircumflex
    t[0xF4] = Some('\u{00D9}'); // Ugrave
    t[0xF5] = Some('\u{0131}'); // dotlessi
    t[0xF6] = Some('\u{02C6}'); // circumflex
    t[0xF7] = Some('\u{02DC}'); // tilde
    t[0xF8] = Some('\u{00AF}'); // macron
    t[0xF9] = Some('\u{02D8}'); // breve
    t[0xFA] = Some('\u{02D9}'); // dotaccent
    t[0xFB] = Some('\u{02DA}'); // ring
    t[0xFC] = Some('\u{00B8}'); // cedilla
    t[0xFD] = Some('\u{02DD}'); // hungarumlaut
    t[0xFE] = Some('\u{02DB}'); // ogonek
    t[0xFF] = Some('\u{02C7}'); // caron
    t
};

/// StandardEncoding — the default encoding for Type 1 fonts.
const STANDARD_ENCODING: [Option<char>; 256] = {
    let mut t = [None; 256];
    // ASCII subset (0x20–0x7E matches all encodings)
    let mut i = 0x20u16;
    while i <= 0x7E {
        t[i as usize] = Some(i as u8 as char);
        i += 1;
    }
    // StandardEncoding-specific high-byte mappings
    t[0xA1] = Some('\u{00A1}'); // exclamdown
    t[0xA2] = Some('\u{00A2}'); // cent
    t[0xA3] = Some('\u{00A3}'); // sterling
    t[0xA4] = Some('\u{2044}'); // fraction
    t[0xA5] = Some('\u{00A5}'); // yen
    t[0xA6] = Some('\u{0192}'); // florin
    t[0xA7] = Some('\u{00A7}'); // section
    t[0xA8] = Some('\u{00A4}'); // currency
    t[0xA9] = Some('\u{0027}'); // quotesingle
    t[0xAA] = Some('\u{201C}'); // quotedblleft
    t[0xAB] = Some('\u{00AB}'); // guillemotleft
    t[0xAC] = Some('\u{2039}'); // guilsinglleft
    t[0xAD] = Some('\u{203A}'); // guilsinglright
    t[0xAE] = Some('\u{FB01}'); // fi
    t[0xAF] = Some('\u{FB02}'); // fl
    t[0xB1] = Some('\u{2013}'); // endash
    t[0xB2] = Some('\u{2020}'); // dagger
    t[0xB3] = Some('\u{2021}'); // daggerdbl
    t[0xB4] = Some('\u{00B7}'); // periodcentered
    t[0xB6] = Some('\u{00B6}'); // paragraph
    t[0xB7] = Some('\u{2022}'); // bullet
    t[0xB8] = Some('\u{201A}'); // quotesinglbase
    t[0xB9] = Some('\u{201E}'); // quotedblbase
    t[0xBA] = Some('\u{201D}'); // quotedblright
    t[0xBB] = Some('\u{00BB}'); // guillemotright
    t[0xBC] = Some('\u{2026}'); // ellipsis
    t[0xBD] = Some('\u{2030}'); // perthousand
    t[0xBF] = Some('\u{00BF}'); // questiondown
    t[0xC1] = Some('\u{0060}'); // grave
    t[0xC2] = Some('\u{00B4}'); // acute
    t[0xC3] = Some('\u{02C6}'); // circumflex
    t[0xC4] = Some('\u{02DC}'); // tilde
    t[0xC5] = Some('\u{00AF}'); // macron
    t[0xC6] = Some('\u{02D8}'); // breve
    t[0xC7] = Some('\u{02D9}'); // dotaccent
    t[0xC8] = Some('\u{00A8}'); // dieresis
    t[0xCA] = Some('\u{02DA}'); // ring
    t[0xCB] = Some('\u{00B8}'); // cedilla
    t[0xCD] = Some('\u{02DD}'); // hungarumlaut
    t[0xCE] = Some('\u{02DB}'); // ogonek
    t[0xCF] = Some('\u{02C7}'); // caron
    t[0xD0] = Some('\u{2014}'); // emdash
    t[0xE1] = Some('\u{00C6}'); // AE
    t[0xE3] = Some('\u{00AA}'); // ordfeminine
    t[0xE8] = Some('\u{0141}'); // Lslash
    t[0xE9] = Some('\u{00D8}'); // Oslash
    t[0xEA] = Some('\u{0152}'); // OE
    t[0xEB] = Some('\u{00BA}'); // ordmasculine
    t[0xF1] = Some('\u{00E6}'); // ae
    t[0xF5] = Some('\u{0131}'); // dotlessi
    t[0xF8] = Some('\u{0142}'); // lslash
    t[0xF9] = Some('\u{00F8}'); // oslash
    t[0xFA] = Some('\u{0153}'); // oe
    t[0xFB] = Some('\u{00DF}'); // germandbls
    t
};

/// MacExpertEncoding — used for expert character sets.
const MAC_EXPERT_ENCODING: [Option<char>; 256] = {
    let mut t = [None; 256];
    // MacExpertEncoding maps (subset of most commonly used entries)
    t[0x20] = Some('\u{0020}'); // space
                                // Fractions and special characters
    t[0x21] = Some('\u{F721}'); // exclamsmall (PUA)
    t[0x22] = Some('\u{F6E2}'); // Hungarumlautsmall (PUA)
    t[0x24] = Some('\u{F724}'); // dollaroldstyle (PUA)
    t[0x27] = Some('\u{F6E4}'); // Acutesmall (PUA)
    t[0x2C] = Some('\u{F6E1}'); // commainferior (PUA)
    t[0x2D] = Some('\u{002D}'); // hyphen
    t[0x2E] = Some('\u{F6E3}'); // periodinferior (PUA)
    t[0x30] = Some('\u{F730}'); // zerooldstyle (PUA)
    t[0x31] = Some('\u{F731}'); // oneoldstyle (PUA)
    t[0x32] = Some('\u{F732}'); // twooldstyle (PUA)
    t[0x33] = Some('\u{F733}'); // threeoldstyle (PUA)
    t[0x34] = Some('\u{F734}'); // fouroldstyle (PUA)
    t[0x35] = Some('\u{F735}'); // fiveoldstyle (PUA)
    t[0x36] = Some('\u{F736}'); // sixoldstyle (PUA)
    t[0x37] = Some('\u{F737}'); // sevenoldstyle (PUA)
    t[0x38] = Some('\u{F738}'); // eightoldstyle (PUA)
    t[0x39] = Some('\u{F739}'); // nineoldstyle (PUA)
    t[0x56] = Some('\u{F726}'); // Vsuperior (PUA)
                                // Fractions
    t[0xAC] = Some('\u{00BC}'); // onequarter
    t[0xAD] = Some('\u{00BD}'); // onehalf
    t[0xAE] = Some('\u{00BE}'); // threequarters
    t[0xB3] = Some('\u{2153}'); // onethird
    t[0xB4] = Some('\u{2154}'); // twothirds
    t[0xBE] = Some('\u{215B}'); // oneeighth
    t[0xBF] = Some('\u{215C}'); // threeeighths
    t[0xC0] = Some('\u{215D}'); // fiveeighths
    t[0xC1] = Some('\u{215E}'); // seveneighths
    t
};

// =============================================================================
// Adobe Glyph List (subset covering common PDF glyph names)
// =============================================================================

/// Adobe Glyph List: maps glyph names to Unicode codepoints.
/// This is a subset covering the ~470 most commonly used names in PDF fonts.
static ADOBE_GLYPH_LIST: phf::Map<&'static str, char> = phf::phf_map! {
    "A" => 'A', "AE" => '\u{00C6}', "Aacute" => '\u{00C1}',
    "Acircumflex" => '\u{00C2}', "Adieresis" => '\u{00C4}',
    "Agrave" => '\u{00C0}', "Aring" => '\u{00C5}', "Atilde" => '\u{00C3}',
    "B" => 'B', "C" => 'C', "Ccedilla" => '\u{00C7}',
    "D" => 'D', "E" => 'E', "Eacute" => '\u{00C9}',
    "Ecircumflex" => '\u{00CA}', "Edieresis" => '\u{00CB}',
    "Egrave" => '\u{00C8}', "Eth" => '\u{00D0}',
    "F" => 'F', "G" => 'G', "H" => 'H', "I" => 'I',
    "Iacute" => '\u{00CD}', "Icircumflex" => '\u{00CE}',
    "Idieresis" => '\u{00CF}', "Igrave" => '\u{00CC}',
    "J" => 'J', "K" => 'K', "L" => 'L', "Lslash" => '\u{0141}',
    "M" => 'M', "N" => 'N', "Ntilde" => '\u{00D1}',
    "O" => 'O', "OE" => '\u{0152}', "Oacute" => '\u{00D3}',
    "Ocircumflex" => '\u{00D4}', "Odieresis" => '\u{00D6}',
    "Ograve" => '\u{00D2}', "Oslash" => '\u{00D8}', "Otilde" => '\u{00D5}',
    "P" => 'P', "Q" => 'Q', "R" => 'R', "S" => 'S',
    "Scaron" => '\u{0160}',
    "T" => 'T', "Thorn" => '\u{00DE}',
    "U" => 'U', "Uacute" => '\u{00DA}', "Ucircumflex" => '\u{00DB}',
    "Udieresis" => '\u{00DC}', "Ugrave" => '\u{00D9}',
    "V" => 'V', "W" => 'W', "X" => 'X', "Y" => 'Y',
    "Ydieresis" => '\u{0178}', "Z" => 'Z', "Zcaron" => '\u{017D}',
    "a" => 'a', "aacute" => '\u{00E1}', "acircumflex" => '\u{00E2}',
    "acute" => '\u{00B4}', "adieresis" => '\u{00E4}',
    "ae" => '\u{00E6}', "agrave" => '\u{00E0}',
    "ampersand" => '&', "aring" => '\u{00E5}',
    "asciicircum" => '^', "asciitilde" => '~',
    "asterisk" => '*', "at" => '@', "atilde" => '\u{00E3}',
    "b" => 'b', "backslash" => '\\', "bar" => '|',
    "braceleft" => '{', "braceright" => '}',
    "bracketleft" => '[', "bracketright" => ']',
    "breve" => '\u{02D8}', "brokenbar" => '\u{00A6}',
    "bullet" => '\u{2022}',
    "c" => 'c', "caron" => '\u{02C7}', "ccedilla" => '\u{00E7}',
    "cedilla" => '\u{00B8}', "cent" => '\u{00A2}',
    "circumflex" => '\u{02C6}', "colon" => ':', "comma" => ',',
    "copyright" => '\u{00A9}', "currency" => '\u{00A4}',
    "d" => 'd', "dagger" => '\u{2020}', "daggerdbl" => '\u{2021}',
    "degree" => '\u{00B0}', "dieresis" => '\u{00A8}',
    "divide" => '\u{00F7}', "dollar" => '$',
    "dotaccent" => '\u{02D9}', "dotlessi" => '\u{0131}',
    "e" => 'e', "eacute" => '\u{00E9}', "ecircumflex" => '\u{00EA}',
    "edieresis" => '\u{00EB}', "egrave" => '\u{00E8}',
    "eight" => '8', "ellipsis" => '\u{2026}',
    "emdash" => '\u{2014}', "endash" => '\u{2013}',
    "equal" => '=', "eth" => '\u{00F0}',
    "exclam" => '!', "exclamdown" => '\u{00A1}',
    "f" => 'f', "fi" => '\u{FB01}', "five" => '5',
    "fl" => '\u{FB02}', "florin" => '\u{0192}',
    "four" => '4', "fraction" => '\u{2044}',
    "g" => 'g', "germandbls" => '\u{00DF}', "grave" => '`',
    "greater" => '>', "guillemotleft" => '\u{00AB}',
    "guillemotright" => '\u{00BB}', "guilsinglleft" => '\u{2039}',
    "guilsinglright" => '\u{203A}',
    "h" => 'h', "hungarumlaut" => '\u{02DD}', "hyphen" => '-',
    "i" => 'i', "iacute" => '\u{00ED}', "icircumflex" => '\u{00EE}',
    "idieresis" => '\u{00EF}', "igrave" => '\u{00EC}',
    "j" => 'j', "k" => 'k',
    "l" => 'l', "less" => '<', "logicalnot" => '\u{00AC}',
    "lslash" => '\u{0142}',
    "m" => 'm', "macron" => '\u{00AF}',
    "minus" => '\u{2212}', "mu" => '\u{00B5}',
    "multiply" => '\u{00D7}',
    "n" => 'n', "nine" => '9', "ntilde" => '\u{00F1}',
    "numbersign" => '#',
    "o" => 'o', "oacute" => '\u{00F3}', "ocircumflex" => '\u{00F4}',
    "odieresis" => '\u{00F6}', "oe" => '\u{0153}',
    "ogonek" => '\u{02DB}', "ograve" => '\u{00F2}',
    "one" => '1', "onehalf" => '\u{00BD}',
    "onequarter" => '\u{00BC}', "onesuperior" => '\u{00B9}',
    "ordfeminine" => '\u{00AA}', "ordmasculine" => '\u{00BA}',
    "oslash" => '\u{00F8}', "otilde" => '\u{00F5}',
    "p" => 'p', "paragraph" => '\u{00B6}', "parenleft" => '(',
    "parenright" => ')', "percent" => '%',
    "period" => '.', "periodcentered" => '\u{00B7}',
    "perthousand" => '\u{2030}', "plus" => '+',
    "plusminus" => '\u{00B1}',
    "q" => 'q', "question" => '?', "questiondown" => '\u{00BF}',
    "quotedbl" => '"', "quotedblbase" => '\u{201E}',
    "quotedblleft" => '\u{201C}', "quotedblright" => '\u{201D}',
    "quoteleft" => '\u{2018}', "quoteright" => '\u{2019}',
    "quotesinglbase" => '\u{201A}', "quotesingle" => '\'',
    "r" => 'r', "registered" => '\u{00AE}', "ring" => '\u{02DA}',
    "s" => 's', "scaron" => '\u{0161}', "section" => '\u{00A7}',
    "semicolon" => ';', "seven" => '7', "six" => '6',
    "slash" => '/', "space" => ' ', "sterling" => '\u{00A3}',
    "t" => 't', "thorn" => '\u{00FE}', "three" => '3',
    "threequarters" => '\u{00BE}', "threesuperior" => '\u{00B3}',
    "tilde" => '\u{02DC}', "trademark" => '\u{2122}',
    "two" => '2', "twosuperior" => '\u{00B2}',
    "u" => 'u', "uacute" => '\u{00FA}', "ucircumflex" => '\u{00FB}',
    "udieresis" => '\u{00FC}', "ugrave" => '\u{00F9}',
    "underscore" => '_',
    "v" => 'v', "w" => 'w', "x" => 'x', "y" => 'y',
    "yacute" => '\u{00FD}', "ydieresis" => '\u{00FF}',
    "yen" => '\u{00A5}', "z" => 'z', "zcaron" => '\u{017E}',
    "zero" => '0',
    // Additional common glyphs
    "Euro" => '\u{20AC}',
    "Delta" => '\u{0394}',
    "Omega" => '\u{2126}',
    "Pi" => '\u{03A0}',
    "Sigma" => '\u{03A3}',
    "alpha" => '\u{03B1}',
    "beta" => '\u{03B2}',
    "gamma" => '\u{03B3}',
    "delta" => '\u{03B4}',
    "epsilon" => '\u{03B5}',
    "theta" => '\u{03B8}',
    "lambda" => '\u{03BB}',
    "pi" => '\u{03C0}',
    "sigma" => '\u{03C3}',
    "tau" => '\u{03C4}',
    "phi" => '\u{03C6}',
    "omega" => '\u{03C9}',
    "infinity" => '\u{221E}',
    "partialdiff" => '\u{2202}',
    "integral" => '\u{222B}',
    "product" => '\u{220F}',
    "summation" => '\u{2211}',
    "radical" => '\u{221A}',
    "approxequal" => '\u{2248}',
    "notequal" => '\u{2260}',
    "lessequal" => '\u{2264}',
    "greaterequal" => '\u{2265}',
    "lozenge" => '\u{25CA}',
    "nbspace" => '\u{00A0}',
    "sfthyphen" => '\u{00AD}',
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;

    // --- WinAnsiEncoding tests ---

    #[test]
    fn win_ansi_ascii_range() {
        let enc = Encoding::win_ansi();
        assert_eq!(enc.decode_byte(0x41), Some('A'));
        assert_eq!(enc.decode_byte(0x7A), Some('z'));
        assert_eq!(enc.decode_byte(0x20), Some(' '));
    }

    #[test]
    fn win_ansi_special_chars() {
        let enc = Encoding::win_ansi();
        assert_eq!(enc.decode_byte(0x80), Some('\u{20AC}')); // Euro
        assert_eq!(enc.decode_byte(0x93), Some('\u{201C}')); // left double quote
        assert_eq!(enc.decode_byte(0x96), Some('\u{2013}')); // en dash
        assert_eq!(enc.decode_byte(0x97), Some('\u{2014}')); // em dash
    }

    #[test]
    fn win_ansi_latin1_supplement() {
        let enc = Encoding::win_ansi();
        assert_eq!(enc.decode_byte(0xC0), Some('\u{00C0}')); // Agrave
        assert_eq!(enc.decode_byte(0xE9), Some('\u{00E9}')); // eacute
        assert_eq!(enc.decode_byte(0xFF), Some('\u{00FF}')); // ydieresis
    }

    #[test]
    fn win_ansi_undefined_codes() {
        let enc = Encoding::win_ansi();
        assert_eq!(enc.decode_byte(0x81), None); // undefined in WinAnsi
        assert_eq!(enc.decode_byte(0x00), None); // control char
    }

    // --- StandardEncoding tests ---

    #[test]
    fn standard_ascii_range() {
        let enc = Encoding::standard();
        assert_eq!(enc.decode_byte(0x41), Some('A'));
        assert_eq!(enc.decode_byte(0x61), Some('a'));
    }

    #[test]
    fn standard_special_chars() {
        let enc = Encoding::standard();
        assert_eq!(enc.decode_byte(0xAE), Some('\u{FB01}')); // fi ligature
        assert_eq!(enc.decode_byte(0xAF), Some('\u{FB02}')); // fl ligature
        assert_eq!(enc.decode_byte(0xD0), Some('\u{2014}')); // emdash
    }

    // --- MacRomanEncoding tests ---

    #[test]
    fn mac_roman_special_chars() {
        let enc = Encoding::mac_roman();
        assert_eq!(enc.decode_byte(0x80), Some('\u{00C4}')); // Adieresis
        assert_eq!(enc.decode_byte(0xCA), Some('\u{00A0}')); // nbspace
        assert_eq!(enc.decode_byte(0xDE), Some('\u{FB01}')); // fi ligature
    }

    // --- Latin-1 fallback encoding ---

    #[test]
    fn latin1_identity() {
        let enc = Encoding::latin1();
        for i in 0u8..=255 {
            let ch = enc.decode_byte(i);
            assert!(ch.is_some(), "code {} should be defined", i);
            assert_eq!(ch.unwrap() as u32, i as u32);
        }
    }

    // --- Differences tests ---

    #[test]
    fn differences_override() {
        let mut enc = Encoding::win_ansi();
        let diffs = vec![
            Object::Integer(65), // start at code 65 (A)
            Object::Name(PdfName::new("Euro")),
        ];
        enc.apply_differences(&diffs).unwrap();
        assert_eq!(enc.decode_byte(65), Some('\u{20AC}')); // Euro instead of A
    }

    #[test]
    fn differences_sequential() {
        let mut enc = Encoding::win_ansi();
        let diffs = vec![
            Object::Integer(65),
            Object::Name(PdfName::new("bullet")),
            Object::Name(PdfName::new("dagger")),
        ];
        enc.apply_differences(&diffs).unwrap();
        assert_eq!(enc.decode_byte(65), Some('\u{2022}')); // bullet
        assert_eq!(enc.decode_byte(66), Some('\u{2020}')); // dagger
    }

    #[test]
    fn differences_multiple_ranges() {
        let mut enc = Encoding::standard();
        let diffs = vec![
            Object::Integer(100),
            Object::Name(PdfName::new("Euro")),
            Object::Integer(200),
            Object::Name(PdfName::new("bullet")),
        ];
        enc.apply_differences(&diffs).unwrap();
        assert_eq!(enc.decode_byte(100), Some('\u{20AC}'));
        assert_eq!(enc.decode_byte(200), Some('\u{2022}'));
    }

    // --- Glyph name lookup tests ---

    #[test]
    fn glyph_name_basic() {
        assert_eq!(glyph_name_to_unicode("A"), Some('A'));
        assert_eq!(glyph_name_to_unicode("space"), Some(' '));
        assert_eq!(glyph_name_to_unicode("eacute"), Some('\u{00E9}'));
        assert_eq!(glyph_name_to_unicode("Euro"), Some('\u{20AC}'));
    }

    #[test]
    fn glyph_name_uni_convention() {
        assert_eq!(glyph_name_to_unicode("uni0041"), Some('A'));
        assert_eq!(glyph_name_to_unicode("uni20AC"), Some('\u{20AC}'));
    }

    #[test]
    fn glyph_name_u_convention() {
        assert_eq!(glyph_name_to_unicode("u0041"), Some('A'));
        assert_eq!(glyph_name_to_unicode("u20AC"), Some('\u{20AC}'));
    }

    #[test]
    fn glyph_name_unknown() {
        assert_eq!(glyph_name_to_unicode("nonexistentglyph"), None);
    }

    // --- from_object tests ---

    #[test]
    fn encoding_from_name_object() {
        let obj = Object::Name(PdfName::new("WinAnsiEncoding"));
        let enc = Encoding::from_object(&obj).unwrap();
        assert_eq!(enc.decode_byte(0x41), Some('A'));
        assert_eq!(enc.decode_byte(0x80), Some('\u{20AC}'));
    }

    #[test]
    fn encoding_from_dict_with_differences() {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("BaseEncoding"),
            Object::Name(PdfName::new("WinAnsiEncoding")),
        );
        dict.insert(
            PdfName::new("Differences"),
            Object::Array(vec![
                Object::Integer(32),
                Object::Name(PdfName::new("bullet")),
            ]),
        );
        let obj = Object::Dictionary(dict);
        let enc = Encoding::from_object(&obj).unwrap();
        assert_eq!(enc.decode_byte(32), Some('\u{2022}')); // bullet
        assert_eq!(enc.decode_byte(0x41), Some('A')); // unchanged
    }

    #[test]
    fn decode_bytes_string() {
        let enc = Encoding::win_ansi();
        let result = enc.decode_bytes(&[0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        assert_eq!(result, "Hello");
    }
}
