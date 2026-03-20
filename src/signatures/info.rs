//! Signature information extraction from PDF signature dictionaries.

use crate::core::objects::{DictExt, Dictionary, Object};
use crate::error::{PdfError, PdfResult};

/// The sub-filter (signature format) of a PDF digital signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubFilter {
    /// PKCS#7 detached signature (`adbe.pkcs7.detached`).
    Pkcs7Detached,
    /// PKCS#7 SHA-1 signature (`adbe.pkcs7.sha1`).
    Pkcs7Sha1,
    /// CMS advanced electronic signature (`ETSI.CAdES.detached`).
    CadesDetached,
    /// RFC 3161 timestamp (`ETSI.RFC3161`).
    Rfc3161,
}

impl SubFilter {
    /// Parses a sub-filter from its PDF name string.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "adbe.pkcs7.detached" => Some(SubFilter::Pkcs7Detached),
            "adbe.pkcs7.sha1" => Some(SubFilter::Pkcs7Sha1),
            "ETSI.CAdES.detached" => Some(SubFilter::CadesDetached),
            "ETSI.RFC3161" => Some(SubFilter::Rfc3161),
            _ => None,
        }
    }
}

/// A byte range specifying which portions of the PDF are signed.
///
/// PDF signatures use `/ByteRange [offset1 length1 offset2 length2]`
/// to define two ranges: the content before and after the signature
/// value hex string. The gap between them is the signature itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Offset of the first signed range (always 0).
    pub offset1: usize,
    /// Length of the first signed range.
    pub length1: usize,
    /// Offset of the second signed range (after the signature value).
    pub offset2: usize,
    /// Length of the second signed range.
    pub length2: usize,
}

impl ByteRange {
    /// The total number of signed bytes.
    pub fn signed_length(&self) -> usize {
        self.length1 + self.length2
    }

    /// Whether this byte range covers the entire file (minus the signature gap).
    pub fn covers_whole_file(&self, file_len: usize) -> bool {
        self.offset1 == 0 && self.offset2 + self.length2 == file_len
    }
}

/// Information extracted from a PDF signature dictionary (`/Type /Sig`).
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureInfo {
    /// The signature sub-filter (format).
    pub sub_filter: SubFilter,
    /// The byte range covered by this signature.
    pub byte_range: ByteRange,
    /// The raw PKCS#7/CMS signature bytes (from `/Contents`).
    pub contents: Vec<u8>,
    /// Signer name (`/Name`), if present.
    pub name: Option<String>,
    /// Signing reason (`/Reason`), if present.
    pub reason: Option<String>,
    /// Signing location (`/Location`), if present.
    pub location: Option<String>,
    /// Signing date (`/M`), if present.
    pub signing_date: Option<String>,
    /// Contact info (`/ContactInfo`), if present.
    pub contact_info: Option<String>,
}

impl SignatureInfo {
    /// Parses a signature dictionary into a `SignatureInfo`.
    ///
    /// The dictionary must have `/Type /Sig` (or be a signature value
    /// dictionary referenced from a `/Sig` form field).
    pub fn from_dict(dict: &Dictionary) -> PdfResult<Self> {
        // Parse /SubFilter
        let sub_filter_name = dict
            .get_name("SubFilter")
            .ok_or_else(|| PdfError::InvalidStructure("Signature missing /SubFilter".into()))?;

        let sub_filter = SubFilter::from_name(sub_filter_name).ok_or_else(|| {
            PdfError::UnsupportedFeature(format!("Signature sub-filter: {}", sub_filter_name))
        })?;

        // Parse /ByteRange [offset1 length1 offset2 length2]
        let byte_range =
            parse_byte_range(dict.get_str("ByteRange").ok_or_else(|| {
                PdfError::InvalidStructure("Signature missing /ByteRange".into())
            })?)?;

        // Parse /Contents (hex string containing PKCS#7 data)
        let contents = dict
            .get_str("Contents")
            .and_then(|o| o.as_pdf_string())
            .map(|s| s.bytes.clone())
            .ok_or_else(|| PdfError::InvalidStructure("Signature missing /Contents".into()))?;

        Ok(SignatureInfo {
            sub_filter,
            byte_range,
            contents,
            name: dict.get_text("Name"),
            reason: dict.get_text("Reason"),
            location: dict.get_text("Location"),
            signing_date: dict.get_text("M"),
            contact_info: dict.get_text("ContactInfo"),
        })
    }
}

/// Parses a `/ByteRange` array object into a `ByteRange`.
fn parse_byte_range(obj: &Object) -> PdfResult<ByteRange> {
    let arr = obj.as_array().ok_or_else(|| PdfError::TypeError {
        expected: "Array".into(),
        found: obj.type_name().into(),
    })?;

    if arr.len() != 4 {
        return Err(PdfError::InvalidStructure(format!(
            "ByteRange must have 4 elements, got {}",
            arr.len()
        )));
    }

    let to_usize = |obj: &Object, name: &str| -> PdfResult<usize> {
        obj.as_i64().map(|v| v as usize).ok_or_else(|| {
            PdfError::InvalidStructure(format!("ByteRange {name} is not an integer"))
        })
    };

    Ok(ByteRange {
        offset1: to_usize(&arr[0], "offset1")?,
        length1: to_usize(&arr[1], "length1")?,
        offset2: to_usize(&arr[2], "offset2")?,
        length2: to_usize(&arr[3], "length2")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{PdfName, PdfString, StringFormat};
    use crate::test_utils::make_dict;

    #[test]
    fn sub_filter_from_name() {
        assert_eq!(
            SubFilter::from_name("adbe.pkcs7.detached"),
            Some(SubFilter::Pkcs7Detached)
        );
        assert_eq!(
            SubFilter::from_name("adbe.pkcs7.sha1"),
            Some(SubFilter::Pkcs7Sha1)
        );
        assert_eq!(
            SubFilter::from_name("ETSI.CAdES.detached"),
            Some(SubFilter::CadesDetached)
        );
        assert_eq!(
            SubFilter::from_name("ETSI.RFC3161"),
            Some(SubFilter::Rfc3161)
        );
        assert_eq!(SubFilter::from_name("unknown"), None);
    }

    #[test]
    fn byte_range_signed_length() {
        let br = ByteRange {
            offset1: 0,
            length1: 100,
            offset2: 200,
            length2: 300,
        };
        assert_eq!(br.signed_length(), 400);
    }

    #[test]
    fn byte_range_covers_whole_file() {
        let br = ByteRange {
            offset1: 0,
            length1: 100,
            offset2: 200,
            length2: 300,
        };
        assert!(br.covers_whole_file(500));
        assert!(!br.covers_whole_file(600));
    }

    #[test]
    fn parse_byte_range_valid() {
        let obj = Object::Array(vec![
            Object::Integer(0),
            Object::Integer(1024),
            Object::Integer(2048),
            Object::Integer(512),
        ]);
        let br = parse_byte_range(&obj).unwrap();
        assert_eq!(br.offset1, 0);
        assert_eq!(br.length1, 1024);
        assert_eq!(br.offset2, 2048);
        assert_eq!(br.length2, 512);
    }

    #[test]
    fn parse_byte_range_wrong_length() {
        let obj = Object::Array(vec![Object::Integer(0), Object::Integer(1024)]);
        assert!(parse_byte_range(&obj).is_err());
    }

    #[test]
    fn parse_byte_range_not_array() {
        assert!(parse_byte_range(&Object::Integer(42)).is_err());
    }

    #[test]
    fn signature_info_from_dict() {
        let dict = make_dict(vec![
            ("Type", Object::Name(PdfName::new("Sig"))),
            (
                "SubFilter",
                Object::Name(PdfName::new("adbe.pkcs7.detached")),
            ),
            (
                "ByteRange",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(840),
                    Object::Integer(960),
                    Object::Integer(240),
                ]),
            ),
            (
                "Contents",
                Object::String(PdfString {
                    bytes: vec![0x30, 0x82, 0x01, 0x00], // DER prefix
                    format: StringFormat::Hexadecimal,
                }),
            ),
            (
                "Name",
                Object::String(PdfString::from_literal("Alice Smith")),
            ),
            (
                "Reason",
                Object::String(PdfString::from_literal("I approve")),
            ),
            (
                "Location",
                Object::String(PdfString::from_literal("New York")),
            ),
            (
                "M",
                Object::String(PdfString::from_literal("D:20260315120000Z")),
            ),
        ]);

        let info = SignatureInfo::from_dict(&dict).unwrap();
        assert_eq!(info.sub_filter, SubFilter::Pkcs7Detached);
        assert_eq!(info.byte_range.offset1, 0);
        assert_eq!(info.byte_range.length1, 840);
        assert_eq!(info.byte_range.offset2, 960);
        assert_eq!(info.byte_range.length2, 240);
        assert_eq!(info.contents, vec![0x30, 0x82, 0x01, 0x00]);
        assert_eq!(info.name, Some("Alice Smith".to_string()));
        assert_eq!(info.reason, Some("I approve".to_string()));
        assert_eq!(info.location, Some("New York".to_string()));
        assert_eq!(info.signing_date, Some("D:20260315120000Z".to_string()));
    }

    #[test]
    fn signature_info_missing_subfilter() {
        let dict = make_dict(vec![
            ("Type", Object::Name(PdfName::new("Sig"))),
            (
                "ByteRange",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(100),
                    Object::Integer(200),
                    Object::Integer(100),
                ]),
            ),
            ("Contents", Object::String(PdfString::from_hex(vec![0x00]))),
        ]);

        assert!(SignatureInfo::from_dict(&dict).is_err());
    }

    #[test]
    fn signature_info_missing_byte_range() {
        let dict = make_dict(vec![
            ("Type", Object::Name(PdfName::new("Sig"))),
            (
                "SubFilter",
                Object::Name(PdfName::new("adbe.pkcs7.detached")),
            ),
            ("Contents", Object::String(PdfString::from_hex(vec![0x00]))),
        ]);

        assert!(SignatureInfo::from_dict(&dict).is_err());
    }

    #[test]
    fn signature_info_minimal() {
        let dict = make_dict(vec![
            (
                "SubFilter",
                Object::Name(PdfName::new("adbe.pkcs7.detached")),
            ),
            (
                "ByteRange",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(100),
                    Object::Integer(200),
                    Object::Integer(100),
                ]),
            ),
            ("Contents", Object::String(PdfString::from_hex(vec![0x30]))),
        ]);

        let info = SignatureInfo::from_dict(&dict).unwrap();
        assert!(info.name.is_none());
        assert!(info.reason.is_none());
        assert!(info.location.is_none());
        assert!(info.signing_date.is_none());
        assert!(info.contact_info.is_none());
    }
}
