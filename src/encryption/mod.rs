//! PDF encryption and security handler support.
//!
//! Implements the Standard security handler for decrypting
//! password-protected PDF files per ISO 32000-2:2020, Section 7.6.
//!
//! Supports:
//! - R2/R3: MD5-based key derivation, RC4 cipher (V=1, V=2)
//! - R4: MD5-based key derivation, RC4 or AES-128 cipher (V=4)
//! - R5: SHA-256 key derivation, AES-256 cipher (V=5, deprecated but common)
//! - R6: Algorithm 2.B iterative key derivation, AES-256 cipher (V=5, ISO 32000-2)

mod key;
mod rc4;
mod standard;

pub use key::EncryptionKey;
pub use standard::{CryptAlgorithm, EncryptionHandler, Permissions};
