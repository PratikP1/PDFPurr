//! Native Windows OCR engine using WinRT `Windows.Media.Ocr` API.
//!
//! Requires the `ocr-windows-native` feature and Windows 10+.
//! Faster and more reliable than the PowerShell subprocess approach.

#[cfg(feature = "ocr-windows-native")]
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
};

use super::engine::{OcrEngine as PdfOcrEngine, OcrImage, OcrResult, OcrWord};
use crate::error::{PdfError, PdfResult};

/// Native Windows OCR engine using WinRT API.
///
/// Uses `Windows.Media.Ocr.OcrEngine` directly — no subprocess.
/// Available on Windows 10 and later.
///
/// # Example
///
/// ```no_run
/// use pdfpurr::ocr::windows_native_engine::WindowsNativeOcrEngine;
///
/// let engine = WindowsNativeOcrEngine::new().unwrap();
/// ```
#[cfg(feature = "ocr-windows-native")]
pub struct WindowsNativeOcrEngine {
    engine: OcrEngine,
}

#[cfg(feature = "ocr-windows-native")]
impl WindowsNativeOcrEngine {
    /// Creates a new engine using the system's default language profile.
    pub fn new() -> PdfResult<Self> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|e| PdfError::OcrError(format!("Failed to create WinRT OCR engine: {e}")))?;
        Ok(Self { engine })
    }

    /// Creates an engine for a specific language (e.g., "en-US", "ja").
    pub fn with_language(lang_tag: &str) -> PdfResult<Self> {
        let language = Language::CreateLanguage(&lang_tag.into())
            .map_err(|e| PdfError::OcrError(format!("Invalid language tag '{lang_tag}': {e}")))?;
        let engine = OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|e| PdfError::OcrError(format!("OCR engine for '{lang_tag}': {e}")))?;
        Ok(Self { engine })
    }
}

#[cfg(feature = "ocr-windows-native")]
impl PdfOcrEngine for WindowsNativeOcrEngine {
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult> {
        let w = image.width as i32;
        let h = image.height as i32;

        // Create BGRA8 SoftwareBitmap from grayscale image data
        let bitmap = SoftwareBitmap::Create(BitmapPixelFormat::Gray8, w, h)
            .map_err(|e| PdfError::OcrError(format!("Create bitmap: {e}")))?;

        // Copy pixel data
        bitmap
            .CopyFromBuffer(&create_buffer(&image.data)?)
            .map_err(|e| PdfError::OcrError(format!("Copy pixels: {e}")))?;

        // Run OCR (async → blocking wait)
        let async_op = self
            .engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| PdfError::OcrError(format!("RecognizeAsync start: {e}")))?;

        // Block on the WinRT async operation using a manual event
        // SetCompleted callback signals when done, then GetResults retrieves output
        let event = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let event2 = event.clone();
        async_op
            .SetCompleted(&windows_future::AsyncOperationCompletedHandler::new(
                move |_, _| {
                    let (lock, cvar) = &*event2;
                    *lock.lock().unwrap() = true;
                    cvar.notify_one();
                    Ok(())
                },
            ))
            .map_err(|e| PdfError::OcrError(format!("SetCompleted: {e}")))?;

        // Wait for completion (with 30s timeout)
        let (lock, cvar) = &*event;
        let guard = lock.lock().unwrap();
        let _guard = cvar
            .wait_timeout_while(guard, std::time::Duration::from_secs(30), |done| !*done)
            .unwrap();

        let result = async_op
            .GetResults()
            .map_err(|e| PdfError::OcrError(format!("GetResults: {e}")))?;

        // Extract words with bounding boxes
        let mut words = Vec::new();
        let lines = result
            .Lines()
            .map_err(|e| PdfError::OcrError(format!("Get lines: {e}")))?;

        for i in 0..lines.Size().unwrap_or(0) {
            let line: windows::Media::Ocr::OcrLine = lines
                .GetAt(i)
                .map_err(|e| PdfError::OcrError(format!("Get line {i}: {e}")))?;
            let line_words = line
                .Words()
                .map_err(|e| PdfError::OcrError(format!("Get words: {e}")))?;
            for j in 0..line_words.Size().unwrap_or(0) {
                let word: windows::Media::Ocr::OcrWord = line_words
                    .GetAt(j)
                    .map_err(|e| PdfError::OcrError(format!("Get word {j}: {e}")))?;
                let text = word
                    .Text()
                    .map_err(|e| PdfError::OcrError(format!("Get text: {e}")))?
                    .to_string();
                let rect = word
                    .BoundingRect()
                    .map_err(|e| PdfError::OcrError(format!("Get rect: {e}")))?;

                if !text.is_empty() {
                    words.push(OcrWord {
                        text,
                        x: rect.X as u32,
                        y: rect.Y as u32,
                        width: rect.Width as u32,
                        height: rect.Height as u32,
                        confidence: 0.95, // Windows OCR doesn't expose per-word confidence
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

/// Creates an IBuffer from a byte slice for SoftwareBitmap::CopyFromBuffer.
#[cfg(feature = "ocr-windows-native")]
fn create_buffer(data: &[u8]) -> PdfResult<windows::Storage::Streams::IBuffer> {
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| PdfError::OcrError(format!("Create stream: {e}")))?;
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| PdfError::OcrError(format!("Create writer: {e}")))?;
    writer
        .WriteBytes(data)
        .map_err(|e| PdfError::OcrError(format!("Write bytes: {e}")))?;
    let buffer = writer
        .DetachBuffer()
        .map_err(|e| PdfError::OcrError(format!("Detach buffer: {e}")))?;
    Ok(buffer)
}

#[cfg(all(feature = "ocr-windows-native", test))]
mod tests {
    use super::*;

    #[test]
    fn native_engine_creates_successfully() {
        let engine = WindowsNativeOcrEngine::new();
        assert!(
            engine.is_ok(),
            "Native engine should create: {:?}",
            engine.err()
        );
    }

    #[test]
    fn native_engine_recognizes_blank_image() {
        let engine = WindowsNativeOcrEngine::new().unwrap();
        let image = OcrImage {
            data: vec![255; 100 * 100], // white image
            width: 100,
            height: 100,
        };
        let result = engine.recognize(&image).unwrap();
        assert!(result.words.is_empty(), "Blank image should have no words");
    }

    #[test]
    fn native_engine_with_english() {
        let engine = WindowsNativeOcrEngine::with_language("en-US");
        assert!(
            engine.is_ok(),
            "English OCR engine should create: {:?}",
            engine.err()
        );
    }
}
