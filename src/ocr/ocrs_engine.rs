//! OCR engine implementation using `ocrs` (pure Rust).
//!
//! Uses the RTen inference engine to run neural network models for
//! text detection and recognition. No C/C++ dependencies.
//!
//! # Model Files
//!
//! Two `.rten` model files are required:
//! - `text-detection.rten` — detects word bounding boxes
//! - `text-recognition.rten` — recognizes text in detected regions
//!
//! Download from: <https://github.com/robertknight/ocrs-models>

use std::path::Path;

use crate::error::{PdfError, PdfResult};

use super::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};

/// OCR engine backed by `ocrs` (pure Rust, RTen inference).
pub struct OcrsEngine {
    engine: ocrs::OcrEngine,
}

impl OcrsEngine {
    /// Creates a new OCR engine by loading model files from disk.
    pub fn new<P: AsRef<Path>>(
        detection_model_path: P,
        recognition_model_path: P,
    ) -> PdfResult<Self> {
        let detection_model = rten::Model::load_file(detection_model_path.as_ref())
            .map_err(|e| PdfError::Other(format!("Load detection model: {}", e)))?;

        let recognition_model = rten::Model::load_file(recognition_model_path.as_ref())
            .map_err(|e| PdfError::Other(format!("Load recognition model: {}", e)))?;

        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| PdfError::Other(format!("Init OCR engine: {}", e)))?;

        Ok(Self { engine })
    }
}

impl OcrEngine for OcrsEngine {
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult> {
        let width = image.width;
        let height = image.height;

        // ocrs ImageSource::from_bytes expects HWC u8 RGB data.
        // Convert grayscale to RGB by tripling each byte.
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for &gray in &image.data {
            rgb.push(gray);
            rgb.push(gray);
            rgb.push(gray);
        }

        let img_source = ocrs::ImageSource::from_bytes(&rgb, (width, height))
            .map_err(|e| PdfError::Other(format!("OCR image source: {}", e)))?;

        let input = self
            .engine
            .prepare_input(img_source)
            .map_err(|e| PdfError::Other(format!("OCR prepare: {}", e)))?;

        let word_rects = self
            .engine
            .detect_words(&input)
            .map_err(|e| PdfError::Other(format!("OCR detect: {}", e)))?;

        let lines = self.engine.find_text_lines(&input, &word_rects);

        let text_lines = self
            .engine
            .recognize_text(&input, &lines)
            .map_err(|e| PdfError::Other(format!("OCR recognize: {}", e)))?;

        let mut words = Vec::new();
        for (line_rects, text_line) in lines.iter().zip(text_lines.iter()) {
            if let Some(text_line) = text_line {
                for (rect, word_text) in line_rects.iter().zip(text_line.words()) {
                    let text = word_text.to_string();
                    if text.trim().is_empty() {
                        continue;
                    }

                    // Get axis-aligned bounding box from the rotated rect corners
                    let corners = rect.corners();
                    let min_x = corners.iter().map(|c| c.x).fold(f32::MAX, f32::min);
                    let min_y = corners.iter().map(|c| c.y).fold(f32::MAX, f32::min);
                    let max_x = corners.iter().map(|c| c.x).fold(f32::MIN, f32::max);
                    let max_y = corners.iter().map(|c| c.y).fold(f32::MIN, f32::max);

                    words.push(OcrWord {
                        text,
                        x: min_x.max(0.0) as u32,
                        y: min_y.max(0.0) as u32,
                        width: (max_x - min_x).max(1.0) as u32,
                        height: (max_y - min_y).max(1.0) as u32,
                        confidence: 0.9,
                    });
                }
            }
        }

        Ok(OcrResult {
            words,
            image_width: image.width,
            image_height: image.height,
        })
    }
}
