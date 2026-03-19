//! PDF structure tree parsing.
//!
//! Parses the document structure tree (`/StructTreeRoot`) which provides
//! logical structure for tagged PDFs. ISO 32000-2:2020, Section 14.7.

use std::collections::HashMap;

use crate::core::objects::{DictExt, Dictionary, Object};

/// Standard PDF structure element roles (ISO 32000-2:2020, Table 368).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StandardRole {
    /// Document root.
    Document,
    /// Part of a document.
    Part,
    /// Article.
    Art,
    /// Section.
    Sect,
    /// Division (generic block-level grouping).
    Div,
    /// Block quotation.
    BlockQuote,
    /// Caption for a figure or table.
    Caption,
    /// Table of contents.
    TOC,
    /// Table of contents item.
    TOCI,
    /// Index.
    Index,
    /// Paragraph.
    P,
    /// Heading (level 1–6).
    H(u8),
    /// Generic heading (unnumbered).
    HGroup,
    /// List.
    L,
    /// List item.
    LI,
    /// Label (list bullet/number).
    Lbl,
    /// List body.
    LBody,
    /// Table.
    Table,
    /// Table row.
    TR,
    /// Table header cell.
    TH,
    /// Table data cell.
    TD,
    /// Table header row group.
    THead,
    /// Table body row group.
    TBody,
    /// Table footer row group.
    TFoot,
    /// Span (inline content).
    Span,
    /// Link.
    Link,
    /// Annotation reference.
    Annot,
    /// Figure.
    Figure,
    /// Formula.
    Formula,
    /// Form widget.
    Form,
    /// Ruby annotation.
    Ruby,
    // --- PDF/UA-2 (ISO 14289-2:2024) additions ---
    /// Partial document (PDF 2.0 / PDF/UA-2).
    DocumentFragment,
    /// Footnote or endnote (PDF/UA-2, replaces Note).
    FENote,
    /// Note (PDF/UA-1, retained for backward compatibility).
    Note,
    /// Emphasis (inline, PDF/UA-2).
    Em,
    /// Strong emphasis (inline, PDF/UA-2).
    Strong,
    /// Sidebar content (PDF/UA-2).
    Aside,
    /// Document title element (PDF/UA-2).
    Title,
    /// Subscript (PDF/UA-2).
    Sub,
    /// Non-standard or unknown role.
    NonStandard(String),
}

impl StandardRole {
    /// Parses a structure element type name into a `StandardRole`.
    pub fn from_name(name: &str) -> Self {
        match name {
            "Document" => StandardRole::Document,
            "Part" => StandardRole::Part,
            "Art" => StandardRole::Art,
            "Sect" => StandardRole::Sect,
            "Div" => StandardRole::Div,
            "BlockQuote" => StandardRole::BlockQuote,
            "Caption" => StandardRole::Caption,
            "TOC" => StandardRole::TOC,
            "TOCI" => StandardRole::TOCI,
            "Index" => StandardRole::Index,
            "P" => StandardRole::P,
            "H" => StandardRole::H(0),
            "H1" => StandardRole::H(1),
            "H2" => StandardRole::H(2),
            "H3" => StandardRole::H(3),
            "H4" => StandardRole::H(4),
            "H5" => StandardRole::H(5),
            "H6" => StandardRole::H(6),
            "L" => StandardRole::L,
            "LI" => StandardRole::LI,
            "Lbl" => StandardRole::Lbl,
            "LBody" => StandardRole::LBody,
            "Table" => StandardRole::Table,
            "TR" => StandardRole::TR,
            "TH" => StandardRole::TH,
            "TD" => StandardRole::TD,
            "THead" => StandardRole::THead,
            "TBody" => StandardRole::TBody,
            "TFoot" => StandardRole::TFoot,
            "Span" => StandardRole::Span,
            "Link" => StandardRole::Link,
            "Annot" => StandardRole::Annot,
            "Figure" => StandardRole::Figure,
            "Formula" => StandardRole::Formula,
            "Form" => StandardRole::Form,
            "Ruby" => StandardRole::Ruby,
            // PDF/UA-2 (ISO 14289-2:2024)
            "DocumentFragment" => StandardRole::DocumentFragment,
            "FENote" => StandardRole::FENote,
            "Note" => StandardRole::Note,
            "Em" => StandardRole::Em,
            "Strong" => StandardRole::Strong,
            "Aside" => StandardRole::Aside,
            "Title" => StandardRole::Title,
            "Sub" => StandardRole::Sub,
            other => StandardRole::NonStandard(other.to_string()),
        }
    }

    /// Returns `true` if this is a block-level element.
    pub fn is_block(&self) -> bool {
        matches!(
            self,
            StandardRole::Document
                | StandardRole::Part
                | StandardRole::Art
                | StandardRole::Sect
                | StandardRole::Div
                | StandardRole::BlockQuote
                | StandardRole::P
                | StandardRole::H(_)
                | StandardRole::HGroup
                | StandardRole::L
                | StandardRole::LI
                | StandardRole::Table
                | StandardRole::TR
                | StandardRole::THead
                | StandardRole::TBody
                | StandardRole::TFoot
                | StandardRole::Figure
                | StandardRole::DocumentFragment
                | StandardRole::FENote
                | StandardRole::Note
                | StandardRole::Aside
                | StandardRole::Title
        )
    }

    /// Returns `true` if this is an inline-level element.
    pub fn is_inline(&self) -> bool {
        matches!(
            self,
            StandardRole::Span
                | StandardRole::Link
                | StandardRole::Annot
                | StandardRole::Lbl
                | StandardRole::Formula
                | StandardRole::Ruby
                | StandardRole::Em
                | StandardRole::Strong
                | StandardRole::Sub
        )
    }
}

/// A role mapping from custom structure types to standard types.
///
/// PDF documents can define custom element types and map them to
/// standard roles via the `/RoleMap` dictionary.
pub type RoleMap = HashMap<String, StandardRole>;

/// Parses a `/RoleMap` dictionary into a `RoleMap`.
pub fn parse_role_map(dict: &Dictionary) -> RoleMap {
    let mut map = HashMap::new();
    for (key, val) in dict {
        if let Some(name) = val.as_name() {
            map.insert(key.as_str().to_string(), StandardRole::from_name(name));
        }
    }
    map
}

/// A structure element in the PDF structure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct StructElem {
    /// The structure type (e.g., "P", "H1", "Table", or custom).
    pub struct_type: String,
    /// The resolved standard role (after applying the role map).
    pub role: StandardRole,
    /// The element's title (`/T`), if present.
    pub title: Option<String>,
    /// The element's language (`/Lang`), if present.
    pub lang: Option<String>,
    /// Alternative text (`/Alt`) for figures and non-text content.
    pub alt_text: Option<String>,
    /// Actual text (`/ActualText`) replacement.
    pub actual_text: Option<String>,
    /// Child structure elements.
    pub children: Vec<StructElem>,
}

/// The parsed structure tree of a tagged PDF.
#[derive(Debug, Clone, PartialEq)]
pub struct StructTree {
    /// The role mapping from custom types to standard roles.
    pub role_map: RoleMap,
    /// The top-level structure elements (children of `/StructTreeRoot`).
    pub children: Vec<StructElem>,
    /// The document language from `/StructTreeRoot /Lang`.
    pub lang: Option<String>,
}

impl StructTree {
    /// Maximum recursion depth for structure tree traversal.
    const MAX_DEPTH: usize = 64;

    /// Parses the structure tree from the `/StructTreeRoot` dictionary.
    pub fn from_dict<'d, R>(dict: &Dictionary, resolve: &R) -> Self
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        let role_map = dict
            .get_str("RoleMap")
            .and_then(|o| resolve(o).or(Some(o)))
            .and_then(|o| o.as_dict())
            .map(parse_role_map)
            .unwrap_or_default();

        let lang = dict.get_text("Lang");

        let children = dict
            .get_str("K")
            .map(|k| Self::parse_children(k, &role_map, resolve, Self::MAX_DEPTH))
            .unwrap_or_default();

        StructTree {
            role_map,
            children,
            lang,
        }
    }

    /// Parses the `/K` (kids) entry which can be a single element or an array.
    fn parse_children<'d, R>(
        obj: &Object,
        role_map: &RoleMap,
        resolve: &R,
        depth: usize,
    ) -> Vec<StructElem>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        if depth == 0 {
            return Vec::new();
        }

        // Resolve indirect reference
        let obj = resolve(obj).unwrap_or(obj);

        match obj {
            Object::Array(arr) => arr
                .iter()
                .filter_map(|item| Self::parse_struct_elem(item, role_map, resolve, depth - 1))
                .collect(),
            Object::Dictionary(_) => Self::parse_struct_elem(obj, role_map, resolve, depth - 1)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Parses a single structure element dictionary.
    fn parse_struct_elem<'d, R>(
        obj: &Object,
        role_map: &RoleMap,
        resolve: &R,
        depth: usize,
    ) -> Option<StructElem>
    where
        R: Fn(&Object) -> Option<&'d Object>,
    {
        let obj = resolve(obj).unwrap_or(obj);
        let dict = obj.as_dict()?;

        // Must be /Type /StructElem (or have /S which is the structure type)
        let struct_type = dict.get_name("S")?;

        let role = role_map
            .get(struct_type)
            .cloned()
            .unwrap_or_else(|| StandardRole::from_name(struct_type));

        let children = dict
            .get_str("K")
            .map(|k| Self::parse_children(k, role_map, resolve, depth))
            .unwrap_or_default();

        Some(StructElem {
            struct_type: struct_type.to_string(),
            role,
            title: dict.get_text("T"),
            lang: dict.get_text("Lang"),
            alt_text: dict.get_text("Alt"),
            actual_text: dict.get_text("ActualText"),
            children,
        })
    }

    /// Returns a flat iterator over all structure elements (depth-first).
    pub fn iter_elements(&self) -> Vec<&StructElem> {
        let mut result = Vec::new();
        for child in &self.children {
            Self::collect_elements(child, &mut result);
        }
        result
    }

    /// Recursively collects elements depth-first.
    fn collect_elements<'a>(elem: &'a StructElem, result: &mut Vec<&'a StructElem>) {
        result.push(elem);
        for child in &elem.children {
            Self::collect_elements(child, result);
        }
    }

    /// Returns all figures that lack alt text.
    pub fn figures_without_alt_text(&self) -> Vec<&StructElem> {
        self.iter_elements()
            .into_iter()
            .filter(|e| e.role == StandardRole::Figure && e.alt_text.is_none())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{Object, PdfName};
    use crate::test_utils::make_dict;

    #[test]
    fn standard_role_from_name() {
        assert_eq!(StandardRole::from_name("P"), StandardRole::P);
        assert_eq!(StandardRole::from_name("H1"), StandardRole::H(1));
        assert_eq!(StandardRole::from_name("Table"), StandardRole::Table);
        assert_eq!(
            StandardRole::from_name("Custom"),
            StandardRole::NonStandard("Custom".into())
        );
    }

    #[test]
    fn standard_role_is_block() {
        assert!(StandardRole::P.is_block());
        assert!(StandardRole::H(1).is_block());
        assert!(StandardRole::Table.is_block());
        assert!(StandardRole::Figure.is_block());
        assert!(!StandardRole::Span.is_inline() || !StandardRole::Span.is_block());
    }

    #[test]
    fn standard_role_is_inline() {
        assert!(StandardRole::Span.is_inline());
        assert!(StandardRole::Link.is_inline());
        assert!(!StandardRole::P.is_inline());
    }

    #[test]
    fn parse_role_map_basic() {
        let dict = make_dict(vec![
            ("MyParagraph", Object::Name(PdfName::new("P"))),
            ("MyHeading", Object::Name(PdfName::new("H1"))),
        ]);

        let role_map = parse_role_map(&dict);
        assert_eq!(role_map.get("MyParagraph"), Some(&StandardRole::P));
        assert_eq!(role_map.get("MyHeading"), Some(&StandardRole::H(1)));
    }

    #[test]
    fn parse_role_map_empty() {
        let dict = make_dict(vec![]);
        let role_map = parse_role_map(&dict);
        assert!(role_map.is_empty());
    }

    #[test]
    fn struct_tree_from_dict_empty() {
        let dict = make_dict(vec![("Type", Object::Name(PdfName::new("StructTreeRoot")))]);

        let resolve = |_: &Object| -> Option<&Object> { None };
        let tree = StructTree::from_dict(&dict, &resolve);
        assert!(tree.children.is_empty());
        assert!(tree.role_map.is_empty());
        assert!(tree.lang.is_none());
    }

    #[test]
    fn struct_tree_with_language() {
        let dict = make_dict(vec![
            ("Type", Object::Name(PdfName::new("StructTreeRoot"))),
            (
                "Lang",
                Object::String(crate::core::objects::PdfString::from_literal("en-US")),
            ),
        ]);

        let resolve = |_: &Object| -> Option<&Object> { None };
        let tree = StructTree::from_dict(&dict, &resolve);
        assert_eq!(tree.lang, Some("en-US".to_string()));
    }

    #[test]
    fn struct_tree_single_element() {
        let elem = Object::Dictionary(make_dict(vec![
            ("Type", Object::Name(PdfName::new("StructElem"))),
            ("S", Object::Name(PdfName::new("P"))),
        ]));

        let root = make_dict(vec![
            ("Type", Object::Name(PdfName::new("StructTreeRoot"))),
            ("K", Object::Array(vec![Object::Integer(99)])),
        ]);

        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(99)) {
                Some(&elem)
            } else {
                None
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].struct_type, "P");
        assert_eq!(tree.children[0].role, StandardRole::P);
    }

    #[test]
    fn struct_elem_with_alt_text() {
        let elem = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("Figure"))),
            (
                "Alt",
                Object::String(crate::core::objects::PdfString::from_literal(
                    "A chart showing sales data",
                )),
            ),
        ]));

        let root = make_dict(vec![("K", Object::Array(vec![Object::Integer(1)]))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            if matches!(obj, Object::Integer(1)) {
                Some(&elem)
            } else {
                None
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].role, StandardRole::Figure);
        assert_eq!(
            tree.children[0].alt_text,
            Some("A chart showing sales data".into())
        );
    }

    #[test]
    fn struct_tree_with_role_map() {
        let role_map_dict =
            Object::Dictionary(make_dict(vec![("MyPara", Object::Name(PdfName::new("P")))]));

        let elem = Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("MyPara")))]));

        let root = make_dict(vec![
            ("K", Object::Array(vec![Object::Integer(2)])),
            ("RoleMap", Object::Integer(3)),
        ]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(2) => Some(&elem),
                Object::Integer(3) => Some(&role_map_dict),
                _ => None,
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        assert_eq!(tree.role_map.get("MyPara"), Some(&StandardRole::P));
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].struct_type, "MyPara");
        assert_eq!(tree.children[0].role, StandardRole::P);
    }

    #[test]
    fn struct_tree_nested_elements() {
        let child_elem =
            Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Span")))]));

        let parent_elem = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("P"))),
            ("K", Object::Array(vec![Object::Integer(11)])),
        ]));

        let root = make_dict(vec![("K", Object::Array(vec![Object::Integer(10)]))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(10) => Some(&parent_elem),
                Object::Integer(11) => Some(&child_elem),
                _ => None,
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].struct_type, "P");
        assert_eq!(tree.children[0].children.len(), 1);
        assert_eq!(tree.children[0].children[0].struct_type, "Span");
    }

    #[test]
    fn figures_without_alt_text() {
        let fig_no_alt =
            Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Figure")))]));
        let fig_with_alt = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("Figure"))),
            (
                "Alt",
                Object::String(crate::core::objects::PdfString::from_literal("Photo")),
            ),
        ]));
        let para = Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("P")))]));

        let root = make_dict(vec![(
            "K",
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(2),
                Object::Integer(3),
            ]),
        )]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(1) => Some(&fig_no_alt),
                Object::Integer(2) => Some(&fig_with_alt),
                Object::Integer(3) => Some(&para),
                _ => None,
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        let missing = tree.figures_without_alt_text();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].struct_type, "Figure");
        assert!(missing[0].alt_text.is_none());
    }

    #[test]
    fn iter_elements_depth_first() {
        let span1 = Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Span")))]));
        let span2 = Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Link")))]));
        let para = Object::Dictionary(make_dict(vec![
            ("S", Object::Name(PdfName::new("P"))),
            (
                "K",
                Object::Array(vec![Object::Integer(21), Object::Integer(22)]),
            ),
        ]));

        let root = make_dict(vec![("K", Object::Array(vec![Object::Integer(20)]))]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(20) => Some(&para),
                Object::Integer(21) => Some(&span1),
                Object::Integer(22) => Some(&span2),
                _ => None,
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        let elems = tree.iter_elements();
        assert_eq!(elems.len(), 3);
        assert_eq!(elems[0].struct_type, "P");
        assert_eq!(elems[1].struct_type, "Span");
        assert_eq!(elems[2].struct_type, "Link");
    }

    // --- PDF/UA-2 (ISO 14289-2:2024) structure elements ---

    #[test]
    fn pdfua2_document_fragment_role() {
        assert_eq!(
            StandardRole::from_name("DocumentFragment"),
            StandardRole::DocumentFragment
        );
        assert!(StandardRole::DocumentFragment.is_block());
    }

    #[test]
    fn pdfua2_fenote_role() {
        assert_eq!(StandardRole::from_name("FENote"), StandardRole::FENote);
        assert!(StandardRole::FENote.is_block());
    }

    #[test]
    fn pdfua2_em_role() {
        assert_eq!(StandardRole::from_name("Em"), StandardRole::Em);
        assert!(StandardRole::Em.is_inline());
    }

    #[test]
    fn pdfua2_strong_role() {
        assert_eq!(StandardRole::from_name("Strong"), StandardRole::Strong);
        assert!(StandardRole::Strong.is_inline());
    }

    #[test]
    fn pdfua2_aside_role() {
        assert_eq!(StandardRole::from_name("Aside"), StandardRole::Aside);
        assert!(StandardRole::Aside.is_block());
    }

    #[test]
    fn pdfua2_title_role() {
        assert_eq!(StandardRole::from_name("Title"), StandardRole::Title);
        assert!(StandardRole::Title.is_block());
    }

    #[test]
    fn pdfua2_sub_role() {
        assert_eq!(StandardRole::from_name("Sub"), StandardRole::Sub);
        assert!(StandardRole::Sub.is_inline());
    }

    #[test]
    fn pdfua2_note_is_recognized() {
        // PDF/UA-1 Note element should still be recognized (FENote replaces it in UA-2)
        assert_eq!(StandardRole::from_name("Note"), StandardRole::Note);
        assert!(StandardRole::Note.is_block());
    }

    #[test]
    fn pdfua2_elements_in_structure_tree() {
        let em_elem = Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Em")))]));
        let strong_elem =
            Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("Strong")))]));
        let fenote_elem =
            Object::Dictionary(make_dict(vec![("S", Object::Name(PdfName::new("FENote")))]));

        let root = make_dict(vec![(
            "K",
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(2),
                Object::Integer(3),
            ]),
        )]);

        let resolve = |obj: &Object| -> Option<&Object> {
            match obj {
                Object::Integer(1) => Some(&em_elem),
                Object::Integer(2) => Some(&strong_elem),
                Object::Integer(3) => Some(&fenote_elem),
                _ => None,
            }
        };

        let tree = StructTree::from_dict(&root, &resolve);
        assert_eq!(tree.children.len(), 3);
        assert_eq!(tree.children[0].role, StandardRole::Em);
        assert_eq!(tree.children[1].role, StandardRole::Strong);
        assert_eq!(tree.children[2].role, StandardRole::FENote);
    }
}
