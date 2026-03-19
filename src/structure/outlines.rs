//! PDF outline (bookmark) tree traversal.
//!
//! Outlines provide a hierarchical table of contents for PDF documents.
//! ISO 32000-2:2020, Section 12.3.3.

use crate::core::objects::{DictExt, Dictionary, Object};

/// Outline item style flag: italic text.
const FLAG_ITALIC: u32 = 1;
/// Outline item style flag: bold text.
const FLAG_BOLD: u32 = 2;

/// A node in the PDF outline (bookmark) tree.
///
/// Captures title, destination, action, style, and children per
/// ISO 32000-2:2020, Section 12.3.3 (Table 153).
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
    /// The title text displayed for this bookmark.
    pub title: String,
    /// The destination page number (0-based), if resolved from `/Dest` or GoTo `/D`.
    pub page: Option<usize>,
    /// Child outline entries (sub-bookmarks).
    pub children: Vec<Outline>,
    /// URI for outline items with a URI action (`/A << /S /URI /URI (...) >>`).
    pub uri: Option<String>,
    /// Action type name (e.g., "GoTo", "URI", "GoToR") from `/A << /S ... >>`.
    pub action_type: Option<String>,
    /// Text color as `[R, G, B]` in range 0.0–1.0, from `/C`.
    pub color: Option<[f64; 3]>,
    /// Style flags bitfield from `/F` (bit 0 = italic, bit 1 = bold).
    pub flags: u32,
}

impl Outline {
    /// Returns `true` if this outline item should be displayed in italic.
    pub fn is_italic(&self) -> bool {
        self.flags & FLAG_ITALIC != 0
    }

    /// Returns `true` if this outline item should be displayed in bold.
    pub fn is_bold(&self) -> bool {
        self.flags & FLAG_BOLD != 0
    }

    /// Maximum recursion depth for outline tree traversal.
    const MAX_DEPTH: usize = 32;

    /// Builds an outline tree from the `/Outlines` dictionary.
    ///
    /// The `resolve` function should resolve indirect references to their
    /// target objects.
    pub fn from_outlines_dict<'d, R>(dict: &Dictionary, resolve: &R) -> Vec<Outline>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        // Get /First child
        let first = match dict.get_str("First") {
            Some(obj) => obj,
            None => return Vec::new(),
        };

        let first_dict = match resolve(first).and_then(|o| o.as_dict()) {
            Some(d) => d,
            None => return Vec::new(),
        };

        Self::collect_siblings(first, first_dict, resolve, Self::MAX_DEPTH)
    }

    /// Collects a linked list of sibling outline items starting from
    /// the given first item.
    fn collect_siblings<'d, R>(
        _first_ref: &Object,
        first_dict: &'d Dictionary,
        resolve: &R,
        depth: usize,
    ) -> Vec<Outline>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        if depth == 0 {
            return Vec::new();
        }
        let mut outlines = Vec::new();
        let mut current_dict = first_dict;
        // Safety limit to prevent infinite loops
        let mut count = 0;
        const MAX_SIBLINGS: usize = 10_000;

        loop {
            if count >= MAX_SIBLINGS {
                break;
            }
            count += 1;

            let title = current_dict.get_text("Title").unwrap_or_default();
            let flags = current_dict.get_i64("F").unwrap_or(0) as u32;

            // Extract /C color array (3 RGB floats)
            let color = current_dict.get_str("C").and_then(|obj| {
                let arr = obj.as_array()?;
                if arr.len() >= 3 {
                    Some([
                        arr[0].as_f64().unwrap_or(0.0),
                        arr[1].as_f64().unwrap_or(0.0),
                        arr[2].as_f64().unwrap_or(0.0),
                    ])
                } else {
                    None
                }
            });

            // Extract action (/A dictionary)
            let action = current_dict.get_str("A").and_then(|o| o.as_dict());
            let action_type = action.and_then(|a| a.get_name("S")).map(String::from);

            // Extract URI from /A << /S /URI /URI (...) >>
            let uri = action.and_then(|a| {
                if a.get_name("S") == Some("URI") {
                    a.get_text("URI")
                } else {
                    None
                }
            });

            // Extract destination page from /Dest or GoTo /A /D
            let page = current_dict
                .get_str("Dest")
                .and_then(|o| resolve(o).or(Some(o)))
                .and_then(|o| o.as_array())
                .and_then(|arr| arr.first())
                .and_then(|o| o.as_i64().map(|n| n as usize))
                .or_else(|| {
                    // GoTo action: /A << /S /GoTo /D [page ...] >>
                    action
                        .filter(|a| a.get_name("S") == Some("GoTo"))
                        .and_then(|a| a.get_str("D"))
                        .and_then(|d| d.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|o| o.as_i64().map(|n| n as usize))
                });

            // Recurse into children
            let children = if let Some(child_ref) = current_dict.get_str("First") {
                match resolve(child_ref).and_then(|o| o.as_dict()) {
                    Some(child_dict) => {
                        Self::collect_siblings(child_ref, child_dict, resolve, depth - 1)
                    }
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };

            outlines.push(Outline {
                title,
                page,
                children,
                uri,
                action_type,
                color,
                flags,
            });

            // Move to next sibling
            match current_dict.get_str("Next") {
                Some(next_ref) => match resolve(next_ref).and_then(|o| o.as_dict()) {
                    Some(next_dict) => current_dict = next_dict,
                    None => break,
                },
                None => break,
            }
        }

        outlines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;
    use crate::test_utils::{make_dict, str_obj};

    #[test]
    fn empty_outlines() {
        let dict = make_dict(vec![]);
        let resolve = |_: &Object| -> Option<&Object> { None };
        let outlines = Outline::from_outlines_dict(&dict, &resolve);
        assert!(outlines.is_empty());
    }

    #[test]
    fn single_outline_item() {
        let item = make_dict(vec![("Title", str_obj("Chapter 1"))]);
        let item_obj = Object::Dictionary(item);

        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };

        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].title, "Chapter 1");
        assert!(outlines[0].children.is_empty());
    }

    #[test]
    fn multiple_siblings() {
        let item3 = make_dict(vec![("Title", str_obj("Chapter 3"))]);
        let item2 = make_dict(vec![
            ("Title", str_obj("Chapter 2")),
            ("Next", Object::Integer(3)),
        ]);
        let item1 = make_dict(vec![
            ("Title", str_obj("Chapter 1")),
            ("Next", Object::Integer(2)),
        ]);

        let obj1 = Object::Dictionary(item1);
        let obj2 = Object::Dictionary(item2);
        let obj3 = Object::Dictionary(item3);

        let outlines_dict = make_dict(vec![("First", Object::Integer(1))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(1) => Some(&obj1),
                Object::Integer(2) => Some(&obj2),
                Object::Integer(3) => Some(&obj3),
                _ => None,
            }
        };

        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines.len(), 3);
        assert_eq!(outlines[0].title, "Chapter 1");
        assert_eq!(outlines[1].title, "Chapter 2");
        assert_eq!(outlines[2].title, "Chapter 3");
    }

    #[test]
    fn nested_children() {
        let child = make_dict(vec![("Title", str_obj("Chapter 1"))]);
        let child_obj = Object::Dictionary(child);

        let parent = make_dict(vec![
            ("Title", str_obj("Part I")),
            ("First", Object::Integer(10)),
        ]);
        let parent_obj = Object::Dictionary(parent);

        let outlines_dict = make_dict(vec![("First", Object::Integer(1))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(1) => Some(&parent_obj),
                Object::Integer(10) => Some(&child_obj),
                _ => None,
            }
        };

        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].title, "Part I");
        assert_eq!(outlines[0].children.len(), 1);
        assert_eq!(outlines[0].children[0].title, "Chapter 1");
    }

    #[test]
    fn depth_limit_prevents_infinite_recursion() {
        let item = make_dict(vec![
            ("Title", str_obj("Loop")),
            ("First", Object::Integer(1)),
        ]);
        let item_obj = Object::Dictionary(item);

        let outlines_dict = make_dict(vec![("First", Object::Integer(1))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(1) => Some(&item_obj),
                _ => None,
            }
        };

        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert!(!outlines.is_empty());
        let mut depth = 0;
        let mut current = &outlines[0];
        while !current.children.is_empty() {
            depth += 1;
            current = &current.children[0];
            if depth > Outline::MAX_DEPTH + 1 {
                panic!("Recursion exceeded MAX_DEPTH");
            }
        }
        assert!(depth <= Outline::MAX_DEPTH);
    }

    #[test]
    fn unresolvable_first_returns_empty() {
        let outlines_dict = make_dict(vec![("First", Object::Integer(99))]);
        let resolve = |_: &Object| -> Option<&Object> { None };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert!(outlines.is_empty());
    }

    #[test]
    fn missing_title_uses_empty_string() {
        let item = make_dict(vec![]);
        let item_obj = Object::Dictionary(item);
        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);
        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].title, "");
    }

    #[test]
    fn outline_with_dest_page() {
        let item = make_dict(vec![
            ("Title", str_obj("Page 5")),
            ("Dest", Object::Array(vec![Object::Integer(4)])),
        ]);
        let item_obj = Object::Dictionary(item);
        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);
        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines[0].page, Some(4));
    }

    #[test]
    fn outline_with_uri_action() {
        let action = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("URI"))),
            ("URI", str_obj("https://example.com")),
        ]));
        let item = make_dict(vec![("Title", str_obj("Website")), ("A", action)]);
        let item_obj = Object::Dictionary(item);
        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);
        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert_eq!(outlines[0].uri, Some("https://example.com".to_string()));
    }

    #[test]
    fn outline_with_style_flags() {
        let item = make_dict(vec![
            ("Title", str_obj("Bold Italic")),
            ("F", Object::Integer(3)), // italic (1) + bold (2)
            (
                "C",
                Object::Array(vec![
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                ]),
            ),
        ]);
        let item_obj = Object::Dictionary(item);
        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);
        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        assert!(outlines[0].is_italic());
        assert!(outlines[0].is_bold());
        assert_eq!(outlines[0].color, Some([1.0, 0.0, 0.0]));
    }

    #[test]
    fn outline_with_goto_action() {
        let action = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("GoTo"))),
            (
                "D",
                Object::Array(vec![Object::Integer(7), Object::Name(PdfName::new("Fit"))]),
            ),
        ]));
        let item = make_dict(vec![("Title", str_obj("Chapter 3")), ("A", action)]);
        let item_obj = Object::Dictionary(item);
        let outlines_dict = make_dict(vec![("First", Object::Integer(0))]);
        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(0)) {
                Some(&item_obj)
            } else {
                None
            }
        };
        let outlines = Outline::from_outlines_dict(&outlines_dict, &resolve);
        // GoTo action should be stored; page extracted from /D array
        assert_eq!(outlines[0].action_type, Some("GoTo".to_string()));
    }
}
