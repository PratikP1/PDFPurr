//! Invisible text layer generation from OCR results.
//!
//! Converts OCR word bounding boxes into a PDF content stream with
//! invisible text (rendering mode 3) positioned to overlay the
//! original page image.

use crate::content::ContentStreamBuilder;

use super::config::OcrConfig;
use super::engine::OcrResult;

/// Font resource name used for OCR invisible text.
pub const OCR_FONT_NAME: &str = "F_OCR";

/// Builds a PDF content stream containing invisible text from OCR results.
///
/// Each word is positioned absolutely using the `Tm` operator. Text
/// rendering mode 3 makes the text invisible but searchable/selectable.
///
/// # Arguments
///
/// * `result` — OCR output with words and bounding boxes in pixel coordinates
/// * `page_width` — Page width in PDF points
/// * `page_height` — Page height in PDF points
/// * `config` — OCR configuration (min confidence, DPI)
pub fn build_ocr_text_layer(
    result: &OcrResult,
    page_width: f64,
    page_height: f64,
    config: &OcrConfig,
) -> Vec<u8> {
    let mut builder = ContentStreamBuilder::new();

    // Scale factor: OCR pixels → PDF points
    let scale_x = page_width / result.image_width as f64;
    let scale_y = page_height / result.image_height as f64;

    builder.begin_text().set_text_rendering_mode(3); // invisible

    for word in &result.words {
        if word.confidence < config.min_confidence {
            continue;
        }
        if word.text.trim().is_empty() {
            continue;
        }

        // Map OCR pixel coords to PDF user space (Y-flipped)
        let pdf_x = word.x as f64 * scale_x;
        let pdf_y = page_height - (word.y as f64 + word.height as f64) * scale_y;
        let font_size = (word.height as f64 * scale_y).max(1.0);

        // Use Tm for absolute positioning per word
        // Tm sets both font size (via matrix scale) and position
        builder
            .set_font(OCR_FONT_NAME, font_size)
            .set_text_matrix(1.0, 0.0, 0.0, 1.0, pdf_x, pdf_y)
            .show_text(&word.text);
    }

    builder.end_text();
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::engine::{OcrResult, OcrWord};

    fn make_result(words: Vec<OcrWord>) -> OcrResult {
        OcrResult {
            words,
            image_width: 2550,  // 8.5" at 300 DPI
            image_height: 3300, // 11" at 300 DPI
        }
    }

    #[test]
    fn empty_result_produces_minimal_stream() {
        let result = make_result(vec![]);
        let config = OcrConfig::default();
        let data = build_ocr_text_layer(&result, 612.0, 792.0, &config);
        let text = String::from_utf8(data).unwrap();
        assert!(text.contains("BT"));
        assert!(text.contains("3 Tr"));
        assert!(text.contains("ET"));
        // No Tj operators since no words
        assert!(!text.contains("Tj"));
    }

    #[test]
    fn single_word_produces_positioned_text() {
        let result = make_result(vec![OcrWord {
            text: "Hello".to_string(),
            x: 300,
            y: 300,
            width: 200,
            height: 50,
            confidence: 0.95,
        }]);
        let config = OcrConfig::default();
        let data = build_ocr_text_layer(&result, 612.0, 792.0, &config);
        let text = String::from_utf8(data).unwrap();

        assert!(text.contains("3 Tr"), "should set invisible mode");
        assert!(text.contains("Tm"), "should use Tm for positioning");
        assert!(text.contains("(Hello) Tj"), "should show the word");
        assert!(text.contains("F_OCR"), "should reference OCR font");
    }

    #[test]
    fn low_confidence_words_filtered() {
        let result = make_result(vec![
            OcrWord {
                text: "Good".to_string(),
                x: 100,
                y: 100,
                width: 100,
                height: 30,
                confidence: 0.9,
            },
            OcrWord {
                text: "Bad".to_string(),
                x: 300,
                y: 100,
                width: 80,
                height: 30,
                confidence: 0.1, // below threshold
            },
        ]);
        let config = OcrConfig {
            min_confidence: 0.3,
            ..Default::default()
        };
        let data = build_ocr_text_layer(&result, 612.0, 792.0, &config);
        let text = String::from_utf8(data).unwrap();

        assert!(text.contains("(Good) Tj"));
        assert!(
            !text.contains("(Bad) Tj"),
            "low confidence should be filtered"
        );
    }

    #[test]
    fn y_coordinate_is_flipped() {
        // Word at top of image (y=50) should map to near top of page (high PDF y)
        let result = make_result(vec![OcrWord {
            text: "Top".to_string(),
            x: 100,
            y: 50,
            width: 100,
            height: 30,
            confidence: 0.95,
        }]);
        let config = OcrConfig::default();
        let data = build_ocr_text_layer(&result, 612.0, 792.0, &config);
        let text = String::from_utf8(data).unwrap();

        // PDF y should be near page_height (792) since image y is near 0
        // Extract Tm line and check y value is > 700
        let tm_line = text.lines().find(|l| l.contains("Tm")).unwrap();
        let parts: Vec<&str> = tm_line.split_whitespace().collect();
        // Tm format: "1 0 0 1 x y Tm"
        let y_val: f64 = parts[parts.len() - 2].parse().unwrap();
        assert!(
            y_val > 700.0,
            "Top-of-image word should map to high PDF y, got {}",
            y_val
        );
    }
}
