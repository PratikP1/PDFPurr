//! PDF/A validation (ISO 19005).
//!
//! Checks whether a [`Document`] conforms to PDF/A archival standards.
//! Each check function returns a [`StandardsCheck`]; the top-level
//! [`validate_pdfa`] aggregates them into a [`StandardsReport`].

use crate::core::objects::{DictExt, Object};
use crate::document::Document;
use crate::structure::Metadata;

use super::common::{
    PdfALevel, StandardsCheck, StandardsReport, CHECK_COLOR_SPACES, CHECK_FONTS_EMBEDDED,
    CHECK_METADATA_CONSISTENCY, CHECK_NO_ENCRYPTION, CHECK_NO_JAVASCRIPT, CHECK_NO_LZW,
    CHECK_NO_TRANSPARENCY, CHECK_XMP_METADATA,
};

/// Validates a document against PDF/A requirements.
pub fn validate_pdfa(doc: &Document, level: PdfALevel) -> StandardsReport {
    let mut checks = vec![
        check_no_encryption(doc),
        check_xmp_metadata(doc),
        check_no_lzw_filters(doc),
        check_no_javascript(doc),
        check_fonts_embedded(doc),
        check_color_spaces(doc),
        check_metadata_consistency(doc),
    ];
    if level == PdfALevel::A1b {
        checks.push(check_no_transparency(doc));
    }
    StandardsReport {
        standard: format!("{}", level),
        checks,
    }
}

// --- Shared checks (also used by PDF/X via pub(crate)) ---

const DESC_NO_ENCRYPTION: &str = "Document must not be encrypted";
const DESC_FONTS_EMBEDDED: &str = "All fonts must be embedded";
const DESC_NO_TRANSPARENCY: &str = "No transparency groups allowed (PDF/A-1)";
const DESC_XMP_METADATA: &str = "XMP metadata must be present with PDF/A identification";
const DESC_NO_LZW: &str = "LZWDecode filter is not permitted";
const DESC_NO_JAVASCRIPT: &str = "JavaScript is not permitted";
const DESC_METADATA_CONSISTENCY: &str = "Info dict and XMP metadata must be consistent";
const DESC_COLOR_SPACES: &str = "Device-dependent color spaces require OutputIntents";

/// Checks that the document is not encrypted.
pub(crate) fn check_no_encryption(doc: &Document) -> StandardsCheck {
    if doc.trailer.get_str("Encrypt").is_some() {
        StandardsCheck::fail(
            CHECK_NO_ENCRYPTION,
            DESC_NO_ENCRYPTION,
            "Document has /Encrypt entry in trailer",
        )
    } else {
        StandardsCheck::pass(CHECK_NO_ENCRYPTION, DESC_NO_ENCRYPTION)
    }
}

/// Checks that all fonts in the document are embedded.
pub(crate) fn check_fonts_embedded(doc: &Document) -> StandardsCheck {
    let mut details = Vec::new();

    let pages = match doc.pages() {
        Ok(p) => p,
        Err(_) => return StandardsCheck::pass(CHECK_FONTS_EMBEDDED, DESC_FONTS_EMBEDDED),
    };

    for (page_idx, page_dict) in pages.iter().enumerate() {
        let resources = match doc.page_resources(page_dict) {
            Some(r) => r,
            None => continue,
        };

        let font_dict = match resources.get_str("Font").and_then(|o| doc.resolve(o)) {
            Some(Object::Dictionary(d)) => d,
            _ => continue,
        };

        for (name, font_obj) in font_dict.iter() {
            let font = match doc.resolve(font_obj).and_then(|o| o.as_dict()) {
                Some(d) => d,
                None => continue,
            };

            if !font_has_embedded_program(font, |o| doc.resolve(o)) {
                details.push(format!(
                    "Page {}: font /{} is not embedded",
                    page_idx + 1,
                    name.as_str()
                ));
            }
        }
    }

    StandardsCheck::from_details(CHECK_FONTS_EMBEDDED, DESC_FONTS_EMBEDDED, details)
}

/// Checks whether a font dictionary has an embedded font program.
pub(crate) fn font_has_embedded_program<'a>(
    font_dict: &'a crate::core::objects::Dictionary,
    resolve: impl Fn(&'a Object) -> Option<&'a Object>,
) -> bool {
    // Resolve the font descriptor
    let descriptor = match font_dict
        .get_str("FontDescriptor")
        .and_then(&resolve)
        .and_then(|o| o.as_dict())
    {
        Some(d) => d,
        None => return false,
    };

    // Check for any font file key
    descriptor.get_str("FontFile").is_some()
        || descriptor.get_str("FontFile2").is_some()
        || descriptor.get_str("FontFile3").is_some()
}

/// Checks that no page has a transparency group (PDF/A-1 only).
pub(crate) fn check_no_transparency(doc: &Document) -> StandardsCheck {
    let mut details = Vec::new();

    if let Ok(pages) = doc.pages() {
        for (i, page) in pages.iter().enumerate() {
            if let Some(group) = page.get_str("Group").and_then(|o| doc.resolve(o)) {
                if let Some(d) = group.as_dict() {
                    if d.get_name("S") == Some("Transparency") {
                        details.push(format!("Page {} has /Group /S /Transparency", i + 1));
                    }
                }
            }
        }
    }

    StandardsCheck::from_details(CHECK_NO_TRANSPARENCY, DESC_NO_TRANSPARENCY, details)
}

// --- PDF/A-specific checks ---

/// Checks that XMP metadata is present in the catalog.
fn check_xmp_metadata(doc: &Document) -> StandardsCheck {
    let fail = |detail: &str| StandardsCheck::fail(CHECK_XMP_METADATA, DESC_XMP_METADATA, detail);

    let catalog = match doc.catalog() {
        Ok(c) => c,
        Err(_) => return fail("Cannot access document catalog"),
    };

    let metadata_obj = match catalog.get_str("Metadata").and_then(|o| doc.resolve(o)) {
        Some(o) => o,
        None => return fail("Catalog has no /Metadata entry"),
    };

    let xmp_data = match metadata_obj {
        Object::Stream(stream) => match stream.decode_data() {
            Ok(data) => String::from_utf8_lossy(&data).into_owned(),
            Err(_) => return fail("Cannot decode /Metadata stream"),
        },
        _ => return fail("/Metadata is not a stream"),
    };

    let has_pdfaid = xmp_data.contains("pdfaid:part") || xmp_data.contains("pdfa:part");
    if has_pdfaid {
        StandardsCheck::pass(CHECK_XMP_METADATA, DESC_XMP_METADATA)
    } else {
        fail("XMP metadata lacks pdfaid:part identification")
    }
}

/// Checks that no stream uses LZWDecode filter.
fn check_no_lzw_filters(doc: &Document) -> StandardsCheck {
    let mut details = Vec::new();

    for (id, obj) in doc.iter_objects() {
        if let Object::Stream(stream) = obj {
            if let Some(filter) = stream.dict.get_str("Filter") {
                let has_lzw = match filter {
                    Object::Name(n) => n.as_str() == "LZWDecode",
                    Object::Array(arr) => arr.iter().any(|o| o.as_name() == Some("LZWDecode")),
                    _ => false,
                };
                if has_lzw {
                    details.push(format!("Object {} {} uses LZWDecode filter", id.0, id.1));
                }
            }
        }
    }

    StandardsCheck::from_details(CHECK_NO_LZW, DESC_NO_LZW, details)
}

/// Checks that no JavaScript actions exist in the document.
fn check_no_javascript(doc: &Document) -> StandardsCheck {
    let mut details = Vec::new();

    if let Ok(catalog) = doc.catalog() {
        // Check /OpenAction for JS
        if let Some(action) = catalog
            .get_str("OpenAction")
            .and_then(|o| doc.resolve(o))
            .and_then(|o| o.as_dict())
        {
            if action.get_str("JS").is_some() || action.get_name("S") == Some("JavaScript") {
                details.push("Catalog /OpenAction contains JavaScript".into());
            }
        }

        // Check /AA (additional actions)
        if let Some(aa) = catalog
            .get_str("AA")
            .and_then(|o| doc.resolve(o))
            .and_then(|o| o.as_dict())
        {
            for (key, val) in aa.iter() {
                if let Some(action_dict) = doc.resolve(val).and_then(|o| o.as_dict()) {
                    if action_dict.get_name("S") == Some("JavaScript") {
                        details.push(format!(
                            "Catalog /AA/{} contains JavaScript action",
                            key.as_str()
                        ));
                    }
                }
            }
        }

        // Check /Names/JavaScript
        if let Some(names) = catalog
            .get_str("Names")
            .and_then(|o| doc.resolve(o))
            .and_then(|o| o.as_dict())
        {
            if names.get_str("JavaScript").is_some() {
                details.push("Document has /Names/JavaScript name tree".into());
            }
        }
    }

    StandardsCheck::from_details(CHECK_NO_JAVASCRIPT, DESC_NO_JAVASCRIPT, details)
}

/// Checks that Info dict and XMP metadata are consistent.
fn check_metadata_consistency(doc: &Document) -> StandardsCheck {
    let info_meta = doc.info().map(Metadata::from_info_dict);

    // For now, just check that if both Info and XMP exist, key fields match
    // This is a simplified check — full PDF/A validation would be more thorough
    let catalog = match doc.catalog() {
        Ok(c) => c,
        Err(_) => {
            return StandardsCheck::pass(CHECK_METADATA_CONSISTENCY, DESC_METADATA_CONSISTENCY);
        }
    };

    let xmp_meta = catalog
        .get_str("Metadata")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| {
            if let Object::Stream(s) = o {
                s.decode_data().ok()
            } else {
                None
            }
        })
        .map(|data| Metadata::from_xmp(&String::from_utf8_lossy(&data)));

    let mut details = Vec::new();

    if let (Some(info), Some(xmp)) = (&info_meta, &xmp_meta) {
        if let (Some(info_title), Some(xmp_title)) = (&info.title, &xmp.title) {
            if info_title != xmp_title {
                details.push(format!(
                    "Title mismatch: Info='{}', XMP='{}'",
                    info_title, xmp_title
                ));
            }
        }
        if let (Some(info_author), Some(xmp_author)) = (&info.author, &xmp.author) {
            if info_author != xmp_author {
                details.push(format!(
                    "Author mismatch: Info='{}', XMP='{}'",
                    info_author, xmp_author
                ));
            }
        }
    }

    StandardsCheck::from_details(
        CHECK_METADATA_CONSISTENCY,
        DESC_METADATA_CONSISTENCY,
        details,
    )
}

/// Checks that color spaces are properly managed (OutputIntents present).
fn check_color_spaces(doc: &Document) -> StandardsCheck {
    let catalog = match doc.catalog() {
        Ok(c) => c,
        Err(_) => {
            return StandardsCheck::fail(
                CHECK_COLOR_SPACES,
                DESC_COLOR_SPACES,
                "Cannot access catalog",
            );
        }
    };

    let has_output_intents = catalog
        .get_str("OutputIntents")
        .and_then(|o| doc.resolve(o))
        .and_then(|o| o.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if has_output_intents {
        return StandardsCheck::pass(CHECK_COLOR_SPACES, DESC_COLOR_SPACES);
    }

    // No OutputIntents — check for device-dependent color spaces in page resources
    let mut details = Vec::new();
    if let Ok(pages) = doc.pages() {
        for (i, page) in pages.iter().enumerate() {
            if let Some(resources) = doc.page_resources(page) {
                if let Some(cs_dict) = resources
                    .get_str("ColorSpace")
                    .and_then(|o| doc.resolve(o))
                    .and_then(|o| o.as_dict())
                {
                    for (name, _) in cs_dict.iter() {
                        let n = name.as_str();
                        if n == "DeviceRGB" || n == "DeviceCMYK" || n == "DeviceGray" {
                            details.push(format!(
                                "Page {}: uses {} without OutputIntents",
                                i + 1,
                                n
                            ));
                        }
                    }
                }
            }
        }
    }

    StandardsCheck::from_details(CHECK_COLOR_SPACES, DESC_COLOR_SPACES, details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{Dictionary, IndirectRef, PdfName, PdfStream, PdfString};

    /// Helper to build a minimal document for testing.
    fn test_doc() -> Document {
        Document::new()
    }

    /// Adds /Encrypt to the trailer.
    fn add_encryption(doc: &mut Document) {
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
    }

    /// Sets /Metadata on the catalog with given XMP content.
    fn set_xmp_metadata(doc: &mut Document, xmp: &str) {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Metadata")));
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("XML")));
        let stream = PdfStream::new(dict, xmp.as_bytes().to_vec());
        let meta_id = doc.add_object(Object::Stream(stream));

        // Update catalog to reference the metadata
        let catalog_id = (1u32, 0u16);
        if let Some(Object::Dictionary(ref mut catalog)) = doc.get_object_mut(catalog_id) {
            catalog.insert(
                PdfName::new("Metadata"),
                Object::Reference(IndirectRef::new(meta_id.0, meta_id.1)),
            );
        }
    }

    /// Adds a stream with LZWDecode filter.
    fn add_lzw_stream(doc: &mut Document) {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("LZWDecode")),
        );
        let stream = PdfStream::new(dict, vec![0u8; 10]);
        doc.add_object(Object::Stream(stream));
    }

    /// Sets /OpenAction with JavaScript on the catalog.
    fn add_javascript_action(doc: &mut Document) {
        let mut action = Dictionary::new();
        action.insert(PdfName::new("S"), Object::Name(PdfName::new("JavaScript")));
        action.insert(
            PdfName::new("JS"),
            Object::String(PdfString::from_literal("app.alert('hi');")),
        );
        let action_id = doc.add_object(Object::Dictionary(action));

        let catalog_id = (1u32, 0u16);
        if let Some(Object::Dictionary(ref mut catalog)) = doc.get_object_mut(catalog_id) {
            catalog.insert(
                PdfName::new("OpenAction"),
                Object::Reference(IndirectRef::new(action_id.0, action_id.1)),
            );
        }
    }

    // --- 8B Tests: Encryption ---

    #[test]
    fn pdfa_no_encryption_passes() {
        let doc = test_doc();
        let check = check_no_encryption(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_encryption_present_fails() {
        let mut doc = test_doc();
        add_encryption(&mut doc);
        let check = check_no_encryption(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("Encrypt"));
    }

    // --- 8B Tests: XMP Metadata ---

    #[test]
    fn pdfa_xmp_metadata_present_passes() {
        let mut doc = test_doc();
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
<pdfaid:part>1</pdfaid:part>
<pdfaid:conformance>B</pdfaid:conformance>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;
        set_xmp_metadata(&mut doc, xmp);
        let check = check_xmp_metadata(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_xmp_metadata_missing_fails() {
        let doc = test_doc();
        let check = check_xmp_metadata(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("no /Metadata"));
    }

    #[test]
    fn pdfa_xmp_without_pdfaid_fails() {
        let mut doc = test_doc();
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Test</rdf:li></rdf:Alt></dc:title>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;
        set_xmp_metadata(&mut doc, xmp);
        let check = check_xmp_metadata(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("pdfaid"));
    }

    // --- 8B Tests: LZW Filter ---

    #[test]
    fn pdfa_no_lzw_filter_passes() {
        let doc = test_doc();
        let check = check_no_lzw_filters(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_lzw_filter_present_fails() {
        let mut doc = test_doc();
        add_lzw_stream(&mut doc);
        let check = check_no_lzw_filters(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("LZWDecode"));
    }

    // --- 8B Tests: JavaScript ---

    #[test]
    fn pdfa_no_javascript_passes() {
        let doc = test_doc();
        let check = check_no_javascript(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_javascript_in_catalog_fails() {
        let mut doc = test_doc();
        add_javascript_action(&mut doc);
        let check = check_no_javascript(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("JavaScript"));
    }

    // --- 8B Tests: Metadata Consistency ---

    #[test]
    fn pdfa_metadata_consistency_passes() {
        // No Info dict and no XMP → trivially consistent
        let doc = test_doc();
        let check = check_metadata_consistency(&doc);
        assert!(check.passed);
    }

    // --- 8C Tests: Font Embedding ---

    /// Creates a document with a page that has a font resource.
    fn doc_with_font(has_descriptor: bool, has_fontfile: Option<&str>) -> Document {
        let mut doc = Document::new();

        // Build font dictionary
        let mut font = Dictionary::new();
        font.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("TrueType")),
        );
        font.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("TestFont")),
        );

        if has_descriptor {
            let mut desc = Dictionary::new();
            desc.insert(
                PdfName::new("Type"),
                Object::Name(PdfName::new("FontDescriptor")),
            );
            if let Some(key) = has_fontfile {
                // Add a dummy font file stream
                let stream = PdfStream::new(Dictionary::new(), vec![0u8; 100]);
                let file_id = doc.add_object(Object::Stream(stream));
                desc.insert(
                    PdfName::new(key),
                    Object::Reference(IndirectRef::new(file_id.0, file_id.1)),
                );
            }
            let desc_id = doc.add_object(Object::Dictionary(desc));
            font.insert(
                PdfName::new("FontDescriptor"),
                Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
            );
        }

        let font_id = doc.add_object(Object::Dictionary(font));

        // Build font dict resource
        let mut font_res = Dictionary::new();
        font_res.insert(
            PdfName::new("F1"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let font_res_id = doc.add_object(Object::Dictionary(font_res));

        // Build resources dict
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(font_res_id.0, font_res_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Add a page with those resources
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

        // Update Pages to include this page
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

    #[test]
    fn pdfa_embedded_font_passes() {
        let doc = doc_with_font(true, Some("FontFile2"));
        let check = check_fonts_embedded(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_standard14_without_embedding_fails() {
        let doc = doc_with_font(false, None);
        let check = check_fonts_embedded(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("not embedded"));
    }

    #[test]
    fn pdfa_font_with_fontfile_passes() {
        let doc = doc_with_font(true, Some("FontFile"));
        let check = check_fonts_embedded(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_font_with_fontfile3_passes() {
        let doc = doc_with_font(true, Some("FontFile3"));
        let check = check_fonts_embedded(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_no_fonts_passes() {
        // Document with no pages → no fonts to check
        let doc = test_doc();
        let check = check_fonts_embedded(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_multiple_pages_all_fonts_checked() {
        // Two pages: one with embedded font, one without
        let mut doc = doc_with_font(true, Some("FontFile2"));

        // Add a second page with unembedded font
        let mut font2 = Dictionary::new();
        font2.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font2.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Helvetica")),
        );
        let font2_id = doc.add_object(Object::Dictionary(font2));

        let mut font_res2 = Dictionary::new();
        font_res2.insert(
            PdfName::new("F2"),
            Object::Reference(IndirectRef::new(font2_id.0, font2_id.1)),
        );
        let font_res2_id = doc.add_object(Object::Dictionary(font_res2));

        let mut resources2 = Dictionary::new();
        resources2.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(font_res2_id.0, font_res2_id.1)),
        );
        let res2_id = doc.add_object(Object::Dictionary(resources2));

        let mut page2 = Dictionary::new();
        page2.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page2.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page2.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        page2.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res2_id.0, res2_id.1)),
        );
        let page2_id = doc.add_object(Object::Dictionary(page2));

        // Get existing Kids array and add page2
        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            if let Some(Object::Array(ref mut kids)) = pages.get_mut(&PdfName::new("Kids")) {
                kids.push(Object::Reference(IndirectRef::new(page2_id.0, page2_id.1)));
            }
            pages.insert(PdfName::new("Count"), Object::Integer(2));
        }

        let check = check_fonts_embedded(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("Page 2"));
    }

    // --- 8D Tests: Transparency ---

    /// Adds a transparency group to a page.
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

    #[test]
    fn pdfa1_no_transparency_passes() {
        let doc = test_doc();
        let check = check_no_transparency(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa1_transparency_group_fails() {
        let doc = doc_with_transparency();
        let check = check_no_transparency(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("Transparency"));
    }

    #[test]
    fn pdfa2_transparency_allowed() {
        let doc = doc_with_transparency();
        // PDF/A-2b does NOT include the transparency check
        let report = validate_pdfa(&doc, PdfALevel::A2b);
        let transparency_check = report.checks.iter().find(|c| c.id == "no-transparency");
        assert!(transparency_check.is_none());
    }

    // --- 8D Tests: Color Spaces ---

    #[test]
    fn pdfa_output_intent_present_passes() {
        let mut doc = test_doc();
        // Add OutputIntents to catalog
        let mut intent = Dictionary::new();
        intent.insert(PdfName::new("S"), Object::Name(PdfName::new("GTS_PDFA1")));
        intent.insert(
            PdfName::new("OutputConditionIdentifier"),
            Object::String(PdfString::from_literal("sRGB")),
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

        let check = check_color_spaces(&doc);
        assert!(check.passed);
    }

    #[test]
    fn pdfa_device_colorspace_without_output_intent_fails() {
        let mut doc = Document::new();

        // Create a page with DeviceRGB in ColorSpace resources
        let mut cs_dict = Dictionary::new();
        cs_dict.insert(
            PdfName::new("DeviceRGB"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        let cs_id = doc.add_object(Object::Dictionary(cs_dict));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ColorSpace"),
            Object::Reference(IndirectRef::new(cs_id.0, cs_id.1)),
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

        let check = check_color_spaces(&doc);
        assert!(!check.passed);
        assert!(check.details[0].contains("DeviceRGB"));
    }

    #[test]
    fn pdfa_output_intent_with_dest_profile_passes() {
        let mut doc = test_doc();
        // Add OutputIntents with DestOutputProfile
        let profile_stream = PdfStream::new(Dictionary::new(), vec![0u8; 50]);
        let profile_id = doc.add_object(Object::Stream(profile_stream));

        let mut intent = Dictionary::new();
        intent.insert(PdfName::new("S"), Object::Name(PdfName::new("GTS_PDFA1")));
        intent.insert(
            PdfName::new("DestOutputProfile"),
            Object::Reference(IndirectRef::new(profile_id.0, profile_id.1)),
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

        let check = check_color_spaces(&doc);
        assert!(check.passed);
    }

    // --- 8E Tests: Top-level Validator ---

    #[test]
    fn validate_pdfa_1b_fully_compliant() {
        let mut doc = test_doc();
        // Add XMP with pdfaid
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
<pdfaid:part>1</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;
        set_xmp_metadata(&mut doc, xmp);

        // Add OutputIntents
        let mut intent = Dictionary::new();
        intent.insert(PdfName::new("S"), Object::Name(PdfName::new("GTS_PDFA1")));
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

        let report = validate_pdfa(&doc, PdfALevel::A1b);
        assert!(report.is_compliant(), "Failures: {:?}", report.failures());
    }

    #[test]
    fn validate_pdfa_1b_multiple_failures() {
        let mut doc = test_doc();
        add_encryption(&mut doc);
        add_lzw_stream(&mut doc);

        let report = validate_pdfa(&doc, PdfALevel::A1b);
        assert!(!report.is_compliant());
        // Should fail: encryption, xmp, lzw at minimum
        assert!(report.failures().len() >= 3);
    }

    #[test]
    fn validate_pdfa_report_standard_name() {
        let doc = test_doc();
        let report = validate_pdfa(&doc, PdfALevel::A1b);
        assert_eq!(report.standard, "PDF/A-1b");

        let report2 = validate_pdfa(&doc, PdfALevel::A2b);
        assert_eq!(report2.standard, "PDF/A-2b");
    }

    #[test]
    fn document_validate_pdfa_method() {
        // Test the Document convenience method
        let doc = test_doc();
        let report = validate_pdfa(&doc, PdfALevel::A1b);
        // Should run without panicking and return a report
        assert!(report.total_checks() > 0);
    }
}
