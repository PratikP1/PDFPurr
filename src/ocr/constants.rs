//! Shared constants for OCR engines.

/// Vertical gap (pixels) above which OCR words start a new paragraph.
pub const PARAGRAPH_GAP_THRESHOLD: u32 = 30;

/// Temp file name for OCR input image (PNG format, for Windows OCR).
pub const TEMP_INPUT_PNG: &str = "pdfpurr_ocr_input.png";
/// Temp file name for OCR input image (PGM format, for Tesseract).
pub const TEMP_INPUT_PGM: &str = "pdfpurr_ocr_input.pgm";
/// Temp file base name for Tesseract TSV output.
pub const TEMP_OUTPUT_BASE: &str = "pdfpurr_ocr_output";
/// Windows PowerShell 5.1 executable (required for WinRT type projection).
pub const POWERSHELL_CMD: &str = "powershell";
/// Default Tesseract executable name.
pub const TESSERACT_CMD: &str = "tesseract";
