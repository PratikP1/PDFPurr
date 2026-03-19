//! Multi-column reading order analysis for OCR output.
//!
//! Uses the XY-Cut algorithm to detect columns and establish correct
//! reading order from OCR word bounding boxes.

use super::engine::OcrWord;

/// A rectangular region on the page containing a group of words.
#[derive(Debug, Clone)]
pub struct TextRegion {
    /// Word indices in the original OcrResult.words array.
    pub word_indices: Vec<usize>,
    /// Bounding box: (x, y, width, height) in pixel coordinates.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Region width in pixels.
    pub width: u32,
    /// Region height in pixels.
    pub height: u32,
}

/// Minimum gap (in pixels) to consider as a column separator.
const MIN_COLUMN_GAP: u32 = 40;

/// Maximum recursion depth for XY-Cut.
const MAX_DEPTH: usize = 10;

/// Analyzes OCR words and groups them into reading-order regions.
///
/// Uses the XY-Cut algorithm: recursively split along the largest
/// horizontal or vertical whitespace gap. Columns are read top-to-bottom,
/// left-to-right (for LTR documents).
pub fn detect_reading_order(words: &[OcrWord]) -> Vec<TextRegion> {
    if words.is_empty() {
        return Vec::new();
    }

    let indices: Vec<usize> = (0..words.len()).collect();
    let mut regions = Vec::new();
    xy_cut(words, &indices, &mut regions, 0);
    regions
}

fn xy_cut(words: &[OcrWord], indices: &[usize], regions: &mut Vec<TextRegion>, depth: usize) {
    if indices.is_empty() || depth >= MAX_DEPTH {
        return;
    }

    // Compute bounding box of this group (single pass)
    let (min_x, min_y, max_x, max_y) = indices.iter().fold(
        (u32::MAX, u32::MAX, 0u32, 0u32),
        |(mn_x, mn_y, mx_x, mx_y), &i| {
            let w = &words[i];
            (
                mn_x.min(w.x),
                mn_y.min(w.y),
                mx_x.max(w.x + w.width),
                mx_y.max(w.y + w.height),
            )
        },
    );

    // Try vertical split (detect columns) — find widest horizontal gap
    let v_split = find_widest_gap(words, indices, min_x, max_x, |w| (w.x, w.x + w.width));

    // Try horizontal split — find widest vertical gap
    let h_split = find_widest_gap(words, indices, min_y, max_y, |w| (w.y, w.y + w.height));

    // Choose the larger gap
    let (v_gap, v_pos) = v_split.unwrap_or((0, 0));
    let (h_gap, h_pos) = h_split.unwrap_or((0, 0));

    if v_gap >= MIN_COLUMN_GAP && v_gap >= h_gap {
        // Split vertically (left column, then right column)
        let (left, right): (Vec<usize>, Vec<usize>) = indices
            .iter()
            .partition(|&&i| words[i].x + words[i].width / 2 < v_pos);
        xy_cut(words, &left, regions, depth + 1);
        xy_cut(words, &right, regions, depth + 1);
    } else if h_gap >= MIN_COLUMN_GAP {
        // Split horizontally (top section, then bottom section)
        let (top, bottom): (Vec<usize>, Vec<usize>) = indices
            .iter()
            .partition(|&&i| words[i].y + words[i].height / 2 < h_pos);
        xy_cut(words, &top, regions, depth + 1);
        xy_cut(words, &bottom, regions, depth + 1);
    } else {
        // No significant gap — this is a leaf region
        // Sort words by Y then X for reading order within the region
        let mut sorted = indices.to_vec();
        sorted.sort_by(|&a, &b| {
            let ya = words[a].y;
            let yb = words[b].y;
            if ya.abs_diff(yb) <= words[a].height / 2 {
                // Same line — sort by X
                words[a].x.cmp(&words[b].x)
            } else {
                ya.cmp(&yb)
            }
        });

        regions.push(TextRegion {
            word_indices: sorted,
            x: min_x,
            y: min_y,
            width: max_x.saturating_sub(min_x),
            height: max_y.saturating_sub(min_y),
        });
    }
}

/// Finds the widest gap along a given axis.
///
/// `edge_fn` extracts (start, end) interval from each word along the axis.
/// Returns `(gap_width, split_position)` at the midpoint of the widest gap.
fn find_widest_gap(
    words: &[OcrWord],
    indices: &[usize],
    min_val: u32,
    max_val: u32,
    edge_fn: fn(&OcrWord) -> (u32, u32),
) -> Option<(u32, u32)> {
    if max_val <= min_val {
        return None;
    }

    let mut edges: Vec<(u32, u32)> = indices.iter().map(|&i| edge_fn(&words[i])).collect();
    edges.sort_by_key(|&(start, _)| start);

    let mut best_gap = 0u32;
    let mut best_pos = 0u32;
    let mut max_end = edges[0].1;

    for &(start, end) in &edges[1..] {
        if start > max_end {
            let gap = start - max_end;
            if gap > best_gap {
                best_gap = gap;
                best_pos = max_end + gap / 2;
            }
        }
        max_end = max_end.max(end);
    }

    if best_gap > 0 {
        Some((best_gap, best_pos))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: u32, y: u32, w: u32, h: u32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            x,
            y,
            width: w,
            height: h,
            confidence: 0.9,
        }
    }

    #[test]
    fn single_column_stays_together() {
        let words = vec![
            word("Line", 100, 100, 80, 20),
            word("one", 200, 100, 60, 20),
            word("Line", 100, 130, 80, 20),
            word("two", 200, 130, 60, 20),
        ];
        let regions = detect_reading_order(&words);
        assert_eq!(regions.len(), 1, "Single column should produce one region");
        assert_eq!(regions[0].word_indices.len(), 4);
    }

    #[test]
    fn two_columns_split_correctly() {
        let words = vec![
            // Left column
            word("Left", 50, 100, 80, 20),
            word("col", 50, 130, 60, 20),
            // Right column (large gap at x=400)
            word("Right", 450, 100, 90, 20),
            word("col", 450, 130, 60, 20),
        ];
        let regions = detect_reading_order(&words);
        assert_eq!(regions.len(), 2, "Should detect two columns");
        // First region should be the left column
        assert!(regions[0].word_indices.iter().all(|&i| words[i].x < 200));
        // Second region should be the right column
        assert!(regions[1].word_indices.iter().all(|&i| words[i].x > 400));
    }

    #[test]
    fn empty_input() {
        let regions = detect_reading_order(&[]);
        assert!(regions.is_empty());
    }

    #[test]
    fn reading_order_within_region_is_top_to_bottom_left_to_right() {
        // Words close together (gaps < MIN_COLUMN_GAP) on two lines
        let words = vec![
            word("C", 120, 100, 40, 20), // line 1, right
            word("A", 50, 100, 40, 20),  // line 1, left
            word("B", 92, 100, 26, 20),  // line 1, middle (no gap > 40px)
            word("D", 50, 130, 40, 20),  // line 2, left
        ];
        let regions = detect_reading_order(&words);
        assert_eq!(regions.len(), 1);

        let ordered: Vec<&str> = regions[0]
            .word_indices
            .iter()
            .map(|&i| words[i].text.as_str())
            .collect();
        assert_eq!(ordered, vec!["A", "B", "C", "D"]);
    }
}
