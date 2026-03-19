//! Text rendering for the PDF rendering engine.
//!
//! Handles text positioning (Td, TD, Tm, T*) and glyph painting
//! (Tj, TJ, ', ") using font metrics from Standard 14 fonts or
//! glyph outlines from embedded font programs.

use tiny_skia::{FillRule, Paint, Pixmap, Stroke, Transform};

use crate::fonts::standard14::Standard14Font;

use super::glyph::{CidFontProgram, FontProgram};
use super::graphics::RenderState;
use super::path::PathAccumulator;

/// Tracks text object state between BT and ET.
#[derive(Debug)]
pub(crate) struct TextObject {
    /// Text matrix (Tm in PDF spec 9.4.2).
    tm: [f64; 6],
    /// Text line matrix — remembers the start of the current line.
    tlm: [f64; 6],
    /// Whether we are inside a BT/ET block.
    active: bool,
}

impl TextObject {
    pub fn new() -> Self {
        Self {
            tm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            tlm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            active: false,
        }
    }

    /// BT — begin text object.
    pub fn begin(&mut self) {
        self.tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.active = true;
    }

    /// ET — end text object.
    pub fn end(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Td — move to the start of the next line, offset by (tx, ty).
    pub fn move_text_position(&mut self, tx: f64, ty: f64) {
        self.tlm = mat_translate(self.tlm, tx, ty);
        self.tm = self.tlm;
    }

    /// TD — same as setting leading to -ty, then doing Td.
    pub fn move_text_position_td(&mut self, tx: f64, ty: f64, leading: &mut f64) {
        *leading = -ty;
        self.move_text_position(tx, ty);
    }

    /// Tm — set text matrix directly.
    pub fn set_text_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        self.tm = [a, b, c, d, e, f];
        self.tlm = self.tm;
    }

    /// T* — move to start of next line (uses leading).
    pub fn next_line(&mut self, leading: f64) {
        self.move_text_position(0.0, -leading);
    }

    /// Returns the current text matrix.
    pub fn text_matrix(&self) -> [f64; 6] {
        self.tm
    }

    /// Advances the text position by the given width in text space.
    pub fn advance(&mut self, width: f64) {
        self.tm[4] += width * self.tm[0];
        self.tm[5] += width * self.tm[1];
    }
}

/// Renders a text string using the current font and text state.
///
/// When a `FontProgram` is available, renders actual glyph outlines.
/// Otherwise falls back to placeholder rectangles using Standard 14 metrics.
pub(crate) fn render_text_string(
    text_bytes: &[u8],
    text_obj: &mut TextObject,
    state: &RenderState,
    font: Option<&Standard14Font>,
    font_program: Option<&FontProgram>,
    pixmap: &mut Pixmap,
    mask: Option<&tiny_skia::Mask>,
) {
    let font_size = state.text_state.font_size;
    let char_spacing = state.text_state.character_spacing;
    let word_spacing = state.text_state.word_spacing;
    let h_scale = state.text_state.horizontal_scaling / 100.0;
    let rise = state.text_state.rise;

    let mode = state.text_state.rendering_mode;
    let do_fill = matches!(mode, 0 | 2 | 4 | 6);
    let do_stroke = matches!(mode, 1 | 2 | 5 | 6);

    // Mode 3 (or 7) = invisible — skip painting, just advance positions
    if !do_fill && !do_stroke {
        for &byte in text_bytes {
            let glyph_width = glyph_width_pts(byte, font, font_program, font_size);
            let total_advance = (glyph_width + char_spacing) * h_scale;
            let extra = if byte == b' ' {
                word_spacing * h_scale
            } else {
                0.0
            };
            text_obj.advance(total_advance + extra);
        }
        return;
    }

    let mut fill_paint = Paint::default();
    fill_paint.set_color(state.effective_fill_color());
    fill_paint.anti_alias = true;
    fill_paint.blend_mode = state.blend_mode;

    let mut stroke_paint = Paint::default();
    stroke_paint.set_color(state.effective_stroke_color());
    stroke_paint.anti_alias = true;
    stroke_paint.blend_mode = state.blend_mode;

    let sk_stroke = state.to_stroke();

    for &byte in text_bytes {
        let glyph_width = glyph_width_pts(byte, font, font_program, font_size);

        let tm = text_obj.text_matrix();
        let gx = tm[4] as f32;
        let gy = (tm[5] + rise) as f32;

        // Try real glyph outline from embedded font program
        let rendered = font_program
            .and_then(|fp| {
                render_glyph_outline(
                    fp,
                    byte,
                    font_size,
                    h_scale,
                    gx,
                    gy,
                    state,
                    do_fill,
                    do_stroke,
                    &fill_paint,
                    &stroke_paint,
                    &sk_stroke,
                    pixmap,
                    mask,
                )
            })
            .is_some();

        // Fall back to placeholder rectangles
        if !rendered {
            let rect_width = (glyph_width * h_scale).abs() as f32;
            let rect_height = (font_size * 0.7) as f32;
            if rect_width > 0.1 && rect_height > 0.1 {
                let mut path_acc = PathAccumulator::new();
                path_acc.rect(gx, gy, rect_width, rect_height);
                if let Some(path) = path_acc.finish() {
                    if do_fill {
                        pixmap.fill_path(&path, &fill_paint, FillRule::Winding, state.ctm, mask);
                    }
                    if do_stroke {
                        pixmap.stroke_path(&path, &stroke_paint, &sk_stroke, state.ctm, mask);
                    }
                }
            }
        }

        let total_advance = (glyph_width + char_spacing) * h_scale;
        let extra = if byte == b' ' {
            word_spacing * h_scale
        } else {
            0.0
        };
        text_obj.advance(total_advance + extra);
    }
}

/// Renders a single glyph outline from an embedded font program.
///
/// Returns `Some(())` if the glyph was rendered, `None` if no outline available.
#[allow(clippy::too_many_arguments)]
fn render_glyph_outline(
    fp: &FontProgram,
    code: u8,
    font_size: f64,
    h_scale: f64,
    gx: f32,
    gy: f32,
    state: &RenderState,
    do_fill: bool,
    do_stroke: bool,
    fill_paint: &Paint<'_>,
    stroke_paint: &Paint<'_>,
    sk_stroke: &Stroke,
    pixmap: &mut Pixmap,
    mask: Option<&tiny_skia::Mask>,
) -> Option<()> {
    let glyph_path = fp.glyph_path(code)?;

    // Scale from font units to text space, then apply text matrix + CTM.
    // Font outlines are in font units (typically 2048 upem).
    // We need to scale to points (font_size / units_per_em) and flip Y
    // (font coords are Y-up, PDF is Y-up but tiny-skia is Y-down after CTM).
    let scale = font_size as f32 / fp.units_per_em();
    let sx = scale * h_scale as f32;
    let sy = -scale; // Flip Y: font coords are Y-up

    // Build the glyph transform: scale + translate to glyph position
    let glyph_transform = Transform::from_row(sx, 0.0, 0.0, sy, gx, gy);
    let combined = state.ctm.pre_concat(glyph_transform);

    if do_fill {
        pixmap.fill_path(&glyph_path, fill_paint, FillRule::EvenOdd, combined, mask);
    }
    if do_stroke {
        pixmap.stroke_path(&glyph_path, stroke_paint, sk_stroke, combined, mask);
    }

    Some(())
}

/// Returns the glyph width in points for a given character code.
fn glyph_width_pts(
    code: u8,
    font: Option<&Standard14Font>,
    font_program: Option<&FontProgram>,
    font_size: f64,
) -> f64 {
    // Try embedded font metrics first
    if let Some(fp) = font_program {
        if let Some(aw) = fp.advance_width(code) {
            return aw as f64 / fp.units_per_em() as f64 * font_size;
        }
    }
    // Fall back to Standard 14 metrics
    let glyph_width_units = font.map(|f| f.glyph_width(code)).unwrap_or(500) as f64;
    glyph_width_units / 1000.0 * font_size
}

/// Renders text from a CID-keyed (Type 0 composite) font.
///
/// Character codes are 2-byte big-endian CIDs (Identity-H encoding).
/// Each CID maps directly to a GID for glyph lookup.
pub(crate) fn render_cid_text_string(
    text_bytes: &[u8],
    text_obj: &mut TextObject,
    state: &RenderState,
    cid_font: &CidFontProgram,
    pixmap: &mut Pixmap,
    mask: Option<&tiny_skia::Mask>,
) {
    let font_size = state.text_state.font_size;
    let char_spacing = state.text_state.character_spacing;
    let word_spacing = state.text_state.word_spacing;
    let h_scale = state.text_state.horizontal_scaling / 100.0;
    let rise = state.text_state.rise;

    let mode = state.text_state.rendering_mode;
    let do_fill = matches!(mode, 0 | 2 | 4 | 6);
    let do_stroke = matches!(mode, 1 | 2 | 5 | 6);

    // Mode 3 (or 7) = invisible — skip painting, just advance positions
    if !do_fill && !do_stroke {
        let upem = cid_font.units_per_em();
        let mut i = 0;
        while i + 1 < text_bytes.len() {
            let cid = u16::from_be_bytes([text_bytes[i], text_bytes[i + 1]]);
            i += 2;
            let glyph_width = cid_font.advance_width(cid) as f64 / upem as f64 * font_size;
            let total_advance = (glyph_width + char_spacing) * h_scale;
            let extra = if cid == 0x0020 {
                word_spacing * h_scale
            } else {
                0.0
            };
            text_obj.advance(total_advance + extra);
        }
        return;
    }

    let upem = cid_font.units_per_em();
    let scale = font_size as f32 / upem;
    let sx = scale * h_scale as f32;
    let sy = -scale; // Flip Y

    let mut fill_paint = Paint::default();
    fill_paint.set_color(state.effective_fill_color());
    fill_paint.anti_alias = true;
    fill_paint.blend_mode = state.blend_mode;

    let mut stroke_paint = Paint::default();
    stroke_paint.set_color(state.effective_stroke_color());
    stroke_paint.anti_alias = true;
    stroke_paint.blend_mode = state.blend_mode;

    let sk_stroke = state.to_stroke();

    // Process 2-byte CID codes
    let mut i = 0;
    while i + 1 < text_bytes.len() {
        let cid = u16::from_be_bytes([text_bytes[i], text_bytes[i + 1]]);
        i += 2;

        let glyph_width = cid_font.advance_width(cid) as f64 / upem as f64 * font_size;

        let tm = text_obj.text_matrix();
        let gx = tm[4] as f32;
        let gy = (tm[5] + rise) as f32;

        if let Some(glyph_path) = cid_font.glyph_path_by_gid(cid) {
            let glyph_transform = Transform::from_row(sx, 0.0, 0.0, sy, gx, gy);
            let combined = state.ctm.pre_concat(glyph_transform);

            if do_fill {
                pixmap.fill_path(&glyph_path, &fill_paint, FillRule::EvenOdd, combined, mask);
            }
            if do_stroke {
                pixmap.stroke_path(&glyph_path, &stroke_paint, &sk_stroke, combined, mask);
            }
        }

        let total_advance = (glyph_width + char_spacing) * h_scale;
        let extra = if cid == 0x0020 {
            word_spacing * h_scale
        } else {
            0.0
        };
        text_obj.advance(total_advance + extra);
    }
}

/// Multiplies a 2D affine matrix by a translation.
fn mat_translate(m: [f64; 6], tx: f64, ty: f64) -> [f64; 6] {
    [
        m[0],
        m[1],
        m[2],
        m[3],
        tx * m[0] + ty * m[2] + m[4],
        tx * m[1] + ty * m[3] + m[5],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- TextObject state machine tests ----

    #[test]
    fn text_object_starts_inactive() {
        let to = TextObject::new();
        assert!(!to.is_active());
    }

    #[test]
    fn begin_activates() {
        let mut to = TextObject::new();
        to.begin();
        assert!(to.is_active());
        let tm = to.text_matrix();
        // Identity matrix
        assert_eq!(tm, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn end_deactivates() {
        let mut to = TextObject::new();
        to.begin();
        to.end();
        assert!(!to.is_active());
    }

    #[test]
    fn move_text_position_td() {
        let mut to = TextObject::new();
        to.begin();
        to.move_text_position(100.0, 200.0);
        let tm = to.text_matrix();
        assert!((tm[4] - 100.0).abs() < 1e-10);
        assert!((tm[5] - 200.0).abs() < 1e-10);
    }

    #[test]
    fn move_text_position_td_uppercase_sets_leading() {
        let mut to = TextObject::new();
        to.begin();
        let mut leading = 0.0;
        to.move_text_position_td(0.0, -14.0, &mut leading);
        assert!((leading - 14.0).abs() < 1e-10);
        let tm = to.text_matrix();
        assert!((tm[5] - (-14.0)).abs() < 1e-10);
    }

    #[test]
    fn set_text_matrix_tm() {
        let mut to = TextObject::new();
        to.begin();
        to.set_text_matrix(2.0, 0.0, 0.0, 2.0, 50.0, 100.0);
        let tm = to.text_matrix();
        assert_eq!(tm, [2.0, 0.0, 0.0, 2.0, 50.0, 100.0]);
    }

    #[test]
    fn next_line_uses_leading() {
        let mut to = TextObject::new();
        to.begin();
        to.move_text_position(0.0, 700.0);
        to.next_line(12.0);
        let tm = to.text_matrix();
        // next_line moves by (0, -leading)
        assert!((tm[5] - 688.0).abs() < 1e-10);
    }

    #[test]
    fn advance_moves_x() {
        let mut to = TextObject::new();
        to.begin();
        to.advance(5.0);
        let tm = to.text_matrix();
        assert!((tm[4] - 5.0).abs() < 1e-10);
        assert!(tm[5].abs() < 1e-10);
    }

    #[test]
    fn advance_with_scaled_matrix() {
        let mut to = TextObject::new();
        to.begin();
        to.set_text_matrix(2.0, 0.0, 0.0, 2.0, 10.0, 20.0);
        to.advance(5.0);
        let tm = to.text_matrix();
        // advance: tm[4] += width * tm[0] = 5 * 2 = 10 → 10 + 10 = 20
        assert!((tm[4] - 20.0).abs() < 1e-10);
    }

    // ---- mat_translate tests ----

    #[test]
    fn mat_translate_identity() {
        let m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let result = mat_translate(m, 10.0, 20.0);
        assert!((result[4] - 10.0).abs() < 1e-10);
        assert!((result[5] - 20.0).abs() < 1e-10);
    }

    #[test]
    fn mat_translate_with_scale() {
        let m = [2.0, 0.0, 0.0, 3.0, 0.0, 0.0];
        let result = mat_translate(m, 5.0, 10.0);
        // tx * m[0] + ty * m[2] + m[4] = 5*2 + 10*0 + 0 = 10
        // tx * m[1] + ty * m[3] + m[5] = 5*0 + 10*3 + 0 = 30
        assert!((result[4] - 10.0).abs() < 1e-10);
        assert!((result[5] - 30.0).abs() < 1e-10);
    }

    #[test]
    fn begin_resets_matrix() {
        let mut to = TextObject::new();
        to.begin();
        to.advance(100.0);
        to.begin(); // BT resets
        let tm = to.text_matrix();
        assert_eq!(tm, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }
}
