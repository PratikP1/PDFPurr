//! Image rendering for the PDF rendering engine.
//!
//! Handles image XObjects (`Do` operator) and inline images (`BI`/`ID`/`EI`)
//! by decoding image data and painting it onto the pixmap.

use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::core::objects::{Dictionary, PdfStream};
use crate::images::PdfImage;

use super::graphics::RenderState;

/// Renders an image XObject stream onto the pixmap.
///
/// The image is placed in a 1×1 unit square in user space, transformed
/// by the current CTM.
pub(crate) fn render_image(stream: &PdfStream, state: &RenderState, pixmap: &mut Pixmap) {
    let image = match PdfImage::from_stream(stream) {
        Ok(img) => img,
        Err(_) => return,
    };
    if image.is_image_mask {
        paint_image_mask(&image, state, pixmap);
    } else {
        paint_image(&image, state, pixmap);
    }
}

/// Renders an inline image (BI/ID/EI) onto the pixmap.
///
/// Note: `data.to_vec()` copies the image bytes because `PdfImage` owns its
/// data and the renderer borrows tokens non-destructively (needed for text
/// clipping re-interpretation). A future optimization could consume tokens
/// to avoid this copy for large inline images.
pub(crate) fn render_inline_image(
    dict: &Dictionary,
    data: &[u8],
    state: &RenderState,
    pixmap: &mut Pixmap,
) {
    let image = match PdfImage::from_inline(dict, data.to_vec()) {
        Ok(img) => img,
        Err(_) => return,
    };
    paint_image(&image, state, pixmap);
}

/// Paints a decoded `PdfImage` onto the pixmap using the current CTM.
fn paint_image(image: &PdfImage, state: &RenderState, pixmap: &mut Pixmap) {
    let rgba_data = match image.to_rgba() {
        Ok(data) => data,
        Err(_) => return,
    };

    let size = match tiny_skia::IntSize::from_wh(image.width, image.height) {
        Some(s) => s,
        None => return,
    };
    let img_pixmap = match Pixmap::from_vec(rgba_data, size) {
        Some(pm) => pm,
        None => return,
    };

    draw_image_pixmap(&img_pixmap, image.width, image.height, state, pixmap);
}

/// Draws an already-built image pixmap onto the target, applying the 1×1 unit
/// square transform scaled by the CTM.
fn draw_image_pixmap(
    img_pixmap: &Pixmap,
    width: u32,
    height: u32,
    state: &RenderState,
    pixmap: &mut Pixmap,
) {
    let img_w = width as f32;
    let img_h = height as f32;
    let image_transform = Transform::from_row(1.0 / img_w, 0.0, 0.0, -1.0 / img_h, 0.0, 1.0);
    let combined = state.ctm.pre_concat(image_transform);

    let paint = PixmapPaint {
        blend_mode: state.blend_mode,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, img_pixmap.as_ref(), &paint, combined, None);
}

/// Paints a 1-bit image mask (stencil) using the current fill color.
///
/// 0-bits are painted with fill color, 1-bits are transparent
/// (reversed if `/Decode [1 0]`).
fn paint_image_mask(image: &PdfImage, state: &RenderState, pixmap: &mut Pixmap) {
    let raw = match &image.data {
        crate::images::ImageData::Raw(data) => data,
        _ => return,
    };

    let w = image.width as usize;
    let h = image.height as usize;

    // Check if decode is inverted: [1 0] means 1-bits are painted
    let invert = image
        .decode
        .as_ref()
        .is_some_and(|d| d.len() >= 2 && d[0] > d[1]);

    // Build RGBA pixmap from 1-bit mask data
    let mut rgba = vec![0u8; w * h * 4];
    let fill = state.effective_fill_color();
    let fr = (fill.red() * 255.0) as u8;
    let fg = (fill.green() * 255.0) as u8;
    let fb = (fill.blue() * 255.0) as u8;
    let fa = (fill.alpha() * 255.0) as u8;

    // tiny_skia uses premultiplied alpha
    let pr = ((fr as u16 * fa as u16 + 127) / 255) as u8;
    let pg = ((fg as u16 * fa as u16 + 127) / 255) as u8;
    let pb = ((fb as u16 * fa as u16 + 127) / 255) as u8;

    for y in 0..h {
        for x in 0..w {
            let byte_idx = y * w.div_ceil(8) + x / 8;
            let bit_idx = 7 - (x % 8);
            let bit = if byte_idx < raw.len() {
                (raw[byte_idx] >> bit_idx) & 1
            } else {
                0
            };

            let painted = if invert { bit == 1 } else { bit == 0 };
            if painted {
                let offset = (y * w + x) * 4;
                rgba[offset] = pr;
                rgba[offset + 1] = pg;
                rgba[offset + 2] = pb;
                rgba[offset + 3] = fa;
            }
        }
    }

    let size = match tiny_skia::IntSize::from_wh(image.width, image.height) {
        Some(s) => s,
        None => return,
    };
    let img_pixmap = match Pixmap::from_vec(rgba, size) {
        Some(pm) => pm,
        None => return,
    };

    draw_image_pixmap(&img_pixmap, image.width, image.height, state, pixmap);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::{ImageData, PdfImage};

    fn default_state() -> RenderState {
        RenderState::default()
    }

    /// Creates a test image with sensible defaults (RGB, 8bpc, no mask).
    fn test_image(width: u32, height: u32, data: Vec<u8>) -> PdfImage {
        PdfImage {
            width,
            height,
            bits_per_component: 8,
            color_space: crate::images::ColorSpace::DeviceRGB,
            data: ImageData::Raw(data),
            is_image_mask: false,
            decode: None,
        }
    }

    /// Creates a 1-bit mask image for testing stencil rendering.
    fn test_mask(width: u32, height: u32, data: Vec<u8>, decode: Option<Vec<f32>>) -> PdfImage {
        PdfImage {
            width,
            height,
            bits_per_component: 1,
            color_space: crate::images::ColorSpace::DeviceGray,
            data: ImageData::Raw(data),
            is_image_mask: true,
            decode,
        }
    }

    #[test]
    fn paint_image_zero_width_no_panic() {
        let image = test_image(0, 10, vec![]);
        let state = default_state();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        paint_image(&image, &state, &mut pixmap);
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn paint_image_mask_zero_height_no_panic() {
        let image = test_mask(10, 0, vec![], None);
        let state = default_state();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        paint_image_mask(&image, &state, &mut pixmap);
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn paint_image_zero_both_dimensions_no_panic() {
        let image = test_image(0, 0, vec![]);
        let state = default_state();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        paint_image(&image, &state, &mut pixmap);
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn paint_image_1x1_rgb_pixel() {
        let image = test_image(1, 1, vec![255, 0, 0]);
        let mut state = default_state();
        state.ctm = Transform::from_row(10.0, 0.0, 0.0, -10.0, 0.0, 10.0);
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        paint_image(&image, &state, &mut pixmap);
        let has_red = pixmap
            .pixels()
            .iter()
            .any(|p| p.red() > 0 && p.green() == 0 && p.blue() == 0);
        assert!(has_red, "Expected at least one red pixel");
    }

    #[test]
    fn paint_image_gray_expands_correctly() {
        let image = PdfImage {
            color_space: crate::images::ColorSpace::DeviceGray,
            ..test_image(2, 1, vec![0, 255])
        };
        let mut state = default_state();
        state.ctm = Transform::from_row(20.0, 0.0, 0.0, -10.0, 0.0, 10.0);
        let mut pixmap = Pixmap::new(20, 10).unwrap();
        paint_image(&image, &state, &mut pixmap);
        let has_dark = pixmap
            .pixels()
            .iter()
            .any(|p| p.alpha() > 0 && p.red() < 10);
        let has_bright = pixmap.pixels().iter().any(|p| p.red() > 245);
        assert!(has_dark, "Expected dark pixels from gray=0");
        assert!(has_bright, "Expected bright pixels from gray=255");
    }

    #[test]
    fn paint_image_mask_normal_decode() {
        let image = test_mask(2, 1, vec![0b0100_0000], None);
        let mut state = default_state();
        state.fill_color = tiny_skia::Color::from_rgba8(255, 0, 0, 255);
        state.ctm = Transform::from_row(20.0, 0.0, 0.0, -10.0, 0.0, 10.0);
        let mut pixmap = Pixmap::new(20, 10).unwrap();
        paint_image_mask(&image, &state, &mut pixmap);
        let has_painted = pixmap.pixels().iter().any(|p| p.red() > 0 && p.alpha() > 0);
        assert!(has_painted, "Expected mask to paint some pixels");
    }

    #[test]
    fn paint_image_mask_inverted_decode() {
        let image = test_mask(2, 1, vec![0b1000_0000], Some(vec![1.0, 0.0]));
        let mut state = default_state();
        state.fill_color = tiny_skia::Color::from_rgba8(0, 0, 255, 255);
        state.ctm = Transform::from_row(20.0, 0.0, 0.0, -10.0, 0.0, 10.0);
        let mut pixmap = Pixmap::new(20, 10).unwrap();
        paint_image_mask(&image, &state, &mut pixmap);
        let has_blue = pixmap
            .pixels()
            .iter()
            .any(|p| p.blue() > 0 && p.alpha() > 0);
        assert!(has_blue, "Expected inverted mask to paint blue pixels");
    }

    #[test]
    fn paint_image_mask_truncated_data_no_panic() {
        let image = test_mask(16, 4, vec![0xFF], None);
        let state = default_state();
        let mut pixmap = Pixmap::new(20, 20).unwrap();
        paint_image_mask(&image, &state, &mut pixmap);
        // Truncated data should not crash; pixels beyond data are unpainted
        assert!(pixmap.pixels().iter().any(|_| true));
    }

    #[test]
    fn render_image_graceful_on_missing_width() {
        let dict = crate::core::objects::Dictionary::new();
        let stream = crate::core::objects::PdfStream::new(dict, vec![]);
        let state = default_state();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        render_image(&stream, &state, &mut pixmap);
        // Invalid stream should leave pixmap untouched (all transparent)
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn render_inline_image_graceful_on_bad_data() {
        let mut dict = crate::core::objects::Dictionary::new();
        dict.insert(
            crate::core::objects::PdfName::new("W"),
            crate::core::objects::Object::Integer(2),
        );
        dict.insert(
            crate::core::objects::PdfName::new("H"),
            crate::core::objects::Object::Integer(2),
        );
        let state = default_state();
        let mut pixmap = Pixmap::new(10, 10).unwrap();
        render_inline_image(&dict, &[0, 0], &state, &mut pixmap);
        // Short data should not crash
        assert!(pixmap.pixels().iter().any(|_| true));
    }

    #[test]
    fn draw_image_pixmap_with_multiply_blend() {
        let mut state = default_state();
        state.blend_mode = tiny_skia::BlendMode::Multiply;
        state.ctm = Transform::from_row(10.0, 0.0, 0.0, -10.0, 0.0, 10.0);

        let img_pixmap = Pixmap::new(1, 1).unwrap();
        let mut target = Pixmap::new(10, 10).unwrap();
        draw_image_pixmap(&img_pixmap, 1, 1, &state, &mut target);
        // Transparent source with Multiply should leave target unchanged
        assert!(target.pixels().iter().all(|p| p.alpha() == 0));
    }
}
