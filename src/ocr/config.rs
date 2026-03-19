//! OCR configuration.

/// Configuration for OCR processing.
#[derive(Debug, Clone)]
pub struct OcrConfig {
    /// DPI for rendering pages before OCR (default: 300).
    ///
    /// Higher DPI improves accuracy but increases processing time.
    pub dpi: f64,
    /// Minimum word confidence threshold (0.0–1.0, default: 0.3).
    ///
    /// Words below this confidence are excluded from the invisible text layer.
    pub min_confidence: f32,
    /// Skip pages that already contain extractable text (default: true).
    pub skip_text_pages: bool,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            dpi: 300.0,
            min_confidence: 0.3,
            skip_text_pages: true,
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
