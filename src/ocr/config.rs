//! OCR configuration.

/// Configuration for OCR processing.
#[derive(Debug, Clone)]
pub struct OcrConfig {
    /// DPI for rendering pages before OCR (default: 300).
    ///
    /// Higher DPI improves accuracy but increases processing time.
    /// 300 DPI is recommended for scanned documents; 150 may suffice
    /// for clean digital content.
    pub dpi: f64,
    /// Minimum word confidence threshold (0.0–1.0, default: 0.3).
    ///
    /// Words below this confidence are excluded from the invisible text layer.
    pub min_confidence: f32,
    /// Skip pages that already contain extractable text (default: true).
    pub skip_text_pages: bool,
    /// Apply contrast enhancement and Otsu binarization before OCR (default: true).
    ///
    /// Preprocessing significantly improves accuracy on low-contrast,
    /// washed-out, or noisy scans. Disable for already-clean digital images.
    pub preprocess: bool,
    /// Document language for tagged PDF structure (BCP 47, default: "en").
    ///
    /// Set to the primary language of the scanned document for correct
    /// screen reader pronunciation.
    pub language: String,
    /// When true, strip existing OCR content before re-processing (default: false).
    ///
    /// Used by `redo_ocr_page` to replace a previous OCR pass with fresh results.
    pub should_redo: bool,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            dpi: 300.0,
            min_confidence: 0.3,
            skip_text_pages: true,
            preprocess: true,
            language: "en".to_string(),
            should_redo: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let config = OcrConfig::default();
        assert_eq!(config.dpi, 300.0);
        assert_eq!(config.min_confidence, 0.3);
        assert!(config.skip_text_pages);
    }
}
