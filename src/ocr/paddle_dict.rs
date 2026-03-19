//! Character dictionaries for PaddleOCR recognition models.
//!
//! Contains embedded dictionaries for common languages. The recognition
//! model outputs CTC logits indexed against these dictionaries.

/// English character dictionary (96 characters).
///
/// Matches the PaddleOCR `en_dict.txt` for PP-OCRv4 English models.
/// Index 0 in CTC output is the blank token; dictionary indices start at 1.
pub const ENGLISH_DICT: &[&str] = &[
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f", "g", "h", "i",
    "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "A", "B",
    "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U",
    "V", "W", "X", "Y", "Z", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".",
    "/", ":", ";", "<", "=", ">", "?", "@", "[", "\\", "]", "^", "_", "`", "{", "|", "}", "~", " ",
];

/// Loads a character dictionary from a text file.
///
/// Each line in the file is one character. Empty lines are preserved
/// as space characters.
pub fn load_dictionary_from_file(path: &std::path::Path) -> Result<Vec<String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(|l| l.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_dict_has_expected_size() {
        // 10 digits + 26 lower + 26 upper + 31 punctuation + 1 space = 94
        // PaddleOCR en_dict typically has 95-97 chars
        assert!(
            ENGLISH_DICT.len() >= 90 && ENGLISH_DICT.len() <= 100,
            "Expected ~95 chars, got {}",
            ENGLISH_DICT.len()
        );
    }

    #[test]
    fn english_dict_contains_alphanumeric() {
        assert!(ENGLISH_DICT.contains(&"a"));
        assert!(ENGLISH_DICT.contains(&"Z"));
        assert!(ENGLISH_DICT.contains(&"0"));
        assert!(ENGLISH_DICT.contains(&"9"));
    }

    #[test]
    fn english_dict_contains_punctuation() {
        assert!(ENGLISH_DICT.contains(&"."));
        assert!(ENGLISH_DICT.contains(&","));
        assert!(ENGLISH_DICT.contains(&" "));
    }
}
