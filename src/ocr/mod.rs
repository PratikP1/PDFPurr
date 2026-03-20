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
//! The [`OcrEngine`] trait abstracts over OCR backends:
//! - [`windows_engine::WindowsOcrEngine`] — Windows OCR (always available, ~95% accuracy)
//! - [`tesseract_engine::TesseractEngine`] — Tesseract CLI (always available, ~85-89%)
//! - `ocrs_engine::OcrsEngine` — pure-Rust ocrs (requires `ocr` feature, Latin only)

pub mod config;
pub mod constants;
pub mod engine;
pub mod hybrid;
pub mod layout;
#[cfg(feature = "ocr")]
pub mod ocrs_engine;
pub mod preprocess;
pub mod tesseract_engine;
pub mod text_layer;
pub mod windows_engine;
#[cfg(feature = "ocr-windows-native")]
pub mod windows_native_engine;

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
    fn ocr_text_extractable_after_apply() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let engine = MockOcrEngine {
            words: vec![
                OcrWord {
                    text: "Hello".to_string(),
                    x: 100,
                    y: 100,
                    width: 200,
                    height: 40,
                    confidence: 0.95,
                },
                OcrWord {
                    text: "World".to_string(),
                    x: 350,
                    y: 100,
                    width: 200,
                    height: 40,
                    confidence: 0.92,
                },
            ],
        };

        let applied = doc.ocr_page(0, &engine, &OcrConfig::default()).unwrap();
        assert!(applied);

        // The invisible OCR text should be extractable
        let text = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text.contains("Hello"),
            "Extracted text should contain 'Hello', got: {:?}",
            text
        );
        assert!(
            text.contains("World"),
            "Extracted text should contain 'World', got: {:?}",
            text
        );
    }

    #[test]
    fn ocr_text_survives_roundtrip() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "Roundtrip".to_string(),
                x: 100,
                y: 100,
                width: 300,
                height: 40,
                confidence: 0.95,
            }],
        };

        doc.ocr_page(0, &engine, &OcrConfig::default()).unwrap();

        // Save to bytes and reload
        let bytes = doc.to_bytes().unwrap();
        let reloaded = Document::from_bytes(&bytes).unwrap();
        let text = reloaded.extract_page_text(0).unwrap_or_default();
        assert!(
            text.contains("Roundtrip"),
            "OCR text should survive save/reload, got: {:?}",
            text
        );
    }

    #[test]
    fn ocr_text_extractable_on_page_with_existing_image_content() {
        // Simulates a scanned PDF: page has existing content (image Do),
        // then OCR appends invisible text. Text extraction must find the OCR text.
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Add fake image content (simulates scanned page)
        let image_content = b"q 612 0 0 792 0 0 cm /Im0 Do Q";
        doc.append_content_stream(0, image_content, None).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "ScannedText".to_string(),
                x: 100,
                y: 100,
                width: 300,
                height: 40,
                confidence: 0.95,
            }],
        };

        doc.ocr_page(0, &engine, &OcrConfig::default()).unwrap();

        // Verify the OCR font was added to the page resources
        let page = doc.get_page(0).unwrap();
        let fonts = doc.page_fonts(page);
        assert!(
            fonts.contains_key("F_OCR"),
            "Page should have F_OCR font after OCR, got keys: {:?}",
            fonts.keys().collect::<Vec<_>>()
        );

        let text = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text.contains("ScannedText"),
            "Should extract OCR text from page with existing image content, got: {:?}",
            text
        );
    }

    #[test]
    fn ocr_font_found_on_real_scanned_pdf() {
        // Test with a real scanned PDF (if available) to verify font resolution
        use crate::Document;
        use std::path::Path;

        let path = Path::new("tests/corpus/scanned/graph_scanned.pdf");
        if !path.exists() {
            return;
        }
        let data = std::fs::read(path).unwrap();
        let mut doc = Document::from_bytes(&data).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "TestWord".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 30,
                confidence: 0.9,
            }],
        };

        doc.ocr_page(0, &engine, &OcrConfig::default()).unwrap();

        let page = doc.get_page(0).unwrap();
        let fonts = doc.page_fonts(page);
        assert!(
            fonts.contains_key("F_OCR"),
            "Real scanned PDF should have F_OCR after OCR, got keys: {:?}",
            fonts.keys().collect::<Vec<_>>()
        );

        let text = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text.contains("TestWord"),
            "Should extract mock OCR text from real scanned PDF, got: {:?}",
            text
        );
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

    #[test]
    fn redo_ocr_replaces_existing_ocr_text() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // First OCR pass
        let engine1 = MockOcrEngine {
            words: vec![OcrWord {
                text: "FirstPass".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 40,
                confidence: 0.9,
            }],
        };
        doc.ocr_page(0, &engine1, &OcrConfig::default()).unwrap();

        let text1 = doc.extract_page_text(0).unwrap_or_default();
        assert!(text1.contains("FirstPass"), "First OCR should work");

        // Redo OCR with different text
        let engine2 = MockOcrEngine {
            words: vec![OcrWord {
                text: "SecondPass".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 40,
                confidence: 0.95,
            }],
        };
        let config = OcrConfig {
            should_redo: true,
            ..Default::default()
        };
        let applied = doc.redo_ocr_page(0, &engine2, &config).unwrap();
        assert!(applied, "Redo should apply");

        let text2 = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text2.contains("SecondPass"),
            "Redo should produce new text, got: {:?}",
            text2
        );
        assert!(
            !text2.contains("FirstPass"),
            "Old OCR text should be gone, got: {:?}",
            text2
        );
    }

    #[test]
    fn ocr_rotated_page_produces_text() {
        use crate::Document;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.rotate_page(0, 90).unwrap();

        let engine = MockOcrEngine {
            words: vec![OcrWord {
                text: "Rotated".to_string(),
                x: 100,
                y: 100,
                width: 200,
                height: 40,
                confidence: 0.9,
            }],
        };

        let applied = doc.ocr_page(0, &engine, &OcrConfig::default()).unwrap();
        assert!(applied);

        let text = doc.extract_page_text(0).unwrap_or_default();
        assert!(
            text.contains("Rotated"),
            "Rotated page OCR should work, got: {:?}",
            text
        );
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

        // Convert to grayscale
        let grayscale = pixmap_to_grayscale(&pixmap);

        // Optionally preprocess (contrast + binarization)
        let ocr_image = if config.preprocess {
            preprocess::preprocess_for_ocr(&grayscale)
        } else {
            grayscale
        };

        // Run OCR
        let result = engine.recognize(&ocr_image)?;
        if result.words.is_empty() {
            return Ok(false);
        }

        // Generate invisible text layer with ToUnicode CMap
        let layer = text_layer::build_ocr_text_layer(&result, page_width, page_height, config);

        // Create OCR font with ToUnicode CMap for full Unicode support
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type1")));
        font_dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("Helvetica")),
        );

        // Attach ToUnicode CMap if present (enables correct Unicode extraction)
        if let Some(cmap_stream) = layer.to_unicode_cmap {
            let cmap_id = self.add_object(Object::Stream(cmap_stream));
            font_dict.insert(
                PdfName::new("ToUnicode"),
                Object::Reference(crate::core::objects::IndirectRef::new(cmap_id.0, cmap_id.1)),
            );
        }

        let mut fonts = Dictionary::new();
        fonts.insert(PdfName::new(OCR_FONT_NAME), Object::Dictionary(font_dict));

        // Append to page
        self.append_content_stream(page_index, &layer.content, Some(fonts))?;

        // Build tagged PDF structure (StructTreeRoot, Document → P/H elements)
        crate::accessibility::structure_builder::add_tagged_structure_from_ocr(
            self,
            page_index,
            &result,
            &config.language,
        )?;

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

    /// Strips existing OCR content and re-runs OCR on a page.
    ///
    /// Removes the OCR content stream (identified by the `F_OCR` font)
    /// and the `F_OCR` font resource, then runs OCR fresh. Use this when
    /// a previous OCR pass produced poor results and you want to try
    /// with a different engine or settings.
    pub fn redo_ocr_page(
        &mut self,
        page_index: usize,
        engine: &dyn OcrEngine,
        config: &OcrConfig,
    ) -> PdfResult<bool> {
        self.strip_ocr_content(page_index)?;

        // Run OCR without skip_text_pages (we just stripped)
        let mut redo_config = config.clone();
        redo_config.skip_text_pages = false;
        self.ocr_page(page_index, engine, &redo_config)
    }

    /// Removes OCR-generated content from a page.
    ///
    /// Finds and removes content streams that reference `F_OCR` font,
    /// and removes the `F_OCR` entry from the page's font resources.
    fn strip_ocr_content(&mut self, page_index: usize) -> PdfResult<()> {
        let page_id = self.page_object_id(page_index)?;

        // Get the page's /Contents
        let page = self
            .get_object(page_id)
            .and_then(|o| o.as_dict())
            .ok_or_else(|| PdfError::InvalidStructure("Page is not a dictionary".to_string()))?;

        let contents = page.get(&PdfName::new("Contents")).cloned();

        // Pre-build PdfName keys to avoid repeated allocations
        let ocr_font_name = text_layer::OCR_FONT_NAME;
        let contents_key = PdfName::new("Contents");
        let resources_key = PdfName::new("Resources");
        let font_key = PdfName::new("Font");
        let ocr_font_key = PdfName::new(ocr_font_name);

        // Check if a content stream references the OCR font.
        // Falls back to raw data if decode fails — the F_OCR string
        // is plain ASCII and visible even in compressed data.
        let is_ocr_stream = |stream: &crate::core::objects::PdfStream| -> bool {
            let data = stream.decode_data().unwrap_or_else(|_| stream.data.clone());
            String::from_utf8_lossy(&data).contains(ocr_font_name)
        };

        // Collect non-OCR content stream refs
        let mut keep_ids: Vec<Object> = Vec::new();
        match contents {
            Some(Object::Array(refs)) => {
                keep_ids.reserve(refs.len());
                for r in &refs {
                    if let Object::Reference(iref) = r {
                        let is_ocr = self
                            .get_object(iref.id())
                            .and_then(|o| o.as_stream())
                            .is_some_and(is_ocr_stream);
                        if !is_ocr {
                            keep_ids.push(r.clone());
                        }
                    }
                }
            }
            Some(Object::Reference(r)) => {
                let is_ocr = self
                    .get_object(r.id())
                    .and_then(|o| o.as_stream())
                    .is_some_and(is_ocr_stream);
                if !is_ocr {
                    keep_ids.push(Object::Reference(r));
                }
            }
            _ => {}
        }

        // Update page /Contents
        let page_dict = self
            .get_object_mut(page_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Page is not a dictionary".to_string()))?;

        match keep_ids.len() {
            0 => {
                page_dict.remove(&contents_key);
            }
            1 => {
                page_dict.insert(contents_key.clone(), keep_ids.remove(0));
            }
            _ => {
                page_dict.insert(contents_key.clone(), Object::Array(keep_ids));
            }
        }

        // Remove F_OCR from font resources (handles both indirect and inline)
        let res_ref = page_dict.get(&resources_key).cloned();
        if let Some(Object::Reference(r)) = res_ref {
            if let Some(Object::Dictionary(res)) = self.get_object_mut(r.id()) {
                if let Some(Object::Dictionary(fonts)) = res.get_mut(&font_key) {
                    fonts.remove(&ocr_font_key);
                }
            }
        } else {
            let page_dict = self
                .get_object_mut(page_id)
                .and_then(|o| {
                    if let Object::Dictionary(d) = o {
                        Some(d)
                    } else {
                        None
                    }
                })
                .ok_or_else(|| PdfError::InvalidStructure("Page dict lost".to_string()))?;
            if let Some(Object::Dictionary(res)) = page_dict.get_mut(&resources_key) {
                if let Some(Object::Dictionary(fonts)) = res.get_mut(&font_key) {
                    fonts.remove(&ocr_font_key);
                }
            }
        }

        Ok(())
    }
}
