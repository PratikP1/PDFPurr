//! Path construction and painting for the rendering engine.
//!
//! Handles PDF path operators (m, l, c, v, y, h, re) and painting
//! operators (S, s, f, F, f*, B, B*, b, b*, n).

use tiny_skia::{FillRule, FilterQuality, Paint, PathBuilder, Pixmap, SpreadMode, Transform};

use super::graphics::RenderState;

/// Builds a tiny-skia path from accumulated path segments.
#[derive(Debug, Default)]
pub(crate) struct PathAccumulator {
    builder: PathBuilder,
    has_content: bool,
    /// Current point X (tracked for `v` operator).
    last_x: f32,
    /// Current point Y (tracked for `v` operator).
    last_y: f32,
    /// Start of current subpath (for `close` to restore current point).
    subpath_start_x: f32,
    subpath_start_y: f32,
}

impl PathAccumulator {
    pub fn new() -> Self {
        Self {
            builder: PathBuilder::new(),
            has_content: false,
            last_x: 0.0,
            last_y: 0.0,
            subpath_start_x: 0.0,
            subpath_start_y: 0.0,
        }
    }

    /// `m` operator — begin a new subpath.
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, y);
        self.last_x = x;
        self.last_y = y;
        self.subpath_start_x = x;
        self.subpath_start_y = y;
        self.has_content = true;
    }

    /// `l` operator — straight line to point.
    pub fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, y);
        self.last_x = x;
        self.last_y = y;
        self.has_content = true;
    }

    /// `c` operator — cubic Bézier curve.
    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        self.builder.cubic_to(x1, y1, x2, y2, x3, y3);
        self.last_x = x3;
        self.last_y = y3;
        self.has_content = true;
    }

    /// `v` operator — cubic Bézier with first control point = current point.
    pub fn cubic_to_v(&mut self, x2: f32, y2: f32, x3: f32, y3: f32) {
        self.builder
            .cubic_to(self.last_x, self.last_y, x2, y2, x3, y3);
        self.last_x = x3;
        self.last_y = y3;
        self.has_content = true;
    }

    /// `y` operator — cubic Bézier with second control point = endpoint.
    pub fn cubic_to_y(&mut self, x1: f32, y1: f32, x3: f32, y3: f32) {
        self.builder.cubic_to(x1, y1, x3, y3, x3, y3);
        self.last_x = x3;
        self.last_y = y3;
        self.has_content = true;
    }

    /// `h` operator — close current subpath.
    pub fn close(&mut self) {
        self.builder.close();
        self.last_x = self.subpath_start_x;
        self.last_y = self.subpath_start_y;
    }

    /// `re` operator — append a rectangle as a complete subpath.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.builder.move_to(x, y);
        self.builder.line_to(x + w, y);
        self.builder.line_to(x + w, y + h);
        self.builder.line_to(x, y + h);
        self.builder.close();
        self.last_x = x;
        self.last_y = y;
        self.subpath_start_x = x;
        self.subpath_start_y = y;
        self.has_content = true;
    }

    // --- Operand-parsing wrappers (reduce dispatch_operator verbosity) ---

    /// Parses `x y` from operands and calls `move_to`.
    pub fn move_to_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if let (Some(x), Some(y)) = (super::colors::op_f32(ops, 0), super::colors::op_f32(ops, 1)) {
            self.move_to(x, y);
        }
    }

    /// Parses `x y` from operands and calls `line_to`.
    pub fn line_to_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if let (Some(x), Some(y)) = (super::colors::op_f32(ops, 0), super::colors::op_f32(ops, 1)) {
            self.line_to(x, y);
        }
    }

    /// Parses `x1 y1 x2 y2 x3 y3` from operands and calls `cubic_to`.
    pub fn cubic_to_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if ops.len() >= 6 {
            if let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x3), Some(y3)) = (
                super::colors::op_f32(ops, 0),
                super::colors::op_f32(ops, 1),
                super::colors::op_f32(ops, 2),
                super::colors::op_f32(ops, 3),
                super::colors::op_f32(ops, 4),
                super::colors::op_f32(ops, 5),
            ) {
                self.cubic_to(x1, y1, x2, y2, x3, y3);
            }
        }
    }

    /// Parses `x2 y2 x3 y3` from operands and calls `cubic_to_v`.
    pub fn cubic_to_v_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if ops.len() >= 4 {
            if let (Some(x2), Some(y2), Some(x3), Some(y3)) = (
                super::colors::op_f32(ops, 0),
                super::colors::op_f32(ops, 1),
                super::colors::op_f32(ops, 2),
                super::colors::op_f32(ops, 3),
            ) {
                self.cubic_to_v(x2, y2, x3, y3);
            }
        }
    }

    /// Parses `x1 y1 x3 y3` from operands and calls `cubic_to_y`.
    pub fn cubic_to_y_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if ops.len() >= 4 {
            if let (Some(x1), Some(y1), Some(x3), Some(y3)) = (
                super::colors::op_f32(ops, 0),
                super::colors::op_f32(ops, 1),
                super::colors::op_f32(ops, 2),
                super::colors::op_f32(ops, 3),
            ) {
                self.cubic_to_y(x1, y1, x3, y3);
            }
        }
    }

    /// Parses `x y w h` from operands and calls `rect`.
    pub fn rect_ops(&mut self, ops: &[&crate::core::objects::Object]) {
        if ops.len() >= 4 {
            if let (Some(x), Some(y), Some(w), Some(h)) = (
                super::colors::op_f32(ops, 0),
                super::colors::op_f32(ops, 1),
                super::colors::op_f32(ops, 2),
                super::colors::op_f32(ops, 3),
            ) {
                self.rect(x, y, w, h);
            }
        }
    }

    /// Returns whether any path segments have been added.
    pub fn has_content(&self) -> bool {
        self.has_content
    }

    /// Consumes the accumulator and returns the built path.
    pub fn finish(self) -> Option<tiny_skia::Path> {
        self.builder.finish()
    }

    /// Resets the accumulator for the next path.
    pub fn reset(&mut self) {
        self.builder = PathBuilder::new();
        self.has_content = false;
        self.last_x = 0.0;
        self.last_y = 0.0;
        self.subpath_start_x = 0.0;
        self.subpath_start_y = 0.0;
    }
}

/// Creates a `Paint` from a pattern pixmap for use as a tiled shader.
///
/// Uses the pattern's step sizes and transform to correctly tile and
/// position the pattern in user space.
fn pattern_paint(
    pat_data: &super::graphics::PatternData,
    blend_mode: tiny_skia::BlendMode,
) -> Paint<'_> {
    Paint {
        shader: tiny_skia::Pattern::new(
            pat_data.pixmap.as_ref(),
            SpreadMode::Repeat,
            FilterQuality::Bilinear,
            1.0,
            pat_data.transform,
        ),
        anti_alias: true,
        blend_mode,
        ..Paint::default()
    }
}

/// Paints the current path onto the pixmap.
pub(crate) fn paint_path(
    path: &tiny_skia::Path,
    state: &RenderState,
    pixmap: &mut Pixmap,
    fill: bool,
    stroke: bool,
    fill_rule: FillRule,
    clip_mask: Option<&tiny_skia::Mask>,
) {
    let ctm = state.ctm;

    if fill {
        if let Some(ref pat_data) = state.fill_pattern {
            let paint = pattern_paint(pat_data, state.blend_mode);
            if let Some(transformed_path) = path.clone().transform(ctm) {
                pixmap.fill_path(
                    &transformed_path,
                    &paint,
                    fill_rule,
                    Transform::identity(),
                    clip_mask,
                );
            }
        } else {
            let mut paint = Paint::default();
            paint.set_color(state.effective_fill_color());
            paint.anti_alias = true;
            paint.blend_mode = state.blend_mode;
            pixmap.fill_path(path, &paint, fill_rule, ctm, clip_mask);
        }
    }

    if stroke {
        let sk_stroke = state.to_stroke();
        if let Some(ref pat_data) = state.stroke_pattern {
            let paint = pattern_paint(pat_data, state.blend_mode);
            if let Some(transformed_path) = path.clone().transform(ctm) {
                pixmap.stroke_path(
                    &transformed_path,
                    &paint,
                    &sk_stroke,
                    Transform::identity(),
                    clip_mask,
                );
            }
        } else {
            let mut paint = Paint::default();
            paint.set_color(state.effective_stroke_color());
            paint.anti_alias = true;
            paint.blend_mode = state.blend_mode;
            pixmap.stroke_path(path, &paint, &sk_stroke, ctm, clip_mask);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accumulator_empty() {
        let acc = PathAccumulator::new();
        assert!(!acc.has_content());
    }

    #[test]
    fn move_to_marks_content() {
        let mut acc = PathAccumulator::new();
        acc.move_to(0.0, 0.0);
        assert!(acc.has_content());
    }

    #[test]
    fn line_to_marks_content() {
        let mut acc = PathAccumulator::new();
        acc.move_to(0.0, 0.0);
        acc.line_to(10.0, 10.0);
        assert!(acc.has_content());
    }

    #[test]
    fn rect_produces_path() {
        let mut acc = PathAccumulator::new();
        acc.rect(0.0, 0.0, 100.0, 50.0);
        assert!(acc.has_content());
        let path = acc.finish();
        assert!(path.is_some());
        let path = path.unwrap();
        let bounds = path.bounds();
        assert!((bounds.width() - 100.0).abs() < 0.1);
        assert!((bounds.height() - 50.0).abs() < 0.1);
    }

    #[test]
    fn cubic_to_marks_content() {
        let mut acc = PathAccumulator::new();
        acc.move_to(0.0, 0.0);
        acc.cubic_to(10.0, 20.0, 30.0, 40.0, 50.0, 0.0);
        assert!(acc.has_content());
        assert!(acc.finish().is_some());
    }

    #[test]
    fn cubic_to_y_variant() {
        let mut acc = PathAccumulator::new();
        acc.move_to(0.0, 0.0);
        acc.cubic_to_y(10.0, 20.0, 50.0, 0.0);
        assert!(acc.has_content());
        assert!(acc.finish().is_some());
    }

    #[test]
    fn close_on_rect() {
        let mut acc = PathAccumulator::new();
        acc.move_to(0.0, 0.0);
        acc.line_to(10.0, 0.0);
        acc.line_to(10.0, 10.0);
        acc.close();
        assert!(acc.finish().is_some());
    }

    #[test]
    fn reset_clears_content() {
        let mut acc = PathAccumulator::new();
        acc.rect(0.0, 0.0, 10.0, 10.0);
        assert!(acc.has_content());
        acc.reset();
        assert!(!acc.has_content());
    }

    #[test]
    fn finish_empty_returns_none() {
        let acc = PathAccumulator::new();
        assert!(acc.finish().is_none());
    }

    #[test]
    fn v_operator_uses_current_point() {
        // `v` operator: cubic Bézier with first control point = current point.
        // move_to(0, 100) then v(50, 0, 100, 0)
        // should be equivalent to c(0, 100, 50, 0, 100, 0)
        let mut acc_v = PathAccumulator::new();
        acc_v.move_to(0.0, 100.0);
        acc_v.cubic_to_v(50.0, 0.0, 100.0, 0.0);
        let path_v = acc_v.finish().unwrap();

        let mut acc_c = PathAccumulator::new();
        acc_c.move_to(0.0, 100.0);
        acc_c.cubic_to(0.0, 100.0, 50.0, 0.0, 100.0, 0.0);
        let path_c = acc_c.finish().unwrap();

        let bv = path_v.bounds();
        let bc = path_c.bounds();
        assert!((bv.left() - bc.left()).abs() < 0.1);
        assert!((bv.top() - bc.top()).abs() < 0.1);
        assert!((bv.right() - bc.right()).abs() < 0.1);
        assert!((bv.bottom() - bc.bottom()).abs() < 0.1);
    }
}
