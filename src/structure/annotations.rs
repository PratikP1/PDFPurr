//! PDF annotation extraction.
//!
//! Extracts annotations from PDF pages (links, text notes, highlights, etc.).
//! ISO 32000-2:2020, Section 12.5.

use crate::core::objects::{DictExt, Dictionary, Object};

/// Annotation flag bits (ISO 32000-2:2020, Table 167).
#[allow(dead_code)]
pub mod flags {
    /// The annotation is not visible on screen.
    pub const INVISIBLE: u32 = 1;
    /// The annotation is hidden from view and printing.
    pub const HIDDEN: u32 = 1 << 1;
    /// Print the annotation when the page is printed.
    pub const PRINT: u32 = 1 << 2;
    /// Do not zoom the annotation with the page.
    pub const NO_ZOOM: u32 = 1 << 3;
    /// Do not rotate the annotation with the page.
    pub const NO_ROTATE: u32 = 1 << 4;
    /// Do not display or print the annotation.
    pub const NO_VIEW: u32 = 1 << 5;
    /// Do not allow user interaction.
    pub const READ_ONLY: u32 = 1 << 6;
    /// Do not allow deletion or property modification.
    pub const LOCKED: u32 = 1 << 7;
    /// Invert the interpretation of the NoView flag for certain events.
    pub const TOGGLE_NO_VIEW: u32 = 1 << 8;
    /// Do not allow content modification.
    pub const LOCKED_CONTENTS: u32 = 1 << 9;
}

/// A PDF annotation extracted from a page.
///
/// Captures the common properties shared by all annotation subtypes
/// (ISO 32000-2:2020, Table 166) plus subtype-specific fields for
/// Link, Markup, and Text annotations.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// The annotation subtype (e.g., "Link", "Text", "Highlight", "FreeText").
    pub subtype: String,
    /// The annotation rectangle [x1, y1, x2, y2] in page coordinates.
    pub rect: [f64; 4],
    /// The annotation's `/Contents` text, if present.
    pub contents: Option<String>,
    /// Annotation flags bitfield (`/F`). Bit 1 = Hidden, bit 2 = Print, etc.
    pub flags: u32,
    /// Color components (`/C`), typically 0, 1, 3, or 4 values (gray, RGB, CMYK).
    pub color: Option<Vec<f64>>,
    /// Author / title (`/T`) — who created the annotation.
    pub author: Option<String>,
    /// Modification date (`/M`) as a PDF date string.
    pub modified_date: Option<String>,
    /// URI for Link annotations with a URI action (`/A << /S /URI /URI (...) >>`).
    pub uri: Option<String>,
    /// QuadPoints for markup annotations (Highlight, Underline, StrikeOut, Squiggly).
    /// Each group of 8 values defines the four corners of a highlighted quad.
    pub quad_points: Vec<f64>,
}

impl Annotation {
    /// Returns `true` if the Hidden flag is set.
    pub fn is_hidden(&self) -> bool {
        self.flags & flags::HIDDEN != 0
    }

    /// Returns `true` if the Print flag is set.
    pub fn is_printable(&self) -> bool {
        self.flags & flags::PRINT != 0
    }

    /// Returns `true` if the ReadOnly flag is set.
    pub fn is_read_only(&self) -> bool {
        self.flags & flags::READ_ONLY != 0
    }

    /// Extracts an annotation from its dictionary.
    pub fn from_dict(dict: &Dictionary) -> Option<Self> {
        let subtype = dict.get_name("Subtype")?.to_string();
        let rect = dict.get_str("Rect")?.parse_rect()?;
        let contents = dict.get_text("Contents");
        let flags = dict.get_i64("F").unwrap_or(0) as u32;
        let author = dict.get_text("T");
        let modified_date = dict.get_text("M");

        let color = dict.get_str("C").and_then(|obj| {
            let arr = obj.as_array()?;
            let values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
            if values.is_empty() {
                None
            } else {
                Some(values)
            }
        });

        // Link annotation: extract URI from the /A action dictionary
        let uri = if subtype == "Link" {
            dict.get_str("A").and_then(|a| {
                let action = a.as_dict()?;
                if action.get_name("S") == Some("URI") {
                    action.get_text("URI")
                } else {
                    None
                }
            })
        } else {
            None
        };

        // Markup annotations: extract QuadPoints
        let quad_points = dict
            .get_str("QuadPoints")
            .and_then(|obj| obj.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();

        Some(Annotation {
            subtype,
            rect,
            contents,
            flags,
            color,
            author,
            modified_date,
            uri,
            quad_points,
        })
    }

    /// Extracts all annotations from a page's `/Annots` array.
    pub fn from_page<'d, R>(page_dict: &Dictionary, resolve: &R) -> Vec<Annotation>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        let annots_obj = match page_dict.get_str("Annots") {
            Some(obj) => obj,
            None => return Vec::new(),
        };

        // Resolve the /Annots value (may be an indirect reference)
        let annots_obj = resolve(annots_obj).unwrap_or(annots_obj);

        let annots_arr = match annots_obj.as_array() {
            Some(arr) => arr,
            None => return Vec::new(),
        };

        annots_arr
            .iter()
            .filter_map(|annot_ref| {
                let annot_obj = resolve(annot_ref).unwrap_or(annot_ref);
                let annot_dict = annot_obj.as_dict()?;
                Annotation::from_dict(annot_dict)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{PdfName, PdfString, StringFormat};
    use crate::test_utils::make_dict;

    #[test]
    fn link_annotation_extracts_uri() {
        let action = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("URI"))),
            (
                "URI",
                Object::String(PdfString {
                    bytes: b"https://example.com".to_vec(),
                    format: StringFormat::Literal,
                }),
            ),
        ]));
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Link"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Real(72.0),
                    Object::Real(700.0),
                    Object::Real(200.0),
                    Object::Real(720.0),
                ]),
            ),
            ("A", action),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.subtype, "Link");
        assert_eq!(annot.uri, Some("https://example.com".to_string()));
    }

    #[test]
    fn annotation_extracts_flags() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Text"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(50),
                    Object::Integer(50),
                ]),
            ),
            ("F", Object::Integer(2)), // Hidden flag (bit 1)
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.flags, 2);
        assert!(annot.is_hidden());
    }

    #[test]
    fn annotation_extracts_color() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Highlight"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(10),
                    Object::Integer(20),
                    Object::Integer(100),
                    Object::Integer(40),
                ]),
            ),
            (
                "C",
                Object::Array(vec![
                    Object::Real(1.0),
                    Object::Real(1.0),
                    Object::Real(0.0),
                ]),
            ),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.color, Some(vec![1.0, 1.0, 0.0]));
    }

    #[test]
    fn annotation_extracts_author_and_date() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Text"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(50),
                    Object::Integer(50),
                ]),
            ),
            (
                "T",
                Object::String(PdfString {
                    bytes: b"John Doe".to_vec(),
                    format: StringFormat::Literal,
                }),
            ),
            (
                "M",
                Object::String(PdfString {
                    bytes: b"D:20260315120000Z".to_vec(),
                    format: StringFormat::Literal,
                }),
            ),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.author, Some("John Doe".to_string()));
        assert_eq!(annot.modified_date, Some("D:20260315120000Z".to_string()));
    }

    #[test]
    fn highlight_annotation_extracts_quad_points() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Highlight"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(72),
                    Object::Integer(700),
                    Object::Integer(200),
                    Object::Integer(720),
                ]),
            ),
            (
                "QuadPoints",
                Object::Array(vec![
                    Object::Real(72.0),
                    Object::Real(720.0),
                    Object::Real(200.0),
                    Object::Real(720.0),
                    Object::Real(72.0),
                    Object::Real(700.0),
                    Object::Real(200.0),
                    Object::Real(700.0),
                ]),
            ),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.quad_points.len(), 8);
        assert_eq!(annot.quad_points[0], 72.0);
    }

    #[test]
    fn annotation_from_dict() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Link"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Real(72.0),
                    Object::Real(700.0),
                    Object::Real(200.0),
                    Object::Real(720.0),
                ]),
            ),
            (
                "Contents",
                Object::String(PdfString {
                    bytes: b"Click here".to_vec(),
                    format: StringFormat::Literal,
                }),
            ),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.subtype, "Link");
        assert_eq!(annot.rect, [72.0, 700.0, 200.0, 720.0]);
        assert_eq!(annot.contents, Some("Click here".to_string()));
    }

    #[test]
    fn annotation_missing_subtype() {
        let dict = make_dict(vec![(
            "Rect",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        )]);
        assert!(Annotation::from_dict(&dict).is_none());
    }

    #[test]
    fn annotation_no_contents() {
        let dict = make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Text"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(50),
                    Object::Integer(50),
                ]),
            ),
        ]);

        let annot = Annotation::from_dict(&dict).unwrap();
        assert_eq!(annot.subtype, "Text");
        assert!(annot.contents.is_none());
    }

    #[test]
    fn annotations_from_page() {
        let annot_dict = Object::Dictionary(make_dict(vec![
            ("Subtype", Object::Name(PdfName::new("Highlight"))),
            (
                "Rect",
                Object::Array(vec![
                    Object::Integer(10),
                    Object::Integer(20),
                    Object::Integer(30),
                    Object::Integer(40),
                ]),
            ),
        ]));

        let page = make_dict(vec![("Annots", Object::Array(vec![Object::Integer(99)]))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(99)) {
                Some(&annot_dict)
            } else {
                None
            }
        };

        let annots = Annotation::from_page(&page, &resolve);
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].subtype, "Highlight");
    }

    #[test]
    fn annotations_from_page_empty() {
        let page = make_dict(vec![]);
        let resolve = |_: &Object| -> Option<&Object> { None };
        let annots = Annotation::from_page(&page, &resolve);
        assert!(annots.is_empty());
    }
}
