//! Rendering graphics state — extends the font-level graphics state with
//! CTM, colors, and line properties needed for rasterization.

use tiny_skia::{BlendMode, LineCap, LineJoin, Stroke, Transform};

use super::color_space::RenderColorSpace;
use crate::fonts::graphics_state::TextState;

/// Full rendering state for a single level of the graphics state stack.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct RenderState {
    /// Current Transformation Matrix (device space).
    pub ctm: Transform,
    /// Non-stroking (fill) color as premultiplied RGBA.
    pub fill_color: tiny_skia::Color,
    /// Stroking color as premultiplied RGBA.
    pub stroke_color: tiny_skia::Color,
    /// Non-stroking alpha (from ExtGState `/ca`).
    pub fill_alpha: f32,
    /// Stroking alpha (from ExtGState `/CA`).
    pub stroke_alpha: f32,
    /// Line width in user space units.
    pub line_width: f64,
    /// Line cap style (0 = butt, 1 = round, 2 = square).
    pub line_cap: u8,
    /// Line join style (0 = miter, 1 = round, 2 = bevel).
    pub line_join: u8,
    /// Miter limit.
    pub miter_limit: f64,
    /// Dash array (lengths of dashes and gaps).
    pub dash_array: Vec<f32>,
    /// Dash phase offset.
    pub dash_phase: f32,
    /// Text state parameters.
    pub text_state: TextState,
    /// Blend mode for compositing (from ExtGState `/BM`).
    pub blend_mode: BlendMode,
    /// Current non-stroking color space (set by `cs` operator).
    pub fill_color_space: RenderColorSpace,
    /// Current stroking color space (set by `CS` operator).
    pub stroke_color_space: RenderColorSpace,
    /// Flatness tolerance (from `i` operator or ExtGState `/FL`).
    pub flatness: f64,
    /// Rendering intent (from `ri` operator or ExtGState `/RI`).
    pub rendering_intent: &'static str,
    /// Soft mask from ExtGState `/SMask` — applied as alpha mask during painting.
    pub soft_mask: Option<std::sync::Arc<tiny_skia::Mask>>,
    /// Tiling pattern fill — rendered tile pixmap with step sizes and transform.
    pub fill_pattern: Option<std::sync::Arc<PatternData>>,
    /// Stroke pattern — rendered pattern pixmap for stroke operations.
    pub stroke_pattern: Option<std::sync::Arc<PatternData>>,
}

/// Pre-rendered pattern data (tiling or shading) for use as a fill shader.
#[derive(Debug, Clone)]
pub(crate) struct PatternData {
    /// The rendered tile pixmap.
    pub pixmap: tiny_skia::Pixmap,
    // TODO: x_step and y_step are stored for future use when XStep/YStep differ
    // from pixmap dimensions (e.g., fractional steps, inter-tile gaps). Currently
    // the pixmap is sized to ceil(xstep) x ceil(ystep) and tiled at pixmap size.
    #[allow(dead_code)]
    /// Horizontal step size for tiling (XStep in pattern space).
    pub x_step: Option<f32>,
    #[allow(dead_code)]
    /// Vertical step size for tiling (YStep in pattern space).
    pub y_step: Option<f32>,
    /// Pattern matrix (pattern space → user space).
    /// Identity if not specified by the pattern dictionary.
    pub transform: Transform,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            ctm: Transform::identity(),
            fill_color: tiny_skia::Color::BLACK,
            stroke_color: tiny_skia::Color::BLACK,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash_array: Vec::new(),
            dash_phase: 0.0,
            text_state: TextState::default(),
            blend_mode: BlendMode::SourceOver,
            fill_color_space: RenderColorSpace::DeviceGray,
            stroke_color_space: RenderColorSpace::DeviceGray,
            flatness: 0.0,
            rendering_intent: "RelativeColorimetric",
            soft_mask: None,
            fill_pattern: None,
            stroke_pattern: None,
        }
    }
}

impl RenderState {
    /// Builds a `tiny_skia::Stroke` from the current line properties.
    pub(crate) fn to_stroke(&self) -> Stroke {
        let mut stroke = Stroke {
            width: self.line_width as f32,
            line_cap: match self.line_cap {
                1 => LineCap::Round,
                2 => LineCap::Square,
                _ => LineCap::Butt,
            },
            line_join: match self.line_join {
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => LineJoin::Miter,
            },
            miter_limit: self.miter_limit as f32,
            ..Default::default()
        };
        if !self.dash_array.is_empty() {
            stroke.dash = tiny_skia::StrokeDash::new(self.dash_array.clone(), self.dash_phase);
        }
        stroke
    }

    /// Returns the fill color with ExtGState non-stroking alpha (`ca`) applied.
    pub(crate) fn effective_fill_color(&self) -> tiny_skia::Color {
        apply_alpha(self.fill_color, self.fill_alpha)
    }

    /// Returns the stroke color with ExtGState stroking alpha (`CA`) applied.
    pub(crate) fn effective_stroke_color(&self) -> tiny_skia::Color {
        apply_alpha(self.stroke_color, self.stroke_alpha)
    }
}

/// Multiplies a color's alpha by an external alpha factor.
fn apply_alpha(c: tiny_skia::Color, alpha: f32) -> tiny_skia::Color {
    if alpha >= 1.0 {
        return c;
    }
    tiny_skia::Color::from_rgba(c.red(), c.green(), c.blue(), c.alpha() * alpha).unwrap_or(c)
}

/// Stack of rendering states supporting `q`/`Q` save/restore.
#[derive(Debug)]
pub(crate) struct RenderStateStack {
    stack: Vec<RenderState>,
    current: RenderState,
}

impl RenderStateStack {
    /// Creates a new stack with default state and the given base transform.
    pub fn new(base_transform: Transform) -> Self {
        Self {
            stack: Vec::new(),
            current: RenderState {
                ctm: base_transform,
                ..Default::default()
            },
        }
    }

    /// Pushes a copy of the current state (`q` operator).
    pub fn save(&mut self) {
        self.stack.push(self.current.clone());
    }

    /// Pops the most recent saved state (`Q` operator).
    /// Does nothing if the stack is empty (resilient to malformed PDFs).
    pub fn restore(&mut self) {
        if let Some(prev) = self.stack.pop() {
            self.current = prev;
        }
    }

    /// Returns a reference to the current state.
    pub fn state(&self) -> &RenderState {
        &self.current
    }

    /// Returns a mutable reference to the current state.
    pub fn state_mut(&mut self) -> &mut RenderState {
        &mut self.current
    }

    /// Concatenates a matrix onto the current CTM (`cm` operator).
    pub fn concat_ctm(&mut self, transform: Transform) {
        self.current.ctm = self.current.ctm.pre_concat(transform);
    }

    /// Returns the current stack depth.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_data_carries_step_sizes_and_transform() {
        let pixmap = tiny_skia::Pixmap::new(10, 20).unwrap();
        let pat = PatternData {
            pixmap,
            x_step: Some(15.0),
            y_step: Some(25.0),
            transform: Transform::from_translate(5.0, 10.0),
        };
        assert_eq!(pat.x_step, Some(15.0));
        assert_eq!(pat.y_step, Some(25.0));
        assert_eq!(pat.transform.tx, 5.0);
        assert_eq!(pat.transform.ty, 10.0);
    }

    #[test]
    fn pattern_data_shading_has_no_step_sizes() {
        let pixmap = tiny_skia::Pixmap::new(10, 10).unwrap();
        let pat = PatternData {
            pixmap,
            x_step: None,
            y_step: None,
            transform: Transform::identity(),
        };
        assert!(pat.x_step.is_none());
        assert!(pat.y_step.is_none());
    }

    #[test]
    fn render_state_default_values() {
        let state = RenderState::default();
        assert_eq!(state.ctm, Transform::identity());
        assert_eq!(state.fill_color, tiny_skia::Color::BLACK);
        assert_eq!(state.stroke_color, tiny_skia::Color::BLACK);
        assert_eq!(state.line_width, 1.0);
        assert_eq!(state.line_cap, 0);
        assert_eq!(state.line_join, 0);
        assert_eq!(state.miter_limit, 10.0);
        assert_eq!(state.fill_alpha, 1.0);
        assert_eq!(state.stroke_alpha, 1.0);
    }

    #[test]
    fn render_state_save_restore() {
        let mut stack = RenderStateStack::new(Transform::identity());
        stack.state_mut().line_width = 5.0;
        stack.state_mut().fill_color = tiny_skia::Color::from_rgba8(255, 0, 0, 255);

        stack.save();
        stack.state_mut().line_width = 10.0;
        stack.state_mut().fill_color = tiny_skia::Color::from_rgba8(0, 255, 0, 255);

        assert_eq!(stack.state().line_width, 10.0);

        stack.restore();
        assert_eq!(stack.state().line_width, 5.0);
        assert_eq!(
            stack.state().fill_color,
            tiny_skia::Color::from_rgba8(255, 0, 0, 255)
        );
    }

    #[test]
    fn render_state_cm_transform() {
        let mut stack = RenderStateStack::new(Transform::identity());
        // Translate by (100, 200)
        let t = Transform::from_translate(100.0, 200.0);
        stack.concat_ctm(t);

        assert_eq!(stack.state().ctm.tx, 100.0);
        assert_eq!(stack.state().ctm.ty, 200.0);
    }

    #[test]
    fn render_state_nested_save_restore() {
        let mut stack = RenderStateStack::new(Transform::identity());
        stack.state_mut().line_width = 1.0;
        stack.save();
        stack.state_mut().line_width = 2.0;
        stack.save();
        stack.state_mut().line_width = 3.0;

        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.state().line_width, 3.0);

        stack.restore();
        assert_eq!(stack.state().line_width, 2.0);

        stack.restore();
        assert_eq!(stack.state().line_width, 1.0);
    }

    #[test]
    fn render_state_cm_concatenation() {
        let mut stack = RenderStateStack::new(Transform::identity());
        // Two translations should compose
        stack.concat_ctm(Transform::from_translate(50.0, 0.0));
        stack.concat_ctm(Transform::from_translate(30.0, 0.0));

        let eps = 0.001;
        assert!((stack.state().ctm.tx - 80.0).abs() < eps);
    }

    #[test]
    fn render_state_restore_underflow_ignored() {
        let mut stack = RenderStateStack::new(Transform::identity());
        stack.state_mut().line_width = 5.0;

        // Extra restore doesn't crash, keeps current state
        stack.restore();
        assert_eq!(stack.state().line_width, 5.0);
    }
}
