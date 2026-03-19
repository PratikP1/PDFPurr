//! PDF/X validation (ISO 15930).
//!
//! Checks whether a [`Document`] conforms to PDF/X print production standards.
//! Each check function returns a [`StandardsCheck`]; the top-level
//! [`validate_pdfx`] aggregates them into a [`StandardsReport`].

use crate::core::objects::DictExt;
use crate::document::Document;

use super::common::{
    PdfXLevel, StandardsCheck, StandardsReport, CHECK_OUTPUT_INTENT_PDFX, CHECK_TRIM_OR_ART_BOX,
};
use super::pdfa;

const DESC_OUTPUT_INTENT: &str = "OutputIntents with GTS_PDFX must be present";
const DESC_TRIM_OR_ART_BOX: &str = "Every page must have /TrimBox or /ArtBox";

/// Validates a document against PDF/X requirements.
pub fn validate_pdfx(doc: &Document, level: PdfXLevel) -> StandardsReport {
    let mut checks = vec![
        pdfa::check_no_encryption(doc),
        check_output_intent_pdfx(doc),
        check_trim_or_art_box(doc),
        pdfa::check_fonts_embedded(doc),
    ];
    if level == PdfXLevel::X1a {
        checks.push(pdfa::check_no_transparency(doc));
    }
    StandardsReport {
        standard: format!("{}", level),
        checks,
    }
}

/// Checks for `/OutputIntents` with `/S /GTS_PDFX`.
fn check_output_intent_pdfx(doc: &Document) -> StandardsCheck {
    let fail =
        |detail: &str| StandardsCheck::fail(CHECK_OUTPUT_INTENT_PDFX, DESC_OUTPUT_INTENT, detail);

    let catalog = match doc.catalog() {
        Ok(c) => c,
        Err(_) => return fail("Cannot access catalog"),
    };

    let intents = match catalog
        .get_str("OutputIntents")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| o.as_array())
    {
        Some(arr) => arr,
        None => return fail("Catalog has no /OutputIntents"),
    };

    let has_pdfx = intents.iter().any(|intent_obj| {
        doc.resolve(intent_obj)
            .and_then(|o| o.as_dict())
            .and_then(|d| d.get_name("S"))
            .map(|s| s == "GTS_PDFX")
            .unwrap_or(false)
    });

    if has_pdfx {
        StandardsCheck::pass(CHECK_OUTPUT_INTENT_PDFX, DESC_OUTPUT_INTENT)
    } else {
        fail("No OutputIntent with /S /GTS_PDFX found")
    }
}

/// Checks that every page has a `/TrimBox` or `/ArtBox`.
fn check_trim_or_art_box(doc: &Document) -> StandardsCheck {
    let mut details = Vec::new();

    if let Ok(pages) = doc.pages() {
        for (i, page) in pages.iter().enumerate() {
            let has_trim = page.get_str("TrimBox").is_some();
            let has_art = page.get_str("ArtBox").is_some();
            if !has_trim && !has_art {
                details.push(format!("Page {} has no /TrimBox or /ArtBox", i + 1));
            }
        }
    }

    StandardsCheck::from_details(CHECK_TRIM_OR_ART_BOX, DESC_TRIM_OR_ART_BOX, details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{Dictionary, IndirectRef, Object, PdfName, PdfStream, PdfString};

    fn test_doc() -> Document {
        Document::new()
    }

    /// Adds OutputIntents with given /S value to the catalog.
    fn add_output_intent(doc: &mut Document, subtype: &str) {
        let mut intent = Dictionary::new();
        intent.insert(PdfName::new("S"), Object::Name(PdfName::new(subtype)));
        intent.insert(
            PdfName::new("OutputConditionIdentifier"),
            Object::String(PdfString::from_literal("CGATS TR 001")),
        );
        let intent_id = doc.add_object(Object::Dictionary(intent));

        let catalog_id = (1u32, 0u16);
        if let Some(Object::Dictionary(ref mut catalog)) = doc.get_object_mut(catalog_id) {
            catalog.insert(
                PdfName::new("OutputIntents"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    intent_id.0,
                    intent_id.1,
                ))]),
            );
        }
    }

    /// Creates a document with a page that has the given boxes.
    fn doc_with_page_box(box_name: Option<&str>) -> Document {
        let mut doc = Document::new();

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        if let Some(name) = box_name {
            page.insert(
                PdfName::new(name),
                Object::Array(vec![
                    Object::Integer(10),
                    Object::Integer(10),
                    Object::Integer(602),
                    Object::Integer(782),
                ]),
            );
        }
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Creates a document with transparency on a page.
    fn doc_with_transparency() -> Document {
        let mut doc = Document::new();

        let mut group = Dictionary::new();
        group.insert(
            PdfName::new("S"),
            Object::Name(PdfName::new("Transparency")),
        );
        let group_id = doc.add_object(Object::Dictionary(group));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page.insert(
            PdfName::new("Group"),
            Object::Reference(IndirectRef::new(group_id.0, group_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Creates a document with an embedded font on a page.
    fn doc_with_embedded_font() -> Document {
        let mut doc = Document::new();

        let font_stream = PdfStream::new(Dictionary::new(), vec![0u8; 100]);
        let file_id = doc.add_object(Object::Stream(font_stream));

        let mut desc = Dictionary::new();
        desc.insert(
            PdfName::new("Type"),
            Object::Name(PdfName::new("FontDescriptor")),
        );
        desc.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(file_id.0, file_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(desc));

        let mut font = Dictionary::new();
        font.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );
        let font_id = doc.add_object(Object::Dictionary(font));

        let mut font_res = Dictionary::new();
        font_res.insert(
            PdfName::new("F1"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let font_res_id = doc.add_object(Object::Dictionary(font_res));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(font_res_id.0, font_res_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    // --- 8F Tests: OutputIntent ---

    #[test]
    fn pdfx_output_intent_present_passes() {
        let mut doc = test_doc();
        add_output_intent(&mut doc, "GTS_PDFX");
        let check = check_output_intent_pdfx(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfx_output_intent_missing_fails() {
        let doc = test_doc();
        let check = check_output_intent_pdfx(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("no /OutputIntents"));
    }

    #[test]
    fn pdfx_output_intent_wrong_subtype_fails() {
        let mut doc = test_doc();
        add_output_intent(&mut doc, "GTS_PDFA1");
        let check = check_output_intent_pdfx(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("GTS_PDFX"));
    }

    // --- 8F Tests: TrimBox / ArtBox ---

    #[test]
    fn pdfx_trimbox_present_passes() {
        let doc = doc_with_page_box(Some("TrimBox"));
        let check = check_trim_or_art_box(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfx_artbox_accepted() {
        let doc = doc_with_page_box(Some("ArtBox"));
        let check = check_trim_or_art_box(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfx_no_trimbox_or_artbox_fails() {
        let doc = doc_with_page_box(None);
        let check = check_trim_or_art_box(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("no /TrimBox or /ArtBox"));
    }

    // --- 8F Tests: Encryption ---

    #[test]
    fn pdfx_no_encryption_passes() {
        let doc = test_doc();
        let check = pdfa::check_no_encryption(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfx_encryption_fails() {
        let mut doc = test_doc();
        let mut encrypt = Dictionary::new();
        encrypt.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("Standard")),
        );
        let id = doc.add_object(Object::Dictionary(encrypt));
        doc.trailer.insert(
            PdfName::new("Encrypt"),
            Object::Reference(IndirectRef::new(id.0, id.1)),
        );
        let check = pdfa::check_no_encryption(&doc);
        assert!(!check.passed);
    }

    // --- 8G Tests: Transparency ---

    #[test]
    fn pdfx1a_no_transparency_passes() {
        let doc = test_doc();
        let report = validate_pdfx(&doc, PdfXLevel::X1a);
        let check = report.checks.iter().find(|c| c.id == "no-transparency");
        assert!(check.is_some());
        assert!(check.unwrap().passed);
    }

    #[test]
    fn pdfx1a_transparency_fails() {
        let doc = doc_with_transparency();
        let report = validate_pdfx(&doc, PdfXLevel::X1a);
        let check = report
            .checks
            .iter()
            .find(|c| c.id == "no-transparency")
            .unwrap();
        assert!(!check.passed);
    }

    #[test]
    fn pdfx4_transparency_allowed() {
        let doc = doc_with_transparency();
        let report = validate_pdfx(&doc, PdfXLevel::X4);
        let check = report.checks.iter().find(|c| c.id == "no-transparency");
        // PDF/X-4 doesn't include transparency check
        assert!(check.is_none());
    }

    // --- 8G Tests: Fonts ---

    #[test]
    fn pdfx_fonts_embedded_passes() {
        let doc = doc_with_embedded_font();
        let check = pdfa::check_fonts_embedded(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfx_fonts_not_embedded_fails() {
        let mut doc = Document::new();
        // Add a page with an unembedded font
        let mut font = Dictionary::new();
        font.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Helvetica")),
        );
        let font_id = doc.add_object(Object::Dictionary(font));

        let mut font_res = Dictionary::new();
        font_res.insert(
            PdfName::new("F1"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let font_res_id = doc.add_object(Object::Dictionary(font_res));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(font_res_id.0, font_res_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let check = pdfa::check_fonts_embedded(&doc);
        assert!(!check.passed);
    }

    // --- 8G Tests: Top-level Validator ---

    #[test]
    fn validate_pdfx_1a_full_report() {
        let mut doc = doc_with_page_box(Some("TrimBox"));
        add_output_intent(&mut doc, "GTS_PDFX");
        let report = validate_pdfx(&doc, PdfXLevel::X1a);
        // Should have: encryption, output intent, trim box, fonts, transparency = 5
        assert_eq!(report.total_checks(), 5);
        assert_eq!(report.standard, "PDF/X-1a:2001");
    }

    #[test]
    fn document_validate_pdfx_method() {
        let doc = test_doc();
        let report = validate_pdfx(&doc, PdfXLevel::X3);
        assert!(report.total_checks() > 0);
        assert_eq!(report.standard, "PDF/X-3:2002");
    }
}
