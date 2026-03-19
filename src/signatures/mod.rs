//! PDF digital signature support.
//!
//! Parses and verifies digital signatures in PDF documents per
//! ISO 32000-2:2020, Section 12.8. Supports PKCS#7 / CMS signatures
//! with SHA-256 and SHA-512 digests.

mod info;
mod verify;

pub use info::{SignatureInfo, SubFilter};
pub use verify::{compute_sha256, compute_sha512, verify_signature_bytes, SignatureValidity};
