//! RC4 stream cipher implementation.
//!
//! Used by PDF encryption revisions R2–R4 for encrypting strings
//! and streams. This is a minimal implementation sufficient for
//! PDF decryption (RC4 is symmetric: encrypt == decrypt).

/// Encrypts/decrypts data using the RC4 stream cipher.
///
/// RC4 is symmetric, so the same function handles both encryption
/// and decryption.
pub(crate) fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }

    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;

    // Key-Scheduling Algorithm (KSA)
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }

    // Pseudo-Random Generation Algorithm (PRGA)
    let mut i: u8 = 0;
    j = 0;
    data.iter()
        .map(|&byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
            byte ^ k
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        // RFC 6229 test vector: Key = 0x0102030405
        let key = [0x01, 0x02, 0x03, 0x04, 0x05];
        let plaintext = [0x00u8; 16];
        let ciphertext = rc4(&key, &plaintext);

        // First 16 bytes of RC4 keystream for this key
        assert_eq!(
            ciphertext,
            [
                0xb2, 0x39, 0x63, 0x05, 0xf0, 0x3d, 0xc0, 0x27, 0xcc, 0xc3, 0x52, 0x4a, 0x0a, 0x11,
                0x18, 0xa8
            ]
        );
    }

    #[test]
    fn rc4_round_trip() {
        let key = b"test key";
        let plaintext = b"Hello, PDF encryption!";
        let ciphertext = rc4(key, plaintext);
        let decrypted = rc4(key, &ciphertext);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn rc4_empty_data() {
        let result = rc4(b"key", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn rc4_empty_key_returns_data_unchanged() {
        let data = b"some data";
        let result = rc4(&[], data);
        assert_eq!(result, data);
    }

    #[test]
    fn rc4_empty_key_empty_data() {
        let result = rc4(&[], &[]);
        assert!(result.is_empty());
    }
}
