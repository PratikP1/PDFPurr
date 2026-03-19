//! Windows OCR engine via the built-in Windows.Media.Ocr API.
//!
//! Uses PowerShell to invoke the Windows Runtime OCR API, which provides
//! high-quality recognition with no additional software installation.
//! Available on Windows 10 and later.
//!
//! # Requirements
//!
//! - Windows 10 or later
//! - PowerShell 5.1+ (included with Windows)
//! - Language packs for non-English recognition (English is always available)

use std::io::Write;
use std::process::Command;

use super::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
use crate::error::{PdfError, PdfResult};

/// OCR engine backed by the Windows.Media.Ocr API.
///
/// Invokes PowerShell to run Windows Runtime OCR on a temporary BMP image.
/// Returns words with bounding boxes and confidence scores.
///
/// # Example
///
/// ```no_run
/// use pdfpurr::ocr::windows_engine::WindowsOcrEngine;
/// use pdfpurr::ocr::OcrEngine;
///
/// let engine = WindowsOcrEngine::new("en-US");
/// ```
pub struct WindowsOcrEngine {
    /// BCP 47 language tag (e.g., "en-US", "de-DE", "ja").
    language: String,
}

impl WindowsOcrEngine {
    /// Creates a new Windows OCR engine for the given language.
    ///
    /// The language must be installed on the system. English ("en-US")
    /// is always available on Windows 10+.
    pub fn new(language: &str) -> Self {
        Self {
            language: language.to_string(),
        }
    }

    /// Creates a Windows OCR engine for English.
    pub fn english() -> Self {
        Self::new("en-US")
    }
}

impl OcrEngine for WindowsOcrEngine {
    fn recognize(&self, image: &OcrImage) -> PdfResult<OcrResult> {
        // Write image as RGB PNG (WinRT BitmapDecoder may not handle grayscale PNGs).
        let temp_dir = std::env::temp_dir();
        let bmp_path = temp_dir.join(super::constants::TEMP_INPUT_PNG);
        write_rgb_png(&bmp_path, image)?;

        // PowerShell script that invokes Windows OCR and outputs JSON.
        // Uses WindowsRuntime interop with proper async/await pattern.
        let ps_script = format!(
            r#"
Add-Type -AssemblyName System.Runtime.WindowsRuntime

# Load WinRT types
$null = [Windows.Media.Ocr.OcrEngine,Windows.Foundation,ContentType=WindowsRuntime]
$null = [Windows.Graphics.Imaging.BitmapDecoder,Windows.Foundation,ContentType=WindowsRuntime]
$null = [Windows.Storage.StorageFile,Windows.Foundation,ContentType=WindowsRuntime]

# Load WinRT async extensions via reflection (reliable in subprocess mode)
$asm = [System.Reflection.Assembly]::LoadWithPartialName('System.Runtime.WindowsRuntime')
$extType = $asm.GetType('System.WindowsRuntimeSystemExtensions')
$asTask = ($extType.GetMethods() | Where-Object {{
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation``1'
}})[0]

function AwaitT($AsyncOp, [Type]$ResultType) {{
    $task = $asTask.MakeGenericMethod($ResultType).Invoke($null, @($AsyncOp))
    $task.Wait()
    $task.Result
}}

# Create engine — TryCreateFromUserProfileLanguages is most reliable
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($engine -eq $null) {{
    # Fall back to specific language
    try {{
        $lang = [Windows.Globalization.Language]::new("{lang}")
        $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage($lang)
    }} catch {{}}
}}
if ($engine -eq $null) {{
    Write-Error "No OCR engine available"
    exit 1
}}

$path = "{bmp_path}"
$file = AwaitT ([Windows.Storage.StorageFile]::GetFileFromPathAsync($path)) ([Windows.Storage.StorageFile])
$stream = AwaitT ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
$decoder = AwaitT ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
$bitmap = AwaitT ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
$result = AwaitT ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])

$stream.Dispose()

$words = @()
foreach ($line in $result.Lines) {{
    foreach ($word in $line.Words) {{
        $rect = $word.BoundingRect
        $words += @{{
            text = $word.Text
            x = [int]$rect.X
            y = [int]$rect.Y
            width = [int]$rect.Width
            height = [int]$rect.Height
        }}
    }}
}}

$output = @{{ words = $words }} | ConvertTo-Json -Depth 3 -Compress
Write-Output $output
"#,
            lang = self.language,
            bmp_path = bmp_path.to_string_lossy().replace('\\', "/"),
        );

        // Must use Windows PowerShell 5.1 (not pwsh/PS 7) for WinRT type projection.
        // PS 7 removed WinRT support. Use "powershell" (not "pwsh") to get 5.1.
        let output = Command::new(super::constants::POWERSHELL_CMD)
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
            .output()
            .map_err(|e| PdfError::OcrError(format!("Failed to run PowerShell: {e}")))?;

        // Clean up temp file (keep on failure for debugging)
        if output.status.success() {
            let _ = std::fs::remove_file(&bmp_path);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(PdfError::OcrError(format!(
                "Windows OCR failed: {stderr}\nstdout: {stdout}"
            )));
        }

        parse_windows_ocr_output(&stdout, image.width, image.height)
    }
}

/// Writes a grayscale image as RGB PNG (WinRT BitmapDecoder handles RGB reliably).
fn write_rgb_png(path: &std::path::Path, image: &OcrImage) -> PdfResult<()> {
    let file = std::fs::File::create(path)
        .map_err(|e| PdfError::OcrError(format!("Cannot create PNG: {e}")))?;
    let w = std::io::BufWriter::new(file);

    let mut encoder = png::Encoder::new(w, image.width, image.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);

    // Expand grayscale → RGB (triple each byte)
    let rgb_data: Vec<u8> = image.data.iter().flat_map(|&g| [g, g, g]).collect();

    let mut writer = encoder
        .write_header()
        .map_err(|e| PdfError::OcrError(format!("PNG header error: {e}")))?;
    writer
        .write_image_data(&rgb_data)
        .map_err(|e| PdfError::OcrError(format!("PNG write error: {e}")))?;

    Ok(())
}

/// Writes a grayscale image as a 8-bit BMP file (kept for platforms where PNG crate unavailable).
#[allow(dead_code)]
fn write_grayscale_bmp(path: &std::path::Path, image: &OcrImage) -> PdfResult<()> {
    let w = image.width as usize;
    let h = image.height as usize;

    // BMP row stride must be multiple of 4
    let row_stride = (w + 3) & !3;
    let pixel_data_size = row_stride * h;

    // BMP file header (14 bytes) + DIB header (40 bytes) + color table (256 * 4 bytes)
    let color_table_size = 256 * 4;
    let header_size = 14 + 40 + color_table_size;
    let file_size = header_size + pixel_data_size;

    let mut file = std::fs::File::create(path)
        .map_err(|e| PdfError::OcrError(format!("Cannot create BMP: {e}")))?;

    // BMP file header
    file.write_all(b"BM")?; // signature
    file.write_all(&(file_size as u32).to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?; // reserved
    file.write_all(&(header_size as u32).to_le_bytes())?; // pixel data offset

    // DIB header (BITMAPINFOHEADER)
    file.write_all(&40u32.to_le_bytes())?; // header size
    file.write_all(&(w as i32).to_le_bytes())?; // width
    file.write_all(&(-(h as i32)).to_le_bytes())?; // height (negative = top-down)
    file.write_all(&1u16.to_le_bytes())?; // planes
    file.write_all(&8u16.to_le_bytes())?; // bits per pixel
    file.write_all(&0u32.to_le_bytes())?; // compression (none)
    file.write_all(&(pixel_data_size as u32).to_le_bytes())?;
    file.write_all(&2835u32.to_le_bytes())?; // x pixels per meter (72 DPI)
    file.write_all(&2835u32.to_le_bytes())?; // y pixels per meter
    file.write_all(&256u32.to_le_bytes())?; // colors used
    file.write_all(&0u32.to_le_bytes())?; // important colors

    // Grayscale color table (256 entries: R,G,B,0)
    for i in 0..=255u8 {
        file.write_all(&[i, i, i, 0])?;
    }

    // Pixel data (top-down, padded rows)
    let padding = [0u8; 4];
    let pad_bytes = row_stride - w;
    for y in 0..h {
        let row_start = y * w;
        let row_end = row_start + w;
        file.write_all(&image.data[row_start..row_end])?;
        if pad_bytes > 0 {
            file.write_all(&padding[..pad_bytes])?;
        }
    }

    Ok(())
}

/// Parses the JSON output from the PowerShell OCR script.
fn parse_windows_ocr_output(
    json: &str,
    image_width: u32,
    image_height: u32,
) -> PdfResult<OcrResult> {
    let json = json.trim();
    if json.is_empty() {
        return Ok(OcrResult {
            words: Vec::new(),
            image_width,
            image_height,
        });
    }

    // Simple JSON parsing — avoid adding serde_json as required dep.
    // Format: {"words":[{"text":"Hello","x":10,"y":20,"width":80,"height":25},...]}
    let mut words = Vec::new();

    // Find the words array
    let words_start = json.find("[").unwrap_or(0);
    let words_end = json.rfind("]").unwrap_or(json.len());
    if words_start >= words_end {
        return Ok(OcrResult {
            words,
            image_width,
            image_height,
        });
    }

    let words_str = &json[words_start + 1..words_end];

    // Split by },{ to get individual word objects
    for obj_str in words_str.split("},{") {
        let obj = obj_str.trim().trim_start_matches('{').trim_end_matches('}');
        let text = extract_json_string(obj, "text").unwrap_or_default();
        let x = extract_json_int(obj, "x").unwrap_or(0);
        let y = extract_json_int(obj, "y").unwrap_or(0);
        let width = extract_json_int(obj, "width").unwrap_or(0);
        let height = extract_json_int(obj, "height").unwrap_or(0);

        if !text.is_empty() {
            words.push(OcrWord {
                text,
                x: x as u32,
                y: y as u32,
                width: width as u32,
                height: height as u32,
                confidence: 0.9, // Windows OCR doesn't expose per-word confidence
            });
        }
    }

    Ok(OcrResult {
        words,
        image_width,
        image_height,
    })
}

fn extract_json_string(obj: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = obj.find(&pattern)? + pattern.len();
    let end = obj[start..].find('"')? + start;
    Some(obj[start..end].to_string())
}

fn extract_json_int(obj: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\":", key);
    let start = obj.find(&pattern)? + pattern.len();
    let num_str: String = obj[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    num_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_json() {
        let result = parse_windows_ocr_output("", 100, 100).unwrap();
        assert!(result.words.is_empty());
    }

    #[test]
    fn parse_single_word() {
        let json = r#"{"words":[{"text":"Hello","x":10,"y":20,"width":80,"height":25}]}"#;
        let result = parse_windows_ocr_output(json, 800, 600).unwrap();
        assert_eq!(result.words.len(), 1);
        assert_eq!(result.words[0].text, "Hello");
        assert_eq!(result.words[0].x, 10);
        assert_eq!(result.words[0].y, 20);
        assert_eq!(result.words[0].width, 80);
        assert_eq!(result.words[0].height, 25);
    }

    #[test]
    fn parse_multiple_words() {
        let json = r#"{"words":[{"text":"Hello","x":10,"y":20,"width":80,"height":25},{"text":"World","x":100,"y":20,"width":90,"height":25}]}"#;
        let result = parse_windows_ocr_output(json, 800, 600).unwrap();
        assert_eq!(result.words.len(), 2);
        assert_eq!(result.words[0].text, "Hello");
        assert_eq!(result.words[1].text, "World");
    }

    #[test]
    fn write_and_verify_bmp() {
        let image = OcrImage {
            data: vec![128; 100],
            width: 10,
            height: 10,
        };
        let path = std::env::temp_dir().join("pdfpurr_test_bmp.bmp");
        write_grayscale_bmp(&path, &image).unwrap();
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..2], b"BM"); // BMP signature
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn extract_json_fields() {
        let obj = r#""text":"Hello","x":42,"y":10,"width":80,"height":25"#;
        assert_eq!(extract_json_string(obj, "text"), Some("Hello".to_string()));
        assert_eq!(extract_json_int(obj, "x"), Some(42));
        assert_eq!(extract_json_int(obj, "width"), Some(80));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_ocr_engine_creates() {
        let engine = WindowsOcrEngine::english();
        assert_eq!(engine.language, "en-US");
    }
}
