//! Rendering-level color space support.
//!
//! Resolves color spaces from page resources and converts color component
//! values to `tiny_skia::Color` for rasterization.
//!
//! Supports DeviceGray, DeviceRGB, DeviceCMYK, CalGray, CalRGB, and Indexed.

use crate::core::objects::{DictExt, Object};
use crate::document::Document;

use super::colors::cmyk_to_rgb;
use crate::rendering::function::PdfFunction;

/// A resolved color space for rendering.
#[derive(Debug, Clone)]
pub(crate) enum RenderColorSpace {
    /// Device gray (1 component).
    DeviceGray,
    /// Device RGB (3 components).
    DeviceRGB,
    /// Device CMYK (4 components).
    DeviceCMYK,
    /// Calibrated grayscale with gamma correction.
    CalGray {
        /// Gamma exponent (default 1.0).
        gamma: f32,
    },
    /// Calibrated RGB with gamma and optional matrix.
    CalRGB {
        /// Per-channel gamma exponents (default [1, 1, 1]).
        gamma: [f32; 3],
        /// 3×3 matrix mapping ABC to XYZ (row-major, default identity).
        matrix: [f32; 9],
        /// White point XYZ for D50→D65 adaptation.
        white_point: [f32; 3],
    },
    /// CIE L*a*b* color space.
    Lab {
        /// White point XYZ (required).
        white_point: [f32; 3],
        /// Range for a* and b* (default [-100, 100, -100, 100]).
        range: [f32; 4],
    },
    /// Separation (spot) color space — 1 tint component mapped to an alternate space.
    Separation {
        /// Alternate color space for rendering.
        alternate: Box<RenderColorSpace>,
        /// Tint transform function mapping tint value to alternate space components.
        tint_transform: PdfFunction,
    },
    /// DeviceN color space — multi-component generalization of Separation.
    DeviceN {
        /// Alternate color space for rendering.
        alternate: Box<RenderColorSpace>,
        /// Tint transform function mapping tint values to alternate space components.
        tint_transform: PdfFunction,
        /// Number of input colorant components.
        num_components: usize,
    },
    /// ICCBased color space — falls back to alternate for now.
    ICCBased {
        /// Number of color components (1, 3, or 4).
        num_components: u8,
        /// Alternate color space for fallback rendering.
        alternate: Box<RenderColorSpace>,
    },
    /// Pattern color space — color is defined by a tiling/shading pattern.
    Pattern,
    /// Indexed (palette) color space.
    Indexed {
        /// Base color space for palette entries.
        base: Box<RenderColorSpace>,
        /// Lookup table: sequential component values for each index.
        lookup: Vec<u8>,
        /// Maximum valid index.
        hival: u8,
    },
}

impl RenderColorSpace {
    /// Number of input components for this color space.
    pub fn num_components(&self) -> usize {
        match self {
            Self::DeviceGray
            | Self::CalGray { .. }
            | Self::Indexed { .. }
            | Self::Pattern
            | Self::Separation { .. } => 1,
            Self::DeviceN { num_components, .. } => *num_components,
            Self::DeviceRGB | Self::CalRGB { .. } | Self::Lab { .. } => 3,
            Self::DeviceCMYK => 4,
            Self::ICCBased { num_components, .. } => *num_components as usize,
        }
    }

    /// Converts component values to an RGBA color.
    pub fn to_color(&self, components: &[f32]) -> Option<tiny_skia::Color> {
        match self {
            Self::DeviceGray => {
                let g = clamped(components, 0)?;
                tiny_skia::Color::from_rgba(g, g, g, 1.0)
            }
            Self::DeviceRGB => {
                let r = clamped(components, 0)?;
                let g = clamped(components, 1)?;
                let b = clamped(components, 2)?;
                tiny_skia::Color::from_rgba(r, g, b, 1.0)
            }
            Self::DeviceCMYK => {
                let (r, g, b) = cmyk_to_rgb(
                    clamped(components, 0)?,
                    clamped(components, 1)?,
                    clamped(components, 2)?,
                    clamped(components, 3)?,
                );
                tiny_skia::Color::from_rgba(r, g, b, 1.0)
            }
            Self::CalGray { gamma } => {
                let a = clamped(components, 0)?;
                // Apply gamma: linear = a^gamma, then convert to sRGB
                let linear = a.powf(*gamma);
                let srgb = linear_to_srgb(linear);
                tiny_skia::Color::from_rgba(srgb, srgb, srgb, 1.0)
            }
            Self::CalRGB {
                gamma,
                matrix,
                white_point,
            } => {
                let a = clamped(components, 0)?;
                let b = clamped(components, 1)?;
                let c = clamped(components, 2)?;

                // Apply gamma per channel
                let ag = a.powf(gamma[0]);
                let bg = b.powf(gamma[1]);
                let cg = c.powf(gamma[2]);

                // Matrix multiply: ABC → XYZ
                let x = matrix[0] * ag + matrix[3] * bg + matrix[6] * cg;
                let y = matrix[1] * ag + matrix[4] * bg + matrix[7] * cg;
                let z = matrix[2] * ag + matrix[5] * bg + matrix[8] * cg;

                // Adapt from source white point to D65
                let (r, g, b) = xyz_to_srgb(x, y, z, white_point);
                tiny_skia::Color::from_rgba(
                    r.clamp(0.0, 1.0),
                    g.clamp(0.0, 1.0),
                    b.clamp(0.0, 1.0),
                    1.0,
                )
            }
            Self::Lab { white_point, range } => {
                let l_star = components.first().copied().unwrap_or(0.0);
                let a_star = components
                    .get(1)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(range[0], range[1]);
                let b_star = components
                    .get(2)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(range[2], range[3]);
                let (r, g, b) = lab_to_srgb(l_star, a_star, b_star, white_point);
                tiny_skia::Color::from_rgba(
                    r.clamp(0.0, 1.0),
                    g.clamp(0.0, 1.0),
                    b.clamp(0.0, 1.0),
                    1.0,
                )
            }
            Self::Separation {
                alternate,
                tint_transform,
            } => {
                let tint = components.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let alt_components = tint_transform.evaluate(tint);
                alternate.to_color(&alt_components)
            }
            Self::DeviceN {
                alternate,
                tint_transform,
                ..
            } => {
                // Pass all components to the tint transform for multi-colorant support
                let clamped: Vec<f32> = components.iter().map(|c| c.clamp(0.0, 1.0)).collect();
                let alt_components = tint_transform.evaluate_multi(&clamped);
                alternate.to_color(&alt_components)
            }
            Self::ICCBased { alternate, .. } => alternate.to_color(components),
            Self::Pattern => None, // Color is set via the pattern, not components
            Self::Indexed {
                base,
                lookup,
                hival,
            } => {
                let idx = *components.first()? as u8;
                if idx > *hival {
                    return None;
                }
                let nc = base.num_components();
                let offset = idx as usize * nc;
                if offset + nc > lookup.len() {
                    return None;
                }
                let base_components: Vec<f32> = lookup[offset..offset + nc]
                    .iter()
                    .map(|&v| v as f32 / 255.0)
                    .collect();
                base.to_color(&base_components)
            }
        }
    }

    /// Parses a color space from a PDF object, resolving references as needed.
    pub fn from_object(obj: &Object, doc: &Document) -> Option<Self> {
        match obj {
            Object::Name(name) => Self::from_name(name.as_str()),
            Object::Array(arr) if !arr.is_empty() => {
                let cs_name = arr[0].as_name()?;
                Self::from_array(cs_name, arr, doc)
            }
            _ => None,
        }
    }

    /// Parses from a simple name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "DeviceGray" | "G" => Some(Self::DeviceGray),
            "DeviceRGB" | "RGB" => Some(Self::DeviceRGB),
            "DeviceCMYK" | "CMYK" => Some(Self::DeviceCMYK),
            "Pattern" => Some(Self::Pattern),
            _ => None,
        }
    }

    /// Parses from an array form like `[/CalGray << ... >>]`.
    fn from_array(cs_name: &str, arr: &[Object], doc: &Document) -> Option<Self> {
        match cs_name {
            "CalGray" => {
                let dict = arr.get(1).and_then(|o| match o {
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                })?;
                let gamma = dict
                    .get_str("Gamma")
                    .and_then(|o| match o {
                        Object::Real(f) => Some(*f as f32),
                        Object::Integer(i) => Some(*i as f32),
                        _ => None,
                    })
                    .unwrap_or(1.0);
                Some(Self::CalGray { gamma })
            }
            "CalRGB" => {
                let dict = arr.get(1).and_then(|o| match o {
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                })?;
                let gamma = parse_f32_array3(dict.get_str("Gamma")).unwrap_or([1.0, 1.0, 1.0]);
                let matrix = parse_f32_array9(dict.get_str("Matrix"))
                    .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
                let white_point =
                    parse_f32_array3(dict.get_str("WhitePoint")).unwrap_or([0.9505, 1.0, 1.0890]);
                Some(Self::CalRGB {
                    gamma,
                    matrix,
                    white_point,
                })
            }
            "Lab" => {
                let dict = arr.get(1).and_then(|o| match o {
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                })?;
                let white_point =
                    parse_f32_array3(dict.get_str("WhitePoint")).unwrap_or([0.9505, 1.0, 1.0890]);
                let range = match dict.get_str("Range").and_then(|o| o.as_array()) {
                    Some(r) if r.len() >= 4 => [
                        obj_to_f32(&r[0]).unwrap_or(-100.0),
                        obj_to_f32(&r[1]).unwrap_or(100.0),
                        obj_to_f32(&r[2]).unwrap_or(-100.0),
                        obj_to_f32(&r[3]).unwrap_or(100.0),
                    ],
                    _ => [-100.0, 100.0, -100.0, 100.0],
                };
                Some(Self::Lab { white_point, range })
            }
            "Separation" => {
                // [/Separation /name /AlternateSpace tintTransform]
                let (alternate, tint_transform) = parse_alternate_and_tint(arr, doc, 2, 3)?;
                Some(Self::Separation {
                    alternate: Box::new(alternate),
                    tint_transform,
                })
            }
            "DeviceN" => {
                // [/DeviceN [/names...] /AlternateSpace tintTransform]
                let names = arr.get(1).and_then(|o| o.as_array())?;
                let num_components = names.len();
                let (alternate, tint_transform) = parse_alternate_and_tint(arr, doc, 2, 3)?;
                Some(Self::DeviceN {
                    alternate: Box::new(alternate),
                    tint_transform,
                    num_components,
                })
            }
            "ICCBased" => {
                // [/ICCBased stream] — stream dict has /N and optional /Alternate
                let stream = match arr.get(1).and_then(|o| doc.resolve(o)) {
                    Some(Object::Stream(s)) => s,
                    _ => return None,
                };
                let n = match stream.dict.get_str("N") {
                    Some(Object::Integer(n)) => *n as u8,
                    _ => return None,
                };
                let alternate = stream
                    .dict
                    .get_str("Alternate")
                    .and_then(|o| Self::from_object(o, doc))
                    .unwrap_or(match n {
                        1 => Self::DeviceGray,
                        3 => Self::DeviceRGB,
                        4 => Self::DeviceCMYK,
                        _ => Self::DeviceRGB,
                    });
                Some(Self::ICCBased {
                    num_components: n,
                    alternate: Box::new(alternate),
                })
            }
            "Indexed" | "I" => {
                if arr.len() < 4 {
                    return None;
                }
                let base_obj = doc.resolve(&arr[1]).unwrap_or(&arr[1]);
                let base = Self::from_object(base_obj, doc)?;
                let hival = match &arr[2] {
                    Object::Integer(i) => *i as u8,
                    _ => return None,
                };
                let lookup = match &arr[3] {
                    Object::String(s) => s.bytes.clone(),
                    Object::Stream(s) => s.decode_data().ok()?,
                    _ => return None,
                };
                Some(Self::Indexed {
                    base: Box::new(base),
                    lookup,
                    hival,
                })
            }
            _ => None,
        }
    }
}

/// Parses alternate color space and tint transform from a color space array.
///
/// Shared by Separation and DeviceN parsing.
fn parse_alternate_and_tint(
    arr: &[Object],
    doc: &Document,
    alternate_idx: usize,
    tint_idx: usize,
) -> Option<(RenderColorSpace, PdfFunction)> {
    let alternate_obj = arr
        .get(alternate_idx)
        .and_then(|o| doc.resolve(o))
        .unwrap_or(&arr[alternate_idx]);
    let alternate = RenderColorSpace::from_object(alternate_obj, doc)?;
    let tint_transform = arr
        .get(tint_idx)
        .and_then(|o| doc.resolve(o).or(Some(o)))
        .and_then(|o| PdfFunction::from_object(o, doc))
        .unwrap_or_else(|| PdfFunction::identity(alternate.num_components()));
    Some((alternate, tint_transform))
}

/// Gets component at `idx`, clamped to [0, 1].
fn clamped(components: &[f32], idx: usize) -> Option<f32> {
    components.get(idx).map(|v| v.clamp(0.0, 1.0))
}

/// Converts a linear light value to sRGB gamma (approximate).
fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Converts CIE L*a*b* to sRGB via XYZ.
fn lab_to_srgb(l: f32, a: f32, b: f32, white_point: &[f32; 3]) -> (f32, f32, f32) {
    // L*a*b* to XYZ (CIE standard)
    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;

    let delta = 6.0 / 29.0;
    let delta_sq = delta * delta;
    let xr = if fx > delta {
        fx * fx * fx
    } else {
        3.0 * delta_sq * (fx - 4.0 / 29.0)
    };
    // κ = 903.3, ε = 0.008856, κε ≈ 8.0
    let kappa = 903.3;
    let yr = if l > 8.0 { fy * fy * fy } else { l / kappa };
    let zr = if fz > delta {
        fz * fz * fz
    } else {
        3.0 * delta_sq * (fz - 4.0 / 29.0)
    };

    let x = xr * white_point[0];
    let y = yr * white_point[1];
    let z = zr * white_point[2];

    xyz_to_srgb(x, y, z, white_point)
}

/// Converts XYZ to sRGB with chromatic adaptation from source white point.
fn xyz_to_srgb(x: f32, y: f32, z: f32, _white_point: &[f32; 3]) -> (f32, f32, f32) {
    // sRGB matrix (D65 illuminant)
    let rl = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gl = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bl = 0.0557 * x - 0.2040 * y + 1.0570 * z;

    (linear_to_srgb(rl), linear_to_srgb(gl), linear_to_srgb(bl))
}

/// Parses a 3-element f32 array from a PDF object.
fn parse_f32_array3(obj: Option<&Object>) -> Option<[f32; 3]> {
    let arr = obj?.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    Some([
        obj_to_f32(&arr[0])?,
        obj_to_f32(&arr[1])?,
        obj_to_f32(&arr[2])?,
    ])
}

/// Parses a 9-element f32 array from a PDF object.
fn parse_f32_array9(obj: Option<&Object>) -> Option<[f32; 9]> {
    let arr = obj?.as_array()?;
    if arr.len() < 9 {
        return None;
    }
    Some([
        obj_to_f32(&arr[0])?,
        obj_to_f32(&arr[1])?,
        obj_to_f32(&arr[2])?,
        obj_to_f32(&arr[3])?,
        obj_to_f32(&arr[4])?,
        obj_to_f32(&arr[5])?,
        obj_to_f32(&arr[6])?,
        obj_to_f32(&arr[7])?,
        obj_to_f32(&arr[8])?,
    ])
}

fn obj_to_f32(obj: &Object) -> Option<f32> {
    match obj {
        Object::Real(f) => Some(*f as f32),
        Object::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_gray_to_color() {
        let cs = RenderColorSpace::DeviceGray;
        let c = cs.to_color(&[0.5]).unwrap();
        assert!((c.red() - 0.5).abs() < 0.01);
        assert!((c.green() - 0.5).abs() < 0.01);
        assert!((c.blue() - 0.5).abs() < 0.01);
    }

    #[test]
    fn device_rgb_to_color() {
        let cs = RenderColorSpace::DeviceRGB;
        let c = cs.to_color(&[1.0, 0.0, 0.5]).unwrap();
        assert!((c.red() - 1.0).abs() < 0.01);
        assert!(c.green().abs() < 0.01);
        assert!((c.blue() - 0.5).abs() < 0.01);
    }

    #[test]
    fn cal_gray_gamma_correction() {
        let cs = RenderColorSpace::CalGray { gamma: 2.2 };
        let c = cs.to_color(&[0.5]).unwrap();
        // 0.5^2.2 ≈ 0.2176, then linear_to_srgb(0.2176) ≈ 0.498
        // The key point: CalGray 0.5 with gamma 2.2 should differ from DeviceGray 0.5
        let dg = RenderColorSpace::DeviceGray.to_color(&[0.5]).unwrap();
        assert!(
            (c.red() - dg.red()).abs() > 0.001,
            "CalGray with gamma should differ from DeviceGray"
        );
    }

    #[test]
    fn cal_rgb_identity_matrix() {
        // With gamma=[1,1,1] and identity matrix, CalRGB should approximate DeviceRGB
        let cs = RenderColorSpace::CalRGB {
            gamma: [1.0, 1.0, 1.0],
            matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            white_point: [0.9505, 1.0, 1.0890],
        };
        let c = cs.to_color(&[0.5, 0.0, 0.0]).unwrap();
        // With identity matrix, X=0.5, Y=0, Z=0 → XYZ to sRGB
        assert!(c.red() > 0.3, "red channel should be positive");
    }

    #[test]
    fn indexed_rgb_lookup() {
        let cs = RenderColorSpace::Indexed {
            base: Box::new(RenderColorSpace::DeviceRGB),
            lookup: vec![
                255, 0, 0, // index 0 = red
                0, 255, 0, // index 1 = green
                0, 0, 255, // index 2 = blue
            ],
            hival: 2,
        };
        // Index 1 should be green
        let c = cs.to_color(&[1.0]).unwrap();
        assert!(c.red() < 0.01);
        assert!((c.green() - 1.0).abs() < 0.01);
        assert!(c.blue() < 0.01);
    }

    #[test]
    fn indexed_out_of_range() {
        let cs = RenderColorSpace::Indexed {
            base: Box::new(RenderColorSpace::DeviceRGB),
            lookup: vec![255, 0, 0],
            hival: 0,
        };
        assert!(cs.to_color(&[5.0]).is_none());
    }

    #[test]
    fn num_components() {
        assert_eq!(RenderColorSpace::DeviceGray.num_components(), 1);
        assert_eq!(RenderColorSpace::DeviceRGB.num_components(), 3);
        assert_eq!(RenderColorSpace::DeviceCMYK.num_components(), 4);
        assert_eq!(RenderColorSpace::CalGray { gamma: 1.0 }.num_components(), 1);
    }

    #[test]
    fn lab_white_point() {
        // L*=100, a*=0, b*=0 → D65 white → should be close to (1, 1, 1)
        let cs = RenderColorSpace::Lab {
            white_point: [0.9505, 1.0, 1.0890],
            range: [-128.0, 127.0, -128.0, 127.0],
        };
        let c = cs.to_color(&[100.0, 0.0, 0.0]).unwrap();
        assert!(
            (c.red() - 1.0).abs() < 0.05
                && (c.green() - 1.0).abs() < 0.05
                && (c.blue() - 1.0).abs() < 0.05,
            "Lab L*=100 should be white, got ({}, {}, {})",
            c.red(),
            c.green(),
            c.blue()
        );
    }

    #[test]
    fn lab_black() {
        // L*=0, a*=0, b*=0 → black
        let cs = RenderColorSpace::Lab {
            white_point: [0.9505, 1.0, 1.0890],
            range: [-128.0, 127.0, -128.0, 127.0],
        };
        let c = cs.to_color(&[0.0, 0.0, 0.0]).unwrap();
        assert!(
            c.red() < 0.05 && c.green() < 0.05 && c.blue() < 0.05,
            "Lab L*=0 should be black, got ({}, {}, {})",
            c.red(),
            c.green(),
            c.blue()
        );
    }

    #[test]
    fn lab_red_ish() {
        // L*=50, a*=80, b*=0 → reddish/magenta
        let cs = RenderColorSpace::Lab {
            white_point: [0.9505, 1.0, 1.0890],
            range: [-128.0, 127.0, -128.0, 127.0],
        };
        let c = cs.to_color(&[50.0, 80.0, 0.0]).unwrap();
        assert!(
            c.red() > c.green() && c.red() > 0.3,
            "Lab a*=80 should have strong red, got ({}, {}, {})",
            c.red(),
            c.green(),
            c.blue()
        );
    }

    #[test]
    fn separation_to_alternate() {
        use crate::rendering::function::PdfFunction;
        // Separation with DeviceRGB alternate: tint maps to (tint, 0, 0)
        let cs = RenderColorSpace::Separation {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: PdfFunction::Exponential {
                c0: vec![0.0, 0.0, 0.0],
                c1: vec![1.0, 0.0, 0.0],
                n: 1.0,
            },
        };
        let c = cs.to_color(&[0.8]).unwrap();
        assert!(
            (c.red() - 0.8).abs() < 0.05,
            "Separation tint 0.8 should produce red ~0.8, got {}",
            c.red()
        );
        assert!(c.green() < 0.05);
        assert!(c.blue() < 0.05);
    }

    #[test]
    fn separation_with_cmyk_tint_transform() {
        use crate::rendering::function::PdfFunction;
        // Separation "PantoneRed" maps tint → CMYK (0, tint, tint, 0) = pure red via CMYK
        let cs = RenderColorSpace::Separation {
            alternate: Box::new(RenderColorSpace::DeviceCMYK),
            tint_transform: PdfFunction::Exponential {
                c0: vec![0.0, 0.0, 0.0, 0.0],
                c1: vec![0.0, 1.0, 1.0, 0.0],
                n: 1.0,
            },
        };
        let c = cs.to_color(&[1.0]).unwrap();
        // CMYK (0, 1, 1, 0) → RGB (1, 0, 0)
        assert!(
            (c.red() - 1.0).abs() < 0.05,
            "Full tint should be red, got {}",
            c.red()
        );
        assert!(c.green() < 0.05);
        assert!(c.blue() < 0.05);
    }

    #[test]
    fn icc_based_fallback_to_alternate() {
        // ICCBased with 3 components and DeviceRGB alternate should pass through
        let cs = RenderColorSpace::ICCBased {
            num_components: 3,
            alternate: Box::new(RenderColorSpace::DeviceRGB),
        };
        assert_eq!(cs.num_components(), 3);
        let c = cs.to_color(&[0.5, 0.3, 0.8]).unwrap();
        assert!(
            (c.red() - 0.5).abs() < 0.01,
            "ICCBased should fall back to alternate, got red={}",
            c.red()
        );
        assert!((c.green() - 0.3).abs() < 0.01);
        assert!((c.blue() - 0.8).abs() < 0.01);
    }

    #[test]
    fn devicen_num_components() {
        use crate::rendering::function::PdfFunction;
        let cs = RenderColorSpace::DeviceN {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: PdfFunction::identity(3),
            num_components: 2,
        };
        assert_eq!(cs.num_components(), 2);
    }

    #[test]
    fn devicen_to_color() {
        use crate::rendering::function::PdfFunction;
        // DeviceN with 2 colorants, alternate=DeviceRGB, tint transform maps
        // first component to (component, 0, 0) = red channel
        let cs = RenderColorSpace::DeviceN {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: PdfFunction::Exponential {
                c0: vec![0.0, 0.0, 0.0],
                c1: vec![1.0, 0.0, 0.0],
                n: 1.0,
            },
            num_components: 2,
        };
        let c = cs.to_color(&[0.8, 0.5]).unwrap();
        // First component (0.8) is used as tint input → (0.8, 0, 0)
        assert!(
            (c.red() - 0.8).abs() < 0.05,
            "DeviceN should map first component through tint transform, got red={}",
            c.red()
        );
    }

    #[test]
    fn devicen_single_matches_separation() {
        use crate::rendering::function::PdfFunction;
        let tint = PdfFunction::Exponential {
            c0: vec![0.0, 0.0, 0.0],
            c1: vec![0.0, 1.0, 0.0],
            n: 1.0,
        };
        let sep = RenderColorSpace::Separation {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: tint.clone(),
        };
        let dn = RenderColorSpace::DeviceN {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: tint,
            num_components: 1,
        };
        let sep_c = sep.to_color(&[0.6]).unwrap();
        let dn_c = dn.to_color(&[0.6]).unwrap();
        assert!(
            (sep_c.green() - dn_c.green()).abs() < 0.01,
            "1-component DeviceN should match Separation"
        );
    }

    #[test]
    fn icc_based_gray_fallback() {
        // ICCBased with 1 component defaults to DeviceGray
        let cs = RenderColorSpace::ICCBased {
            num_components: 1,
            alternate: Box::new(RenderColorSpace::DeviceGray),
        };
        assert_eq!(cs.num_components(), 1);
        let c = cs.to_color(&[0.7]).unwrap();
        assert!((c.red() - 0.7).abs() < 0.01);
    }

    #[test]
    fn devicen_passes_all_components_to_tint() {
        use crate::rendering::function::PdfFunction;

        // DeviceN with 2 colorants mapping to DeviceRGB via an exponential tint.
        // With evaluate_multi (currently single-input), it uses the first component.
        // This test verifies the DeviceN path calls evaluate_multi, not evaluate.
        let cs = RenderColorSpace::DeviceN {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: PdfFunction::Exponential {
                c0: vec![0.0, 0.0, 0.0],
                c1: vec![1.0, 0.5, 0.0],
                n: 1.0,
            },
            num_components: 2,
        };

        // 2 components: [0.6, 0.3]
        let c = cs.to_color(&[0.6, 0.3]).unwrap();
        // With first component (0.6): R=0.6, G=0.3, B=0.0
        assert!((c.red() - 0.6).abs() < 0.02);
        assert!((c.green() - 0.3).abs() < 0.02);
    }

    #[test]
    fn separation_still_uses_single_component() {
        use crate::rendering::function::PdfFunction;

        let cs = RenderColorSpace::Separation {
            alternate: Box::new(RenderColorSpace::DeviceRGB),
            tint_transform: PdfFunction::Exponential {
                c0: vec![1.0, 1.0, 1.0],
                c1: vec![0.0, 0.0, 0.0],
                n: 1.0,
            },
        };

        // Separation with tint 0.0 → C0 (white)
        let c = cs.to_color(&[0.0]).unwrap();
        assert!((c.red() - 1.0).abs() < 0.01);

        // Separation with tint 1.0 → C1 (black)
        let c = cs.to_color(&[1.0]).unwrap();
        assert!(c.red() < 0.01);
    }
}
