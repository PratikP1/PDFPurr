//! PaddleOCR engine via tract-onnx (pure Rust, multi-language).
//!
//! Implements the full PaddleOCR PP-OCRv4 pipeline:
//! 1. Text detection (DBNet) → bounding boxes
//! 2. Text recognition (SVTR) → CTC decoded text
//!
//! # Model Files
//!
//! Two ONNX model files are required:
//! - `det.onnx` — text detection (~2.3 MB)
//! - `rec.onnx` — text recognition (~7.5 MB)
//!
//! Pre-converted models: <https://huggingface.co/monkt/paddleocr-onnx>
//!
//! # Example
//!
//! ```no_run
//! use pdfpurr::ocr::paddle_engine::PaddleOcrEngine;
//! use pdfpurr::ocr::OcrEngine;
//!
//! let engine = PaddleOcrEngine::with_english("det.onnx", "rec.onnx").unwrap();
//! ```

use std::path::Path;

use tract_onnx::prelude::*;

use crate::error::{PdfError, PdfResult};

use super::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
use super::paddle_dict::ENGLISH_DICT;
use super::paddle_postprocess::{ctc_greedy_decode, extract_text_boxes};
use super::paddle_preprocess::{grayscale_to_chw_tensor, prepare_recognition_input};

/// Maximum detection input dimension (pixels).
const MAX_DET_SIZE: usize = 960;
/// DBNet probability threshold for text detection.
const DET_THRESHOLD: f32 = 0.3;
/// Minimum text box area in pixels.
const MIN_BOX_AREA: u32 = 16;

/// PaddleOCR engine backed by tract-onnx (pure Rust).
pub struct PaddleOcrEngine {
    det_model: TypedRunnableModel<TypedModel>,
    rec_model: TypedRunnableModel<TypedModel>,
    dictionary: Vec<String>,
}

impl PaddleOcrEngine {
    /// Creates a PaddleOCR engine with the English dictionary.
    pub fn with_english<P: AsRef<Path>>(det_model_path: P, rec_model_path: P) -> PdfResult<Self> {
        let dict = ENGLISH_DICT.iter().map(|s| s.to_string()).collect();
        Self::new(det_model_path, rec_model_path, dict)
    }

    /// Creates a PaddleOCR engine with a custom dictionary.
    pub fn new<P: AsRef<Path>>(
        det_model_path: P,
        rec_model_path: P,
        dictionary: Vec<String>,
    ) -> PdfResult<Self> {
        let det_model = tract_onnx::onnx()
            .model_for_path(det_model_path.as_ref())
            .map_err(|e| PdfError::Other(format!("Load det model: {}", e)))?
            .into_optimized()
            .map_err(|e| PdfError::Other(format!("Optimize det model: {}", e)))?
            .into_runnable()
            .map_err(|e| PdfError::Other(format!("Prepare det model: {}", e)))?;

        let rec_model = tract_onnx::onnx()
            .model_for_path(rec_model_path.as_ref())
            .map_err(|e| PdfError::Other(format!("Load rec model: {}", e)))?
            .into_optimized()
            .map_err(|e| PdfError::Other(format!("Optimize rec model: {}", e)))?
            .into_runnable()
            .map_err(|e| PdfError::Other(format!("Prepare rec model: {}", e)))?;

        Ok(Self {
            det_model,
            rec_model,
            dictionary,
        })
    }
}

impl OcrEngine for PaddleOcrEngine {
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult> {
        let w = image.width as usize;
        let h = image.height as usize;

        // Step 1: Resize for detection (max dimension = MAX_DET_SIZE)
        let scale = (MAX_DET_SIZE as f32) / (w.max(h) as f32);
        let det_h = ((h as f32 * scale) as usize).max(32);
        let det_w = ((w as f32 * scale) as usize).max(32);
        // Round up to multiples of 32 (DBNet requirement)
        let det_h = det_h.next_multiple_of(32);
        let det_w = det_w.next_multiple_of(32);

        let det_input = grayscale_to_chw_tensor(image, det_h, det_w);

        // Step 2: Run detection
        let det_output = self
            .det_model
            .run(tvec![det_input.into()])
            .map_err(|e| PdfError::Other(format!("Det inference: {}", e)))?;

        let det_tensor = det_output[0]
            .to_array_view::<f32>()
            .map_err(|e| PdfError::Other(format!("Det output: {}", e)))?;

        // Extract probability map (shape: [1, 1, det_h, det_w] or [1, det_h, det_w])
        let prob_len = det_h * det_w;
        let prob_data: Vec<f32> = det_tensor.iter().take(prob_len).copied().collect();

        // Step 3: Extract bounding boxes
        let boxes = extract_text_boxes(&prob_data, det_h, det_w, DET_THRESHOLD, MIN_BOX_AREA);

        // Map detection boxes back to original image coordinates
        let scale_x = w as f32 / det_w as f32;
        let scale_y = h as f32 / det_h as f32;

        // Step 4: Recognize text in each box
        let dict_refs: Vec<&str> = self.dictionary.iter().map(|s| s.as_str()).collect();
        let mut words = Vec::new();

        for text_box in &boxes {
            let orig_x = (text_box.x as f32 * scale_x) as u32;
            let orig_y = (text_box.y as f32 * scale_y) as u32;
            let orig_w = (text_box.width as f32 * scale_x).max(1.0) as u32;
            let orig_h = (text_box.height as f32 * scale_y).max(1.0) as u32;

            let rec_input = prepare_recognition_input(image, orig_x, orig_y, orig_w, orig_h);

            let rec_output = match self.rec_model.run(tvec![rec_input.into()]) {
                Ok(o) => o,
                Err(_) => continue,
            };

            let rec_tensor = match rec_output[0].to_array_view::<f32>() {
                Ok(t) => t,
                Err(_) => continue,
            };

            // rec output shape: [1, seq_len, num_chars]
            let shape = rec_tensor.shape();
            if shape.len() < 2 {
                continue;
            }
            let seq_len = shape[shape.len() - 2];
            let num_chars = shape[shape.len() - 1];

            let logits: Vec<f32> = rec_tensor.iter().copied().collect();
            let (text, confidence) = ctc_greedy_decode(&logits, seq_len, num_chars, &dict_refs);

            if !text.trim().is_empty() {
                words.push(OcrWord {
                    text,
                    x: orig_x,
                    y: orig_y,
                    width: orig_w,
                    height: orig_h,
                    confidence,
                });
            }
        }

        // Sort by reading order (top to bottom, left to right)
        words.sort_by(|a, b| {
            let line_diff = a.y.abs_diff(b.y);
            if line_diff <= a.height / 2 {
                a.x.cmp(&b.x)
            } else {
                a.y.cmp(&b.y)
            }
        });

        Ok(OcrResult {
            words,
            image_width: image.width,
            image_height: image.height,
        })
    }
}
