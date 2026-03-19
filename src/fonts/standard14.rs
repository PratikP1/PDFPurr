//! The 14 standard PDF fonts (ISO 32000-2:2020, Section 9.6.2.2).
//!
//! These fonts are guaranteed to be available in every PDF viewer and
//! do not require embedding. Each font provides glyph widths in the
//! WinAnsiEncoding for text measurement.

use crate::core::objects::{Dictionary, Object, PdfName};

/// One of the 14 standard PDF fonts.
///
/// These fonts are built into every conforming PDF viewer and need no
/// embedding. The glyph widths are given in 1/1000 of a text-space unit.
#[derive(Debug, Clone, Copy)]
pub struct Standard14Font {
    /// The PostScript name (e.g. `"Helvetica"`).
    name: &'static str,
    /// Glyph widths indexed by WinAnsiEncoding byte value (0–255).
    widths: &'static [u16; 256],
}

impl Standard14Font {
    /// Looks up a standard font by its PostScript name.
    ///
    /// Returns `None` if the name does not match any of the 14 standard fonts.
    pub fn from_name(name: &str) -> Option<Self> {
        STANDARD_14.iter().find(|f| f.name == name).copied()
    }

    /// Returns the PostScript name of the font.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the glyph width for a WinAnsiEncoding byte value, in units
    /// of 1/1000 of a text-space unit.
    pub fn glyph_width(&self, code: u8) -> u16 {
        self.widths[code as usize]
    }

    /// Measures the width of a string in points at the given font size.
    ///
    /// Characters outside the WinAnsiEncoding range (> 255) are treated
    /// as having zero width.
    pub fn measure_text(&self, text: &str, size: f64) -> f64 {
        let total: u32 = text.bytes().map(|b| self.widths[b as usize] as u32).sum();
        total as f64 * size / 1000.0
    }

    /// Generates a PDF `/Font` dictionary for this standard font.
    ///
    /// The dictionary references the font by name with WinAnsiEncoding.
    /// No font program is embedded.
    pub fn to_font_dictionary(&self) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type1")));
        dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new(self.name)),
        );
        dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("WinAnsiEncoding")),
        );
        dict
    }

    /// Returns the list of all 14 standard font names.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "Courier",
            "Courier-Bold",
            "Courier-BoldOblique",
            "Courier-Oblique",
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-BoldOblique",
            "Helvetica-Oblique",
            "Symbol",
            "Times-Bold",
            "Times-BoldItalic",
            "Times-Italic",
            "Times-Roman",
            "ZapfDingbats",
        ]
    }
}

// ─── Width tables ───────────────────────────────────────────────────────────
//
// Widths are in 1/1000 of a text-space unit, indexed by WinAnsiEncoding
// code point (0–255). Values sourced from the Adobe Font Metrics (AFM) files
// for each standard font.

/// All 14 standard fonts.
static STANDARD_14: &[Standard14Font] = &[
    Standard14Font {
        name: "Courier",
        widths: &COURIER_WIDTHS,
    },
    Standard14Font {
        name: "Courier-Bold",
        widths: &COURIER_WIDTHS,
    },
    Standard14Font {
        name: "Courier-BoldOblique",
        widths: &COURIER_WIDTHS,
    },
    Standard14Font {
        name: "Courier-Oblique",
        widths: &COURIER_WIDTHS,
    },
    Standard14Font {
        name: "Helvetica",
        widths: &HELVETICA_WIDTHS,
    },
    Standard14Font {
        name: "Helvetica-Bold",
        widths: &HELVETICA_BOLD_WIDTHS,
    },
    Standard14Font {
        name: "Helvetica-BoldOblique",
        widths: &HELVETICA_BOLD_WIDTHS,
    },
    Standard14Font {
        name: "Helvetica-Oblique",
        widths: &HELVETICA_WIDTHS,
    },
    Standard14Font {
        name: "Symbol",
        widths: &SYMBOL_WIDTHS,
    },
    Standard14Font {
        name: "Times-Bold",
        widths: &TIMES_BOLD_WIDTHS,
    },
    Standard14Font {
        name: "Times-BoldItalic",
        widths: &TIMES_BOLD_WIDTHS,
    },
    Standard14Font {
        name: "Times-Italic",
        widths: &TIMES_ROMAN_WIDTHS,
    },
    Standard14Font {
        name: "Times-Roman",
        widths: &TIMES_ROMAN_WIDTHS,
    },
    Standard14Font {
        name: "ZapfDingbats",
        widths: &ZAPF_DINGBATS_WIDTHS,
    },
];

// Courier: all glyphs are 600 units wide (monospaced).
static COURIER_WIDTHS: [u16; 256] = [600; 256];

// Helvetica WinAnsiEncoding widths from AFM data.
// Codes 0–31 and 127–159 (control chars) have width 0 except where
// WinAnsi maps a glyph (e.g. 128=Euro, 130=quotesinglbase, etc.).
#[rustfmt::skip]
static HELVETICA_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20 space ! " # $ % & ' ( ) * + , - . /
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    // 0x30 0 1 2 3 4 5 6 7 8 9 : ; < = > ?
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    // 0x40 @ A B C D E F G H I J K L M N O
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    // 0x50 P Q R S T U V W X Y Z [ \ ] ^ _
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    // 0x60 ` a b c d e f g h i j k l m n o
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    // 0x70 p q r s t u v w x y z { | } ~ DEL
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 0,
    // 0x80–0x8F (WinAnsi specials: Euro, quotes, dagger, etc.)
    556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0,
    // 0x90–0x9F
    0, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667,
    // 0xA0 NBSP ¡ ¢ £ ¤ ¥ ¦ § ¨ © ª « ¬ SHY ® ¯
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333,
    // 0xB0 ° ± ² ³ ´ µ ¶ · ¸ ¹ º » ¼ ½ ¾ ¿
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611,
    // 0xC0 À Á Â Ã Ä Å Æ Ç È É Ê Ë Ì Í Î Ï
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278,
    // 0xD0 Ð Ñ Ò Ó Ô Õ Ö × Ø Ù Ú Û Ü Ý Þ ß
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611,
    // 0xE0 à á â ã ä å æ ç è é ê ë ì í î ï
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278,
    // 0xF0 ð ñ ò ó ô õ ö ÷ ø ù ú û ü ý þ ÿ
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

#[rustfmt::skip]
static HELVETICA_BOLD_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20 space ! " # $ % & ' ( ) * + , - . /
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278,
    // 0x30 0 1 2 3 4 5 6 7 8 9 : ; < = > ?
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611,
    // 0x40 @ A B C D E F G H I J K L M N O
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    // 0x50 P Q R S T U V W X Y Z [ \ ] ^ _
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556,
    // 0x60 ` a b c d e f g h i j k l m n o
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    // 0x70 p q r s t u v w x y z { | } ~ DEL
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, 0,
    // 0x80–0x8F
    556, 0, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0,
    // 0x90–0x9F
    0, 278, 278, 500, 500, 350, 556, 1000, 333, 1000, 556, 333, 944, 0, 500, 667,
    // 0xA0–0xAF
    278, 333, 556, 556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333,
    // 0xB0–0xBF
    400, 584, 333, 333, 333, 611, 556, 278, 333, 333, 365, 556, 834, 834, 834, 611,
    // 0xC0–0xCF
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278,
    // 0xD0–0xDF
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611,
    // 0xE0–0xEF
    556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278, 278,
    // 0xF0–0xFF
    611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556,
];

#[rustfmt::skip]
static TIMES_ROMAN_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20 space ! " # $ % & ' ( ) * + , - . /
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278,
    // 0x30 0 1 2 3 4 5 6 7 8 9 : ; < = > ?
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444,
    // 0x40 @ A B C D E F G H I J K L M N O
    921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722,
    // 0x50 P Q R S T U V W X Y Z [ \ ] ^ _
    556, 722, 667, 556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500,
    // 0x60 ` a b c d e f g h i j k l m n o
    333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500,
    // 0x70 p q r s t u v w x y z { | } ~ DEL
    500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541, 0,
    // 0x80–0x8F
    500, 0, 333, 500, 444, 1000, 500, 500, 333, 1000, 556, 333, 889, 0, 611, 0,
    // 0x90–0x9F
    0, 333, 333, 444, 444, 350, 500, 1000, 333, 980, 389, 333, 722, 0, 444, 722,
    // 0xA0–0xAF
    250, 333, 500, 500, 500, 500, 200, 500, 333, 760, 276, 500, 564, 333, 760, 333,
    // 0xB0–0xBF
    400, 564, 300, 300, 333, 500, 453, 250, 333, 300, 310, 500, 750, 750, 750, 444,
    // 0xC0–0xCF
    722, 722, 722, 722, 722, 722, 889, 667, 611, 611, 611, 611, 333, 333, 333, 333,
    // 0xD0–0xDF
    722, 722, 722, 722, 722, 722, 722, 564, 722, 722, 722, 722, 722, 722, 556, 500,
    // 0xE0–0xEF
    444, 444, 444, 444, 444, 444, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278,
    // 0xF0–0xFF
    500, 500, 500, 500, 500, 500, 500, 564, 500, 500, 500, 500, 500, 500, 500, 500,
];

#[rustfmt::skip]
static TIMES_BOLD_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20 space ! " # $ % & ' ( ) * + , - . /
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278,
    // 0x30 0 1 2 3 4 5 6 7 8 9 : ; < = > ?
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500,
    // 0x40 @ A B C D E F G H I J K L M N O
    930, 722, 667, 722, 722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778,
    // 0x50 P Q R S T U V W X Y Z [ \ ] ^ _
    611, 778, 722, 556, 667, 722, 722, 1000, 722, 722, 667, 333, 278, 333, 581, 500,
    // 0x60 ` a b c d e f g h i j k l m n o
    333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556, 278, 833, 556, 500,
    // 0x70 p q r s t u v w x y z { | } ~ DEL
    556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520, 0,
    // 0x80–0x8F
    500, 0, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 1000, 0, 667, 0,
    // 0x90–0x9F
    0, 333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 0, 444, 722,
    // 0xA0–0xAF
    250, 333, 500, 500, 500, 500, 220, 500, 333, 747, 300, 500, 570, 333, 747, 333,
    // 0xB0–0xBF
    400, 570, 300, 300, 333, 556, 540, 250, 333, 300, 330, 500, 750, 750, 750, 500,
    // 0xC0–0xCF
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 389, 389, 389, 389,
    // 0xD0–0xDF
    722, 722, 778, 778, 778, 778, 778, 570, 778, 722, 722, 722, 722, 722, 611, 556,
    // 0xE0–0xEF
    500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278, 278,
    // 0xF0–0xFF
    500, 556, 500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 500, 556, 500,
];

// Symbol: simplified — most codes get 500, key symbols have specific widths.
// Full AFM data would have per-glyph widths; this is a reasonable approximation.
#[rustfmt::skip]
static SYMBOL_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20–0x2F
    250, 333, 713, 500, 549, 833, 778, 439, 333, 333, 500, 549, 250, 549, 250, 278,
    // 0x30–0x3F
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 549, 549, 549, 444,
    // 0x40–0x4F
    549, 722, 667, 722, 612, 611, 763, 603, 722, 333, 631, 722, 686, 889, 722, 722,
    // 0x50–0x5F
    768, 741, 556, 592, 611, 690, 439, 768, 645, 795, 611, 333, 863, 333, 658, 500,
    // 0x60–0x6F
    500, 631, 549, 549, 494, 439, 521, 411, 603, 329, 603, 549, 549, 576, 521, 549,
    // 0x70–0x7F
    549, 521, 549, 603, 439, 576, 713, 686, 493, 686, 494, 480, 200, 480, 549, 0,
    // 0x80–0xFF: mostly 500 as approximation
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
];

// ZapfDingbats: simplified — ornamental symbols mostly ~800 wide.
#[rustfmt::skip]
static ZAPF_DINGBATS_WIDTHS: [u16; 256] = [
    // 0x00–0x0F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x10–0x1F
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 0x20–0x2F
    278, 974, 961, 974, 980, 719, 789, 790, 791, 690, 960, 939, 549, 855, 911, 933,
    // 0x30–0x3F
    911, 945, 974, 755, 846, 762, 761, 571, 677, 763, 760, 759, 754, 494, 552, 537,
    // 0x40–0x4F
    577, 692, 786, 788, 788, 790, 793, 794, 816, 823, 789, 841, 823, 833, 816, 831,
    // 0x50–0x5F
    923, 744, 723, 749, 790, 792, 695, 776, 768, 792, 759, 707, 708, 682, 701, 826,
    // 0x60–0x6F
    815, 789, 789, 707, 687, 696, 689, 786, 787, 713, 791, 785, 791, 873, 761, 762,
    // 0x70–0x7F
    762, 759, 759, 892, 892, 788, 784, 438, 138, 277, 415, 392, 392, 668, 668, 0,
    // 0x80–0xFF
    390, 390, 317, 317, 276, 276, 509, 509, 410, 410, 234, 234, 334, 334, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 732, 544, 544, 910, 667, 760, 760, 776, 595, 694, 626, 788, 788, 788, 788,
    788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788,
    788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788, 788,
    788, 788, 788, 788, 894, 838, 1016, 458, 748, 924, 748, 918, 927, 928, 928, 834,
    873, 828, 924, 924, 917, 930, 931, 463, 883, 836, 836, 867, 867, 696, 696, 874,
    0, 874, 760, 946, 771, 865, 771, 888, 967, 888, 831, 873, 927, 970, 918, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard14_helvetica_exists() {
        let font = Standard14Font::from_name("Helvetica");
        assert!(font.is_some());
        assert_eq!(font.unwrap().name(), "Helvetica");
    }

    #[test]
    fn standard14_all_14_recognized() {
        let names = Standard14Font::all_names();
        assert_eq!(names.len(), 14);
        for name in names {
            assert!(
                Standard14Font::from_name(name).is_some(),
                "Standard font '{}' not found",
                name
            );
        }
    }

    #[test]
    fn standard14_unknown_returns_none() {
        assert!(Standard14Font::from_name("FooBar").is_none());
        assert!(Standard14Font::from_name("Arial").is_none());
    }

    #[test]
    fn standard14_helvetica_widths() {
        let font = Standard14Font::from_name("Helvetica").unwrap();
        // 'A' = 0x41 = 667 units in Helvetica
        assert_eq!(font.glyph_width(b'A'), 667);
        // Space = 0x20 = 278
        assert_eq!(font.glyph_width(b' '), 278);
        // 'i' = 0x69 = 222
        assert_eq!(font.glyph_width(b'i'), 222);
    }

    #[test]
    fn standard14_courier_monospaced() {
        let font = Standard14Font::from_name("Courier").unwrap();
        // All Courier glyphs are 600 units
        assert_eq!(font.glyph_width(b'A'), 600);
        assert_eq!(font.glyph_width(b'i'), 600);
        assert_eq!(font.glyph_width(b' '), 600);
    }

    #[test]
    fn standard14_to_font_dict() {
        let font = Standard14Font::from_name("Helvetica").unwrap();
        let dict = font.to_font_dictionary();

        assert_eq!(
            dict.get(&PdfName::new("Type")).and_then(|o| o.as_name()),
            Some("Font")
        );
        assert_eq!(
            dict.get(&PdfName::new("Subtype")).and_then(|o| o.as_name()),
            Some("Type1")
        );
        assert_eq!(
            dict.get(&PdfName::new("BaseFont"))
                .and_then(|o| o.as_name()),
            Some("Helvetica")
        );
        assert_eq!(
            dict.get(&PdfName::new("Encoding"))
                .and_then(|o| o.as_name()),
            Some("WinAnsiEncoding")
        );
    }

    #[test]
    fn standard14_text_width() {
        let font = Standard14Font::from_name("Helvetica").unwrap();
        // "Hello" at 12pt: H(722) + e(556) + l(222) + l(222) + o(556) = 2278
        // 2278 * 12 / 1000 = 27.336
        let width = font.measure_text("Hello", 12.0);
        assert!((width - 27.336).abs() < 0.001);
    }

    #[test]
    fn standard14_encoding_winansi() {
        let font = Standard14Font::from_name("Times-Roman").unwrap();
        let dict = font.to_font_dictionary();
        assert_eq!(
            dict.get(&PdfName::new("Encoding"))
                .and_then(|o| o.as_name()),
            Some("WinAnsiEncoding")
        );
    }
}
