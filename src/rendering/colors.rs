//! Color conversion utilities for the rendering engine.
//!
//! Provides PDF color operator helpers (grayscale, RGB, CMYK) and
//! numeric extraction from PDF objects.

use tiny_skia::BlendMode;

use crate::core::objects::Object;

/// Maps a PDF blend mode name to a `tiny_skia::BlendMode`.
///
/// ISO 32000-2:2020, Table 134.
pub(crate) fn pdf_blend_mode(name: &str) -> Option<BlendMode> {
    match name {
        "Normal" | "Compatible" => Some(BlendMode::SourceOver),
        "Multiply" => Some(BlendMode::Multiply),
        "Screen" => Some(BlendMode::Screen),
        "Overlay" => Some(BlendMode::Overlay),
        "Darken" => Some(BlendMode::Darken),
        "Lighten" => Some(BlendMode::Lighten),
        "ColorDodge" => Some(BlendMode::ColorDodge),
        "ColorBurn" => Some(BlendMode::ColorBurn),
        "HardLight" => Some(BlendMode::HardLight),
        "SoftLight" => Some(BlendMode::SoftLight),
        "Difference" => Some(BlendMode::Difference),
        "Exclusion" => Some(BlendMode::Exclusion),
        "Hue" => Some(BlendMode::Hue),
        "Saturation" => Some(BlendMode::Saturation),
        "Color" => Some(BlendMode::Color),
        "Luminosity" => Some(BlendMode::Luminosity),
        _ => None,
    }
}

/// Extracts an `f64` from a PDF number object (Integer or Real).
pub(crate) fn obj_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(f) => Some(*f),
        _ => None,
    }
}

/// Extracts an `f32` from a PDF number object.
pub(crate) fn obj_f32(obj: &Object) -> Option<f32> {
    obj_f64(obj).map(|v| v as f32)
}

/// Gets operand at `idx` as `f32`.
pub(crate) fn op_f32(operands: &[&Object], idx: usize) -> Option<f32> {
    operands.get(idx).and_then(|o| obj_f32(o))
}

/// Sets a color to grayscale.
pub(crate) fn set_gray(color: &mut tiny_skia::Color, g: f32) {
    let c = g.clamp(0.0, 1.0);
    if let Some(col) = tiny_skia::Color::from_rgba(c, c, c, 1.0) {
        *color = col;
    }
}

/// Creates an RGB color from 3 operands.
pub(crate) fn rgb_from_ops(operands: &[&Object]) -> Option<tiny_skia::Color> {
    if operands.len() != 3 {
        return None;
    }
    let r = op_f32(operands, 0)?;
    let g = op_f32(operands, 1)?;
    let b = op_f32(operands, 2)?;
    tiny_skia::Color::from_rgba(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), 1.0)
}

/// Converts CMYK to RGB using the simple subtractive formula.
///
/// Used by both the rendering engine (color operators) and
/// image decoding (`PdfImage::to_rgba`).
pub(crate) fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> (f32, f32, f32) {
    let r = (1.0 - c) * (1.0 - k);
    let g = (1.0 - m) * (1.0 - k);
    let b = (1.0 - y) * (1.0 - k);
    (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
}

/// Creates an RGB color from 4 CMYK operands.
pub(crate) fn cmyk_from_ops(operands: &[&Object]) -> Option<tiny_skia::Color> {
    if operands.len() != 4 {
        return None;
    }
    let (r, g, b) = cmyk_to_rgb(
        op_f32(operands, 0)?,
        op_f32(operands, 1)?,
        op_f32(operands, 2)?,
        op_f32(operands, 3)?,
    );
    tiny_skia::Color::from_rgba(r, g, b, 1.0)
}

/// Sets a color from operands (1 for gray, 3 for RGB, 4 for CMYK).
pub(crate) fn set_color_from_operands(operands: &[&Object], color: &mut tiny_skia::Color) {
    match operands.len() {
        1 => {
            if let Some(g) = op_f32(operands, 0) {
                set_gray(color, g);
            }
        }
        3 => {
            if let Some(c) = rgb_from_ops(operands) {
                *color = c;
            }
        }
        4 => {
            if let Some(c) = cmyk_from_ops(operands) {
                *color = c;
            }
        }
        _ => {}
    }
}

/// Parses a PDF array of numbers into a `Vec<f32>`.
///
/// Returns `None` if the object is not an array or contains no numeric values.
pub(crate) fn parse_f32_array(obj: Option<&Object>) -> Option<Vec<f32>> {
    let arr = obj?.as_array()?;
    let values: Vec<f32> = arr.iter().filter_map(obj_f32).collect();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Parses a 6-element PDF array into a `tiny_skia::Transform`.
///
/// Returns `None` if the array has fewer than 6 numeric elements.
pub(crate) fn parse_transform(obj: Option<&Object>) -> Option<tiny_skia::Transform> {
    let arr = obj?.as_array()?;
    if arr.len() >= 6 {
        Some(tiny_skia::Transform::from_row(
            obj_f32(&arr[0])?,
            obj_f32(&arr[1])?,
            obj_f32(&arr[2])?,
            obj_f32(&arr[3])?,
            obj_f32(&arr[4])?,
            obj_f32(&arr[5])?,
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Blend mode mapping ----

    #[test]
    fn blend_mode_normal() {
        assert_eq!(pdf_blend_mode("Normal"), Some(BlendMode::SourceOver));
    }

    #[test]
    fn blend_mode_compatible_alias() {
        assert_eq!(pdf_blend_mode("Compatible"), Some(BlendMode::SourceOver));
    }

    #[test]
    fn blend_mode_multiply() {
        assert_eq!(pdf_blend_mode("Multiply"), Some(BlendMode::Multiply));
    }

    #[test]
    fn blend_mode_unknown() {
        assert!(pdf_blend_mode("FancyBlend").is_none());
    }

    // ---- Numeric extraction ----

    #[test]
    fn obj_f64_integer() {
        assert_eq!(obj_f64(&Object::Integer(42)), Some(42.0));
    }

    #[test]
    fn obj_f64_real() {
        assert_eq!(obj_f64(&Object::Real(3.14)), Some(3.14));
    }

    #[test]
    fn obj_f64_non_numeric() {
        assert!(obj_f64(&Object::Boolean(true)).is_none());
    }

    #[test]
    fn obj_f32_from_integer() {
        assert_eq!(obj_f32(&Object::Integer(7)), Some(7.0f32));
    }

    #[test]
    fn op_f32_valid_index() {
        let ops: Vec<Object> = vec![Object::Real(1.5), Object::Integer(3)];
        let refs: Vec<&Object> = ops.iter().collect();
        assert_eq!(op_f32(&refs, 0), Some(1.5));
        assert_eq!(op_f32(&refs, 1), Some(3.0));
    }

    #[test]
    fn op_f32_out_of_bounds() {
        let ops: Vec<Object> = vec![Object::Real(1.0)];
        let refs: Vec<&Object> = ops.iter().collect();
        assert!(op_f32(&refs, 5).is_none());
    }

    // ---- Grayscale ----

    #[test]
    fn set_gray_midtone() {
        let mut color = tiny_skia::Color::BLACK;
        set_gray(&mut color, 0.5);
        assert!((color.red() - 0.5).abs() < 0.01);
        assert!((color.green() - 0.5).abs() < 0.01);
        assert!((color.blue() - 0.5).abs() < 0.01);
    }

    #[test]
    fn set_gray_clamps_negative() {
        let mut color = tiny_skia::Color::WHITE;
        set_gray(&mut color, -0.5);
        assert!(color.red() < 0.01);
    }

    // ---- RGB ----

    #[test]
    fn rgb_from_ops_valid() {
        let ops = vec![Object::Real(1.0), Object::Real(0.0), Object::Real(0.5)];
        let refs: Vec<&Object> = ops.iter().collect();
        let c = rgb_from_ops(&refs).unwrap();
        assert!((c.red() - 1.0).abs() < 0.01);
        assert!(c.green() < 0.01);
        assert!((c.blue() - 0.5).abs() < 0.01);
    }

    #[test]
    fn rgb_from_ops_wrong_count() {
        let ops = vec![Object::Real(1.0), Object::Real(0.0)];
        let refs: Vec<&Object> = ops.iter().collect();
        assert!(rgb_from_ops(&refs).is_none());
    }

    // ---- CMYK ----

    #[test]
    fn cmyk_to_rgb_pure_cyan() {
        let (r, g, b) = cmyk_to_rgb(1.0, 0.0, 0.0, 0.0);
        assert!(r < 0.01); // Cyan removes red
        assert!((g - 1.0).abs() < 0.01);
        assert!((b - 1.0).abs() < 0.01);
    }

    #[test]
    fn cmyk_to_rgb_full_black() {
        let (r, g, b) = cmyk_to_rgb(0.0, 0.0, 0.0, 1.0);
        assert!(r < 0.01);
        assert!(g < 0.01);
        assert!(b < 0.01);
    }

    #[test]
    fn cmyk_to_rgb_no_ink() {
        let (r, g, b) = cmyk_to_rgb(0.0, 0.0, 0.0, 0.0);
        assert!((r - 1.0).abs() < 0.01);
        assert!((g - 1.0).abs() < 0.01);
        assert!((b - 1.0).abs() < 0.01);
    }

    #[test]
    fn cmyk_from_ops_valid() {
        let ops = vec![
            Object::Real(0.0),
            Object::Real(1.0),
            Object::Real(1.0),
            Object::Real(0.0),
        ];
        let refs: Vec<&Object> = ops.iter().collect();
        let c = cmyk_from_ops(&refs).unwrap();
        // CMYK (0,1,1,0) → pure red
        assert!((c.red() - 1.0).abs() < 0.01);
        assert!(c.green() < 0.01);
        assert!(c.blue() < 0.01);
    }

    #[test]
    fn cmyk_from_ops_wrong_count() {
        let ops = vec![Object::Real(0.0), Object::Real(0.0)];
        let refs: Vec<&Object> = ops.iter().collect();
        assert!(cmyk_from_ops(&refs).is_none());
    }

    // ---- set_color_from_operands ----

    #[test]
    fn set_color_gray_operand() {
        let ops = vec![Object::Real(0.75)];
        let refs: Vec<&Object> = ops.iter().collect();
        let mut color = tiny_skia::Color::BLACK;
        set_color_from_operands(&refs, &mut color);
        assert!((color.red() - 0.75).abs() < 0.01);
    }

    #[test]
    fn set_color_rgb_operands() {
        let ops = vec![Object::Real(0.0), Object::Real(1.0), Object::Real(0.0)];
        let refs: Vec<&Object> = ops.iter().collect();
        let mut color = tiny_skia::Color::BLACK;
        set_color_from_operands(&refs, &mut color);
        assert!(color.red() < 0.01);
        assert!((color.green() - 1.0).abs() < 0.01);
    }

    #[test]
    fn set_color_unsupported_count_noop() {
        let ops = vec![Object::Real(0.0), Object::Real(0.0)];
        let refs: Vec<&Object> = ops.iter().collect();
        let mut color = tiny_skia::Color::WHITE;
        set_color_from_operands(&refs, &mut color);
        // Should remain white (2 operands not supported)
        assert!((color.red() - 1.0).abs() < 0.01);
    }
}
