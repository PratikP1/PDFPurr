//! Image preprocessing for PaddleOCR models.
//!
//! Converts grayscale images to normalized RGB CHW tensors.

#[cfg(feature = "ocr-paddle")]
use tract_onnx::prelude::*;

use super::engine::OcrImage;

/// PaddleOCR ImageNet normalization mean (per channel).
pub const NORMALIZE_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
/// PaddleOCR ImageNet normalization std (per channel).
pub const NORMALIZE_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Normalizes a pixel value: `(value/255 - mean) / std`.
pub fn normalize_pixel(value: u8, channel: usize) -> f32 {
    (value as f32 / 255.0 - NORMALIZE_MEAN[channel]) / NORMALIZE_STD[channel]
}

/// Converts a grayscale OcrImage to a normalized [1,3,H,W] f32 tensor.
///
/// Grayscale is replicated to 3 RGB channels and normalized with
/// ImageNet mean/std as PaddleOCR expects.
#[cfg(feature = "ocr-paddle")]
pub fn grayscale_to_chw_tensor(image: &OcrImage, target_h: usize, target_w: usize) -> Tensor {
    let h = image.height as usize;
    let w = image.width as usize;

    // Simple nearest-neighbor resize
    let mut data = vec![0.0f32; 3 * target_h * target_w];
    for ty in 0..target_h {
        for tx in 0..target_w {
            let sy = (ty * h / target_h).min(h - 1);
            let sx = (tx * w / target_w).min(w - 1);
            let gray = image.data[sy * w + sx];

            for c in 0..3 {
                let idx = c * target_h * target_w + ty * target_w + tx;
                data[idx] = normalize_pixel(gray, c);
            }
        }
    }

    tract_ndarray::Array4::from_shape_vec((1, 3, target_h, target_w), data)
        .unwrap()
        .into()
}

/// PaddleOCR recognition model input height (fixed by model architecture).
const REC_MODEL_HEIGHT: usize = 48;
/// Maximum recognition input width to prevent excessive memory usage.
const MAX_REC_WIDTH: usize = 960;
/// Padding value for out-of-bounds pixels (white).
const WHITE_PADDING: u8 = 255;

/// Resizes and normalizes a cropped region for the recognition model.
///
/// Target height is always [`REC_MODEL_HEIGHT`] (PaddleOCR rec model input).
/// Width is proportional to maintain aspect ratio, clamped to [`MAX_REC_WIDTH`].
#[cfg(feature = "ocr-paddle")]
pub fn prepare_recognition_input(image: &OcrImage, x: u32, y: u32, w: u32, h: u32) -> Tensor {
    let target_h = REC_MODEL_HEIGHT;
    let aspect = w as f32 / h.max(1) as f32;
    let target_w = ((target_h as f32 * aspect).round() as usize).clamp(1, MAX_REC_WIDTH);

    let img_w = image.width as usize;
    let img_h = image.height as usize;

    let mut data = vec![0.0f32; 3 * target_h * target_w];
    for ty in 0..target_h {
        for tx in 0..target_w {
            let sy = (y as usize + ty * h as usize / target_h).min(img_h.saturating_sub(1));
            let sx = (x as usize + tx * w as usize / target_w).min(img_w.saturating_sub(1));
            let gray = if sy < img_h && sx < img_w {
                image.data[sy * img_w + sx]
            } else {
                WHITE_PADDING
            };

            for c in 0..3 {
                let idx = c * target_h * target_w + ty * target_w + tx;
                data[idx] = normalize_pixel(gray, c);
            }
        }
    }

    tract_ndarray::Array4::from_shape_vec((1, 3, target_h, target_w), data)
        .unwrap()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_pixel_black() {
        // Black (0): (0/255 - 0.485) / 0.229 ≈ -2.117
        let val = normalize_pixel(0, 0);
        assert!((val - (-0.485 / 0.229)).abs() < 0.01);
    }

    #[test]
    fn normalize_pixel_white() {
        // White (255): (1.0 - 0.485) / 0.229 ≈ 2.249
        let val = normalize_pixel(255, 0);
        assert!((val - (0.515 / 0.229)).abs() < 0.01);
    }

    #[test]
    fn normalize_pixel_midgray() {
        // Mid (128): (128/255 - 0.485) / 0.229
        let val = normalize_pixel(128, 0);
        let expected = (128.0 / 255.0 - 0.485) / 0.229;
        assert!((val - expected).abs() < 0.001);
    }

    #[test]
    fn normalize_each_channel_differs() {
        // Each channel has different mean/std
        let r = normalize_pixel(128, 0);
        let g = normalize_pixel(128, 1);
        let b = normalize_pixel(128, 2);
        assert_ne!(r, g);
        assert_ne!(g, b);
    }
}
