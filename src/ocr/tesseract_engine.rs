//! Tesseract OCR engine via command-line subprocess.
//!
//! Invokes the `tesseract` CLI to perform OCR on temporary image files.
//! Outputs TSV format for structured word-level results with bounding
//! boxes and confidence scores.
//!
//! # Requirements
//!
//! - Tesseract 4.x or 5.x installed and on PATH
//! - Language data files (e.g., `eng.traineddata`)
//!
//! # Installation
//!
//! - Windows: `winget install tesseract` or download from GitHub
//! - macOS: `brew install tesseract`
//! - Linux: `apt install tesseract-ocr`

use std::io::Write;
use std::process::Command;

use super::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
use crate::error::{PdfError, PdfResult};

/// OCR engine backed by the Tesseract command-line tool.
///
/// Invokes `tesseract` in TSV output mode for structured results.
///
/// # Example
///
/// ```no_run
/// use pdfpurr::ocr::tesseract_engine::TesseractEngine;
/// use pdfpurr::ocr::OcrEngine;
///
/// let engine = TesseractEngine::new("eng", None);
/// ```
pub struct TesseractEngine {
    /// Tesseract language code (e.g., "eng", "deu", "jpn").
    language: String,
    /// Path to tesseract binary (None = search PATH).
    tesseract_path: Option<String>,
}

impl TesseractEngine {
    /// Creates a new Tesseract engine for the given language.
    ///
    /// Set `tesseract_path` to `None` to find tesseract on PATH,
    /// or provide an explicit path to the binary.
    pub fn new(language: &str, tesseract_path: Option<&str>) -> Self {
        Self {
            language: language.to_string(),
            tesseract_path: tesseract_path.map(|s| s.to_string()),
        }
    }

    /// Creates a Tesseract engine for English using the system PATH.
    pub fn english() -> Self {
        Self::new("eng", None)
    }

    /// Checks whether tesseract is available.
    pub fn is_available(&self) -> bool {
        let cmd = self.tesseract_cmd();
        Command::new(cmd)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn tesseract_cmd(&self) -> &str {
        self.tesseract_path
            .as_deref()
            .unwrap_or(super::constants::TESSERACT_CMD)
    }
}

impl OcrEngine for TesseractEngine {
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult> {
        let temp_dir = std::env::temp_dir();
        let pgm_path = temp_dir.join(super::constants::TEMP_INPUT_PGM);
        let tsv_base = temp_dir.join(super::constants::TEMP_OUTPUT_BASE);

        // Write grayscale image as PGM (simplest format Tesseract accepts)
        write_pgm(&pgm_path, image)?;

        // Run tesseract: input.pgm → output.tsv
        let cmd = self.tesseract_cmd();
        let output = Command::new(cmd)
            .args([
                pgm_path.to_str().unwrap(),
                tsv_base.to_str().unwrap(),
                "-l",
                &self.language,
                "tsv",
            ])
            .output()
            .map_err(|e| {
                PdfError::OcrError(format!("Failed to run tesseract (is it installed?): {e}"))
            })?;

        // Clean up input
        let _ = std::fs::remove_file(&pgm_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let _ = std::fs::remove_file(tsv_base.with_extension("tsv"));
            return Err(PdfError::OcrError(format!("Tesseract failed: {stderr}")));
        }

        // Read TSV output
        let tsv_path = tsv_base.with_extension("tsv");
        let tsv_data = std::fs::read_to_string(&tsv_path)
            .map_err(|e| PdfError::OcrError(format!("Cannot read Tesseract output: {e}")))?;
        let _ = std::fs::remove_file(&tsv_path);

        parse_tesseract_tsv(&tsv_data, image.width, image.height)
    }
}

/// Writes a grayscale image as PGM (Portable Graymap, P5 binary format).
fn write_pgm(path: &std::path::Path, image: &OcrImage) -> PdfResult<()> {
    let mut file = std::fs::File::create(path)
        .map_err(|e| PdfError::OcrError(format!("Cannot create PGM: {e}")))?;

    // PGM header
    write!(file, "P5\n{} {}\n255\n", image.width, image.height)?;
    file.write_all(&image.data)?;
    Ok(())
}

/// Parses Tesseract TSV output into an OcrResult.
///
/// TSV format (tab-separated):
/// level  page_num  block_num  par_num  line_num  word_num  left  top  width  height  conf  text
///
/// We extract word-level entries (level == 5) with confidence > 0.
pub fn parse_tesseract_tsv(tsv: &str, image_width: u32, image_height: u32) -> PdfResult<OcrResult> {
    let mut words = Vec::new();

    for line in tsv.lines().skip(1) {
        // Skip header
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 12 {
            continue;
        }

        let level: i32 = fields[0].parse().unwrap_or(-1);
        if level != 5 {
            continue; // Only word-level entries
        }

        let left: u32 = fields[6].parse().unwrap_or(0);
        let top: u32 = fields[7].parse().unwrap_or(0);
        let width: u32 = fields[8].parse().unwrap_or(0);
        let height: u32 = fields[9].parse().unwrap_or(0);
        let conf: f32 = fields[10].parse().unwrap_or(-1.0);
        let text = fields[11].trim();

        if text.is_empty() || conf < 0.0 {
            continue;
        }

        words.push(OcrWord {
            text: text.to_string(),
            x: left,
            y: top,
            width,
            height,
            confidence: conf / 100.0, // Tesseract uses 0-100, we use 0.0-1.0
        });
    }

    Ok(OcrResult {
        words,
        image_width,
        image_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tsv_empty() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let result = parse_tesseract_tsv(tsv, 800, 600).unwrap();
        assert!(result.words.is_empty());
    }

    #[test]
    fn parse_tsv_single_word() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t100\t200\t150\t30\t95.5\tHello\n";
        let result = parse_tesseract_tsv(tsv, 800, 600).unwrap();
        assert_eq!(result.words.len(), 1);
        assert_eq!(result.words[0].text, "Hello");
        assert_eq!(result.words[0].x, 100);
        assert_eq!(result.words[0].y, 200);
        assert_eq!(result.words[0].width, 150);
        assert_eq!(result.words[0].height, 30);
        assert!((result.words[0].confidence - 0.955).abs() < 0.01);
    }

    #[test]
    fn parse_tsv_skips_non_word_levels() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t\n\
                    2\t1\t1\t0\t0\t0\t50\t50\t700\t500\t-1\t\n\
                    3\t1\t1\t1\t0\t0\t50\t50\t700\t30\t-1\t\n\
                    4\t1\t1\t1\t1\t0\t50\t50\t700\t30\t-1\t\n\
                    5\t1\t1\t1\t1\t1\t100\t200\t150\t30\t92.0\tWorld\n";
        let result = parse_tesseract_tsv(tsv, 800, 600).unwrap();
        assert_eq!(result.words.len(), 1);
        assert_eq!(result.words[0].text, "World");
    }

    #[test]
    fn parse_tsv_multiple_words() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t10\t20\t80\t25\t95.0\tHello\n\
                    5\t1\t1\t1\t1\t2\t100\t20\t90\t25\t88.0\tWorld\n";
        let result = parse_tesseract_tsv(tsv, 800, 600).unwrap();
        assert_eq!(result.words.len(), 2);
        assert_eq!(result.words[0].text, "Hello");
        assert_eq!(result.words[1].text, "World");
    }

    #[test]
    fn parse_tsv_skips_empty_text() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t10\t20\t80\t25\t95.0\t\n\
                    5\t1\t1\t1\t1\t2\t100\t20\t90\t25\t88.0\tReal\n";
        let result = parse_tesseract_tsv(tsv, 800, 600).unwrap();
        assert_eq!(result.words.len(), 1);
        assert_eq!(result.words[0].text, "Real");
    }

    #[test]
    fn write_and_read_pgm() {
        let image = OcrImage {
            data: vec![128; 30],
            width: 6,
            height: 5,
        };
        let path = std::env::temp_dir().join("pdfpurr_test.pgm");
        write_pgm(&path, &image).unwrap();
        let data = std::fs::read(&path).unwrap();
        assert!(data.starts_with(b"P5\n"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn confidence_normalized_to_0_1() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t10\t20\t80\t25\t75.0\tTest\n";
        let result = parse_tesseract_tsv(tsv, 100, 100).unwrap();
        assert!((result.words[0].confidence - 0.75).abs() < 0.01);
    }
}
