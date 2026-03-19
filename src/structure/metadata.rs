//! PDF metadata extraction.
//!
//! Extracts document metadata from the `/Info` dictionary and XMP
//! metadata streams. ISO 32000-2:2020, Sections 14.3 and 14.4.

use crate::core::objects::{DictExt, Dictionary};

/// Document metadata extracted from a PDF file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// Document title (`/Title` or `dc:title`).
    pub title: Option<String>,
    /// Document author (`/Author` or `dc:creator`).
    pub author: Option<String>,
    /// Document subject (`/Subject` or `dc:description`).
    pub subject: Option<String>,
    /// Keywords (`/Keywords`).
    pub keywords: Option<String>,
    /// Creator application (`/Creator` or `xmp:CreatorTool`).
    pub creator: Option<String>,
    /// PDF producer (`/Producer` or `pdf:Producer`).
    pub producer: Option<String>,
    /// Creation date (`/CreationDate` or `xmp:CreateDate`).
    pub creation_date: Option<String>,
    /// Modification date (`/ModDate` or `xmp:ModifyDate`).
    pub mod_date: Option<String>,
}

impl Metadata {
    /// Extracts metadata from the `/Info` dictionary.
    pub fn from_info_dict(dict: &Dictionary) -> Self {
        Metadata {
            title: dict.get_text("Title"),
            author: dict.get_text("Author"),
            subject: dict.get_text("Subject"),
            keywords: dict.get_text("Keywords"),
            creator: dict.get_text("Creator"),
            producer: dict.get_text("Producer"),
            creation_date: dict.get_text("CreationDate"),
            mod_date: dict.get_text("ModDate"),
        }
    }

    /// Extracts metadata from an XMP metadata XML string.
    ///
    /// Uses `roxmltree` for proper namespace-aware XML parsing.
    /// Supports both element content and `rdf:Description` attribute forms.
    /// Performs a single pass over the XML tree for efficiency.
    pub fn from_xmp(xmp_data: &str) -> Self {
        let doc = match roxmltree::Document::parse(xmp_data) {
            Ok(d) => d,
            Err(_) => return Metadata::default(),
        };

        let values = extract_all_xmp_values(&doc);

        Metadata {
            title: values.get(&(NS_DC, "title")).cloned(),
            author: values.get(&(NS_DC, "creator")).cloned(),
            subject: values.get(&(NS_DC, "description")).cloned(),
            keywords: values.get(&(NS_PDF, "Keywords")).cloned(),
            creator: values.get(&(NS_XMP, "CreatorTool")).cloned(),
            producer: values.get(&(NS_PDF, "Producer")).cloned(),
            creation_date: values.get(&(NS_XMP, "CreateDate")).cloned(),
            mod_date: values.get(&(NS_XMP, "ModifyDate")).cloned(),
        }
    }

    /// Merges two metadata sources, preferring non-None values from `other`.
    pub fn merge(self, other: &Metadata) -> Self {
        Metadata {
            title: other.title.clone().or(self.title),
            author: other.author.clone().or(self.author),
            subject: other.subject.clone().or(self.subject),
            keywords: other.keywords.clone().or(self.keywords),
            creator: other.creator.clone().or(self.creator),
            producer: other.producer.clone().or(self.producer),
            creation_date: other.creation_date.clone().or(self.creation_date),
            mod_date: other.mod_date.clone().or(self.mod_date),
        }
    }
}

use std::collections::HashMap;

/// Dublin Core namespace.
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
/// XMP basic namespace.
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
/// Adobe PDF namespace.
const NS_PDF: &str = "http://ns.adobe.com/pdf/1.3/";
/// RDF namespace (for rdf:li elements).
const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// Extracts all XMP property values in a single pass over the XML tree.
///
/// Returns a map keyed by `(namespace, local_name)`. Handles three patterns:
/// 1. Elements containing `rdf:li` items (prefers `xml:lang="x-default"`).
/// 2. Simple element text content.
/// 3. Attributes on `rdf:Description` elements.
fn extract_all_xmp_values<'a>(
    doc: &'a roxmltree::Document<'a>,
) -> HashMap<(&'a str, &'a str), String> {
    let mut values = HashMap::new();

    for node in doc.descendants() {
        let tag = node.tag_name();

        // Check rdf:Description attributes (pattern 3)
        if tag.namespace() == Some(NS_RDF) && tag.name() == "Description" {
            for attr in node.attributes() {
                if let Some(ns) = attr.namespace() {
                    let val = attr.value().trim();
                    if !val.is_empty() {
                        values
                            .entry((ns, attr.name()))
                            .or_insert_with(|| val.to_string());
                    }
                }
            }
            continue;
        }

        // Check namespaced elements (patterns 1 and 2)
        if let Some(ns) = tag.namespace() {
            if values.contains_key(&(ns, tag.name())) {
                continue;
            }

            // Pattern 1: rdf:li children
            let li_items: Vec<_> = node
                .descendants()
                .filter(|n| n.tag_name().namespace() == Some(NS_RDF) && n.tag_name().name() == "li")
                .collect();

            if !li_items.is_empty() {
                let default_li = li_items
                    .iter()
                    .find(|n| n.attribute((roxmltree::NS_XML_URI, "lang")) == Some("x-default"));
                let chosen = default_li.unwrap_or(&li_items[0]);
                let text = chosen.text().unwrap_or("").trim();
                if !text.is_empty() {
                    values.insert((ns, tag.name()), text.to_string());
                    continue;
                }
            }

            // Pattern 2: simple text content
            let text = node.text().unwrap_or("").trim();
            if !text.is_empty() {
                values.insert((ns, tag.name()), text.to_string());
            }
        }
    }

    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_dict, str_obj};

    #[test]
    fn metadata_from_info_dict() {
        let dict = make_dict(vec![
            ("Title", str_obj("My Document")),
            ("Author", str_obj("Jane Doe")),
            ("Producer", str_obj("PDFPurr 0.1")),
        ]);

        let meta = Metadata::from_info_dict(&dict);
        assert_eq!(meta.title, Some("My Document".to_string()));
        assert_eq!(meta.author, Some("Jane Doe".to_string()));
        assert_eq!(meta.producer, Some("PDFPurr 0.1".to_string()));
        assert!(meta.subject.is_none());
    }

    #[test]
    fn metadata_from_empty_dict() {
        let dict = make_dict(vec![]);
        let meta = Metadata::from_info_dict(&dict);
        assert_eq!(meta, Metadata::default());
    }

    #[test]
    fn metadata_from_xmp_simple() {
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description
  xmlns:dc="http://purl.org/dc/elements/1.1/"
  xmlns:xmp="http://ns.adobe.com/xap/1.0/"
  xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">XMP Title</rdf:li></rdf:Alt></dc:title>
<xmp:CreatorTool>TestApp</xmp:CreatorTool>
<pdf:Producer>PDFPurr</pdf:Producer>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;

        let meta = Metadata::from_xmp(xmp);
        assert_eq!(meta.title, Some("XMP Title".to_string()));
        assert_eq!(meta.creator, Some("TestApp".to_string()));
        assert_eq!(meta.producer, Some("PDFPurr".to_string()));
    }

    #[test]
    fn metadata_merge() {
        let base = Metadata {
            title: Some("Base Title".to_string()),
            author: Some("Base Author".to_string()),
            ..Default::default()
        };
        let overlay = Metadata {
            title: Some("Overlay Title".to_string()),
            subject: Some("Overlay Subject".to_string()),
            ..Default::default()
        };

        let merged = base.merge(&overlay);
        assert_eq!(merged.title, Some("Overlay Title".to_string()));
        assert_eq!(merged.author, Some("Base Author".to_string()));
        assert_eq!(merged.subject, Some("Overlay Subject".to_string()));
    }

    #[test]
    fn xmp_extract_empty() {
        let meta = Metadata::from_xmp("");
        assert_eq!(meta, Metadata::default());
    }

    #[test]
    fn xmp_extract_multiline_rdf_description() {
        // Real-world XMP where rdf:Description has attributes across lines
        // and multiple rdf:li entries (should pick x-default)
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
  xmlns:dc="http://purl.org/dc/elements/1.1/"
  xmlns:xmp="http://ns.adobe.com/xap/1.0/"
  xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
<dc:title>
  <rdf:Alt>
    <rdf:li xml:lang="en-US">English Title</rdf:li>
    <rdf:li xml:lang="x-default">Default Title</rdf:li>
  </rdf:Alt>
</dc:title>
<dc:creator>
  <rdf:Seq>
    <rdf:li>Author One</rdf:li>
    <rdf:li>Author Two</rdf:li>
  </rdf:Seq>
</dc:creator>
<xmp:CreateDate>2026-01-15T10:30:00Z</xmp:CreateDate>
<xmp:ModifyDate>2026-03-15T14:00:00Z</xmp:ModifyDate>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;

        let meta = Metadata::from_xmp(xmp);
        assert_eq!(meta.title, Some("Default Title".to_string()));
        assert_eq!(meta.author, Some("Author One".to_string()));
        assert_eq!(meta.creation_date, Some("2026-01-15T10:30:00Z".to_string()));
        assert_eq!(meta.mod_date, Some("2026-03-15T14:00:00Z".to_string()));
    }

    #[test]
    fn metadata_from_info_dict_all_fields() {
        let dict = make_dict(vec![
            ("Title", str_obj("Full Doc")),
            ("Author", str_obj("Alice")),
            ("Subject", str_obj("Testing")),
            ("Keywords", str_obj("pdf,rust,test")),
            ("Creator", str_obj("MyApp")),
            ("Producer", str_obj("PDFPurr")),
            ("CreationDate", str_obj("D:20260101120000Z")),
            ("ModDate", str_obj("D:20260315140000Z")),
        ]);

        let meta = Metadata::from_info_dict(&dict);
        assert_eq!(meta.title.as_deref(), Some("Full Doc"));
        assert_eq!(meta.author.as_deref(), Some("Alice"));
        assert_eq!(meta.subject.as_deref(), Some("Testing"));
        assert_eq!(meta.keywords.as_deref(), Some("pdf,rust,test"));
        assert_eq!(meta.creator.as_deref(), Some("MyApp"));
        assert_eq!(meta.producer.as_deref(), Some("PDFPurr"));
        assert_eq!(meta.creation_date.as_deref(), Some("D:20260101120000Z"));
        assert_eq!(meta.mod_date.as_deref(), Some("D:20260315140000Z"));
    }

    #[test]
    fn xmp_malformed_xml_returns_default() {
        let meta = Metadata::from_xmp("<not valid xml");
        assert_eq!(meta, Metadata::default());
    }

    #[test]
    fn xmp_extract_subject_and_keywords() {
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
  xmlns:dc="http://purl.org/dc/elements/1.1/"
  xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">A test subject</rdf:li></rdf:Alt></dc:description>
<pdf:Keywords>pdf, accessibility, rust</pdf:Keywords>
</rdf:Description>
</rdf:RDF>
</x:xmpmeta>"#;

        let meta = Metadata::from_xmp(xmp);
        assert_eq!(meta.subject.as_deref(), Some("A test subject"));
        assert_eq!(meta.keywords.as_deref(), Some("pdf, accessibility, rust"));
    }

    #[test]
    fn xmp_extract_with_xml_attributes_on_rdf_description() {
        // XMP where properties are set as attributes directly on rdf:Description
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about=""
  xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
  xmlns:xmp="http://ns.adobe.com/xap/1.0/"
  pdf:Producer="Acrobat Pro"
  xmp:CreatorTool="InDesign 2025"/>
</rdf:RDF>
</x:xmpmeta>"#;

        let meta = Metadata::from_xmp(xmp);
        assert_eq!(meta.producer, Some("Acrobat Pro".to_string()));
        assert_eq!(meta.creator, Some("InDesign 2025".to_string()));
    }
}
