//! PaddleOCR end-to-end integration tests.
//!
//! These tests require ONNX model files. They are skipped when models
//! are not present. To run:
//!
//! 1. Download models from <https://huggingface.co/monkt/paddleocr-onnx>
//! 2. Place `det.onnx` and `rec.onnx` in `tests/models/paddle/`
//! 3. Run: `cargo test --features ocr-paddle paddle_integration`

#![cfg(feature = "ocr-paddle")]

use std::path::Path;

const DET_MODEL: &str = "tests/models/paddle/det.onnx";
const REC_MODEL: &str = "tests/models/paddle/rec.onnx";

fn models_available() -> bool {
    Path::new(DET_MODEL).exists() && Path::new(REC_MODEL).exists()
}

#[test]
fn paddle_engine_loads_models() {
    if !models_available() {
        eprintln!("Skipping: PaddleOCR models not found at tests/models/paddle/");
        return;
    }

    use pdfpurr::ocr::paddle_engine::PaddleOcrEngine;
    let engine = PaddleOcrEngine::with_english(DET_MODEL, REC_MODEL);
    assert!(engine.is_ok(), "Engine should load: {:?}", engine.err());
}

#[test]
fn paddle_engine_recognizes_white_image() {
    if !models_available() {
        eprintln!("Skipping: PaddleOCR models not found");
        return;
    }

    use pdfpurr::ocr::engine::{OcrEngine, OcrImage};
    use pdfpurr::ocr::paddle_engine::PaddleOcrEngine;

    let engine = PaddleOcrEngine::with_english(DET_MODEL, REC_MODEL).unwrap();

    // Blank white image — should produce no text
    let image = OcrImage {
        data: vec![255u8; 200 * 100],
        width: 200,
        height: 100,
    };

    let result = engine.recognize(&image).unwrap();
    // A blank image should have zero or near-zero detected words
    assert!(
        result.words.len() <= 2,
        "Blank image should not produce many words, got {}",
        result.words.len()
    );
}

#[test]
fn paddle_engine_recognizes_simple_text() {
    if !models_available() {
        eprintln!("Skipping: PaddleOCR models not found");
        return;
    }

    use pdfpurr::ocr::engine::{OcrEngine, OcrImage};
    use pdfpurr::ocr::paddle_engine::PaddleOcrEngine;

    let engine = PaddleOcrEngine::with_english(DET_MODEL, REC_MODEL).unwrap();

    // Create a simple grayscale image with dark text on white background
    // "HELLO" rendered as crude 5x7 pixel letters on a 200x50 canvas
    let mut data = vec![255u8; 200 * 50]; // white background

    // Draw simple dark horizontal bars to simulate text lines
    // This creates enough contrast for the detector to find something
    for y in 15..35 {
        for x in 20..180 {
            data[y * 200 + x] = 0; // black pixels
        }
    }

    let image = OcrImage {
        data,
        width: 200,
        height: 50,
    };

    let result = engine.recognize(&image);
    assert!(
        result.is_ok(),
        "Recognition should not error: {:?}",
        result.err()
    );
    // We don't assert specific text — just that the pipeline completed without error
}

#[test]
fn paddle_engine_invalid_model_path_errors() {
    use pdfpurr::ocr::paddle_engine::PaddleOcrEngine;

    let result = PaddleOcrEngine::with_english("nonexistent.onnx", "also_missing.onnx");
    assert!(result.is_err());
}
