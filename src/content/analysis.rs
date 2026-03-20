//! Content stream analysis for structure detection.
//!
//! Extracts positioned text runs from PDF content streams, preserving
//! font name, size, position, color, and style (bold/italic/monospaced).
//! This is the foundation for auto-tagging untagged PDFs and detecting
//! headings, lists, tables, and other structural elements.

use std::collections::HashMap;

use crate::core::objects::Object;
use crate::error::PdfResult;
use crate::fonts::font::Font;

/// A positioned text fragment with font and style metadata.
///
/// Each `TextRun` represents a contiguous string rendered with the
/// same font at a specific position. Structure detection algorithms
/// use these runs to identify headings, paragraphs, lists, and tables.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// The decoded Unicode text.
    pub text: String,
    /// Font base name (e.g., "Helvetica-Bold", "Courier").
    pub font_name: String,
    /// Font size in page coordinates (after CTM scaling).
    pub font_size: f64,
    /// X position in page coordinates (origin at bottom-left).
    pub x: f64,
    /// Y position in page coordinates (origin at bottom-left).
    pub y: f64,
    /// Total advance width in page coordinates.
    pub width: f64,
    /// Approximate height (from font size).
    pub height: f64,
    /// Fill color as RGBA (0.0–1.0 per channel).
    pub color: [f32; 4],
    /// PDF text rendering mode (0=fill, 3=invisible/OCR).
    pub rendering_mode: u8,
    /// Whether the font name indicates bold weight.
    pub is_bold: bool,
    /// Whether the font name indicates italic/oblique style.
    pub is_italic: bool,
    /// Whether the font name indicates a monospaced typeface.
    pub is_monospaced: bool,
}

/// Detects font style from a PDF font base name.
///
/// Strips subset prefixes (e.g., "BCDFGX+Calibri" → "Calibri"),
/// then checks for known bold, italic, and monospaced indicators
/// in the font name segments.
///
/// Returns `(is_bold, is_italic, is_monospaced)`.
pub fn font_style_from_name(name: &str) -> (bool, bool, bool) {
    // Strip subset prefix: "ABCDEF+" → remove everything before and including "+"
    let clean = match name.find('+') {
        Some(pos) if pos <= 6 && name[..pos].chars().all(|c| c.is_ascii_uppercase()) => {
            &name[pos + 1..]
        }
        _ => name,
    };

    let lower = clean.to_ascii_lowercase();

    let bold = lower.contains("bold")
        || lower.contains("-bd")
        || lower.contains("heavy")
        || lower.contains("black")
        || lower.contains("demi")
        || lower.contains(",bold");

    let italic = lower.contains("italic")
        || lower.contains("oblique")
        || lower.contains("slant")
        || lower.contains(",italic")
        || lower.contains("-it");

    let monospaced = lower.contains("courier")
        || lower.contains("consolas")
        || lower.contains("mono")
        || lower.contains("menlo")
        || lower.contains("code")
        || lower.contains("fixed")
        || lower.contains("typewriter");

    (bold, italic, monospaced)
}

/// Extracts positioned text runs from a PDF content stream.
///
/// Walks the content stream tokens, tracking the text matrix (tm),
/// current transformation matrix (CTM), font, and fill color. For
/// each text-showing operator (Tj, TJ, ', "), emits a [`TextRun`]
/// with the text decoded via the font's encoding and positioned in
/// page coordinates.
pub fn extract_text_runs(data: &[u8], fonts: &HashMap<String, Font>) -> PdfResult<Vec<TextRun>> {
    use crate::content::operators::{tokenize_content_stream, ContentToken};

    let tokens = tokenize_content_stream(data)?;
    let mut runs = Vec::new();

    // Analysis state
    let mut ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0f64]; // identity
    let mut ctm_stack: Vec<[f64; 6]> = Vec::new();
    let mut fill_color = [0.0f32, 0.0, 0.0, 1.0]; // black, opaque

    // Text state
    let mut font_name = String::new();
    let mut font_size = 0.0f64;
    let mut character_spacing = 0.0f64;
    let mut word_spacing = 0.0f64;
    let mut leading = 0.0f64;
    let mut horizontal_scaling = 1.0f64;
    let mut rise = 0.0f64;
    let mut rendering_mode = 0u8;

    // Text matrix state
    let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0f64];
    let mut tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0f64];
    let mut in_text = false;

    // Operand stack
    let mut operands: Vec<&Object> = Vec::new();

    for token in &tokens {
        match token {
            ContentToken::Operand(obj) => {
                operands.push(obj);
            }
            ContentToken::Operator(op) => {
                match op.as_str() {
                    // --- Graphics state ---
                    "q" => {
                        ctm_stack.push(ctm);
                    }
                    "Q" => {
                        ctm = ctm_stack.pop().unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                    }
                    "cm" => {
                        if operands.len() >= 6 {
                            let m = [
                                obj_f64(operands[0]),
                                obj_f64(operands[1]),
                                obj_f64(operands[2]),
                                obj_f64(operands[3]),
                                obj_f64(operands[4]),
                                obj_f64(operands[5]),
                            ];
                            ctm = mat_concat(&ctm, &m);
                        }
                    }

                    // --- Color operators ---
                    "g" => {
                        if let Some(g) = operands.first().and_then(|o| o.as_f64()) {
                            let g = g as f32;
                            fill_color = [g, g, g, 1.0];
                        }
                    }
                    "rg" => {
                        if operands.len() >= 3 {
                            fill_color = [
                                obj_f32(operands[0]),
                                obj_f32(operands[1]),
                                obj_f32(operands[2]),
                                1.0,
                            ];
                        }
                    }
                    "k" => {
                        if operands.len() >= 4 {
                            let c = obj_f32(operands[0]);
                            let m = obj_f32(operands[1]);
                            let y = obj_f32(operands[2]);
                            let k = obj_f32(operands[3]);
                            // CMYK → RGB approximation
                            fill_color = [
                                (1.0 - c) * (1.0 - k),
                                (1.0 - m) * (1.0 - k),
                                (1.0 - y) * (1.0 - k),
                                1.0,
                            ];
                        }
                    }
                    "sc" | "scn" => {
                        // Generic fill color — use last 1-4 operands as gray/RGB/CMYK
                        match operands.len() {
                            1 => {
                                let g = obj_f32(operands[0]);
                                fill_color = [g, g, g, 1.0];
                            }
                            3 => {
                                fill_color = [
                                    obj_f32(operands[0]),
                                    obj_f32(operands[1]),
                                    obj_f32(operands[2]),
                                    1.0,
                                ];
                            }
                            n if n >= 4 => {
                                let c = obj_f32(operands[0]);
                                let m = obj_f32(operands[1]);
                                let y = obj_f32(operands[2]);
                                let k = obj_f32(operands[3]);
                                fill_color = [
                                    (1.0 - c) * (1.0 - k),
                                    (1.0 - m) * (1.0 - k),
                                    (1.0 - y) * (1.0 - k),
                                    1.0,
                                ];
                            }
                            _ => {}
                        }
                    }

                    // --- Text state ---
                    "BT" => {
                        in_text = true;
                        tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                        tlm = tm;
                    }
                    "ET" => {
                        in_text = false;
                    }
                    "Tf" => {
                        if operands.len() >= 2 {
                            if let Some(name) = operands[0].as_name() {
                                font_name = name.to_string();
                            }
                            font_size = obj_f64(operands[1]);
                        }
                    }
                    "Td" => {
                        if operands.len() >= 2 {
                            let tx = obj_f64(operands[0]);
                            let ty = obj_f64(operands[1]);
                            tlm = mat_translate(&tlm, tx, ty);
                            tm = tlm;
                        }
                    }
                    "TD" => {
                        if operands.len() >= 2 {
                            let tx = obj_f64(operands[0]);
                            let ty = obj_f64(operands[1]);
                            leading = -ty;
                            tlm = mat_translate(&tlm, tx, ty);
                            tm = tlm;
                        }
                    }
                    "Tm" => {
                        if operands.len() >= 6 {
                            tm = [
                                obj_f64(operands[0]),
                                obj_f64(operands[1]),
                                obj_f64(operands[2]),
                                obj_f64(operands[3]),
                                obj_f64(operands[4]),
                                obj_f64(operands[5]),
                            ];
                            tlm = tm;
                        }
                    }
                    "T*" => {
                        tlm = mat_translate(&tlm, 0.0, -leading);
                        tm = tlm;
                    }
                    "Tc" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_f64()) {
                            character_spacing = v;
                        }
                    }
                    "Tw" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_f64()) {
                            word_spacing = v;
                        }
                    }
                    "TL" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_f64()) {
                            leading = v;
                        }
                    }
                    "Tz" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_f64()) {
                            horizontal_scaling = v / 100.0;
                        }
                    }
                    "Ts" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_f64()) {
                            rise = v;
                        }
                    }
                    "Tr" => {
                        if let Some(v) = operands.first().and_then(|o| o.as_i64()) {
                            rendering_mode = v as u8;
                        }
                    }

                    // --- Text showing ---
                    "Tj" | "'" | "\"" => {
                        if !in_text {
                            operands.clear();
                            continue;
                        }
                        // Handle " operator: set word_spacing, char_spacing, then show
                        if op == "\"" && operands.len() >= 3 {
                            word_spacing = obj_f64(operands[0]);
                            character_spacing = obj_f64(operands[1]);
                        }
                        // Handle ' and " operators: advance to next line first
                        if op == "'" || op == "\"" {
                            tlm = mat_translate(&tlm, 0.0, -leading);
                            tm = tlm;
                        }

                        let text_obj = operands.last().and_then(|o| o.as_pdf_string());
                        if let Some(s) = text_obj {
                            let resolved_font = fonts.get(font_name.trim_start_matches('/'));
                            let text = match resolved_font {
                                Some(f) => f.decode_bytes(&s.bytes),
                                None => String::from_utf8_lossy(&s.bytes).to_string(),
                            };

                            if !text.is_empty() {
                                let (px, py) = page_position(&tm, &ctm, rise);
                                let page_font_size = font_size * ctm_scale(&ctm);
                                let (bold, italic, mono) = font_style_from_name(
                                    resolved_font.map(|f| f.name()).unwrap_or(&font_name),
                                );

                                let char_count = text.chars().count() as f64;
                                let space_count = text.chars().filter(|c| *c == ' ').count() as f64;
                                let scale = ctm_scale(&ctm);
                                // Width: approximate glyph widths + character/word spacing
                                let base_width = char_count * font_size * 0.5 * horizontal_scaling;
                                let spacing =
                                    char_count * character_spacing + space_count * word_spacing;
                                let advance = base_width + spacing;
                                let page_width = advance * scale;

                                runs.push(TextRun {
                                    text,
                                    font_name: resolved_font
                                        .map(|f| f.name().to_string())
                                        .unwrap_or_else(|| font_name.clone()),
                                    font_size: page_font_size,
                                    x: px,
                                    y: py,
                                    width: page_width,
                                    height: page_font_size,
                                    color: fill_color,
                                    rendering_mode,
                                    is_bold: bold,
                                    is_italic: italic,
                                    is_monospaced: mono,
                                });

                                tm[4] += advance;
                            }
                        }
                    }
                    "TJ" => {
                        if !in_text {
                            operands.clear();
                            continue;
                        }
                        if let Some(arr) = operands.first().and_then(|o| o.as_array()) {
                            let resolved_font = fonts.get(font_name.trim_start_matches('/'));
                            let (bold, italic, mono) = font_style_from_name(
                                resolved_font.map(|f| f.name()).unwrap_or(&font_name),
                            );
                            let page_font_size = font_size * ctm_scale(&ctm);

                            for element in arr {
                                match element {
                                    Object::String(s) => {
                                        let text = match resolved_font {
                                            Some(f) => f.decode_bytes(&s.bytes),
                                            None => String::from_utf8_lossy(&s.bytes).to_string(),
                                        };
                                        if !text.is_empty() {
                                            let (px, py) = page_position(&tm, &ctm, rise);
                                            let char_count = text.chars().count() as f64;
                                            let space_count =
                                                text.chars().filter(|c| *c == ' ').count() as f64;
                                            let scale = ctm_scale(&ctm);
                                            let base_width =
                                                char_count * font_size * 0.5 * horizontal_scaling;
                                            let spacing = char_count * character_spacing
                                                + space_count * word_spacing;
                                            let advance = base_width + spacing;
                                            let page_width = advance * scale;

                                            runs.push(TextRun {
                                                text,
                                                font_name: resolved_font
                                                    .map(|f| f.name().to_string())
                                                    .unwrap_or_else(|| font_name.clone()),
                                                font_size: page_font_size,
                                                x: px,
                                                y: py,
                                                width: page_width,
                                                height: page_font_size,
                                                color: fill_color,
                                                rendering_mode,
                                                is_bold: bold,
                                                is_italic: italic,
                                                is_monospaced: mono,
                                            });

                                            tm[4] += advance;
                                        }
                                    }
                                    Object::Integer(n) => {
                                        // TJ displacement: negative = move right
                                        let displacement =
                                            -(*n as f64) / 1000.0 * font_size * horizontal_scaling;
                                        tm[4] += displacement;
                                    }
                                    Object::Real(n) => {
                                        let displacement =
                                            -(*n) / 1000.0 * font_size * horizontal_scaling;
                                        tm[4] += displacement;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {} // Skip non-text operators
                }
                operands.clear();
            }
            ContentToken::InlineImage { .. } => {
                operands.clear();
            }
        }
    }

    Ok(runs)
}

// --- Matrix math ---

/// Concatenates two 2D affine matrices: result = a * b.
fn mat_concat(a: &[f64; 6], b: &[f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// Translates a 2D affine matrix by (tx, ty).
fn mat_translate(m: &[f64; 6], tx: f64, ty: f64) -> [f64; 6] {
    [
        m[0],
        m[1],
        m[2],
        m[3],
        m[0] * tx + m[2] * ty + m[4],
        m[1] * tx + m[3] * ty + m[5],
    ]
}

/// Computes page-space position from text matrix and CTM.
fn page_position(tm: &[f64; 6], ctm: &[f64; 6], rise: f64) -> (f64, f64) {
    // Apply rise to text space Y
    let tx = tm[4];
    let ty = tm[5] + rise;
    // Transform to page coordinates: (tx, ty) * CTM
    let px = tx * ctm[0] + ty * ctm[2] + ctm[4];
    let py = tx * ctm[1] + ty * ctm[3] + ctm[5];
    (px, py)
}

/// Extracts the uniform scale factor from a CTM.
fn ctm_scale(ctm: &[f64; 6]) -> f64 {
    (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt()
}

/// Extracts f64 from an Object (Integer or Real).
fn obj_f64(obj: &Object) -> f64 {
    match obj {
        Object::Integer(n) => *n as f64,
        Object::Real(n) => *n,
        _ => 0.0,
    }
}

/// Extracts f32 from an Object.
fn obj_f32(obj: &Object) -> f32 {
    obj_f64(obj) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- font_style_from_name tests ---

    #[test]
    fn style_helvetica_bold() {
        let (bold, italic, mono) = font_style_from_name("Helvetica-Bold");
        assert!(bold);
        assert!(!italic);
        assert!(!mono);
    }

    #[test]
    fn style_times_bold_italic() {
        let (bold, italic, mono) = font_style_from_name("Times-BoldItalic");
        assert!(bold);
        assert!(italic);
        assert!(!mono);
    }

    #[test]
    fn style_courier_is_mono() {
        let (bold, italic, mono) = font_style_from_name("Courier");
        assert!(!bold);
        assert!(!italic);
        assert!(mono);
    }

    #[test]
    fn style_arial_mt_is_plain() {
        let (bold, italic, mono) = font_style_from_name("ArialMT");
        assert!(!bold);
        assert!(!italic);
        assert!(!mono);
    }

    #[test]
    fn style_subset_prefix_stripped() {
        let (bold, italic, mono) = font_style_from_name("BCDFGX+Calibri");
        assert!(!bold);
        assert!(!italic);
        assert!(!mono);
    }

    #[test]
    fn style_noto_sans_black_is_bold() {
        let (bold, italic, _) = font_style_from_name("NotoSans-Black");
        assert!(bold);
        assert!(!italic);
    }

    #[test]
    fn style_liberation_mono_oblique() {
        let (bold, italic, mono) = font_style_from_name("LiberationMono-Oblique");
        assert!(!bold);
        assert!(italic);
        assert!(mono);
    }

    #[test]
    fn style_empty_string() {
        let (bold, italic, mono) = font_style_from_name("");
        assert!(!bold);
        assert!(!italic);
        assert!(!mono);
    }

    #[test]
    fn style_courier_new_bold() {
        let (bold, _, mono) = font_style_from_name("CourierNew,Bold");
        assert!(bold);
        assert!(mono);
    }

    #[test]
    fn style_cambria_italic() {
        let (_, italic, _) = font_style_from_name("Cambria-Italic");
        assert!(italic);
    }

    // --- extract_text_runs tests ---

    fn make_fonts() -> HashMap<String, Font> {
        let mut fonts = HashMap::new();
        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        fonts.insert(
            "F1".to_string(),
            Font::from_dict(&helv.to_font_dictionary(), &|_| None).unwrap(),
        );
        fonts
    }

    #[test]
    fn simple_tj_produces_one_run() {
        let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "Hello");
        assert!((runs[0].x - 100.0).abs() < 1.0);
        assert!((runs[0].y - 700.0).abs() < 1.0);
        assert!((runs[0].font_size - 12.0).abs() < 0.1);
    }

    #[test]
    fn two_tj_operators_produce_two_runs() {
        let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ( World) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs[1].x > runs[0].x, "Second run should be further right");
    }

    #[test]
    fn ctm_translate_shifts_position() {
        let content = b"1 0 0 1 50 100 cm BT /F1 12 Tf 100 700 Td (Shifted) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        assert!((runs[0].x - 150.0).abs() < 1.0, "x should be 100+50=150");
        assert!((runs[0].y - 800.0).abs() < 1.0, "y should be 700+100=800");
    }

    #[test]
    fn ctm_scale_doubles_font_size() {
        let content = b"2 0 0 2 0 0 cm BT /F1 12 Tf 100 700 Td (Big) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(
            (runs[0].font_size - 24.0).abs() < 0.1,
            "font_size should be 12*2=24, got {}",
            runs[0].font_size
        );
    }

    #[test]
    fn color_operator_sets_run_color() {
        let content = b"1 0 0 rg BT /F1 12 Tf 100 700 Td (Red) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        assert!((runs[0].color[0] - 1.0).abs() < 0.01, "R should be 1.0");
        assert!((runs[0].color[1]).abs() < 0.01, "G should be 0.0");
        assert!((runs[0].color[2]).abs() < 0.01, "B should be 0.0");
    }

    #[test]
    fn rendering_mode_3_still_emitted() {
        let content = b"BT 3 Tr /F1 12 Tf 100 700 Td (Invisible) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].rendering_mode, 3);
        assert_eq!(runs[0].text, "Invisible");
    }

    #[test]
    fn empty_content_produces_no_runs() {
        let fonts = make_fonts();
        let runs = extract_text_runs(b"", &fonts).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn no_text_operators_produces_no_runs() {
        let content = b"q 1 0 0 1 0 0 cm Q";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn q_q_restores_ctm() {
        let content = b"q 1 0 0 1 50 100 cm Q BT /F1 12 Tf 100 700 Td (NoShift) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert_eq!(runs.len(), 1);
        // CTM should be restored to identity after Q
        assert!(
            (runs[0].x - 100.0).abs() < 1.0,
            "x should be 100 (CTM restored), got {}",
            runs[0].x
        );
    }

    #[test]
    fn gray_color_operator() {
        let content = b"0.5 g BT /F1 12 Tf 100 700 Td (Gray) Tj ET";
        let fonts = make_fonts();
        let runs = extract_text_runs(content, &fonts).unwrap();
        assert!((runs[0].color[0] - 0.5).abs() < 0.01);
        assert!((runs[0].color[1] - 0.5).abs() < 0.01);
        assert!((runs[0].color[2] - 0.5).abs() < 0.01);
    }
}
