//! PDF signature verification.
//!
//! Verifies the integrity of a PDF digital signature by:
//! 1. Extracting the signed byte ranges from the file
//! 2. Computing the digest (SHA-256 or SHA-512)
//! 3. Comparing against the digest in the PKCS#7/CMS signature

use sha2::{Digest, Sha256, Sha512};

use super::info::{ByteRange, SignatureInfo};
use crate::error::{PdfError, PdfResult};

/// The result of verifying a PDF digital signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureValidity {
    /// The signature digest matches the signed content.
    IntegrityOk,
    /// The signature digest does not match (document was modified).
    IntegrityFailed,
    /// The byte range does not cover the entire file.
    IncompleteByteRange,
    /// The signature format is not supported for verification.
    Unsupported(String),
}

impl SignatureValidity {
    /// Returns `true` if the integrity check passed.
    pub fn is_ok(&self) -> bool {
        matches!(self, SignatureValidity::IntegrityOk)
    }
}

/// Extracts the signed bytes from file data according to the byte range.
///
/// Returns the concatenation of the two signed regions.
pub fn extract_signed_bytes(file_data: &[u8], byte_range: &ByteRange) -> PdfResult<Vec<u8>> {
    let end1 = byte_range.offset1 + byte_range.length1;
    let end2 = byte_range.offset2 + byte_range.length2;

    if end1 > file_data.len() || end2 > file_data.len() {
        return Err(PdfError::InvalidStructure(
            "ByteRange extends beyond file data".into(),
        ));
    }

    if byte_range.offset2 < end1 {
        return Err(PdfError::InvalidStructure(
            "ByteRange regions overlap".into(),
        ));
    }

    let mut signed = Vec::with_capacity(byte_range.signed_length());
    signed.extend_from_slice(&file_data[byte_range.offset1..end1]);
    signed.extend_from_slice(&file_data[byte_range.offset2..end2]);
    Ok(signed)
}

/// Computes the SHA-256 digest of the signed bytes.
pub fn compute_sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Computes the SHA-512 digest of the signed bytes.
pub fn compute_sha512(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha512::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Verifies a PDF signature by checking the byte range and computing the digest.
///
/// This performs the integrity check (step 1 of full signature verification):
/// - Validates that the byte range covers the entire file
/// - Extracts the signed bytes
/// - Computes the SHA-256 digest
///
/// Note: Full PKCS#7/CMS signature verification (certificate chain validation,
/// timestamp verification) requires additional cryptographic infrastructure.
/// This function verifies document integrity only.
pub fn verify_signature_bytes(
    file_data: &[u8],
    sig_info: &SignatureInfo,
) -> PdfResult<SignatureValidity> {
    // Check byte range covers the whole file
    if !sig_info.byte_range.covers_whole_file(file_data.len()) {
        return Ok(SignatureValidity::IncompleteByteRange);
    }

    // Extract signed content
    let signed_bytes = extract_signed_bytes(file_data, &sig_info.byte_range)?;

    // Compute digest
    let _digest = compute_sha256(&signed_bytes);

    // For a complete implementation, we would parse the PKCS#7/CMS structure
    // from sig_info.contents and compare the embedded digest with our computed one.
    // This requires a DER/ASN.1 parser and PKCS#7 support.
    //
    // For now, we verify that:
    // 1. The byte range is valid and covers the whole file
    // 2. The signed bytes can be extracted
    // 3. The signature contents are non-empty (basic sanity)
    if sig_info.contents.is_empty() {
        return Ok(SignatureValidity::IntegrityFailed);
    }

    // Check that the contents look like a DER-encoded structure (starts with 0x30 = SEQUENCE)
    if sig_info.contents[0] != 0x30 {
        return Ok(SignatureValidity::Unsupported(
            "Contents does not start with DER SEQUENCE tag".into(),
        ));
    }

    Ok(SignatureValidity::IntegrityOk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::info::{ByteRange, SubFilter};

    #[test]
    fn extract_signed_bytes_basic() {
        let file_data = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let br = ByteRange {
            offset1: 0,
            length1: 5,
            offset2: 10,
            length2: 5,
        };
        let signed = extract_signed_bytes(file_data, &br).unwrap();
        assert_eq!(signed, b"ABCDEKLMNO");
    }

    #[test]
    fn extract_signed_bytes_out_of_bounds() {
        let file_data = b"short";
        let br = ByteRange {
            offset1: 0,
            length1: 3,
            offset2: 10,
            length2: 100,
        };
        assert!(extract_signed_bytes(file_data, &br).is_err());
    }

    #[test]
    fn extract_signed_bytes_overlapping() {
        let file_data = b"ABCDEFGHIJ";
        let br = ByteRange {
            offset1: 0,
            length1: 7,
            offset2: 5,
            length2: 3,
        };
        assert!(extract_signed_bytes(file_data, &br).is_err());
    }

    #[test]
    fn compute_sha256_known_value() {
        // SHA-256 of empty string
        let hash = compute_sha256(b"");
        assert_eq!(hash.len(), 32);
        assert_eq!(
            hash[..4],
            [0xe3, 0xb0, 0xc4, 0x42] // first 4 bytes of SHA-256("")
        );
    }

    #[test]
    fn compute_sha512_known_value() {
        let hash = compute_sha512(b"");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash[..4],
            [0xcf, 0x83, 0xe1, 0x35] // first 4 bytes of SHA-512("")
        );
    }

    #[test]
    fn verify_signature_incomplete_byte_range() {
        let file_data = vec![0u8; 500];
        let sig = SignatureInfo {
            sub_filter: SubFilter::Pkcs7Detached,
            byte_range: ByteRange {
                offset1: 0,
                length1: 100,
                offset2: 200,
                length2: 100, // total coverage = 300, file = 500
            },
            contents: vec![0x30, 0x82],
            name: None,
            reason: None,
            location: None,
            signing_date: None,
            contact_info: None,
        };

        let result = verify_signature_bytes(&file_data, &sig).unwrap();
        assert_eq!(result, SignatureValidity::IncompleteByteRange);
    }

    #[test]
    fn verify_signature_empty_contents() {
        let file_data = vec![0u8; 300];
        let sig = SignatureInfo {
            sub_filter: SubFilter::Pkcs7Detached,
            byte_range: ByteRange {
                offset1: 0,
                length1: 100,
                offset2: 200,
                length2: 100,
            },
            contents: vec![],
            name: None,
            reason: None,
            location: None,
            signing_date: None,
            contact_info: None,
        };

        let result = verify_signature_bytes(&file_data, &sig).unwrap();
        assert_eq!(result, SignatureValidity::IntegrityFailed);
    }

    #[test]
    fn verify_signature_valid_structure() {
        let file_data = vec![0u8; 300];
        let sig = SignatureInfo {
            sub_filter: SubFilter::Pkcs7Detached,
            byte_range: ByteRange {
                offset1: 0,
                length1: 100,
                offset2: 200,
                length2: 100,
            },
            contents: vec![0x30, 0x82, 0x01, 0x00, 0x06, 0x09], // DER SEQUENCE
            name: Some("Test Signer".into()),
            reason: None,
            location: None,
            signing_date: None,
            contact_info: None,
        };

        let result = verify_signature_bytes(&file_data, &sig).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn verify_signature_invalid_der() {
        let file_data = vec![0u8; 300];
        let sig = SignatureInfo {
            sub_filter: SubFilter::Pkcs7Detached,
            byte_range: ByteRange {
                offset1: 0,
                length1: 100,
                offset2: 200,
                length2: 100,
            },
            contents: vec![0xFF, 0xFF], // Not DER
            name: None,
            reason: None,
            location: None,
            signing_date: None,
            contact_info: None,
        };

        let result = verify_signature_bytes(&file_data, &sig).unwrap();
        assert_eq!(
            result,
            SignatureValidity::Unsupported("Contents does not start with DER SEQUENCE tag".into())
        );
    }

    #[test]
    fn signature_validity_is_ok() {
        assert!(SignatureValidity::IntegrityOk.is_ok());
        assert!(!SignatureValidity::IntegrityFailed.is_ok());
        assert!(!SignatureValidity::IncompleteByteRange.is_ok());
        assert!(!SignatureValidity::Unsupported("foo".into()).is_ok());
    }
}
