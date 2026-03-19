//! OCR support for image-only PDF accessibility.
//!
//! Provides optical character recognition to make scanned/image-only PDFs
//! searchable and accessible. OCR text is overlaid as an invisible text
//! layer (rendering mode 3) on top of the original page image.
//!
//! # Architecture
//!
//! The OCR pipeline:
//! 1. Render the page to a pixel image
//! 2. Convert to grayscale
//! 3. Run OCR engine to get words + bounding boxes
//! 4. Generate invisible text layer with correct positioning
//! 5. Append the text layer to the page
//!
//! The [`OcrEngine`] trait abstracts over OCR backends. Enable the `ocr`
//! feature for the built-in `ocrs`-based engine.

pub mod config;
pub mod engine;
pub mod layout;
#[cfg(feature = "ocr")]
pub mod ocrs_engine;
#[cfg(feature = "ocr-paddle")]
pub mod paddle_dict;
#[cfg(feature = "ocr-paddle")]
pub mod paddle_engine;
#[cfg(feature = "ocr-paddle")]
pub mod paddle_postprocess;
#[cfg(feature = "ocr-paddle")]
pub mod paddle_preprocess;
pub mod text_layer;

pub use config::OcrConfig;
pub use engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
pub use text_layer::OCR_FONT_NAME;

/// Converts a `tiny_skia::Pixmap` (RGBA) to a grayscale [`OcrImage`].
///
/// Uses sRGB luminance coefficients (ITU-R BT.709):
/// `gray = 0.2126*R + 0.7152*G + 0.0722*B`
pub fn pixmap_to_grayscale(pixmap: &tiny_skia::Pixmap) -> OcrImage {
    let w = pixmap.width();
    let h = pixmap.height();
    let pixels = pixmap.pixels();
    let mut gray = Vec::with_capacity((w * h) as usize);

    for px in pixels {
        let r = px.red() as f32;
        let g = px.green() as f32;
        let b = px.blue() as f32;
        let a = px.alpha() as f32;

        // Unpremultiply alpha, then compute luminance
        if a > 0.0 {
            let r = r / a * 255.0;
            let g = g / a * 255.0;
            let b = b / a * 255.0;
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            gray.push(lum.round().clamp(0.0, 255.0) as u8);
        } else {
            gray.push(255); // transparent → white
        }
    }

    OcrImage {
        data: gray,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmap_to_grayscale_white() {
        let mut pixmap = tiny_skia::Pixmap::new(2, 2).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        let img = pixmap_to_grayscale(&pixmap);
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.data.len(), 4);
        // White pixels should produce ~255 grayscale
        assert!(img.data.iter().all(|&v| v >= 250));
    }

    #[test]
    fn pixmap_to_grayscale_black() {
        let mut pixmap = tiny_skia::Pixmap::new(2, 2).unwrap();
        pixmap.fill(tiny_skia::Color::BLACK);
        let img = pixmap_to_grayscale(&pixmap);
        // Black pixels should produce ~0 grayscale
        assert!(img.data.iter().all(|&v| v <= 5));
    }

    #[test]
    fn pixmap_to_grayscale_dimensions() {
        let pixmap = tiny_skia::Pixmap::new(100, 50).unwrap();
        let img = pixmap_to_grayscale(&pixmap);
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 50);
        assert_eq!(img.data.len(), 5000);
    }

    use crate::core::objects::DictExt;

    /// Mock OCR engine for testing — returns fixed words.
    struct MockOcrEngine {
        words: Vec<OcrWord>,
    }

    impl OcrEngine for MockOcrEngine {
        fn recognize(&self, image: &OcrImage) -> crate::error::PdfResult<OcrResult> {
            Ok(OcrResult {
                words: self.words.clone(),
                image_width: image.width,
                image_height: image.height,
            })
        }
    }

    #[test]
    fn ocr_page_with_mock_engine() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "Hello".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 40,
                confidence: 0.95,
            }],
        };
        let config = OcrConfig::default();

        let applied = doc.ocr_page(0, &engine, &config).unwrap();
        assert!(applied, "OCR should be applied to blank page");

        // Verify the page now has content
        let page = doc.get_page(0).unwrap();
        assert!(page.get_str("Contents").is_some());
    }

    #[test]
    fn ocr_page_skips_text_pages() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Add some text content to the page first
        let content = b"BT /F1 12 Tf 100 700 Td (Existing text) Tj ET";
        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            crate::core::objects::PdfName::new("F1"),
            crate::core::objects::Object::Dictionary(helv.to_font_dictionary()),
        );
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "OCR".to_string(),
                x: 0,
                y: 0,
                width: 100,
                height: 30,
                confidence: 0.9,
            }],
        };
        let config = OcrConfig {
            skip_text_pages: true,
            ..Default::default()
        };

        let applied = doc.ocr_page(0, &engine, &config).unwrap();
        assert!(!applied, "Should skip page that already has text");
    }

    #[test]
    fn ocr_all_pages_counts_correctly() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap(); // blank (will be OCR'd)
        doc.add_page(612.0, 792.0).unwrap(); // blank (will be OCR'd)

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "Word".to_string(),
                x: 50,
                y: 50,
                width: 100,
                height: 25,
                confidence: 0.8,
            }],
        };
        let config = OcrConfig::default();

        let count = doc.ocr_all_pages(&engine, &config).unwrap();
        assert_eq!(count, 2, "Should OCR both blank pages");
    }
}

// --- Document integration ---

use crate::core::objects::{Dictionary, Object, PdfName};
use crate::document::Document;
use crate::error::{PdfError, PdfResult};
use crate::rendering::{RenderOptions, Renderer};

impl Document {
    /// Runs OCR on a single page and overlays invisible text.
    ///
    /// Renders the page, runs the OCR engine, and appends an invisible
    /// text layer (rendering mode 3) that makes the page searchable
    /// and accessible to screen readers.
    ///
    /// Returns `Ok(true)` if OCR was applied, `Ok(false)` if skipped
    /// (page already has text and `config.skip_text_pages` is set).
    pub fn ocr_page(
        &mut self,
        page_index: usize,
        engine: &dyn OcrEngine,
        config: &OcrConfig,
    ) -> PdfResult<bool> {
        // Skip pages that already have text
        if config.skip_text_pages {
            if let Ok(text) = self.extract_page_text(page_index) {
                if !text.trim().is_empty() {
                    return Ok(false);
                }
            }
        }

        // Get page dimensions
        let page = self.get_page(page_index)?;
        let media_box = self.page_media_box(page)?;
        let page_width = media_box[2] - media_box[0];
        let page_height = media_box[3] - media_box[1];

        // Render the page at OCR DPI
        let renderer = Renderer::new(
            self,
            RenderOptions {
                dpi: config.dpi,
                ..Default::default()
            },
        );
        let pixmap = renderer.render_page(page_index)?;

        // Convert to grayscale for OCR
        let ocr_image = pixmap_to_grayscale(&pixmap);

        // Run OCR
        let result = engine.recognize(&ocr_image)?;
        if result.words.is_empty() {
            return Ok(false);
        }

        // Generate invisible text layer
        let content = text_layer::build_ocr_text_layer(&result, page_width, page_height, config);

        // Create Helvetica font for invisible text
        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica")
            .ok_or_else(|| PdfError::Other("Helvetica not found".to_string()))?;
        let mut fonts = Dictionary::new();
        fonts.insert(
            PdfName::new(OCR_FONT_NAME),
            Object::Dictionary(helv.to_font_dictionary()),
        );

        // Append to page
        self.append_content_stream(page_index, &content, Some(fonts))?;

        Ok(true)
    }

    /// Runs OCR on all image-only pages in the document.
    ///
    /// Returns the number of pages that were OCR'd.
    pub fn ocr_all_pages(
        &mut self,
        engine: &dyn OcrEngine,
        config: &OcrConfig,
    ) -> PdfResult<usize> {
        let page_count = self.page_count()?;
        let mut ocr_count = 0;

        for i in 0..page_count {
            if self.ocr_page(i, engine, config)? {
                ocr_count += 1;
            }
        }

        Ok(ocr_count)
    }
}
