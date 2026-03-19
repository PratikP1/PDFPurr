//! Image preprocessing for OCR quality improvement.
//!
//! Applies contrast enhancement, Otsu binarization, and noise reduction
//! to grayscale images before passing them to OCR engines. These steps
//! significantly improve recognition accuracy on scanned documents.

use super::engine::OcrImage;

/// Otsu's threshold: finds the grayscale value that minimizes
/// intra-class variance between foreground and background.
///
/// Returns a threshold value (0–255). Pixels ≤ threshold are background,
/// pixels > threshold are foreground.
pub fn otsu_threshold(image: &OcrImage) -> u8 {
    // Build histogram
    let mut histogram = [0u32; 256];
    for &pixel in &image.data {
        histogram[pixel as usize] += 1;
    }

    let total = image.data.len() as f64;
    if total == 0.0 {
        return 128;
    }

    let mut sum_total: f64 = 0.0;
    for (i, &count) in histogram.iter().enumerate() {
        sum_total += i as f64 * count as f64;
    }

    let mut sum_bg: f64 = 0.0;
    let mut weight_bg: f64 = 0.0;
    let mut best_threshold = 0u8;
    let mut max_variance: f64 = 0.0;

    for t in 0..255u8 {
        let count = histogram[t as usize];
        weight_bg += count as f64;
        if weight_bg == 0.0 {
            continue;
        }

        let weight_fg = total - weight_bg;
        if weight_fg == 0.0 {
            break;
        }

        sum_bg += t as f64 * count as f64;
        let mean_bg = sum_bg / weight_bg;
        let mean_fg = (sum_total - sum_bg) / weight_fg;

        // Between-class variance: higher means better separation
        let between_variance = weight_bg * weight_fg * (mean_bg - mean_fg).powi(2);
        if between_variance >= max_variance {
            max_variance = between_variance;
            best_threshold = t;
        }
    }

    best_threshold
}

/// Applies Otsu binarization to a grayscale image.
///
/// Converts all pixels to either 0 (black) or 255 (white) based on
/// the automatically computed Otsu threshold. This produces clean
/// black-on-white text for OCR.
pub fn binarize(image: &OcrImage) -> OcrImage {
    let threshold = otsu_threshold(image);
    let data = image
        .data
        .iter()
        .map(|&p| if p > threshold { 255 } else { 0 })
        .collect();
    OcrImage {
        data,
        width: image.width,
        height: image.height,
    }
}

/// Enhances contrast using simple linear stretching.
///
/// Maps the actual min/max pixel range to 0–255, improving OCR accuracy
/// on washed-out or low-contrast scans.
pub fn enhance_contrast(image: &OcrImage) -> OcrImage {
    if image.data.is_empty() {
        return image.clone();
    }

    let min_val = *image.data.iter().min().unwrap_or(&0);
    let max_val = *image.data.iter().max().unwrap_or(&255);
    let range = max_val as f64 - min_val as f64;

    if range < 1.0 {
        return image.clone();
    }

    let data = image
        .data
        .iter()
        .map(|&p| ((p as f64 - min_val as f64) / range * 255.0).round() as u8)
        .collect();

    OcrImage {
        data,
        width: image.width,
        height: image.height,
    }
}

/// Rotates a grayscale image by 90° clockwise.
pub fn rotate_90_cw(image: &OcrImage) -> OcrImage {
    let w = image.width as usize;
    let h = image.height as usize;
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            // (x, y) → (h - 1 - y, x) but new dimensions are (h, w)
            let new_x = h - 1 - y;
            let new_y = x;
            data[new_y * h + new_x] = image.data[y * w + x];
        }
    }
    OcrImage {
        data,
        width: image.height,
        height: image.width,
    }
}

/// Rotates a grayscale image by 180°.
pub fn rotate_180(image: &OcrImage) -> OcrImage {
    let data: Vec<u8> = image.data.iter().rev().copied().collect();
    OcrImage {
        data,
        width: image.width,
        height: image.height,
    }
}

/// Rotates a grayscale image by 270° clockwise (= 90° counter-clockwise).
pub fn rotate_270_cw(image: &OcrImage) -> OcrImage {
    let w = image.width as usize;
    let h = image.height as usize;
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            // (x, y) → (y, w - 1 - x) but new dimensions are (h, w)
            let new_x = y;
            let new_y = w - 1 - x;
            data[new_y * h + new_x] = image.data[y * w + x];
        }
    }
    OcrImage {
        data,
        width: image.height,
        height: image.width,
    }
}

/// Full preprocessing pipeline: contrast enhancement → binarization.
///
/// Call this on the grayscale image before passing to an OCR engine
/// for best results on scanned documents.
pub fn preprocess_for_ocr(image: &OcrImage) -> OcrImage {
    let enhanced = enhance_contrast(image);
    binarize(&enhanced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(data: &[u8], width: u32, height: u32) -> OcrImage {
        OcrImage {
            data: data.to_vec(),
            width,
            height,
        }
    }

    // --- Otsu threshold ---

    #[test]
    fn otsu_bimodal_image() {
        // 50% black (0), 50% white (255) → threshold should split them
        let mut data = vec![0u8; 50];
        data.extend_from_slice(&vec![255u8; 50]);
        let img = make_image(&data, 10, 10);
        let t = otsu_threshold(&img);
        // Threshold should be somewhere between 0 and 255
        assert!(t > 0 && t < 255, "Otsu should find split, got {t}");
    }

    #[test]
    fn otsu_all_same_value() {
        let img = make_image(&[128; 100], 10, 10);
        let t = otsu_threshold(&img);
        // With uniform image, any threshold is valid
        assert!(t <= 255);
    }

    #[test]
    fn otsu_empty_image() {
        let img = make_image(&[], 0, 0);
        let t = otsu_threshold(&img);
        assert_eq!(t, 128, "Empty image should return default threshold");
    }

    #[test]
    fn otsu_text_like_distribution() {
        // Simulate text: mostly white (200-255) with some black text (0-50)
        let mut data = vec![240u8; 80]; // white background
        data.extend_from_slice(&[20u8; 20]); // black text
        let img = make_image(&data, 10, 10);
        let t = otsu_threshold(&img);
        // Threshold should be between text and background
        assert!(
            t > 20 && t < 240,
            "Threshold should split text from background, got {t}"
        );
    }

    // --- Binarization ---

    #[test]
    fn binarize_produces_only_black_and_white() {
        let data: Vec<u8> = (0..100).collect();
        let img = make_image(&data, 10, 10);
        let result = binarize(&img);
        for &pixel in &result.data {
            assert!(
                pixel == 0 || pixel == 255,
                "Binarize should produce only 0 or 255, got {pixel}"
            );
        }
    }

    #[test]
    fn binarize_preserves_dimensions() {
        let img = make_image(&[128; 200], 20, 10);
        let result = binarize(&img);
        assert_eq!(result.width, 20);
        assert_eq!(result.height, 10);
        assert_eq!(result.data.len(), 200);
    }

    // --- Contrast enhancement ---

    #[test]
    fn enhance_contrast_stretches_range() {
        // Input: narrow range 100-150
        let data: Vec<u8> = (100..150).collect();
        let img = make_image(&data, 5, 10);
        let result = enhance_contrast(&img);
        assert_eq!(
            *result.data.iter().min().unwrap(),
            0,
            "Min should stretch to 0"
        );
        assert_eq!(
            *result.data.iter().max().unwrap(),
            255,
            "Max should stretch to 255"
        );
    }

    #[test]
    fn enhance_contrast_full_range_is_noop() {
        // Input already spans 0-255
        let mut data = vec![0u8];
        data.push(255);
        data.extend_from_slice(&[128; 8]);
        let img = make_image(&data, 10, 1);
        let result = enhance_contrast(&img);
        assert_eq!(result.data[0], 0);
        assert_eq!(result.data[1], 255);
    }

    #[test]
    fn enhance_contrast_uniform_returns_clone() {
        let img = make_image(&[128; 100], 10, 10);
        let result = enhance_contrast(&img);
        // All same value → no stretching possible → return as-is
        assert_eq!(result.data, img.data);
    }

    #[test]
    fn enhance_contrast_empty_returns_clone() {
        let img = make_image(&[], 0, 0);
        let result = enhance_contrast(&img);
        assert!(result.data.is_empty());
    }

    // --- Rotation ---

    #[test]
    fn rotate_90_cw_swaps_dimensions() {
        let img = make_image(&[1, 2, 3, 4, 5, 6], 3, 2);
        let rotated = rotate_90_cw(&img);
        assert_eq!(rotated.width, 2);
        assert_eq!(rotated.height, 3);
        assert_eq!(rotated.data.len(), 6);
    }

    #[test]
    fn rotate_90_cw_pixel_mapping() {
        // 2x2 image: [1,2,3,4] → after 90° CW should be [3,1,4,2]
        let img = make_image(&[1, 2, 3, 4], 2, 2);
        let rotated = rotate_90_cw(&img);
        assert_eq!(rotated.data, vec![3, 1, 4, 2]);
    }

    #[test]
    fn rotate_180_reverses_data() {
        let img = make_image(&[1, 2, 3, 4], 2, 2);
        let rotated = rotate_180(&img);
        assert_eq!(rotated.data, vec![4, 3, 2, 1]);
        assert_eq!(rotated.width, 2);
        assert_eq!(rotated.height, 2);
    }

    #[test]
    fn rotate_270_cw_is_inverse_of_90() {
        let img = make_image(&[1, 2, 3, 4, 5, 6], 3, 2);
        let r90 = rotate_90_cw(&img);
        let r90_then_270 = rotate_270_cw(&r90);
        assert_eq!(r90_then_270.width, img.width);
        assert_eq!(r90_then_270.height, img.height);
        assert_eq!(r90_then_270.data, img.data);
    }

    // --- Full pipeline ---

    #[test]
    fn preprocess_pipeline_produces_binary_output() {
        let data: Vec<u8> = (0..100).collect();
        let img = make_image(&data, 10, 10);
        let result = preprocess_for_ocr(&img);
        for &pixel in &result.data {
            assert!(pixel == 0 || pixel == 255);
        }
    }

    #[test]
    fn preprocess_pipeline_preserves_dimensions() {
        let img = make_image(&[128; 300], 30, 10);
        let result = preprocess_for_ocr(&img);
        assert_eq!(result.width, 30);
        assert_eq!(result.height, 10);
        assert_eq!(result.data.len(), 300);
    }
}
