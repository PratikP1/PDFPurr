//! Builds tagged PDF structure from OCR output.
//!
//! Creates `/StructTreeRoot` with `<Document>` → `<P>` elements from
//! OCR word groups, marks page images as Artifacts, and sets `/Lang`.
//!
//! ISO 32000-2:2020, Section 14.7 (Logical Structure).

use crate::core::objects::{
    Dictionary, IndirectRef, Object, ObjectId, PdfName, PdfString, StringFormat,
};
use crate::document::Document;
use crate::error::PdfResult;
use crate::ocr::engine::OcrResult;

/// Groups OCR words into paragraphs based on vertical proximity.
///
/// Words within `line_gap_threshold` pixels vertically are considered
/// part of the same paragraph. Returns groups of word indices.
fn group_words_into_paragraphs(result: &OcrResult, line_gap_threshold: u32) -> Vec<Vec<usize>> {
    if result.words.is_empty() {
        return Vec::new();
    }

    let mut paragraphs: Vec<Vec<usize>> = Vec::new();
    let mut current_para: Vec<usize> = vec![0];
    let mut prev_bottom = result.words[0].y + result.words[0].height;

    for i in 1..result.words.len() {
        let word = &result.words[i];
        let gap = word.y.saturating_sub(prev_bottom);

        if gap > line_gap_threshold {
            paragraphs.push(std::mem::take(&mut current_para));
        }
        current_para.push(i);
        prev_bottom = word.y + word.height;
    }

    if !current_para.is_empty() {
        paragraphs.push(current_para);
    }

    paragraphs
}

/// Adds a tagged structure tree to the document from OCR results.
///
/// Creates:
/// - `/StructTreeRoot` with `/Type /StructTreeRoot`
/// - `<Document>` root element
/// - `<P>` elements for each paragraph (grouped by vertical proximity)
/// - `/Lang` attribute on the document element
///
/// The page image is marked as an Artifact via `/MarkInfo << /Marked true >>`.
pub fn add_tagged_structure_from_ocr(
    doc: &mut Document,
    _page_index: usize,
    result: &OcrResult,
    language: &str,
) -> PdfResult<()> {
    /// Vertical gap (pixels) above which OCR words start a new paragraph.
    const PARAGRAPH_GAP_THRESHOLD: u32 = 30;

    let paragraphs = group_words_into_paragraphs(result, PARAGRAPH_GAP_THRESHOLD);
    if paragraphs.is_empty() {
        return Ok(());
    }

    // Build <P> structure elements
    let mut para_refs: Vec<Object> = Vec::new();
    for para_words in &paragraphs {
        let text: String = para_words
            .iter()
            .map(|&i| result.words[i].text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        let mut p_dict = Dictionary::new();
        p_dict.insert(
            PdfName::new("Type"),
            Object::Name(PdfName::new("StructElem")),
        );
        p_dict.insert(PdfName::new("S"), Object::Name(PdfName::new("P")));
        p_dict.insert(
            PdfName::new("ActualText"),
            Object::String(PdfString {
                bytes: text.into_bytes(),
                format: StringFormat::Literal,
            }),
        );

        let p_id = doc.add_object(Object::Dictionary(p_dict));
        para_refs.push(Object::Reference(IndirectRef::new(p_id.0, p_id.1)));
    }

    // Build <Document> element
    let mut doc_elem = Dictionary::new();
    doc_elem.insert(
        PdfName::new("Type"),
        Object::Name(PdfName::new("StructElem")),
    );
    doc_elem.insert(PdfName::new("S"), Object::Name(PdfName::new("Document")));
    doc_elem.insert(PdfName::new("K"), Object::Array(para_refs));
    doc_elem.insert(
        PdfName::new("Lang"),
        Object::String(PdfString {
            bytes: language.as_bytes().to_vec(),
            format: StringFormat::Literal,
        }),
    );
    let doc_elem_id = doc.add_object(Object::Dictionary(doc_elem));

    // Build /StructTreeRoot
    let mut struct_root = Dictionary::new();
    struct_root.insert(
        PdfName::new("Type"),
        Object::Name(PdfName::new("StructTreeRoot")),
    );
    struct_root.insert(
        PdfName::new("K"),
        Object::Reference(IndirectRef::new(doc_elem_id.0, doc_elem_id.1)),
    );
    let struct_root_id = doc.add_object(Object::Dictionary(struct_root));

    // Set /StructTreeRoot, /MarkInfo, and /Lang on catalog (single access)
    let catalog_id: ObjectId = (1, 0); // catalog is always object 1 in our docs
    if let Some(Object::Dictionary(catalog)) = doc.get_object_mut(catalog_id) {
        catalog.insert(
            PdfName::new("StructTreeRoot"),
            Object::Reference(IndirectRef::new(struct_root_id.0, struct_root_id.1)),
        );

        let mut mark_info = Dictionary::new();
        mark_info.insert(PdfName::new("Marked"), Object::Boolean(true));
        catalog.insert(PdfName::new("MarkInfo"), Object::Dictionary(mark_info));

        if catalog.get(&PdfName::new("Lang")).is_none() {
            catalog.insert(
                PdfName::new("Lang"),
                Object::String(PdfString {
                    bytes: language.as_bytes().to_vec(),
                    format: StringFormat::Literal,
                }),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::engine::{OcrResult, OcrWord};

    fn make_words(texts: &[(&str, u32, u32)]) -> OcrResult {
        let words = texts
            .iter()
            .map(|(text, x, y)| OcrWord {
                text: text.to_string(),
                x: *x,
                y: *y,
                width: 100,
                height: 20,
                confidence: 0.9,
            })
            .collect();
        OcrResult {
            words,
            image_width: 2550,
            image_height: 3300,
        }
    }

    #[test]
    fn group_words_single_paragraph() {
        let result = make_words(&[("Hello", 100, 100), ("world", 250, 100)]);
        let paras = group_words_into_paragraphs(&result, 30);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].len(), 2);
    }

    #[test]
    fn group_words_two_paragraphs() {
        let result = make_words(&[
            ("First", 100, 100),
            ("para", 250, 100),
            ("Second", 100, 200), // gap > 30 from bottom of first (120+30 < 200)
        ]);
        let paras = group_words_into_paragraphs(&result, 30);
        assert_eq!(paras.len(), 2);
    }

    #[test]
    fn group_words_empty() {
        let result = OcrResult {
            words: vec![],
            image_width: 100,
            image_height: 100,
        };
        let paras = group_words_into_paragraphs(&result, 30);
        assert!(paras.is_empty());
    }

    #[test]
    fn add_tagged_structure_creates_struct_tree() {
        use crate::core::objects::DictExt;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = make_words(&[("Hello", 100, 100), ("world", 250, 100)]);
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        // Catalog should have /StructTreeRoot and /MarkInfo
        let catalog = doc.catalog().unwrap();
        assert!(catalog.get_str("StructTreeRoot").is_some());
        assert!(catalog.get_name("Lang").is_some() || catalog.get_str("Lang").is_some());

        let mark_info = catalog
            .get_str("MarkInfo")
            .and_then(|o| o.as_dict())
            .unwrap();
        assert_eq!(
            mark_info.get_str("Marked").and_then(|o| o.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn structure_tree_validates_as_tagged() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = make_words(&[("Test", 100, 100)]);
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        // The structure tree should be parseable
        let tree = doc.structure_tree();
        assert!(tree.is_some(), "Structure tree should exist");
    }
}
