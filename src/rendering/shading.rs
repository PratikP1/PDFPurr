//! Shading (gradient) rendering for the PDF rendering engine.
//!
//! Handles the `sh` operator which fills the current clipping region with
//! a shading pattern. Supports Type 2 (axial/linear) and Type 3 (radial)
//! gradient shadings.

use tiny_skia::{Color, GradientStop, LinearGradient, Paint, Point, RadialGradient, SpreadMode};

use crate::core::objects::{DictExt, Dictionary, Object};
use crate::document::Document;

use super::colors::{obj_f32, parse_f32_array, parse_transform};
use super::function::PdfFunction;
use super::graphics::RenderState;

/// Renders a shading pattern onto the pixmap.
///
/// The shading fills the entire clipping region (or full pixmap if no clip).
pub(crate) fn render_shading(
    shading_dict: &Dictionary,
    state: &RenderState,
    pixmap: &mut tiny_skia::Pixmap,
    clip_mask: Option<&tiny_skia::Mask>,
    doc: &Document,
) {
    let shading_type = match shading_dict.get_str("ShadingType") {
        Some(Object::Integer(t)) => *t,
        _ => return,
    };

    if shading_type == 1 {
        render_function_shading(shading_dict, state, pixmap, clip_mask, doc);
        return;
    }

    let shader = match shading_type {
        2 => build_axial_shader(shading_dict, doc),
        3 => build_radial_shader(shading_dict, doc),
        _ => return,
    };

    let shader = match shader {
        Some(s) => s,
        None => return,
    };

    let paint = Paint {
        shader,
        anti_alias: true,
        blend_mode: state.blend_mode,
        ..Paint::default()
    };

    // Fill entire pixmap (clipping constrains it)
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;
    let rect = match tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
        Some(r) => r,
        None => return,
    };
    let path = tiny_skia::PathBuilder::from_rect(rect);
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        state.ctm,
        clip_mask,
    );
}

/// Builds a linear gradient shader from a Type 2 (axial) shading dictionary.
fn build_axial_shader(dict: &Dictionary, doc: &Document) -> Option<tiny_skia::Shader<'static>> {
    let coords = parse_coords4(dict.get_str("Coords")?)?;
    let stops = extract_gradient_stops(dict, doc)?;
    let _extend = parse_extend(dict.get_str("Extend"));
    // tiny-skia only supports Pad spread mode; extend flags are noted but not differentiated.
    let spread = SpreadMode::Pad;

    LinearGradient::new(
        Point::from_xy(coords[0], coords[1]),
        Point::from_xy(coords[2], coords[3]),
        stops,
        spread,
        tiny_skia::Transform::identity(),
    )
}

/// Builds a radial gradient shader from a Type 3 (radial) shading dictionary.
fn build_radial_shader(dict: &Dictionary, doc: &Document) -> Option<tiny_skia::Shader<'static>> {
    let coords = parse_coords6(dict.get_str("Coords")?)?;
    let stops = extract_gradient_stops(dict, doc)?;

    // Use the second circle (x1, y1, r1) as the focal/center
    RadialGradient::new(
        Point::from_xy(coords[0], coords[1]), // start circle center
        Point::from_xy(coords[3], coords[4]), // end circle center
        coords[5],                            // end radius
        stops,
        SpreadMode::Pad,
        tiny_skia::Transform::identity(),
    )
}

/// Renders a Type 1 (function-based) shading by per-pixel evaluation.
///
/// The function maps (x, y) domain coordinates to color components.
/// For simplicity, only the x-axis is used as the function input for
/// single-input Type 2 functions.
fn render_function_shading(
    dict: &Dictionary,
    state: &RenderState,
    pixmap: &mut tiny_skia::Pixmap,
    clip_mask: Option<&tiny_skia::Mask>,
    doc: &Document,
) {
    // Parse domain [x0 x1 y0 y1], defaults to [0 1 0 1]
    let domain = match dict.get_str("Domain").and_then(|o| o.as_array()) {
        Some(arr) if arr.len() >= 4 => [
            obj_f32(&arr[0]).unwrap_or(0.0),
            obj_f32(&arr[1]).unwrap_or(1.0),
            obj_f32(&arr[2]).unwrap_or(0.0),
            obj_f32(&arr[3]).unwrap_or(1.0),
        ],
        _ => [0.0, 1.0, 0.0, 1.0],
    };

    // Parse shading matrix (maps domain space to device space)
    let matrix =
        parse_transform(dict.get_str("Matrix")).unwrap_or(tiny_skia::Transform::identity());

    // Parse the function via PdfFunction (supports Type 2 and Type 3)
    let func_obj = match dict.get_str("Function").and_then(|o| doc.resolve(o)) {
        Some(obj) => obj,
        _ => return,
    };
    let func = match PdfFunction::from_object(func_obj, doc) {
        Some(f) => f,
        None => return,
    };

    // Combined transform: CTM × shading matrix
    let combined = state.ctm.pre_concat(matrix);
    let inv = match combined.invert() {
        Some(t) => t,
        None => return,
    };

    let w = pixmap.width();
    let h = pixmap.height();
    let dx = domain[1] - domain[0];

    let mask_data = clip_mask.map(|m| m.data());
    let mut buf = Vec::new();

    for py in 0..h {
        for px in 0..w {
            // Check clip mask
            if let Some(md) = mask_data {
                if md[(py * w + px) as usize] == 0 {
                    continue;
                }
            }

            // Map pixel to domain coordinates via inverse transform
            let px_f = px as f32 + 0.5;
            let py_f = py as f32 + 0.5;
            let dx_coord = inv.sx * px_f + inv.kx * py_f + inv.tx;

            // Normalize to [0, 1] within the domain (only x-axis used for single-input functions)
            let tx = ((dx_coord - domain[0]) / dx).clamp(0.0, 1.0);

            // Evaluate function using reusable buffer (zero per-pixel allocation)
            func.evaluate_into(tx, &mut buf);

            if let Some(color) = components_to_color(&buf) {
                let pixel = match tiny_skia::PremultipliedColorU8::from_rgba(
                    (color.red() * 255.0) as u8,
                    (color.green() * 255.0) as u8,
                    (color.blue() * 255.0) as u8,
                    255,
                ) {
                    Some(p) => p,
                    None => continue,
                };
                pixmap.pixels_mut()[(py * w + px) as usize] = pixel;
            }
        }
    }
}

/// Extracts gradient stops from a shading's /Function entry.
///
/// Supports Type 2 (exponential interpolation) and Type 3 (stitching) functions.
fn extract_gradient_stops(dict: &Dictionary, doc: &Document) -> Option<Vec<GradientStop>> {
    let func_obj = dict.get_str("Function").and_then(|o| doc.resolve(o))?;

    match func_obj {
        Object::Dictionary(func_dict) => extract_stops_from_function(func_dict),
        Object::Array(arr) => {
            // Array of functions → stitching
            extract_stops_from_stitching_array(arr, doc)
        }
        _ => None,
    }
}

/// Extracts stops from a single Type 2 exponential interpolation function.
fn extract_stops_from_function(func_dict: &Dictionary) -> Option<Vec<GradientStop>> {
    let func_type = match func_dict.get_str("FunctionType") {
        Some(Object::Integer(t)) => *t,
        _ => return None,
    };

    match func_type {
        2 => {
            // Type 2: exponential interpolation C0 → C1
            let c0 = parse_f32_array(func_dict.get_str("C0")).unwrap_or(vec![0.0]);
            let c1 = parse_f32_array(func_dict.get_str("C1")).unwrap_or(vec![1.0]);
            let start_color = components_to_color(&c0)?;
            let end_color = components_to_color(&c1)?;
            Some(vec![
                GradientStop::new(0.0, start_color),
                GradientStop::new(1.0, end_color),
            ])
        }
        3 => {
            // Type 3: stitching function
            extract_stops_from_stitching(func_dict)
        }
        _ => None,
    }
}

/// Extracts stops from a Type 3 stitching function.
fn extract_stops_from_stitching(func_dict: &Dictionary) -> Option<Vec<GradientStop>> {
    let functions = func_dict.get_str("Functions")?.as_array()?;
    let bounds = parse_f32_array(func_dict.get_str("Bounds")).unwrap_or_default();
    let domain = parse_f32_array(func_dict.get_str("Domain")).unwrap_or(vec![0.0, 1.0]);

    let mut stops = Vec::new();
    let d0 = domain.first().copied().unwrap_or(0.0);
    let d1 = domain.get(1).copied().unwrap_or(1.0);
    let range = d1 - d0;
    if range <= 0.0 {
        return None;
    }

    // Build stop positions from domain + bounds
    let mut positions = vec![d0];
    positions.extend_from_slice(&bounds);
    positions.push(d1);

    for (i, func_obj) in functions.iter().enumerate() {
        if let Object::Dictionary(sub_func) = func_obj {
            let c0 = parse_f32_array(sub_func.get_str("C0")).unwrap_or(vec![0.0]);
            let c1 = parse_f32_array(sub_func.get_str("C1")).unwrap_or(vec![1.0]);

            if i == 0 {
                if let Some(color) = components_to_color(&c0) {
                    let t = (positions[i] - d0) / range;
                    stops.push(GradientStop::new(t.clamp(0.0, 1.0), color));
                }
            }
            if let Some(color) = components_to_color(&c1) {
                let t = (positions[i + 1] - d0) / range;
                stops.push(GradientStop::new(t.clamp(0.0, 1.0), color));
            }
        }
    }

    if stops.len() >= 2 {
        Some(stops)
    } else {
        None
    }
}

/// Extracts stops from an array of functions (implicit stitching).
fn extract_stops_from_stitching_array(
    arr: &[Object],
    _doc: &Document,
) -> Option<Vec<GradientStop>> {
    let mut stops = Vec::new();
    let n = arr.len();
    for (i, func_obj) in arr.iter().enumerate() {
        if let Object::Dictionary(sub_func) = func_obj {
            let c0 = parse_f32_array(sub_func.get_str("C0")).unwrap_or(vec![0.0]);
            let c1 = parse_f32_array(sub_func.get_str("C1")).unwrap_or(vec![1.0]);

            if i == 0 {
                if let Some(color) = components_to_color(&c0) {
                    stops.push(GradientStop::new(0.0, color));
                }
            }
            if let Some(color) = components_to_color(&c1) {
                let t = (i + 1) as f32 / n as f32;
                stops.push(GradientStop::new(t, color));
            }
        }
    }

    if stops.len() >= 2 {
        Some(stops)
    } else {
        None
    }
}

/// Converts color components to a `tiny_skia::Color`.
fn components_to_color(components: &[f32]) -> Option<Color> {
    match components.len() {
        1 => {
            let g = components[0].clamp(0.0, 1.0);
            Color::from_rgba(g, g, g, 1.0)
        }
        3 => Color::from_rgba(
            components[0].clamp(0.0, 1.0),
            components[1].clamp(0.0, 1.0),
            components[2].clamp(0.0, 1.0),
            1.0,
        ),
        4 => {
            let r = (1.0 - components[0]) * (1.0 - components[3]);
            let g = (1.0 - components[1]) * (1.0 - components[3]);
            let b = (1.0 - components[2]) * (1.0 - components[3]);
            Color::from_rgba(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), 1.0)
        }
        _ => None,
    }
}

// --- Parsing helpers ---

fn parse_coords4(obj: &Object) -> Option<[f32; 4]> {
    let arr = obj.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    Some([
        obj_f32(&arr[0])?,
        obj_f32(&arr[1])?,
        obj_f32(&arr[2])?,
        obj_f32(&arr[3])?,
    ])
}

fn parse_coords6(obj: &Object) -> Option<[f32; 6]> {
    let arr = obj.as_array()?;
    if arr.len() < 6 {
        return None;
    }
    Some([
        obj_f32(&arr[0])?,
        obj_f32(&arr[1])?,
        obj_f32(&arr[2])?,
        obj_f32(&arr[3])?,
        obj_f32(&arr[4])?,
        obj_f32(&arr[5])?,
    ])
}

fn parse_extend(obj: Option<&Object>) -> [bool; 2] {
    match obj {
        Some(Object::Array(arr)) if arr.len() >= 2 => {
            let e0 = matches!(&arr[0], Object::Boolean(true));
            let e1 = matches!(&arr[1], Object::Boolean(true));
            [e0, e1]
        }
        _ => [false, false],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;

    #[test]
    fn type1_shading_with_stitching_function() {
        // A Type 1 shading using a Type 3 (stitching) function should produce
        // colored pixels. Before the refactor, render_function_shading bails
        // on non-Type 2 functions, so this pixmap stays blank.
        let mut shading_dict = Dictionary::new();
        shading_dict.insert(PdfName::new("ShadingType"), Object::Integer(1));

        // Build a Type 3 stitching function: two segments, black→red→white
        let mut seg0 = Dictionary::new();
        seg0.insert(PdfName::new("FunctionType"), Object::Integer(2));
        seg0.insert(PdfName::new("N"), Object::Integer(1));
        seg0.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        seg0.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );

        let mut seg1 = Dictionary::new();
        seg1.insert(PdfName::new("FunctionType"), Object::Integer(2));
        seg1.insert(PdfName::new("N"), Object::Integer(1));
        seg1.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        seg1.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(1.0),
                Object::Real(1.0),
            ]),
        );

        let mut stitch = Dictionary::new();
        stitch.insert(PdfName::new("FunctionType"), Object::Integer(3));
        stitch.insert(
            PdfName::new("Functions"),
            Object::Array(vec![Object::Dictionary(seg0), Object::Dictionary(seg1)]),
        );
        stitch.insert(
            PdfName::new("Bounds"),
            Object::Array(vec![Object::Real(0.5)]),
        );
        stitch.insert(
            PdfName::new("Encode"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );
        stitch.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );

        shading_dict.insert(PdfName::new("Function"), Object::Dictionary(stitch));

        let state = RenderState::default();
        let mut pixmap = tiny_skia::Pixmap::new(4, 4).unwrap();
        let doc = Document::new();

        render_function_shading(&shading_dict, &state, &mut pixmap, None, &doc);

        // At least some pixels should be non-zero (i.e., the function was evaluated)
        let has_color = pixmap
            .pixels()
            .iter()
            .any(|p| p.red() > 0 || p.green() > 0 || p.blue() > 0);
        assert!(
            has_color,
            "Type 1 shading with stitching function should produce colored pixels"
        );
    }

    #[test]
    fn components_to_color_gray() {
        let c = components_to_color(&[0.5]).unwrap();
        assert!((c.red() - 0.5).abs() < 0.01);
    }

    #[test]
    fn components_to_color_rgb() {
        let c = components_to_color(&[1.0, 0.0, 0.5]).unwrap();
        assert!((c.red() - 1.0).abs() < 0.01);
        assert!(c.green() < 0.01);
        assert!((c.blue() - 0.5).abs() < 0.01);
    }

    #[test]
    fn parse_coords4_valid() {
        let arr = Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(100.0),
            Object::Real(100.0),
        ]);
        let coords = parse_coords4(&arr).unwrap();
        assert_eq!(coords, [0.0, 0.0, 100.0, 100.0]);
    }

    #[test]
    fn parse_extend_defaults() {
        assert_eq!(parse_extend(None), [false, false]);
    }

    #[test]
    fn parse_extend_true() {
        let arr = Object::Array(vec![Object::Boolean(true), Object::Boolean(true)]);
        assert_eq!(parse_extend(Some(&arr)), [true, true]);
    }

    #[test]
    fn gradient_stops_from_type2_function() {
        // A Type 2 function dict should produce 2 stops (start and end)
        let mut func_dict = Dictionary::new();
        func_dict.insert(PdfName::new("FunctionType"), Object::Integer(2));
        func_dict.insert(PdfName::new("N"), Object::Integer(1));
        func_dict.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        func_dict.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );

        let stops = extract_stops_from_function(&func_dict).unwrap();
        assert_eq!(stops.len(), 2);
    }

    #[test]
    fn gradient_stops_from_type3_stitching() {
        // A Type 3 stitching function with 2 sub-functions → 3 stops
        let mut seg0 = Dictionary::new();
        seg0.insert(PdfName::new("FunctionType"), Object::Integer(2));
        seg0.insert(PdfName::new("N"), Object::Integer(1));
        seg0.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        seg0.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(1.0),
                Object::Real(0.0),
            ]),
        );

        let mut seg1 = Dictionary::new();
        seg1.insert(PdfName::new("FunctionType"), Object::Integer(2));
        seg1.insert(PdfName::new("N"), Object::Integer(1));
        seg1.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(1.0),
                Object::Real(0.0),
            ]),
        );
        seg1.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );

        let mut func_dict = Dictionary::new();
        func_dict.insert(PdfName::new("FunctionType"), Object::Integer(3));
        func_dict.insert(
            PdfName::new("Functions"),
            Object::Array(vec![Object::Dictionary(seg0), Object::Dictionary(seg1)]),
        );
        func_dict.insert(
            PdfName::new("Bounds"),
            Object::Array(vec![Object::Real(0.5)]),
        );
        func_dict.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );

        let stops = extract_stops_from_function(&func_dict).unwrap();
        assert_eq!(stops.len(), 3);
    }

    #[test]
    fn gradient_stops_from_stitching_array() {
        // An array of 2 Type 2 function dicts → 3 stops (implicit stitching)
        let mut f0 = Dictionary::new();
        f0.insert(PdfName::new("FunctionType"), Object::Integer(2));
        f0.insert(PdfName::new("N"), Object::Integer(1));
        f0.insert(PdfName::new("C0"), Object::Array(vec![Object::Real(0.0)]));
        f0.insert(PdfName::new("C1"), Object::Array(vec![Object::Real(0.5)]));

        let mut f1 = Dictionary::new();
        f1.insert(PdfName::new("FunctionType"), Object::Integer(2));
        f1.insert(PdfName::new("N"), Object::Integer(1));
        f1.insert(PdfName::new("C0"), Object::Array(vec![Object::Real(0.5)]));
        f1.insert(PdfName::new("C1"), Object::Array(vec![Object::Real(1.0)]));

        let arr = vec![Object::Dictionary(f0), Object::Dictionary(f1)];
        let doc = Document::new();
        let stops = extract_stops_from_stitching_array(&arr, &doc).unwrap();
        assert_eq!(stops.len(), 3);
    }

    #[test]
    fn components_to_color_cmyk() {
        // Pure cyan: CMYK (1, 0, 0, 0) → RGB (0, 1, 1)
        let c = components_to_color(&[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert!(c.red() < 0.01);
        assert!((c.green() - 1.0).abs() < 0.01);
        assert!((c.blue() - 1.0).abs() < 0.01);
    }

    #[test]
    fn parse_coords6_valid() {
        let arr = Object::Array(vec![
            Object::Real(10.0),
            Object::Real(20.0),
            Object::Real(5.0),
            Object::Real(30.0),
            Object::Real(40.0),
            Object::Real(15.0),
        ]);
        let coords = parse_coords6(&arr).unwrap();
        assert_eq!(coords, [10.0, 20.0, 5.0, 30.0, 40.0, 15.0]);
    }

    #[test]
    fn parse_coords6_too_short() {
        let arr = Object::Array(vec![Object::Real(1.0), Object::Real(2.0)]);
        assert!(parse_coords6(&arr).is_none());
    }
}
