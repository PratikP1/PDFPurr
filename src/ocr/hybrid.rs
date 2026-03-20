//! Hybrid OCR + text comparison for accessibility.
//!
//! Compares text extracted from the PDF content stream against OCR
//! output from rendering. When they disagree, presents both results
//! to screen readers so no information is lost.

use crate::content::analysis::TextRun;
use crate::ocr::engine::OcrResult;

/// Result of comparing content stream text against OCR text.
#[derive(Debug, Clone, PartialEq)]
pub enum TextSource {
    /// Content stream text is reliable (matches OCR or OCR unavailable).
    ContentStream,
    /// OCR text is better (content stream is garbled or empty).
    Ocr,
    /// Both sources disagree — present both for human review.
    Both,
    /// Neither source produced usable text.
    Neither,
}

/// Comparison result for a single page region.
#[derive(Debug, Clone)]
pub struct HybridResult {
    /// Which text source is recommended.
    pub source: TextSource,
    /// Text from the content stream.
    pub stream_text: String,
    /// Text from OCR.
    pub ocr_text: String,
    /// Similarity score (0.0 = completely different, 1.0 = identical).
    pub similarity: f64,
    /// Combined text for screen reader presentation.
    pub accessible_text: String,
}

/// Compares extracted text runs against OCR words for a page.
///
/// When the content stream text and OCR text agree (similarity > 0.8),
/// uses the content stream text (it preserves exact character codes).
/// When they disagree, combines both for screen reader presentation
/// so that the user gets the best available interpretation.
pub fn compare_text_sources(runs: &[TextRun], ocr_result: &OcrResult) -> HybridResult {
    let stream_text: String = runs
        .iter()
        .filter(|r| r.rendering_mode != 3) // Exclude invisible OCR text
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let ocr_text: String = ocr_result
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let stream_clean = normalize_whitespace(&stream_text);
    let ocr_clean = normalize_whitespace(&ocr_text);

    if stream_clean.is_empty() && ocr_clean.is_empty() {
        return HybridResult {
            source: TextSource::Neither,
            stream_text,
            ocr_text,
            similarity: 0.0,
            accessible_text: String::new(),
        };
    }

    if stream_clean.is_empty() {
        return HybridResult {
            source: TextSource::Ocr,
            stream_text,
            ocr_text: ocr_text.clone(),
            similarity: 0.0,
            accessible_text: ocr_text,
        };
    }

    if ocr_clean.is_empty() {
        return HybridResult {
            source: TextSource::ContentStream,
            stream_text: stream_text.clone(),
            ocr_text,
            similarity: 0.0,
            accessible_text: stream_text,
        };
    }

    let similarity = text_similarity(&stream_clean, &ocr_clean);

    if similarity > 0.8 {
        // Good agreement — use content stream (better character codes)
        HybridResult {
            source: TextSource::ContentStream,
            stream_text: stream_text.clone(),
            ocr_text,
            similarity,
            accessible_text: stream_text,
        }
    } else if is_garbled(&stream_clean) && !is_garbled(&ocr_clean) {
        // Content stream is garbled, OCR is readable
        HybridResult {
            source: TextSource::Ocr,
            stream_text,
            ocr_text: ocr_text.clone(),
            similarity,
            accessible_text: ocr_text,
        }
    } else {
        // Disagreement — present both for human review.
        // Screen reader gets: "OCR reading: [ocr text]. Original text: [stream text]."
        let accessible_text = format!(
            "OCR reading: {}. Original text: {}.",
            ocr_clean, stream_clean
        );
        HybridResult {
            source: TextSource::Both,
            stream_text,
            ocr_text,
            similarity,
            accessible_text,
        }
    }
}

/// Normalizes whitespace: collapse runs of whitespace to single spaces.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Checks whether text appears to be garbled (high ratio of non-ASCII
/// or replacement characters).
fn is_garbled(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let total = text.chars().count();
    let bad = text
        .chars()
        .filter(|c| {
            *c == '\u{FFFD}' // Replacement character
                || (*c as u32 > 0x7F && !c.is_alphanumeric() && !c.is_whitespace())
                || *c == '\0'
        })
        .count();
    // More than 20% non-printable/replacement → garbled
    bad as f64 / total as f64 > 0.2
}

/// Simple character-level similarity using longest common subsequence ratio.
///
/// Returns 0.0 (completely different) to 1.0 (identical).
fn text_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    // Use word overlap for efficiency (exact LCS is O(n²))
    let a_words: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b.split_whitespace().collect();

    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::engine::OcrWord;

    fn make_runs(texts: &[&str]) -> Vec<TextRun> {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| TextRun {
                text: text.to_string(),
                font_name: "Helvetica".to_string(),
                font_size: 12.0,
                x: 100.0 + i as f64 * 50.0,
                y: 700.0,
                width: 50.0,
                height: 12.0,
                color: [0.0, 0.0, 0.0, 1.0],
                rendering_mode: 0,
                is_bold: false,
                is_italic: false,
                is_monospaced: false,
            })
            .collect()
    }

    fn make_ocr(texts: &[&str]) -> OcrResult {
        OcrResult {
            words: texts
                .iter()
                .enumerate()
                .map(|(i, text)| OcrWord {
                    text: text.to_string(),
                    x: 100 + i as u32 * 50,
                    y: 100,
                    width: 50,
                    height: 20,
                    confidence: 0.9,
                })
                .collect(),
            image_width: 612,
            image_height: 792,
        }
    }

    #[test]
    fn identical_text_uses_content_stream() {
        let runs = make_runs(&["Hello", "World"]);
        let ocr = make_ocr(&["Hello", "World"]);
        let result = compare_text_sources(&runs, &ocr);
        assert_eq!(result.source, TextSource::ContentStream);
        assert!(result.similarity > 0.8);
    }

    #[test]
    fn empty_stream_uses_ocr() {
        let runs: Vec<TextRun> = vec![];
        let ocr = make_ocr(&["Hello", "World"]);
        let result = compare_text_sources(&runs, &ocr);
        assert_eq!(result.source, TextSource::Ocr);
        assert!(result.accessible_text.contains("Hello"));
    }

    #[test]
    fn empty_ocr_uses_stream() {
        let runs = make_runs(&["Hello", "World"]);
        let ocr = OcrResult {
            words: vec![],
            image_width: 612,
            image_height: 792,
        };
        let result = compare_text_sources(&runs, &ocr);
        assert_eq!(result.source, TextSource::ContentStream);
    }

    #[test]
    fn both_empty_is_neither() {
        let result = compare_text_sources(
            &[],
            &OcrResult {
                words: vec![],
                image_width: 612,
                image_height: 792,
            },
        );
        assert_eq!(result.source, TextSource::Neither);
    }

    #[test]
    fn garbled_stream_uses_ocr() {
        let mut runs = make_runs(&["\u{FFFD}\u{FFFD}\u{FFFD}", "\u{FFFD}\u{FFFD}"]);
        // garbled content
        let ocr = make_ocr(&["Hello", "World"]);
        let result = compare_text_sources(&runs, &ocr);
        assert_eq!(result.source, TextSource::Ocr);
    }

    #[test]
    fn disagreement_presents_both() {
        let runs = make_runs(&["Completely", "different", "text"]);
        let ocr = make_ocr(&["Something", "else", "entirely", "unrelated"]);
        let result = compare_text_sources(&runs, &ocr);
        assert_eq!(result.source, TextSource::Both);
        assert!(
            result.accessible_text.contains("OCR reading:"),
            "Should present OCR text"
        );
        assert!(
            result.accessible_text.contains("Original text:"),
            "Should present stream text"
        );
    }

    #[test]
    fn invisible_ocr_text_excluded_from_stream() {
        let mut runs = make_runs(&["Visible"]);
        // Add invisible OCR overlay
        runs.push(TextRun {
            text: "OCR overlay".to_string(),
            font_name: "F_OCR".to_string(),
            font_size: 12.0,
            x: 100.0,
            y: 700.0,
            width: 50.0,
            height: 12.0,
            color: [0.0, 0.0, 0.0, 1.0],
            rendering_mode: 3, // invisible
            is_bold: false,
            is_italic: false,
            is_monospaced: false,
        });

        let ocr = make_ocr(&["Visible"]);
        let result = compare_text_sources(&runs, &ocr);
        // Should only compare "Visible" (not the invisible overlay)
        assert_eq!(result.source, TextSource::ContentStream);
        assert!(result.similarity > 0.8);
    }

    #[test]
    fn similarity_identical() {
        assert!((text_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn similarity_different() {
        assert!(text_similarity("hello world", "foo bar baz") < 0.3);
    }

    #[test]
    fn similarity_partial_overlap() {
        let sim = text_similarity("the quick brown fox", "the quick red fox");
        assert!(sim > 0.5 && sim < 1.0, "Partial overlap: {sim}");
    }

    #[test]
    fn garbled_detection() {
        assert!(is_garbled("\u{FFFD}\u{FFFD}\u{FFFD}abc"));
        assert!(!is_garbled("Hello World"));
        assert!(!is_garbled("")); // empty is not garbled
    }
}
