//! Standard PDF security handler.
//!
//! Parses the encryption dictionary and provides methods to decrypt
//! PDF objects using the Standard security handler (R2–R4).

use crate::core::objects::{DictExt, Dictionary, Object, ObjectId, PdfString, StringFormat};
use crate::error::{PdfError, PdfResult};

use super::key::{object_key, EncryptionKey};
use super::rc4;

/// The encryption algorithm used by the security handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptAlgorithm {
    /// RC4 encryption (R2/R3).
    Rc4,
    /// AES-128 in CBC mode (R4, /AESV2).
    AesV2,
    /// AES-256 in CBC mode (R5/R6, /AESV3).
    AesV3,
}

/// Parsed document permissions from the `/P` flag.
#[derive(Debug, Clone, Copy)]
pub struct Permissions {
    /// Raw permission flags.
    pub flags: i32,
}

impl Permissions {
    /// Whether printing the document is allowed.
    pub fn can_print(&self) -> bool {
        self.flags & (1 << 2) != 0
    }

    /// Whether modifying the document is allowed.
    pub fn can_modify(&self) -> bool {
        self.flags & (1 << 3) != 0
    }

    /// Whether extracting text/graphics is allowed.
    pub fn can_extract(&self) -> bool {
        self.flags & (1 << 4) != 0
    }

    /// Whether adding annotations is allowed.
    pub fn can_annotate(&self) -> bool {
        self.flags & (1 << 5) != 0
    }
}

/// The encryption handler for a PDF document.
#[derive(Debug, Clone)]
pub struct EncryptionHandler {
    /// The derived encryption key.
    key: EncryptionKey,
    /// The security handler revision.
    revision: u8,
    /// The encryption algorithm.
    algorithm: CryptAlgorithm,
    /// Whether to encrypt metadata streams.
    encrypt_metadata: bool,
    /// Document permissions.
    pub permissions: Permissions,
}

impl EncryptionHandler {
    /// Creates an encryption handler from the `/Encrypt` dictionary.
    ///
    /// # Parameters
    /// - `encrypt_dict`: The `/Encrypt` dictionary from the trailer
    /// - `id_first`: The first element of the file's `/ID` array
    /// - `password`: The user or owner password
    pub fn from_dict(
        encrypt_dict: &Dictionary,
        id_first: &[u8],
        password: &[u8],
    ) -> PdfResult<Self> {
        let revision = encrypt_dict
            .get_i64("R")
            .ok_or_else(|| PdfError::EncryptionError("Encrypt dict missing /R".to_string()))?
            as u8;

        let v = encrypt_dict.get_i64("V").unwrap_or(0) as u8;

        let key_length = encrypt_dict.get_i64("Length").unwrap_or(40) as usize;

        let p_value = encrypt_dict
            .get_i64("P")
            .ok_or_else(|| PdfError::EncryptionError("Encrypt dict missing /P".to_string()))?
            as i32;

        let o_value = extract_string_bytes(encrypt_dict, "O")?;
        let u_value = extract_string_bytes(encrypt_dict, "U")?;

        let encrypt_metadata = encrypt_dict
            .get_str("EncryptMetadata")
            .and_then(|o| o.as_bool())
            .unwrap_or(true);

        // Determine algorithm from V
        let algorithm = match v {
            1 | 2 => CryptAlgorithm::Rc4,
            4 => {
                if Self::uses_aes_v2(encrypt_dict) {
                    CryptAlgorithm::AesV2
                } else {
                    CryptAlgorithm::Rc4
                }
            }
            5 => CryptAlgorithm::AesV3,
            _ => {
                return Err(PdfError::UnsupportedFeature(format!("Encryption V={}", v)));
            }
        };

        // R5/R6: SHA-256/Algorithm 2.B key derivation, AES-256
        if revision == 5 || revision == 6 {
            let ue_value = extract_string_bytes(encrypt_dict, "UE")?;
            let oe_value = extract_string_bytes(encrypt_dict, "OE")?;
            let perms_value = extract_string_bytes(encrypt_dict, "Perms")?;

            // Try user password first, then owner password
            let file_key = if super::key::validate_user_password_r5_r6(password, &u_value, revision)
            {
                super::key::compute_file_key_user(password, &u_value, &ue_value, revision)?
            } else if super::key::validate_owner_password_r5_r6(
                password, &o_value, &u_value, revision,
            ) {
                super::key::compute_file_key_owner(
                    password, &o_value, &oe_value, &u_value, revision,
                )?
            } else {
                return Err(PdfError::InvalidPassword);
            };

            // Validate /Perms
            validate_perms(&file_key, &perms_value, p_value)?;

            return Ok(EncryptionHandler {
                key: EncryptionKey { key: file_key },
                revision,
                algorithm,
                encrypt_metadata,
                permissions: Permissions { flags: p_value },
            });
        }

        // R2–R4: MD5-based key derivation
        let enc_key = EncryptionKey::compute_r2_r4(
            password,
            &o_value,
            p_value,
            id_first,
            key_length,
            revision,
            encrypt_metadata,
        )?;

        if !enc_key.validate_user_password_r2_r4(&u_value, id_first, revision) {
            return Err(PdfError::InvalidPassword);
        }

        Ok(EncryptionHandler {
            key: enc_key,
            revision,
            algorithm,
            encrypt_metadata,
            permissions: Permissions { flags: p_value },
        })
    }

    /// Checks if the /CF dictionary specifies AESV2.
    fn uses_aes_v2(encrypt_dict: &Dictionary) -> bool {
        let cf = match encrypt_dict.get_str("CF").and_then(|o| o.as_dict()) {
            Some(d) => d,
            None => return false,
        };
        // Check /StdCF entry
        let std_cf = match cf.get_str("StdCF").and_then(|o| o.as_dict()) {
            Some(d) => d,
            None => return false,
        };
        std_cf.get_name("CFM") == Some("AESV2")
    }

    /// Decrypts a string value for the given object.
    pub fn decrypt_string(&self, obj_id: ObjectId, data: &[u8]) -> Vec<u8> {
        match self.algorithm {
            CryptAlgorithm::Rc4 => {
                let key = object_key(&self.key.key, obj_id.0, obj_id.1, false);
                rc4::rc4(&key, data)
            }
            CryptAlgorithm::AesV2 => {
                let key = object_key(&self.key.key, obj_id.0, obj_id.1, true);
                decrypt_aes_cbc::<aes::Aes128>(&key, data).unwrap_or_else(|_| data.to_vec())
            }
            CryptAlgorithm::AesV3 => {
                // R5/R6 uses the file key directly (no per-object derivation)
                decrypt_aes_cbc::<aes::Aes256>(&self.key.key, data)
                    .unwrap_or_else(|_| data.to_vec())
            }
        }
    }

    /// Decrypts stream data for the given object.
    pub fn decrypt_stream(&self, obj_id: ObjectId, data: &[u8]) -> Vec<u8> {
        // Stream decryption uses the same algorithm as string decryption
        self.decrypt_string(obj_id, data)
    }

    /// Maximum recursion depth for `decrypt_object` to prevent stack overflow.
    const MAX_DECRYPT_DEPTH: usize = 64;

    /// Decrypts all strings and streams in an object tree.
    pub fn decrypt_object(&self, obj_id: ObjectId, obj: &mut Object) {
        self.decrypt_object_inner(obj_id, obj, Self::MAX_DECRYPT_DEPTH);
    }

    /// Recursive helper with depth limit.
    fn decrypt_object_inner(&self, obj_id: ObjectId, obj: &mut Object, depth: usize) {
        if depth == 0 {
            return;
        }
        match obj {
            Object::String(s) => {
                let decrypted = self.decrypt_string(obj_id, &s.bytes);
                *s = PdfString {
                    bytes: decrypted,
                    format: StringFormat::Literal,
                };
            }
            Object::Stream(stream) => {
                let decrypted = self.decrypt_stream(obj_id, &stream.data);
                stream.data = decrypted;
            }
            Object::Array(arr) => {
                for item in arr.iter_mut() {
                    self.decrypt_object_inner(obj_id, item, depth - 1);
                }
            }
            Object::Dictionary(dict) => {
                for (_, value) in dict.iter_mut() {
                    self.decrypt_object_inner(obj_id, value, depth - 1);
                }
            }
            _ => {}
        }
    }

    /// Whether this handler encrypts metadata.
    pub fn encrypts_metadata(&self) -> bool {
        self.encrypt_metadata
    }

    /// The security handler revision.
    pub fn revision(&self) -> u8 {
        self.revision
    }

    /// The encryption algorithm.
    pub fn algorithm(&self) -> CryptAlgorithm {
        self.algorithm
    }
}

/// Extracts raw bytes from a string entry in a dictionary.
fn extract_string_bytes(dict: &Dictionary, key: &str) -> PdfResult<Vec<u8>> {
    match dict.get_str(key) {
        Some(Object::String(s)) => Ok(s.bytes.clone()),
        _ => Err(PdfError::EncryptionError(format!(
            "Encrypt dict missing /{}",
            key
        ))),
    }
}

/// Validates the `/Perms` value for R5/R6 encryption.
///
/// AES-256-ECB decrypts the 16-byte `/Perms` value with the file key,
/// then verifies bytes 9–11 are `b"adb"` and byte 8 is `b'T'` or `b'F'`.
fn validate_perms(file_key: &[u8], perms_value: &[u8], expected_p: i32) -> PdfResult<()> {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};

    if perms_value.len() < 16 {
        return Err(PdfError::EncryptionError(
            "/Perms too short (need 16 bytes)".to_string(),
        ));
    }

    let cipher = aes::Aes256::new(GenericArray::from_slice(file_key));
    let mut block = GenericArray::clone_from_slice(&perms_value[..16]);
    cipher.decrypt_block(&mut block);

    // Verify marker bytes 9–11 == "adb"
    if &block[9..12] != b"adb" {
        return Err(PdfError::EncryptionError(
            "/Perms validation failed: marker bytes incorrect".to_string(),
        ));
    }

    // Verify byte 8 is 'T' (encrypt metadata) or 'F' (don't encrypt)
    if block[8] != b'T' && block[8] != b'F' {
        return Err(PdfError::EncryptionError(
            "/Perms validation failed: metadata flag not T or F".to_string(),
        ));
    }

    // Verify P value matches (bytes 0–3, little-endian)
    let perms_p = i32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    if perms_p != expected_p {
        return Err(PdfError::EncryptionError(format!(
            "/Perms P value mismatch: expected {}, got {}",
            expected_p, perms_p
        )));
    }

    Ok(())
}

/// Decrypts AES-CBC data with a 16-byte IV prepended.
///
/// Generic over the AES cipher (Aes128 or Aes256).
fn decrypt_aes_cbc<C>(key: &[u8], data: &[u8]) -> PdfResult<Vec<u8>>
where
    C: aes::cipher::BlockDecryptMut + aes::cipher::BlockCipher,
    cbc::Decryptor<C>: aes::cipher::KeyIvInit,
{
    use aes::cipher::{BlockDecryptMut, KeyIvInit};

    if data.len() < 16 {
        return Err(PdfError::EncryptionError(
            "AES data too short for IV".to_string(),
        ));
    }

    let (iv, ciphertext) = data.split_at(16);
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(PdfError::EncryptionError(
            "AES ciphertext not block-aligned".to_string(),
        ));
    }

    let mut buf = ciphertext.to_vec();
    let decryptor = cbc::Decryptor::<C>::new_from_slices(key, iv)
        .map_err(|e| PdfError::EncryptionError(format!("AES init: {}", e)))?;

    let plaintext = decryptor
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| PdfError::EncryptionError(format!("AES decrypt: {}", e)))?;

    Ok(plaintext.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissions_parsing() {
        // -4 = 0xFFFFFFFC — all bits set except 0 and 1
        let perms = Permissions { flags: -4 };
        assert!(perms.can_print());
        assert!(perms.can_modify());
        assert!(perms.can_extract());
        assert!(perms.can_annotate());
    }

    #[test]
    fn permissions_restricted() {
        // Only bit 2 set (print only)
        let perms = Permissions { flags: 4 };
        assert!(perms.can_print());
        assert!(!perms.can_modify());
        assert!(!perms.can_extract());
        assert!(!perms.can_annotate());
    }

    #[test]
    fn aes128_round_trip() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let key = [0x42u8; 16];
        let iv = [0x00u8; 16];
        let plaintext = b"Hello, AES-128!!"; // exactly 16 bytes

        // Encrypt
        let encryptor = Aes128CbcEnc::new_from_slices(&key, &iv).unwrap();
        let mut buf = [0u8; 32]; // 16 bytes plaintext + 16 bytes PKCS7 padding
        buf[..16].copy_from_slice(plaintext);
        let ciphertext = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, 16)
            .unwrap();

        // Prepend IV
        let mut data = iv.to_vec();
        data.extend_from_slice(ciphertext);

        // Decrypt
        let decrypted = decrypt_aes_cbc::<aes::Aes128>(&key, &data).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes128_too_short() {
        let result = decrypt_aes_cbc::<aes::Aes128>(&[0u8; 16], &[0u8; 8]);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_object_string() {
        let handler = make_test_handler();
        let obj_id = (1, 0);
        let mut obj = Object::String(PdfString {
            bytes: vec![0x01, 0x02, 0x03],
            format: StringFormat::Literal,
        });
        handler.decrypt_object(obj_id, &mut obj);
        // After decryption, bytes should be different (RC4 transformed)
        if let Object::String(s) = &obj {
            assert_ne!(s.bytes, vec![0x01, 0x02, 0x03]);
        } else {
            panic!("Expected String");
        }
    }

    #[test]
    fn decrypt_object_recursion() {
        let handler = make_test_handler();
        let obj_id = (1, 0);
        let inner = Object::String(PdfString {
            bytes: vec![0x42],
            format: StringFormat::Literal,
        });
        let mut obj = Object::Array(vec![inner]);
        handler.decrypt_object(obj_id, &mut obj);
        // Verify the string inside the array was decrypted
        if let Object::Array(arr) = &obj {
            if let Object::String(s) = &arr[0] {
                assert_ne!(s.bytes, vec![0x42]);
            }
        }
    }

    #[test]
    fn from_dict_missing_r() {
        use crate::core::objects::PdfName;
        use std::collections::BTreeMap;

        let mut dict = BTreeMap::new();
        dict.insert(PdfName::new("V"), Object::Integer(1));
        dict.insert(PdfName::new("P"), Object::Integer(-4));
        // Missing /R
        let result = EncryptionHandler::from_dict(&dict, b"file-id", b"");
        assert!(result.is_err());
    }

    #[test]
    fn from_dict_unsupported_v() {
        use crate::core::objects::PdfName;
        use std::collections::BTreeMap;

        let mut dict = BTreeMap::new();
        dict.insert(PdfName::new("R"), Object::Integer(2));
        dict.insert(PdfName::new("V"), Object::Integer(5)); // unsupported
        dict.insert(PdfName::new("P"), Object::Integer(-4));
        dict.insert(
            PdfName::new("O"),
            Object::String(PdfString {
                bytes: vec![0; 32],
                format: StringFormat::Literal,
            }),
        );
        dict.insert(
            PdfName::new("U"),
            Object::String(PdfString {
                bytes: vec![0; 32],
                format: StringFormat::Literal,
            }),
        );
        let result = EncryptionHandler::from_dict(&dict, b"file-id", b"");
        assert!(result.is_err());
    }

    /// Creates a test encryption handler with RC4 and a known key.
    fn make_test_handler() -> EncryptionHandler {
        EncryptionHandler {
            key: EncryptionKey {
                key: vec![0x01, 0x02, 0x03, 0x04, 0x05],
            },
            revision: 2,
            algorithm: CryptAlgorithm::Rc4,
            encrypt_metadata: true,
            permissions: Permissions { flags: -4 },
        }
    }

    // --- R5/R6 (AES-256) tests ---

    /// Builds a synthetic AES-256-ECB encrypted /Perms value for testing.
    fn build_perms(file_key: &[u8], p_value: i32, encrypt_meta: bool) -> Vec<u8> {
        use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
        let mut plaintext = [0u8; 16];
        plaintext[0..4].copy_from_slice(&(p_value as u32).to_le_bytes());
        plaintext[4..8].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        plaintext[8] = if encrypt_meta { b'T' } else { b'F' };
        plaintext[9..12].copy_from_slice(b"adb");
        let cipher = aes::Aes256::new(GenericArray::from_slice(file_key));
        let mut block = GenericArray::clone_from_slice(&plaintext);
        cipher.encrypt_block(&mut block);
        block.to_vec()
    }

    #[test]
    fn validate_perms_valid() {
        let file_key = [0xAB; 32];
        let perms = build_perms(&file_key, -4, true);
        assert!(validate_perms(&file_key, &perms, -4).is_ok());
    }

    #[test]
    fn validate_perms_wrong_key() {
        let file_key = [0xAB; 32];
        let perms = build_perms(&file_key, -4, true);
        assert!(validate_perms(&[0xCD; 32], &perms, -4).is_err());
    }

    #[test]
    fn validate_perms_too_short() {
        assert!(validate_perms(&[0; 32], &[0; 8], -4).is_err());
    }

    #[test]
    fn validate_perms_p_mismatch() {
        let file_key = [0xAB; 32];
        let perms = build_perms(&file_key, -4, true);
        // Expect P = -100, but encrypted P = -4
        assert!(validate_perms(&file_key, &perms, -100).is_err());
    }

    /// Builds a complete synthetic R5 encryption dictionary for testing.
    fn build_r5_encrypt_dict(password: &[u8], file_key: &[u8; 32]) -> Dictionary {
        use crate::core::objects::PdfName;
        use aes::cipher::{BlockEncryptMut, KeyIvInit};

        let p_value: i32 = -4;
        let u_vsalt = [0x11; 8];
        let u_ksalt = [0x22; 8];
        let o_vsalt = [0x33; 8];
        let o_ksalt = [0x44; 8];

        // Build /U
        let u_hash = crate::encryption::key::hash_r5(password, &u_vsalt, &[]);
        let mut u_value = u_hash;
        u_value.extend_from_slice(&u_vsalt);
        u_value.extend_from_slice(&u_ksalt);

        // Build /O (owner password = same as user for this test)
        let o_hash = crate::encryption::key::hash_r5(password, &o_vsalt, &u_value);
        let mut o_value = o_hash;
        o_value.extend_from_slice(&o_vsalt);
        o_value.extend_from_slice(&o_ksalt);

        // Build /UE: AES-256-CBC encrypt file_key
        let u_intermediate = crate::encryption::key::hash_r5(password, &u_ksalt, &[]);
        let enc =
            cbc::Encryptor::<aes::Aes256>::new_from_slices(&u_intermediate, &[0; 16]).unwrap();
        let mut ue_buf = file_key.to_vec();
        let ue_len = ue_buf.len();
        let ue_value = enc
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut ue_buf, ue_len)
            .unwrap()
            .to_vec();

        // Build /OE
        let o_intermediate = crate::encryption::key::hash_r5(password, &o_ksalt, &u_value);
        let enc =
            cbc::Encryptor::<aes::Aes256>::new_from_slices(&o_intermediate, &[0; 16]).unwrap();
        let mut oe_buf = file_key.to_vec();
        let oe_len = oe_buf.len();
        let oe_value = enc
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut oe_buf, oe_len)
            .unwrap()
            .to_vec();

        // Build /Perms
        let perms_value = build_perms(file_key, p_value, true);

        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("R"), Object::Integer(5));
        dict.insert(PdfName::new("V"), Object::Integer(5));
        dict.insert(PdfName::new("P"), Object::Integer(p_value as i64));
        dict.insert(PdfName::new("Length"), Object::Integer(256));
        dict.insert(
            PdfName::new("U"),
            Object::String(PdfString {
                bytes: u_value,
                format: StringFormat::Literal,
            }),
        );
        dict.insert(
            PdfName::new("O"),
            Object::String(PdfString {
                bytes: o_value,
                format: StringFormat::Literal,
            }),
        );
        dict.insert(
            PdfName::new("UE"),
            Object::String(PdfString {
                bytes: ue_value,
                format: StringFormat::Literal,
            }),
        );
        dict.insert(
            PdfName::new("OE"),
            Object::String(PdfString {
                bytes: oe_value,
                format: StringFormat::Literal,
            }),
        );
        dict.insert(
            PdfName::new("Perms"),
            Object::String(PdfString {
                bytes: perms_value,
                format: StringFormat::Literal,
            }),
        );
        dict
    }

    #[test]
    fn from_dict_r5_user_password() {
        let file_key = [0xAA; 32];
        let dict = build_r5_encrypt_dict(b"", &file_key);
        let handler = EncryptionHandler::from_dict(&dict, &[], b"").unwrap();
        assert_eq!(handler.algorithm(), CryptAlgorithm::AesV3);
        assert_eq!(handler.revision(), 5);
        assert_eq!(handler.key.key, file_key);
    }

    #[test]
    fn from_dict_r5_wrong_password() {
        let file_key = [0xAA; 32];
        let dict = build_r5_encrypt_dict(b"correct", &file_key);
        let result = EncryptionHandler::from_dict(&dict, &[], b"wrong");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_string_aesv3() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        let file_key = [0xDD; 32];
        let handler = EncryptionHandler {
            key: EncryptionKey {
                key: file_key.to_vec(),
            },
            revision: 6,
            algorithm: CryptAlgorithm::AesV3,
            encrypt_metadata: true,
            permissions: Permissions { flags: -4 },
        };

        let iv = [0x00; 16];
        let plaintext = b"Hello, AES-256!!"; // 16 bytes
        let encryptor = cbc::Encryptor::<aes::Aes256>::new_from_slices(&file_key, &iv).unwrap();
        let mut buf = [0u8; 32]; // 16 bytes + 16 bytes PKCS7 padding
        buf[..16].copy_from_slice(plaintext);
        let ciphertext = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, 16)
            .unwrap();

        let mut data = iv.to_vec();
        data.extend_from_slice(ciphertext);

        let decrypted = handler.decrypt_string((1, 0), &data);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_stream_aesv3_no_per_object_key() {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        let file_key = [0xDD; 32];
        let handler = EncryptionHandler {
            key: EncryptionKey {
                key: file_key.to_vec(),
            },
            revision: 6,
            algorithm: CryptAlgorithm::AesV3,
            encrypt_metadata: true,
            permissions: Permissions { flags: -4 },
        };

        let iv = [0x42; 16];
        let plaintext = b"stream data here"; // 16 bytes
        let encryptor = cbc::Encryptor::<aes::Aes256>::new_from_slices(&file_key, &iv).unwrap();
        let mut buf = [0u8; 32];
        buf[..16].copy_from_slice(plaintext);
        let ciphertext = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, 16)
            .unwrap();
        let mut data = iv.to_vec();
        data.extend_from_slice(ciphertext);

        // Different object IDs should produce identical decryption (no per-object key)
        let d1 = handler.decrypt_stream((1, 0), &data);
        let d2 = handler.decrypt_stream((999, 5), &data);
        assert_eq!(d1, d2);
        assert_eq!(d1, plaintext);
    }
}
