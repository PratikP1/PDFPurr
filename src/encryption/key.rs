//! PDF encryption key derivation.
//!
//! Implements key derivation algorithms for the Standard security handler:
//! - R2–R4: Algorithm 2 per ISO 32000-1:2008, Section 7.6.3.3 (MD5-based)
//! - R5: SHA-256 based key derivation (deprecated but common in the wild)
//! - R6: Algorithm 2.B per ISO 32000-2:2020, Section 7.6.4.3.4 (iterative SHA-256/384/512)

use crate::error::{PdfError, PdfResult};

/// Padding string used in PDF password-based key derivation (32 bytes).
/// ISO 32000-1:2008, Table 20.
const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// An encryption key derived from a PDF password.
#[derive(Debug, Clone)]
pub struct EncryptionKey {
    /// The derived key bytes.
    pub key: Vec<u8>,
}

impl EncryptionKey {
    /// Computes the encryption key using Algorithm 2
    /// (ISO 32000-1:2008, Section 7.6.3.3).
    ///
    /// # Parameters
    /// - `password`: The user password (may be empty)
    /// - `o_value`: The `/O` value from the encryption dictionary (32 bytes)
    /// - `p_value`: The `/P` permissions value (as i32)
    /// - `id_first`: The first element of the file's `/ID` array
    /// - `key_length`: The key length in bits (40, 56, 64, 80, 96, 112, or 128)
    /// - `revision`: The security handler revision (2, 3, or 4)
    /// - `encrypt_metadata`: Whether metadata is encrypted (R4 only)
    pub fn compute_r2_r4(
        password: &[u8],
        o_value: &[u8],
        p_value: i32,
        id_first: &[u8],
        key_length: usize,
        revision: u8,
        encrypt_metadata: bool,
    ) -> PdfResult<Self> {
        let key_bytes = key_length / 8;

        // Step a: Pad or truncate the password to exactly 32 bytes
        let mut padded = Vec::with_capacity(32);
        padded.extend_from_slice(&password[..password.len().min(32)]);
        if padded.len() < 32 {
            padded.extend_from_slice(&PADDING[..32 - padded.len()]);
        }

        // Step b: Initialize MD5 hash
        let mut ctx = md5::Context::new();

        // Step c: Pass padded password
        ctx.consume(&padded);

        // Step d: Pass /O value
        ctx.consume(o_value);

        // Step e: Pass /P value as unsigned little-endian 4 bytes
        ctx.consume((p_value as u32).to_le_bytes());

        // Step f: Pass first element of /ID array
        ctx.consume(id_first);

        // Step g: (R4 only) If metadata is not encrypted, pass 4 bytes of 0xFF
        if revision >= 4 && !encrypt_metadata {
            ctx.consume([0xFF, 0xFF, 0xFF, 0xFF]);
        }

        let mut digest = ctx.compute().0;

        // Step h: (R3+) Do 50 rounds of MD5 on the first n bytes
        if revision >= 3 {
            for _ in 0..50 {
                let hash = md5::compute(&digest[..key_bytes]);
                digest = hash.0;
            }
        }

        Ok(EncryptionKey {
            key: digest[..key_bytes].to_vec(),
        })
    }

    /// Validates a user password by computing the `/U` value and comparing.
    ///
    /// For R2: Algorithm 4 (simple MD5 of padding + key → RC4).
    /// For R3/R4: Algorithm 5 (MD5 of padding + ID, then 20 rounds of RC4).
    pub fn validate_user_password_r2_r4(
        &self,
        u_value: &[u8],
        id_first: &[u8],
        revision: u8,
    ) -> bool {
        match revision {
            2 => {
                // Algorithm 4: RC4-encrypt the padding with the key
                let computed_u = super::rc4::rc4(&self.key, &PADDING);
                computed_u == u_value
            }
            3 | 4 => {
                // Algorithm 5:
                // Step a: MD5 hash of padding + first ID element
                let mut ctx = md5::Context::new();
                ctx.consume(PADDING);
                ctx.consume(id_first);
                let hash = ctx.compute().0;

                // Step b: RC4-encrypt with the key
                let mut result = super::rc4::rc4(&self.key, &hash);

                // Step c: 19 additional rounds with modified keys
                for i in 1..=19u8 {
                    let modified_key: Vec<u8> = self.key.iter().map(|&b| b ^ i).collect();
                    result = super::rc4::rc4(&modified_key, &result);
                }

                // Compare first 16 bytes only (rest is random padding)
                result[..16] == u_value[..16]
            }
            _ => false,
        }
    }
}

/// Derives a per-object encryption key from the file encryption key.
///
/// Algorithm 1 from ISO 32000-1:2008, Section 7.6.2.
/// When `aes` is true, appends the `b"sAlT"` suffix required for AES.
pub fn object_key(file_key: &[u8], obj_num: u32, gen_num: u16, aes: bool) -> Vec<u8> {
    let extra = if aes { 9 } else { 5 };
    let mut data = Vec::with_capacity(file_key.len() + extra);
    data.extend_from_slice(file_key);
    data.push((obj_num & 0xFF) as u8);
    data.push(((obj_num >> 8) & 0xFF) as u8);
    data.push(((obj_num >> 16) & 0xFF) as u8);
    data.push((gen_num & 0xFF) as u8);
    data.push(((gen_num >> 8) & 0xFF) as u8);
    if aes {
        data.extend_from_slice(b"sAlT");
    }

    let digest = md5::compute(&data);
    let key_len = (file_key.len() + 5).min(16);
    digest.0[..key_len].to_vec()
}

// ---------------------------------------------------------------------------
// R5/R6 (V=5) key derivation — ISO 32000-2:2020, Section 7.6.4
// ---------------------------------------------------------------------------

/// Maximum password length for R5/R6 (bytes, UTF-8).
const MAX_PASSWORD_LEN: usize = 127;

/// /U and /O value layout: 32-byte hash + 8-byte validation salt + 8-byte key salt.
const HASH_LEN: usize = 32;
const VALIDATION_SALT_OFFSET: usize = 32;
const KEY_SALT_OFFSET: usize = 40;
const UV_OV_LEN: usize = 48;

/// Truncates a password to the R5/R6 maximum of 127 bytes.
fn truncate_password(password: &[u8]) -> &[u8] {
    &password[..password.len().min(MAX_PASSWORD_LEN)]
}

/// Computes the R5 or R6 hash for the given password, salt, and extra data.
///
/// R5 uses plain SHA-256; R6 uses the iterative Algorithm 2.B.
fn hash_for_revision(password: &[u8], salt: &[u8], extra: &[u8], revision: u8) -> Vec<u8> {
    if revision == 5 {
        hash_r5(password, salt, extra)
    } else {
        algorithm_2b(password, salt, extra)
    }
}

/// Validates a user password for R5 or R6.
///
/// `/U` is 48 bytes: `hash[32] || validation_salt[8] || key_salt[8]`.
/// For R5: `SHA-256(password || validation_salt)` must equal `hash`.
/// For R6: `Algorithm2B(password, validation_salt, &[])` must equal `hash`.
pub fn validate_user_password_r5_r6(password: &[u8], u_value: &[u8], revision: u8) -> bool {
    if u_value.len() < UV_OV_LEN {
        return false;
    }
    let pw = truncate_password(password);
    let computed = hash_for_revision(
        pw,
        &u_value[VALIDATION_SALT_OFFSET..KEY_SALT_OFFSET],
        &[],
        revision,
    );
    computed == u_value[..HASH_LEN]
}

/// Validates an owner password for R5 or R6.
///
/// `/O` is 48 bytes: `hash[32] || validation_salt[8] || key_salt[8]`.
/// Hash input includes the full 48-byte `/U` value.
pub fn validate_owner_password_r5_r6(
    password: &[u8],
    o_value: &[u8],
    u_value: &[u8],
    revision: u8,
) -> bool {
    if o_value.len() < UV_OV_LEN {
        return false;
    }
    let pw = truncate_password(password);
    let computed = hash_for_revision(
        pw,
        &o_value[VALIDATION_SALT_OFFSET..KEY_SALT_OFFSET],
        u_value,
        revision,
    );
    computed == o_value[..HASH_LEN]
}

/// Derives the 32-byte file encryption key for R5/R6 (user password path).
///
/// The intermediate key is derived from `password || key_salt`, then used to
/// AES-256-CBC decrypt `/UE` (32 bytes, zero IV, no padding) to recover the
/// file encryption key.
pub fn compute_file_key_user(
    password: &[u8],
    u_value: &[u8],
    ue_value: &[u8],
    revision: u8,
) -> PdfResult<Vec<u8>> {
    if u_value.len() < UV_OV_LEN || ue_value.len() < HASH_LEN {
        return Err(PdfError::EncryptionError(
            "R5/R6 /U or /UE too short".to_string(),
        ));
    }
    let pw = truncate_password(password);
    let intermediate_key =
        hash_for_revision(pw, &u_value[KEY_SALT_OFFSET..UV_OV_LEN], &[], revision);
    decrypt_aes256_cbc_no_pad(&intermediate_key, &[0u8; 16], &ue_value[..HASH_LEN])
}

/// Derives the 32-byte file encryption key for R5/R6 (owner password path).
pub fn compute_file_key_owner(
    password: &[u8],
    o_value: &[u8],
    oe_value: &[u8],
    u_value: &[u8],
    revision: u8,
) -> PdfResult<Vec<u8>> {
    if o_value.len() < UV_OV_LEN || oe_value.len() < HASH_LEN {
        return Err(PdfError::EncryptionError(
            "R5/R6 /O or /OE too short".to_string(),
        ));
    }
    let pw = truncate_password(password);
    let intermediate_key =
        hash_for_revision(pw, &o_value[KEY_SALT_OFFSET..UV_OV_LEN], u_value, revision);
    decrypt_aes256_cbc_no_pad(&intermediate_key, &[0u8; 16], &oe_value[..HASH_LEN])
}

/// R5 hash: `SHA-256(password || salt || extra)`.
///
/// Exposed as `pub(crate)` so tests in sibling modules can build synthetic values.
pub(crate) fn hash_r5(password: &[u8], salt: &[u8], extra: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(extra);
    hasher.finalize().to_vec()
}

/// Algorithm 2.B — R6 iterative hash (ISO 32000-2:2020, Section 7.6.4.3.4).
///
/// Rotates between SHA-256, SHA-384, and SHA-512 with AES-128-CBC
/// intermediate rounds. Runs at least 64 iterations.
fn algorithm_2b(password: &[u8], salt: &[u8], u_value: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    use sha2::{Digest, Sha256, Sha384, Sha512};

    /// Number of times the input block is repeated (ISO 32000-2, Algorithm 2.B step b).
    const REPEAT_COUNT: usize = 64;
    /// Minimum number of rounds before the termination check can succeed.
    const MIN_ROUNDS: u32 = 64;
    /// Offset subtracted from round number in the termination condition.
    const TERMINATION_OFFSET: u32 = 32;

    // Step a: initial hash K = SHA-256(password || salt || u_value)
    let mut k: Vec<u8> = {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        h.update(u_value);
        h.finalize().to_vec()
    };

    // Pre-allocate K1 buffer. K length varies (32/48/64 bytes) across
    // iterations, so we size for the largest case and reuse.
    let max_single = password.len() + 64 + u_value.len();
    let mut k1 = Vec::with_capacity(max_single * REPEAT_COUNT);

    let mut round: u32 = 0;
    loop {
        // Step b: Build K1 = (password || K || u_value) repeated 64 times.
        // K may be 32, 48, or 64 bytes depending on which hash was used.
        k1.clear();
        for _ in 0..REPEAT_COUNT {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(u_value);
        }

        // Step c: AES-128-CBC encrypt K1 with key=K[0..16], iv=K[16..32].
        // K is always >= 32 bytes (minimum SHA-256 output).
        let encryptor = cbc::Encryptor::<aes::Aes128>::new_from_slices(&k[..16], &k[16..32])
            .expect("AES-128 key=K[0..16] and IV=K[16..32] are always 16 bytes");

        // K1 length is always a multiple of 16: REPEAT_COUNT (64) is a multiple
        // of 16, so any single_block_len * 64 is divisible by 16.
        let k1_len = k1.len();
        let encrypted = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut k1, k1_len)
            .expect("K1 length is always block-aligned (64 * block_len)");

        // Step d: Determine hash function from first 16 bytes mod 3.
        // A 128-bit big-endian integer mod 3 equals the byte-sum mod 3
        // because 256 mod 3 == 1.
        let remainder: u32 = encrypted.iter().take(16).map(|&b| b as u32).sum::<u32>() % 3;

        // Save last byte before encrypted slice is invalidated by hash
        let last_byte = *encrypted.last().unwrap_or(&0);

        // Step e: Hash the encrypted data with the selected function.
        // K retains the FULL hash output (32, 48, or 64 bytes) per the spec.
        // Only the final return truncates to 32.
        k = match remainder {
            0 => Sha256::digest(encrypted).to_vec(),
            1 => Sha384::digest(encrypted).to_vec(),
            _ => Sha512::digest(encrypted).to_vec(),
        };

        // Step f: Termination — at least MIN_ROUNDS, then check last byte
        if round >= MIN_ROUNDS - 1 && (last_byte as u32) <= round - TERMINATION_OFFSET {
            break;
        }
        round += 1;
    }

    k[..32].to_vec()
}

/// AES-256-CBC decrypt without PKCS7 padding.
///
/// Used for decrypting `/UE` and `/OE` which are exactly 32 bytes
/// (block-aligned, no padding applied).
fn decrypt_aes256_cbc_no_pad(key: &[u8], iv: &[u8], data: &[u8]) -> PdfResult<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};

    if !data.len().is_multiple_of(16) {
        return Err(PdfError::EncryptionError(
            "AES-256 no-pad: data not block-aligned".to_string(),
        ));
    }

    let mut buf = data.to_vec();
    let decryptor = cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
        .map_err(|e| PdfError::EncryptionError(format!("AES-256 init: {}", e)))?;

    let len = decryptor
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| PdfError::EncryptionError(format!("AES-256 decrypt: {}", e)))?
        .len();

    buf.truncate(len);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_is_32_bytes() {
        assert_eq!(PADDING.len(), 32);
    }

    #[test]
    fn object_key_derivation() {
        let file_key = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let key = object_key(&file_key, 10, 0, false);
        // Key length should be min(file_key.len() + 5, 16) = 10
        assert_eq!(key.len(), 10);
    }

    #[test]
    fn object_key_aes_derivation() {
        let file_key = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let key = object_key(&file_key, 10, 0, true);
        assert_eq!(key.len(), 10);
        // AES key should differ from RC4 key (due to "sAlT" suffix)
        let rc4_key = object_key(&file_key, 10, 0, false);
        assert_ne!(key, rc4_key);
    }

    #[test]
    fn compute_key_r2() {
        // Test with empty password
        let o_value = [0u8; 32];
        let id = b"test-file-id";
        let key = EncryptionKey::compute_r2_r4(b"", &o_value, -4, id, 40, 2, true).unwrap();
        assert_eq!(key.key.len(), 5); // 40 bits = 5 bytes
    }

    #[test]
    fn validate_password_round_trip_r2() {
        let o_value = [0u8; 32];
        let id = b"test-file-id";
        let key = EncryptionKey::compute_r2_r4(b"", &o_value, -4, id, 40, 2, true).unwrap();

        // Generate /U value the same way the validator checks it
        let u_value = super::super::rc4::rc4(&key.key, &PADDING);
        assert!(key.validate_user_password_r2_r4(&u_value, id, 2));
    }

    // --- R5/R6 tests ---

    /// Test helper: AES-256-CBC encrypt with no padding (for building /UE, /OE).
    fn encrypt_aes256_no_pad(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Vec<u8> {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        let encryptor = cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv).unwrap();
        let mut buf = plaintext.to_vec();
        let len = buf.len();
        encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, len)
            .unwrap();
        buf
    }

    /// Builds a synthetic R5 /U value for testing.
    fn build_r5_u_value(password: &[u8], validation_salt: &[u8; 8], key_salt: &[u8; 8]) -> Vec<u8> {
        let hash = hash_r5(password, validation_salt, &[]);
        let mut u = hash;
        u.extend_from_slice(validation_salt);
        u.extend_from_slice(key_salt);
        u
    }

    #[test]
    fn r5_user_password_valid() {
        let u = build_r5_u_value(b"secret", &[0x11; 8], &[0x22; 8]);
        assert!(validate_user_password_r5_r6(b"secret", &u, 5));
    }

    #[test]
    fn r5_user_password_wrong() {
        let u = build_r5_u_value(b"secret", &[0x11; 8], &[0x22; 8]);
        assert!(!validate_user_password_r5_r6(b"wrong", &u, 5));
    }

    #[test]
    fn r5_user_password_truncated_u() {
        assert!(!validate_user_password_r5_r6(b"", &[0u8; 40], 5));
    }

    #[test]
    fn r5_owner_password_valid() {
        let u_value = [0xAA; 48];
        let vsalt = [0x33; 8];
        let ksalt = [0x44; 8];
        let hash = hash_r5(b"owner", &vsalt, &u_value);
        let mut o = hash;
        o.extend_from_slice(&vsalt);
        o.extend_from_slice(&ksalt);
        assert!(validate_owner_password_r5_r6(b"owner", &o, &u_value, 5));
    }

    #[test]
    fn r5_owner_password_wrong() {
        let u_value = [0xAA; 48];
        let vsalt = [0x33; 8];
        let ksalt = [0x44; 8];
        let hash = hash_r5(b"owner", &vsalt, &u_value);
        let mut o = hash;
        o.extend_from_slice(&vsalt);
        o.extend_from_slice(&ksalt);
        assert!(!validate_owner_password_r5_r6(
            b"not-owner",
            &o,
            &u_value,
            5
        ));
    }

    #[test]
    fn r5_file_key_user_round_trip() {
        let password = b"test";
        let vsalt = [0x11; 8];
        let ksalt = [0x22; 8];
        let file_key = [0xFF; 32];
        let u_value = build_r5_u_value(password, &vsalt, &ksalt);

        let intermediate_key = hash_r5(password, &ksalt, &[]);
        let ue_value = encrypt_aes256_no_pad(&intermediate_key, &[0u8; 16], &file_key);

        let derived = compute_file_key_user(password, &u_value, &ue_value, 5).unwrap();
        assert_eq!(derived, file_key);
    }

    #[test]
    fn r5_file_key_owner_round_trip() {
        let password = b"owner";
        let u_value = [0xBB; 48];
        let vsalt = [0x33; 8];
        let ksalt = [0x44; 8];
        let file_key = [0xEE; 32];

        let hash = hash_r5(password, &vsalt, &u_value);
        let mut o_value = hash;
        o_value.extend_from_slice(&vsalt);
        o_value.extend_from_slice(&ksalt);

        let intermediate_key = hash_r5(password, &ksalt, &u_value);
        let oe_value = encrypt_aes256_no_pad(&intermediate_key, &[0u8; 16], &file_key);

        let derived = compute_file_key_owner(password, &o_value, &oe_value, &u_value, 5).unwrap();
        assert_eq!(derived, file_key);
    }

    #[test]
    fn algorithm_2b_deterministic() {
        let salt = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let r1 = algorithm_2b(b"password", &salt, &[]);
        let r2 = algorithm_2b(b"password", &salt, &[]);
        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 32);
    }

    #[test]
    fn algorithm_2b_different_passwords_differ() {
        let salt = [0x01; 8];
        let r1 = algorithm_2b(b"pass1", &salt, &[]);
        let r2 = algorithm_2b(b"pass2", &salt, &[]);
        assert_ne!(r1, r2);
    }

    #[test]
    fn algorithm_2b_with_u_value_differs() {
        let salt = [0x01; 8];
        let r1 = algorithm_2b(b"pass", &salt, &[]);
        let r2 = algorithm_2b(b"pass", &salt, &[0xAA; 48]);
        assert_ne!(r1, r2);
    }

    #[test]
    fn algorithm_2b_empty_password() {
        let salt = [0u8; 8];
        let result = algorithm_2b(b"", &salt, &[]);
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn r6_user_password_round_trip() {
        let password = b"r6pass";
        let vsalt = [0x55; 8];
        let ksalt = [0x66; 8];

        // Build /U using algorithm_2b
        let hash = algorithm_2b(password, &vsalt, &[]);
        let mut u_value = hash.clone();
        u_value.extend_from_slice(&vsalt);
        u_value.extend_from_slice(&ksalt);

        assert!(validate_user_password_r5_r6(password, &u_value, 6));
        assert!(!validate_user_password_r5_r6(b"wrong", &u_value, 6));
    }

    #[test]
    fn r6_file_key_user_round_trip() {
        let password = b"r6key";
        let vsalt = [0x77; 8];
        let ksalt = [0x88; 8];
        let file_key = [0xDD; 32];

        let hash = algorithm_2b(password, &vsalt, &[]);
        let mut u_value = hash;
        u_value.extend_from_slice(&vsalt);
        u_value.extend_from_slice(&ksalt);

        let intermediate_key = algorithm_2b(password, &ksalt, &[]);
        let ue_value = encrypt_aes256_no_pad(&intermediate_key, &[0u8; 16], &file_key);

        let derived = compute_file_key_user(password, &u_value, &ue_value, 6).unwrap();
        assert_eq!(derived, file_key);
    }

    #[test]
    fn decrypt_aes256_cbc_no_pad_round_trip() {
        let key = [0xAB; 32];
        let iv = [0x00; 16];
        let plaintext = [0x42; 32]; // two blocks

        let ciphertext = encrypt_aes256_no_pad(&key, &iv, &plaintext);

        let decrypted = decrypt_aes256_cbc_no_pad(&key, &iv, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_aes256_cbc_no_pad_misaligned() {
        let result = decrypt_aes256_cbc_no_pad(&[0; 32], &[0; 16], &[0; 15]);
        assert!(result.is_err());
    }
}
