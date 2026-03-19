//! Post-processing for PaddleOCR model outputs.
//!
//! DBNet probability map → bounding boxes, and CTC logits → text.

/// A detected text bounding box from the DBNet detection model.
#[derive(Debug, Clone)]
pub struct TextBox {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Detection confidence (0.0–1.0).
    pub score: f32,
}

/// Extracts text bounding boxes from a DBNet probability map.
///
/// The probability map has shape [H, W] with values in [0, 1] indicating
/// text presence. Pixels above `threshold` are grouped into connected
/// regions, and their bounding boxes are returned.
pub fn extract_text_boxes(
    prob_map: &[f32],
    map_h: usize,
    map_w: usize,
    threshold: f32,
    min_area: u32,
) -> Vec<TextBox> {
    // Binary threshold
    let binary: Vec<bool> = prob_map.iter().map(|&v| v > threshold).collect();

    // Simple connected component labeling (two-pass)
    let mut labels = vec![0u32; map_h * map_w];
    let mut next_label = 1u32;
    let mut equivalences: Vec<u32> = vec![0]; // union-find parent

    // First pass: assign provisional labels
    for y in 0..map_h {
        for x in 0..map_w {
            let idx = y * map_w + x;
            if !binary[idx] {
                continue;
            }

            let left = if x > 0 { labels[idx - 1] } else { 0 };
            let above = if y > 0 { labels[idx - map_w] } else { 0 };

            match (left > 0, above > 0) {
                (false, false) => {
                    labels[idx] = next_label;
                    equivalences.push(next_label);
                    next_label += 1;
                }
                (true, false) => labels[idx] = left,
                (false, true) => labels[idx] = above,
                (true, true) => {
                    let min_l = left.min(above);
                    labels[idx] = min_l;
                    // Union
                    let max_l = left.max(above);
                    let root_min = find_root(&mut equivalences, min_l);
                    let root_max = find_root(&mut equivalences, max_l);
                    if root_min != root_max {
                        equivalences[root_max as usize] = root_min;
                    }
                }
            }
        }
    }

    // Second pass: flatten labels and compute bounding boxes
    struct BBoxAccum {
        min_x: u32,
        min_y: u32,
        max_x: u32,
        max_y: u32,
        prob_sum: f32,
        count: u32,
    }

    let mut boxes: std::collections::HashMap<u32, BBoxAccum> =
        std::collections::HashMap::new();

    for y in 0..map_h {
        for x in 0..map_w {
            let idx = y * map_w + x;
            let label = labels[idx];
            if label == 0 {
                continue;
            }
            let root = find_root(&mut equivalences, label);
            let px = x as u32;
            let py = y as u32;
            let entry = boxes.entry(root).or_insert(BBoxAccum {
                min_x: px,
                min_y: py,
                max_x: px,
                max_y: py,
                prob_sum: 0.0,
                count: 0,
            });
            entry.min_x = entry.min_x.min(px);
            entry.min_y = entry.min_y.min(py);
            entry.max_x = entry.max_x.max(px);
            entry.max_y = entry.max_y.max(py);
            entry.prob_sum += prob_map[idx];
            entry.count += 1;
        }
    }

    boxes
        .values()
        .filter_map(|b| {
            let w = b.max_x - b.min_x + 1;
            let h = b.max_y - b.min_y + 1;
            if w * h < min_area {
                return None;
            }
            Some(TextBox {
                x: b.min_x,
                y: b.min_y,
                width: w,
                height: h,
                score: b.prob_sum / b.count as f32,
            })
        })
        .collect()
}

fn find_root(equivalences: &mut [u32], mut label: u32) -> u32 {
    // Path compression: flatten chain so future lookups are O(1)
    while equivalences[label as usize] != label {
        equivalences[label as usize] = equivalences[equivalences[label as usize] as usize];
        label = equivalences[label as usize];
    }
    label
}

/// Greedy CTC decoding of recognition model output.
///
/// Takes logits of shape [sequence_length, num_chars] and returns
/// decoded text with confidence. Index 0 is the CTC blank token.
pub fn ctc_greedy_decode(
    logits: &[f32],
    seq_len: usize,
    num_chars: usize,
    dictionary: &[&str],
) -> (String, f32) {
    let mut result = String::new();
    let mut total_conf = 0.0f32;
    let mut count = 0u32;
    let mut last_idx: i32 = -1;

    for t in 0..seq_len {
        let offset = t * num_chars;
        if offset + num_chars > logits.len() {
            break;
        }

        // Argmax
        let mut best_idx = 0usize;
        let mut best_val = logits[offset];
        for c in 1..num_chars {
            if logits[offset + c] > best_val {
                best_val = logits[offset + c];
                best_idx = c;
            }
        }

        // Softmax confidence for the best character
        let max_logit = best_val;
        let sum_exp: f32 = logits[offset..offset + num_chars]
            .iter()
            .map(|&v| (v - max_logit).exp())
            .sum();
        let confidence = 1.0 / sum_exp; // exp(0) / sum = 1/sum

        // Skip blank (0) and repeated characters
        if best_idx != 0 && best_idx as i32 != last_idx && best_idx <= dictionary.len() {
            // dictionary is 0-indexed but CTC blank occupies index 0
            result.push_str(dictionary[best_idx - 1]);
            total_conf += confidence;
            count += 1;
        }
        last_idx = best_idx as i32;
    }

    let avg_conf = if count > 0 {
        total_conf / count as f32
    } else {
        0.0
    };
    (result, avg_conf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_boxes_from_simple_map() {
        // 10x10 probability map with a bright rectangle at (2,2)-(5,5)
        let mut prob = vec![0.0f32; 100];
        for y in 2..=5 {
            for x in 2..=5 {
                prob[y * 10 + x] = 0.8;
            }
        }

        let boxes = extract_text_boxes(&prob, 10, 10, 0.3, 1);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].x, 2);
        assert_eq!(boxes[0].y, 2);
        assert_eq!(boxes[0].width, 4);
        assert_eq!(boxes[0].height, 4);
    }

    #[test]
    fn extract_boxes_filters_small() {
        let mut prob = vec![0.0f32; 100];
        prob[55] = 0.9; // single pixel
        let boxes = extract_text_boxes(&prob, 10, 10, 0.3, 10);
        assert!(
            boxes.is_empty(),
            "Single pixel should be filtered by min_area=10"
        );
    }

    #[test]
    fn extract_boxes_two_regions() {
        let mut prob = vec![0.0f32; 200]; // 10x20
                                          // Region 1: x=1..3, y=1..3
        for y in 1..=3 {
            for x in 1..=3 {
                prob[y * 20 + x] = 0.9;
            }
        }
        // Region 2: x=15..18, y=1..3 (well separated)
        for y in 1..=3 {
            for x in 15..=18 {
                prob[y * 20 + x] = 0.8;
            }
        }
        let boxes = extract_text_boxes(&prob, 10, 20, 0.3, 1);
        assert_eq!(boxes.len(), 2, "Should find two separate regions");
    }

    #[test]
    fn ctc_decode_simple() {
        // Dictionary: ["h", "e", "l", "o"] (indices 1-4, 0=blank)
        let dict = vec!["h", "e", "l", "l", "o"];
        // Logits: 5 chars, 6 timesteps
        // Sequence: blank, h, e, l, l, o (with repeats collapsed)
        let mut logits = vec![0.0f32; 6 * 6]; // 6 timesteps × 6 classes
                                              // t=0: blank (idx 0 highest)
        logits[0] = 10.0;
        // t=1: h (idx 1)
        logits[1 * 6 + 1] = 10.0;
        // t=2: e (idx 2)
        logits[2 * 6 + 2] = 10.0;
        // t=3: l (idx 3)
        logits[3 * 6 + 3] = 10.0;
        // t=4: blank (separate the two l's)
        logits[4 * 6 + 0] = 10.0;
        // t=5: l (idx 3 again, but separated by blank so not collapsed)
        logits[5 * 6 + 3] = 10.0;

        let (text, _conf) = ctc_greedy_decode(&logits, 6, 6, &dict);
        assert_eq!(text, "hell");
    }

    #[test]
    fn ctc_decode_empty_logits() {
        let dict: Vec<&str> = vec!["a"];
        let (text, conf) = ctc_greedy_decode(&[], 0, 2, &dict);
        assert!(text.is_empty());
        assert_eq!(conf, 0.0);
    }

    #[test]
    fn ctc_decode_all_blanks() {
        let dict = vec!["a", "b"];
        let logits = vec![10.0, 0.0, 0.0, 10.0, 0.0, 0.0]; // 2 timesteps, all blank
        let (text, _) = ctc_greedy_decode(&logits, 2, 3, &dict);
        assert!(text.is_empty());
    }
}
