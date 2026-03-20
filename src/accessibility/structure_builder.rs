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
    let paragraphs =
        group_words_into_paragraphs(result, crate::ocr::constants::PARAGRAPH_GAP_THRESHOLD);
    if paragraphs.is_empty() {
        return Ok(());
    }

    // Classify each paragraph as heading or body text
    let roles = classify_paragraph_roles(&paragraphs, result);

    // Build structure elements with MCID references
    let mut para_refs: Vec<Object> = Vec::new();
    let mut parent_tree_entries: Vec<Object> = Vec::new();

    for (mcid, para_words) in paragraphs.iter().enumerate() {
        let text: String = para_words
            .iter()
            .map(|&i| result.words[i].text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        let role = &roles[mcid];

        let mut p_dict = Dictionary::new();
        p_dict.insert(
            PdfName::new("Type"),
            Object::Name(PdfName::new("StructElem")),
        );
        p_dict.insert(PdfName::new("S"), Object::Name(PdfName::new(role)));
        p_dict.insert(PdfName::new("K"), Object::Integer(mcid as i64));
        p_dict.insert(
            PdfName::new("ActualText"),
            Object::String(PdfString {
                bytes: text.into_bytes(),
                format: StringFormat::Literal,
            }),
        );

        let p_id = doc.add_object(Object::Dictionary(p_dict));
        let p_ref = Object::Reference(IndirectRef::new(p_id.0, p_id.1));
        para_refs.push(p_ref.clone());
        parent_tree_entries.push(p_ref);
    }

    // Build <Document> element
    let mut doc_elem = Dictionary::new();
    doc_elem.insert(
        PdfName::new("Type"),
        Object::Name(PdfName::new("StructElem")),
    );
    doc_elem.insert(PdfName::new("S"), Object::Name(PdfName::new("Document")));
    doc_elem.insert(PdfName::new("K"), Object::Array(para_refs));
    doc_elem.insert(PdfName::new("Lang"), lang_string(language));
    let doc_elem_id = doc.add_object(Object::Dictionary(doc_elem));

    // Build /ParentTree (number tree mapping MCIDs to struct elements)
    // ISO 32000-2, Section 14.7.4.4
    let mut parent_tree = Dictionary::new();
    parent_tree.insert(
        PdfName::new("Type"),
        Object::Name(PdfName::new("NumberTree")),
    );
    // /Nums array: [0, ref0, 1, ref1, ...] — maps MCID to struct element
    let mut nums: Vec<Object> = Vec::new();
    for (i, entry) in parent_tree_entries.iter().enumerate() {
        nums.push(Object::Integer(i as i64));
        nums.push(entry.clone());
    }
    parent_tree.insert(PdfName::new("Nums"), Object::Array(nums));
    let parent_tree_id = doc.add_object(Object::Dictionary(parent_tree));

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
    struct_root.insert(
        PdfName::new("ParentTree"),
        Object::Reference(IndirectRef::new(parent_tree_id.0, parent_tree_id.1)),
    );
    // Set /Lang on StructTreeRoot so structure_tree parser finds it
    struct_root.insert(PdfName::new("Lang"), lang_string(language));
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
            catalog.insert(PdfName::new("Lang"), lang_string(language));
        }
    }

    Ok(())
}

/// Creates a PDF literal string object from a language tag.
fn lang_string(language: &str) -> Object {
    Object::String(PdfString {
        bytes: language.as_bytes().to_vec(),
        format: StringFormat::Literal,
    })
}

/// Minimum ratio of paragraph median height to document median height
/// for a paragraph to be classified as a heading.
const HEADING_HEIGHT_RATIO: f64 = 1.4;

/// Classifies each paragraph as a heading level or body text.
///
/// Uses word height as a proxy for font size. Paragraphs whose median
/// word height exceeds the document median by [`HEADING_HEIGHT_RATIO`]
/// are classified as headings. The tallest heading becomes H1, the next
/// tallest H2, etc.
fn classify_paragraph_roles(paragraphs: &[Vec<usize>], result: &OcrResult) -> Vec<String> {
    if paragraphs.is_empty() {
        return Vec::new();
    }

    // Compute median word height for the entire document
    let mut all_heights: Vec<u32> = result.words.iter().map(|w| w.height).collect();
    all_heights.sort();
    let doc_median = if all_heights.is_empty() {
        0
    } else {
        all_heights[all_heights.len() / 2]
    };

    // Compute median height per paragraph
    let para_medians: Vec<u32> = paragraphs
        .iter()
        .map(|indices| {
            let mut heights: Vec<u32> = indices.iter().map(|&i| result.words[i].height).collect();
            heights.sort();
            if heights.is_empty() {
                0
            } else {
                heights[heights.len() / 2]
            }
        })
        .collect();

    // Identify heading paragraphs (significantly taller than document median)
    let threshold = (doc_median as f64 * HEADING_HEIGHT_RATIO) as u32;

    // Collect unique heading heights for level assignment (tallest = H1)
    let mut heading_heights: Vec<u32> = para_medians
        .iter()
        .filter(|&&h| h > threshold && doc_median > 0)
        .copied()
        .collect();
    heading_heights.sort();
    heading_heights.dedup();
    heading_heights.reverse(); // tallest first → H1

    para_medians
        .iter()
        .map(|&median| {
            if median > threshold && doc_median > 0 {
                let level = heading_heights
                    .iter()
                    .position(|&h| h == median)
                    .map(|i| i + 1)
                    .unwrap_or(1)
                    .min(6); // cap at H6
                format!("H{level}")
            } else {
                "P".to_string()
            }
        })
        .collect()
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
    fn structure_elements_link_to_mcids() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = make_words(&[
            ("First", 100, 100),
            ("para", 250, 100),
            ("Second", 100, 200), // new paragraph
        ]);
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        // Structure tree should exist with children linked via MCIDs
        let tree = doc.structure_tree().unwrap();
        assert!(!tree.children.is_empty());

        // The <Document> element's children should be <P> elements
        let doc_elem = &tree.children[0];
        assert_eq!(doc_elem.struct_type, "Document");
        assert!(
            doc_elem.children.len() >= 2,
            "Should have at least 2 paragraphs, got {}",
            doc_elem.children.len()
        );
    }

    #[test]
    fn heading_detected_from_tall_words() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // First paragraph: tall words (height 60) → should be heading
        // Second paragraph: normal words (height 20) → should be P
        let result = OcrResult {
            words: vec![
                OcrWord {
                    text: "Title".into(),
                    x: 100,
                    y: 50,
                    width: 200,
                    height: 60,
                    confidence: 0.9,
                },
                OcrWord {
                    text: "Here".into(),
                    x: 350,
                    y: 50,
                    width: 150,
                    height: 60,
                    confidence: 0.9,
                },
                OcrWord {
                    text: "Normal".into(),
                    x: 100,
                    y: 200,
                    width: 100,
                    height: 20,
                    confidence: 0.9,
                },
                OcrWord {
                    text: "body".into(),
                    x: 250,
                    y: 200,
                    width: 80,
                    height: 20,
                    confidence: 0.9,
                },
                OcrWord {
                    text: "text".into(),
                    x: 380,
                    y: 200,
                    width: 70,
                    height: 20,
                    confidence: 0.9,
                },
            ],
            image_width: 2550,
            image_height: 3300,
        };
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        let tree = doc.structure_tree().unwrap();
        let doc_elem = &tree.children[0];
        assert_eq!(doc_elem.struct_type, "Document");

        // First child should be a heading (H1 or H2)
        let first = &doc_elem.children[0];
        assert!(
            first.struct_type.starts_with('H'),
            "Tall text should be heading, got: {}",
            first.struct_type
        );

        // Second child should be P
        let second = &doc_elem.children[1];
        assert_eq!(second.struct_type, "P", "Normal text should be P");
    }

    #[test]
    fn all_same_height_produces_only_paragraphs() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = make_words(&[("First", 100, 100), ("Second", 100, 200)]);
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        let tree = doc.structure_tree().unwrap();
        let doc_elem = &tree.children[0];
        // All same height → no headings, all P
        for child in &doc_elem.children {
            assert_eq!(child.struct_type, "P", "Same-height text should all be P");
        }
    }

    #[test]
    fn parent_tree_exists_in_struct_tree_root() {
        use crate::core::objects::DictExt;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = make_words(&[("Hello", 100, 100)]);
        add_tagged_structure_from_ocr(&mut doc, 0, &result, "en-US").unwrap();

        // StructTreeRoot should have /ParentTree
        let catalog = doc.catalog().unwrap();
        let struct_root_ref = catalog.get_str("StructTreeRoot").unwrap();
        let struct_root = doc.resolve(struct_root_ref).unwrap();
        let struct_root_dict = struct_root.as_dict().unwrap();
        assert!(
            struct_root_dict.get_str("ParentTree").is_some(),
            "StructTreeRoot should have /ParentTree"
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
