//! OCR engine trait and data types.
//!
//! Defines the abstraction layer for OCR engines. The trait is always
//! available; concrete implementations are behind feature flags.

use crate::error::PdfResult;

/// A single recognized word with its bounding box in image pixel coordinates.
#[derive(Debug, Clone)]
pub struct OcrWord {
    /// The recognized text for this word.
    pub text: String,
    /// X coordinate of the bounding box (pixels from left edge).
    pub x: u32,
    /// Y coordinate of the bounding box (pixels from top edge).
    pub y: u32,
    /// Width of the bounding box in pixels.
    pub width: u32,
    /// Height of the bounding box in pixels.
    pub height: u32,
    /// Recognition confidence (0.0–1.0).
    pub confidence: f32,
}

/// Result of OCR on a single page image.
#[derive(Debug, Clone)]
pub struct OcrResult {
    /// Recognized words with positions, in reading order.
    pub words: Vec<OcrWord>,
    /// Width of the source image in pixels.
    pub image_width: u32,
    /// Height of the source image in pixels.
    pub image_height: u32,
}

/// Grayscale image data for OCR processing.
#[derive(Debug, Clone)]
pub struct OcrImage {
    /// Grayscale pixel data (1 byte per pixel, top-left origin).
    pub data: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// Abstraction over OCR engines.
///
/// Implementations receive a grayscale image and return recognized words
/// with bounding boxes. The engine manages its own model loading and
/// configuration.
pub trait OcrEngine {
    /// Recognizes text in the given grayscale image.
    ///
    /// Returns words with bounding boxes in pixel coordinates and
    /// confidence scores. Words should be in reading order.
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_word_fields() {
        let word = OcrWord {
            text: "Hello".to_string(),
            x: 100,
            y: 200,
            width: 80,
            height: 20,
            confidence: 0.95,
        };
        assert_eq!(word.text, "Hello");
        assert!(word.confidence > 0.9);
    }

    #[test]
    fn ocr_result_default() {
        let result = OcrResult {
            words: Vec::new(),
            image_width: 612,
            image_height: 792,
        };
        assert!(result.words.is_empty());
    }

    #[test]
    fn ocr_image_construction() {
        let img = OcrImage {
            data: vec![128; 100],
            width: 10,
            height: 10,
        };
        assert_eq!(img.data.len(), 100);
    }
}
