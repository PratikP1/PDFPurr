//! Structure detection from text runs.
//!
//! Classifies [`TextRun`] values into structured blocks — headings,
//! paragraphs, list items, table cells, code blocks — by analyzing
//! font metrics, positions, and visual patterns.

use super::analysis::TextRun;

/// Bounding rectangle in page coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Bottom edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// Semantic role of a detected text block.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockRole {
    /// Heading level 1–6 (H1 is most prominent).
    Heading(u8),
    /// Body paragraph.
    Paragraph,
    /// List item (bulleted or numbered).
    ListItem,
    /// Table cell.
    TableCell,
    /// Monospaced code block.
    Code,
    /// Role not yet classified.
    Unknown,
}

impl BlockRole {
    /// Maps to the PDF tagged structure role.
    /// Maps to the PDF tagged structure role.
    pub fn to_standard_role(&self) -> Option<crate::accessibility::StandardRole> {
        match self {
            BlockRole::Heading(n) => Some(crate::accessibility::StandardRole::H(*n)),
            BlockRole::Paragraph => Some(crate::accessibility::StandardRole::P),
            BlockRole::ListItem => Some(crate::accessibility::StandardRole::LI),
            BlockRole::Code => Some(crate::accessibility::StandardRole::P), // Code uses P with /ActualText
            BlockRole::TableCell => Some(crate::accessibility::StandardRole::TD),
            BlockRole::Unknown => None,
        }
    }
}

/// A group of text runs classified as a single structural block.
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// The text runs composing this block.
    pub runs: Vec<TextRun>,
    /// Detected semantic role.
    pub role: BlockRole,
    /// Bounding box in page coordinates.
    pub bbox: Rect,
    /// Left indent relative to page left margin.
    pub indent: f64,
}

/// A horizontal line of text (runs at approximately the same Y).
#[derive(Debug, Clone)]
pub struct TextLine {
    /// Indices into the original runs array.
    pub run_indices: Vec<usize>,
    /// Baseline Y coordinate.
    pub y: f64,
    /// Leftmost X position.
    pub min_x: f64,
    /// Rightmost X + width position.
    pub max_x: f64,
}

/// Y tolerance for grouping runs into the same line (in points).
const LINE_Y_TOLERANCE: f64 = 2.0;

/// Font statistics for the page body text.
#[derive(Debug, Clone)]
pub struct PageFontStats {
    /// Most common font size (the "body" size).
    pub body_font_size: f64,
    /// Most common font name.
    pub body_font_name: String,
    /// Whether the body font is bold.
    pub body_is_bold: bool,
    /// Median vertical spacing between lines.
    pub median_line_spacing: f64,
}

/// Groups text runs into horizontal lines based on Y proximity.
///
/// Runs within [`LINE_Y_TOLERANCE`] points of each other vertically
/// are grouped into the same line. Within each line, runs are sorted
/// left-to-right by X position.
pub fn group_into_lines(runs: &[TextRun]) -> Vec<TextLine> {
    if runs.is_empty() {
        return Vec::new();
    }

    // Sort run indices by Y (descending — top of page first), then X
    let mut indices: Vec<usize> = (0..runs.len()).collect();
    indices.sort_by(|&a, &b| {
        let ya = runs[a].y;
        let yb = runs[b].y;
        yb.partial_cmp(&ya)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                runs[a]
                    .x
                    .partial_cmp(&runs[b].x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut lines: Vec<TextLine> = Vec::new();

    for &idx in &indices {
        let run = &runs[idx];

        // Try to add to the last line if Y is close enough
        let added = if let Some(last_line) = lines.last_mut() {
            if (last_line.y - run.y).abs() <= LINE_Y_TOLERANCE {
                last_line.run_indices.push(idx);
                last_line.min_x = last_line.min_x.min(run.x);
                last_line.max_x = last_line.max_x.max(run.x + run.width);
                true
            } else {
                false
            }
        } else {
            false
        };

        if !added {
            lines.push(TextLine {
                run_indices: vec![idx],
                y: run.y,
                min_x: run.x,
                max_x: run.x + run.width,
            });
        }
    }

    // Sort runs within each line by X (left to right)
    for line in &mut lines {
        line.run_indices.sort_by(|&a, &b| {
            runs[a]
                .x
                .partial_cmp(&runs[b].x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    lines
}

/// Computes body text statistics from a set of text runs.
///
/// The "body" font is the most frequently occurring font size,
/// which is typically the paragraph text. Headings, footnotes,
/// and other elements have different sizes.
pub fn compute_font_stats(runs: &[TextRun]) -> PageFontStats {
    if runs.is_empty() {
        return PageFontStats {
            body_font_size: 12.0,
            body_font_name: String::new(),
            body_is_bold: false,
            median_line_spacing: 14.0,
        };
    }

    // Find most common font size (rounded to 0.5pt for grouping)
    let mut size_counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for run in runs {
        let key = (run.font_size * 2.0).round() as u32; // 0.5pt buckets
        *size_counts.entry(key).or_insert(0) += 1;
    }
    let body_size_key = size_counts
        .iter()
        .max_by_key(|(_, &count)| count)
        .map(|(&key, _)| key)
        .unwrap_or(24); // 12.0 * 2
    let body_font_size = body_size_key as f64 / 2.0;

    // Find most common font name among body-sized runs
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut bold_count = 0usize;
    let mut body_count = 0usize;
    for run in runs {
        let key = (run.font_size * 2.0).round() as u32;
        if key == body_size_key {
            *name_counts.entry(&run.font_name).or_insert(0) += 1;
            body_count += 1;
            if run.is_bold {
                bold_count += 1;
            }
        }
    }
    let body_font_name = name_counts
        .iter()
        .max_by_key(|(_, &count)| count)
        .map(|(&name, _)| name.to_string())
        .unwrap_or_default();
    let body_is_bold = bold_count > body_count / 2;

    // Compute median line spacing from grouped lines
    let lines = group_into_lines(runs);
    let mut spacings: Vec<f64> = Vec::new();
    for pair in lines.windows(2) {
        let spacing = (pair[0].y - pair[1].y).abs();
        if spacing > 0.1 && spacing < body_font_size * 5.0 {
            spacings.push(spacing);
        }
    }
    spacings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_line_spacing = if spacings.is_empty() {
        body_font_size * 1.2
    } else {
        spacings[spacings.len() / 2]
    };

    PageFontStats {
        body_font_size,
        body_font_name,
        body_is_bold,
        median_line_spacing,
    }
}

/// Minimum font size ratio (heading / body) to classify as heading.
const HEADING_SIZE_RATIO: f64 = 1.2;

/// Minimum vertical gap ratio (gap / median spacing) to boost heading score.
const HEADING_GAP_RATIO: f64 = 1.5;

/// Detects headings and classifies text lines into blocks.
///
/// A line is classified as a heading if:
/// - Its font size exceeds the body size by [`HEADING_SIZE_RATIO`], OR
/// - It is bold when the body text is not bold, OR
/// - It is preceded by extra vertical space (> [`HEADING_GAP_RATIO`] × median)
///
/// Heading levels (H1–H6) are assigned by ranking distinct heading
/// font sizes from largest (H1) to smallest (H6).
pub fn detect_headings(
    lines: &[TextLine],
    runs: &[TextRun],
    stats: &PageFontStats,
) -> Vec<TextBlock> {
    if lines.is_empty() {
        return Vec::new();
    }

    let size_threshold = stats.body_font_size * HEADING_SIZE_RATIO;

    // Score each line for heading-ness
    struct LineScore {
        is_heading: bool,
        effective_size: f64, // for ranking H1 vs H2 vs ...
    }

    let mut scores: Vec<LineScore> = Vec::with_capacity(lines.len());

    for (i, line) in lines.iter().enumerate() {
        // Compute line's dominant font size and bold state
        let line_runs: Vec<&TextRun> = line.run_indices.iter().map(|&idx| &runs[idx]).collect();
        let max_font_size = line_runs.iter().map(|r| r.font_size).fold(0.0f64, f64::max);
        let any_bold = line_runs.iter().any(|r| r.is_bold);

        let size_heading = max_font_size > size_threshold;
        let bold_heading = any_bold && !stats.body_is_bold;

        // Check vertical gap before this line
        let gap_heading = if i > 0 {
            let gap = (lines[i - 1].y - line.y).abs();
            gap > stats.median_line_spacing * HEADING_GAP_RATIO
        } else {
            false
        };

        let is_heading =
            size_heading || (bold_heading && (gap_heading || max_font_size > stats.body_font_size));

        scores.push(LineScore {
            is_heading,
            effective_size: if is_heading { max_font_size } else { 0.0 },
        });
    }

    // Rank distinct heading sizes → H1, H2, ...
    let mut heading_sizes: Vec<u32> = scores
        .iter()
        .filter(|s| s.is_heading)
        .map(|s| (s.effective_size * 10.0) as u32)
        .collect();
    heading_sizes.sort();
    heading_sizes.dedup();
    heading_sizes.reverse(); // largest first = H1

    // Build blocks
    let mut blocks: Vec<TextBlock> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let line_runs: Vec<TextRun> = line
            .run_indices
            .iter()
            .map(|&idx| runs[idx].clone())
            .collect();
        if line_runs.is_empty() {
            continue;
        }

        let bbox = compute_line_bbox(&line_runs);

        let role = if scores[i].is_heading {
            let size_key = (scores[i].effective_size * 10.0) as u32;
            let level = heading_sizes
                .iter()
                .position(|&s| s == size_key)
                .map(|pos| (pos + 1).min(6) as u8)
                .unwrap_or(1);
            BlockRole::Heading(level)
        } else {
            BlockRole::Paragraph
        };

        blocks.push(TextBlock {
            runs: line_runs,
            role,
            bbox,
            indent: line.min_x,
        });
    }

    blocks
}

/// Computes a bounding box from a set of text runs.
fn compute_line_bbox(runs: &[TextRun]) -> Rect {
    if runs.is_empty() {
        return Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }
    let min_x = runs.iter().map(|r| r.x).fold(f64::MAX, f64::min);
    let min_y = runs.iter().map(|r| r.y).fold(f64::MAX, f64::min);
    let max_x = runs.iter().map(|r| r.x + r.width).fold(f64::MIN, f64::max);
    let max_y = runs.iter().map(|r| r.y + r.height).fold(f64::MIN, f64::max);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

// --- List detection ---

/// Bullet characters commonly used in PDF content.
const BULLET_CHARS: &[char] = &[
    '\u{2022}', // •
    '\u{2023}', // ‣
    '\u{25CF}', // ●
    '\u{25CB}', // ○
    '\u{25A0}', // ■
    '\u{25AA}', // ▪
    '\u{2013}', // –
    '\u{2014}', // —
    '-', '*',
];

/// Detects whether a line's text starts with a list marker.
///
/// Returns `Some(marker_text)` if the line begins with a bullet,
/// number+period/paren, or letter+period/paren pattern.
pub fn detect_list_marker(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    // Check bullet characters
    let first = trimmed.chars().next()?;
    if BULLET_CHARS.contains(&first) {
        return Some(first.to_string());
    }

    // Check numbered patterns: "1.", "1)", "1:", "(1)", "i.", "iv."
    // and lettered patterns: "a.", "a)", "(a)"
    let bytes = trimmed.as_bytes();

    // "(X)" pattern
    if bytes.first() == Some(&b'(') {
        if let Some(close) = trimmed.find(')') {
            let inner = &trimmed[1..close];
            if inner.len() <= 4 && is_list_number_or_letter(inner) {
                return Some(trimmed[..=close].to_string());
            }
        }
    }

    // "X." or "X)" pattern
    let mut end = 0;
    for (i, ch) in trimmed.char_indices() {
        if ch.is_ascii_digit() || ch.is_ascii_lowercase() || ch == 'i' || ch == 'v' || ch == 'x' {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }

    if end > 0 && end < trimmed.len() {
        let suffix = trimmed.as_bytes()[end];
        if suffix == b'.' || suffix == b')' || suffix == b':' {
            let marker = &trimmed[..end + 1];
            if is_list_number_or_letter(&trimmed[..end]) {
                return Some(marker.to_string());
            }
        }
    }

    None
}

/// Checks whether a string is a valid list number or letter.
fn is_list_number_or_letter(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Pure digits: "1", "12", "123"
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Single letter: "a", "b", "A", "B"
    if s.len() == 1 && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return true;
    }
    // Roman numerals: "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"
    let lower = s.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "i" | "ii" | "iii" | "iv" | "v" | "vi" | "vii" | "viii" | "ix" | "x" | "xi" | "xii"
    )
}

// --- Emphasis and code detection ---

/// Inline emphasis span detected within a text run.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineRole {
    /// Bold text (emphasis).
    Strong,
    /// Italic text (emphasis).
    Emphasis,
    /// Bold + italic.
    StrongEmphasis,
    /// Monospaced / code.
    Code,
    /// Normal text.
    Normal,
}

/// Classifies a text run's inline role from its font style.
pub fn classify_inline_role(run: &TextRun) -> InlineRole {
    if run.is_monospaced {
        InlineRole::Code
    } else if run.is_bold && run.is_italic {
        InlineRole::StrongEmphasis
    } else if run.is_bold {
        InlineRole::Strong
    } else if run.is_italic {
        InlineRole::Emphasis
    } else {
        InlineRole::Normal
    }
}

// --- Decorative element detection ---

/// Checks whether a text run is likely a page number.
///
/// Page numbers are typically: short (1-4 chars), numeric,
/// positioned at page margins (top or bottom, centered or right).
pub fn is_likely_page_number(run: &TextRun, page_height: f64, _page_width: f64) -> bool {
    let text = run.text.trim();

    // Must be short
    if text.len() > 6 {
        return false;
    }

    // Must be numeric (possibly with surrounding dashes/dots: "- 5 -", "5.")
    let stripped = text.trim_matches(|c: char| c == '-' || c == '.' || c == ' ');
    if stripped.is_empty() || !stripped.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    // Must be near top or bottom margin
    let margin = page_height * 0.1;
    let near_top = run.y > page_height - margin;
    let near_bottom = run.y < margin;

    near_top || near_bottom
}

/// Full block classification pipeline.
///
/// Runs heading detection first, then reclassifies blocks that match
/// list patterns or code patterns. This is the main entry point for
/// structure analysis.
pub fn classify_blocks(runs: &[TextRun], _page_width: f64, _page_height: f64) -> Vec<TextBlock> {
    if runs.is_empty() {
        return Vec::new();
    }

    let stats = compute_font_stats(runs);
    let lines = group_into_lines(runs);
    let mut blocks = detect_headings(&lines, runs, &stats);

    // Reclassify paragraphs that are actually list items or code
    for block in &mut blocks {
        if block.role != BlockRole::Paragraph {
            continue;
        }

        let text: String = block
            .runs
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        // List detection: indented + starts with list marker
        if block.indent > stats.body_font_size * 0.5 && detect_list_marker(&text).is_some() {
            block.role = BlockRole::ListItem;
            continue;
        }
        // Also check non-indented list markers (some PDFs don't indent lists)
        if detect_list_marker(&text).is_some() {
            block.role = BlockRole::ListItem;
            continue;
        }

        // Code detection: all runs are monospaced AND the font differs
        // from the body font. This prevents classifying entire documents
        // that use Courier as body text as "code blocks."
        let body_is_mono = crate::content::analysis::font_style_from_name(&stats.body_font_name).2;
        if block.runs.iter().all(|r| r.is_monospaced) && !body_is_mono {
            block.role = BlockRole::Code;
            continue;
        }
    }

    blocks
}

// --- Table detection ---

/// Minimum number of aligned columns to consider a region a table.
const MIN_TABLE_COLUMNS: usize = 2;
/// Minimum number of rows to consider a region a table.
const MIN_TABLE_ROWS: usize = 2;
/// X tolerance for column alignment (in points).
const COLUMN_ALIGN_TOLERANCE: f64 = 3.0;

/// A detected table region with rows and column boundaries.
#[derive(Debug, Clone)]
pub struct DetectedTable {
    /// Row indices (each row is a list of block indices).
    pub rows: Vec<Vec<usize>>,
    /// Column X positions (left edges).
    pub column_xs: Vec<f64>,
    /// Bounding box of the table.
    pub bbox: Rect,
}

/// Detects tables from column-aligned text blocks.
///
/// Groups consecutive lines where text runs align to consistent
/// X positions across multiple rows. Returns detected table regions.
pub fn detect_tables(blocks: &[TextBlock]) -> Vec<DetectedTable> {
    if blocks.len() < MIN_TABLE_ROWS {
        return Vec::new();
    }

    let mut tables = Vec::new();

    // Find groups of consecutive blocks with aligned columns
    let mut i = 0;
    while i < blocks.len() {
        // Skip headings and non-paragraph blocks
        if blocks[i].role != BlockRole::Paragraph {
            i += 1;
            continue;
        }

        // Collect X positions of runs in this line
        let first_xs = collect_run_xs(&blocks[i]);
        if first_xs.len() < MIN_TABLE_COLUMNS {
            i += 1;
            continue;
        }

        // Check subsequent blocks for column alignment
        let mut table_block_indices = vec![i];
        let mut j = i + 1;
        while j < blocks.len() && blocks[j].role == BlockRole::Paragraph {
            let next_xs = collect_run_xs(&blocks[j]);
            if columns_align(&first_xs, &next_xs) {
                table_block_indices.push(j);
                j += 1;
            } else {
                break;
            }
        }

        if table_block_indices.len() >= MIN_TABLE_ROWS {
            // Build column positions from average X values
            let column_xs = first_xs.clone();
            let bbox = compute_table_bbox(blocks, &table_block_indices);
            let rows: Vec<Vec<usize>> = table_block_indices.iter().map(|&idx| vec![idx]).collect();
            tables.push(DetectedTable {
                rows,
                column_xs,
                bbox,
            });
            i = j;
        } else {
            i += 1;
        }
    }

    tables
}

/// Collects the X positions of each run in a block.
fn collect_run_xs(block: &TextBlock) -> Vec<f64> {
    block.runs.iter().map(|r| r.x).collect()
}

/// Checks whether two sets of X positions are column-aligned.
fn columns_align(a: &[f64], b: &[f64]) -> bool {
    if a.len() != b.len() || a.len() < MIN_TABLE_COLUMNS {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(ax, bx)| (ax - bx).abs() < COLUMN_ALIGN_TOLERANCE)
}

/// Computes the bounding box of a table from its constituent blocks.
fn compute_table_bbox(blocks: &[TextBlock], indices: &[usize]) -> Rect {
    let all_runs: Vec<&TextRun> = indices
        .iter()
        .flat_map(|&i| blocks[i].runs.iter())
        .collect();
    if all_runs.is_empty() {
        return Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }
    let min_x = all_runs.iter().map(|r| r.x).fold(f64::MAX, f64::min);
    let min_y = all_runs.iter().map(|r| r.y).fold(f64::MAX, f64::min);
    let max_x = all_runs
        .iter()
        .map(|r| r.x + r.width)
        .fold(f64::MIN, f64::max);
    let max_y = all_runs
        .iter()
        .map(|r| r.y + r.height)
        .fold(f64::MIN, f64::max);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

// --- Header/footer detection (cross-page) ---

/// Text found at consistent positions across multiple pages.
#[derive(Debug, Clone)]
pub struct RepeatedContent {
    /// The repeated text.
    pub text: String,
    /// Average Y position.
    pub y: f64,
    /// Number of pages where this text appears.
    pub page_count: usize,
    /// Whether this is near the top of the page.
    pub is_header: bool,
    /// Whether this is near the bottom of the page.
    pub is_footer: bool,
}

/// Detects headers and footers by finding text repeated across pages.
///
/// Compares text runs from multiple pages. Text that appears at the
/// same Y position on 2+ pages (with different content allowed for
/// page numbers) is classified as header or footer.
pub fn detect_headers_footers(
    pages_runs: &[Vec<TextRun>],
    page_height: f64,
) -> Vec<RepeatedContent> {
    if pages_runs.len() < 2 {
        return Vec::new();
    }

    let margin = page_height * 0.1;
    let mut results = Vec::new();

    // Collect text at margins from each page
    let mut top_texts: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    let mut bottom_texts: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();

    for page_runs in pages_runs {
        let lines = group_into_lines(page_runs);
        for line in &lines {
            let text: String = line
                .run_indices
                .iter()
                .map(|&i| page_runs[i].text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }

            // Normalize: replace numbers with # for page number matching
            let normalized = normalize_for_repeat(&text);

            if line.y > page_height - margin {
                let entry = top_texts.entry(normalized.clone()).or_insert((line.y, 0));
                entry.1 += 1;
            }
            if line.y < margin {
                let entry = bottom_texts.entry(normalized).or_insert((line.y, 0));
                entry.1 += 1;
            }
        }
    }

    for (text, (y, count)) in &top_texts {
        if *count >= 2 {
            results.push(RepeatedContent {
                text: text.clone(),
                y: *y,
                page_count: *count,
                is_header: true,
                is_footer: false,
            });
        }
    }
    for (text, (y, count)) in &bottom_texts {
        if *count >= 2 {
            results.push(RepeatedContent {
                text: text.clone(),
                y: *y,
                page_count: *count,
                is_header: false,
                is_footer: true,
            });
        }
    }

    results
}

/// Normalizes text for repeat detection — replaces digits with #.
fn normalize_for_repeat(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

// --- Form field label association ---

/// A form field paired with its nearest text label.
#[derive(Debug, Clone)]
pub struct LabeledField {
    /// The form field name (from AcroForm).
    pub field_name: String,
    /// The detected label text (nearest text to the field's position).
    pub label: String,
    /// Distance between label and field (in points).
    pub distance: f64,
}

/// Associates form fields with nearby text labels by proximity.
///
/// For each field position, finds the closest text run that is:
/// - To the left of the field (most common label position), OR
/// - Directly above the field
///
/// Returns fields paired with their best-match labels.
pub fn associate_field_labels(
    runs: &[TextRun],
    field_positions: &[(String, f64, f64, f64, f64)], // (name, x, y, width, height)
) -> Vec<LabeledField> {
    let mut results = Vec::new();

    for (name, fx, fy, fw, fh) in field_positions {
        let field_center_y = fy + fh / 2.0;
        let mut best_label = String::new();
        let mut best_distance = f64::MAX;

        for run in runs {
            if run.text.trim().is_empty() {
                continue;
            }

            let run_right = run.x + run.width;
            let run_center_y = run.y + run.height / 2.0;

            // Label to the left of field (same line)
            if (run_center_y - field_center_y).abs() < run.height * 1.5 && run_right < fx + 5.0 {
                let dist = fx - run_right;
                if dist >= 0.0 && dist < best_distance {
                    best_distance = dist;
                    best_label = run.text.trim().to_string();
                }
            }

            // Label directly above field
            if run.y > *fy && (run.x - fx).abs() < *fw && run.y - fy < run.height * 3.0 {
                let dist = run.y - fy;
                if dist < best_distance {
                    best_distance = dist;
                    best_label = run.text.trim().to_string();
                }
            }
        }

        if !best_label.is_empty() {
            results.push(LabeledField {
                field_name: name.clone(),
                label: best_label,
                distance: best_distance,
            });
        }
    }

    results
}

// --- Figure-caption association ---

/// An image paired with a detected caption.
#[derive(Debug, Clone)]
pub struct FigureCaption {
    /// Image resource name.
    pub image_name: String,
    /// Image bounding box.
    pub image_bbox: Rect,
    /// Caption text (from nearby text runs).
    pub caption: String,
    /// Y position of caption.
    pub caption_y: f64,
}

/// Associates images with nearby caption text.
///
/// A caption is text immediately below or above an image that:
/// - Starts with "Figure", "Fig.", "Image", "Photo", or a number
/// - Is within 2× font size distance of the image edge
/// - Is smaller or equal font size to body text
pub fn associate_figure_captions(
    runs: &[TextRun],
    image_bboxes: &[(String, Rect)], // (name, bbox)
    stats: &PageFontStats,
) -> Vec<FigureCaption> {
    let mut results = Vec::new();
    let caption_distance = stats.body_font_size * 2.0;

    for (name, img_bbox) in image_bboxes {
        let mut best_caption = String::new();
        let mut best_y = 0.0;
        let mut best_distance = f64::MAX;

        // Look at each line near the image
        let lines = group_into_lines(runs);
        for line in &lines {
            let line_text: String = line
                .run_indices
                .iter()
                .map(|&i| runs[i].text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let trimmed = line_text.trim();

            // Check if text looks like a caption
            if !is_caption_text(trimmed) {
                continue;
            }

            // Check proximity: below image
            let dist_below = (img_bbox.y - line.y).abs();
            // Check proximity: above image
            let dist_above = (line.y - (img_bbox.y + img_bbox.height)).abs();

            let dist = dist_below.min(dist_above);
            if dist < caption_distance && dist < best_distance {
                // Check horizontal overlap
                if line.min_x < img_bbox.x + img_bbox.width && line.max_x > img_bbox.x {
                    best_caption = trimmed.to_string();
                    best_y = line.y;
                    best_distance = dist;
                }
            }
        }

        if !best_caption.is_empty() {
            results.push(FigureCaption {
                image_name: name.clone(),
                image_bbox: *img_bbox,
                caption: best_caption,
                caption_y: best_y,
            });
        }
    }

    results
}

/// Checks whether text looks like a figure caption.
fn is_caption_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("figure")
        || lower.starts_with("fig.")
        || lower.starts_with("fig ")
        || lower.starts_with("image")
        || lower.starts_with("photo")
        || lower.starts_with("table")
        || lower.starts_with("chart")
        || lower.starts_with("diagram")
        || lower.starts_with("illustration")
        // Numbered captions: "1.", "1:", "(1)"
        || (text.len() > 2 && text.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, x: f64, y: f64, font_size: f64, bold: bool) -> TextRun {
        TextRun {
            text: text.to_string(),
            font_name: if bold {
                "Helvetica-Bold".to_string()
            } else {
                "Helvetica".to_string()
            },
            font_size,
            x,
            y,
            width: text.len() as f64 * font_size * 0.5,
            height: font_size,
            color: [0.0, 0.0, 0.0, 1.0],
            rendering_mode: 0,
            is_bold: bold,
            is_italic: false,
            is_monospaced: false,
        }
    }

    // --- group_into_lines ---

    #[test]
    fn lines_same_y_grouped() {
        let runs = vec![
            run("Hello", 100.0, 700.0, 12.0, false),
            run("World", 200.0, 700.0, 12.0, false),
        ];
        let lines = group_into_lines(&runs);
        assert_eq!(lines.len(), 1, "Same Y should produce one line");
        assert_eq!(lines[0].run_indices.len(), 2);
    }

    #[test]
    fn lines_different_y_separated() {
        let runs = vec![
            run("Line1", 100.0, 700.0, 12.0, false),
            run("Line2", 100.0, 680.0, 12.0, false),
        ];
        let lines = group_into_lines(&runs);
        assert_eq!(lines.len(), 2, "Different Y should produce two lines");
    }

    #[test]
    fn lines_within_tolerance_grouped() {
        let runs = vec![
            run("A", 100.0, 700.0, 12.0, false),
            run("B", 200.0, 701.5, 12.0, false), // within 2pt tolerance
        ];
        let lines = group_into_lines(&runs);
        assert_eq!(lines.len(), 1, "Runs within tolerance should be same line");
    }

    #[test]
    fn lines_sorted_by_x() {
        let runs = vec![
            run("Second", 200.0, 700.0, 12.0, false),
            run("First", 100.0, 700.0, 12.0, false),
        ];
        let lines = group_into_lines(&runs);
        assert_eq!(lines[0].run_indices, vec![1, 0], "Should be sorted by X");
    }

    #[test]
    fn lines_empty_input() {
        let lines = group_into_lines(&[]);
        assert!(lines.is_empty());
    }

    // --- compute_font_stats ---

    #[test]
    fn stats_all_same_size() {
        let runs = vec![
            run("A", 100.0, 700.0, 12.0, false),
            run("B", 100.0, 686.0, 12.0, false),
            run("C", 100.0, 672.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        assert!((stats.body_font_size - 12.0).abs() < 0.5);
    }

    #[test]
    fn stats_majority_determines_body() {
        let runs = vec![
            run("Body", 100.0, 700.0, 12.0, false),
            run("Body", 100.0, 686.0, 12.0, false),
            run("Body", 100.0, 672.0, 12.0, false),
            run("Body", 100.0, 658.0, 12.0, false),
            run("Title", 100.0, 750.0, 24.0, true), // minority
        ];
        let stats = compute_font_stats(&runs);
        assert!(
            (stats.body_font_size - 12.0).abs() < 0.5,
            "Body size should be 12, got {}",
            stats.body_font_size
        );
    }

    #[test]
    fn stats_empty_runs() {
        let stats = compute_font_stats(&[]);
        assert!(
            (stats.body_font_size - 12.0).abs() < 0.1,
            "Default body size should be 12"
        );
    }

    // --- detect_headings ---

    #[test]
    fn heading_larger_font_detected() {
        let runs = vec![
            run("Title", 100.0, 750.0, 24.0, false),
            run("Body text here", 100.0, 700.0, 12.0, false),
            run("More body text", 100.0, 686.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .collect();
        assert_eq!(headings.len(), 1, "Should detect one heading");
        assert!(headings[0].runs[0].text.contains("Title"));
    }

    #[test]
    fn heading_bold_detected() {
        let runs = vec![
            run("Bold Title", 100.0, 750.0, 13.0, true), // slightly larger + bold
            run("Normal body", 100.0, 700.0, 12.0, false),
            run("More normal", 100.0, 686.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .collect();
        assert!(
            !headings.is_empty(),
            "Bold text at slightly larger size should be heading"
        );
    }

    #[test]
    fn heading_two_sizes_ranked() {
        let runs = vec![
            run("Chapter", 100.0, 750.0, 24.0, true),
            run("Section", 100.0, 700.0, 18.0, false),
            run("Body", 100.0, 650.0, 12.0, false),
            run("Body", 100.0, 636.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        let h1: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(1)))
            .collect();
        let h2: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(2)))
            .collect();
        assert_eq!(h1.len(), 1, "Should have one H1");
        assert_eq!(h2.len(), 1, "Should have one H2");
        assert!(h1[0].runs[0].text.contains("Chapter"));
        assert!(h2[0].runs[0].text.contains("Section"));
    }

    #[test]
    fn no_headings_when_all_same() {
        let runs = vec![
            run("Line A", 100.0, 700.0, 12.0, false),
            run("Line B", 100.0, 686.0, 12.0, false),
            run("Line C", 100.0, 672.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .collect();
        assert!(
            headings.is_empty(),
            "All same size/weight should produce no headings"
        );
    }

    #[test]
    fn heading_with_vertical_gap() {
        let runs = vec![
            run("Above", 100.0, 750.0, 12.0, false),
            // Large gap before next line
            run("After Gap", 100.0, 650.0, 13.0, true), // bold + slightly larger + gap
            run("Body", 100.0, 636.0, 12.0, false),
        ];
        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .collect();
        assert!(
            !headings.is_empty(),
            "Bold text after large gap should be heading"
        );
    }

    #[test]
    fn six_heading_sizes_produce_h1_through_h6() {
        let mut runs = Vec::new();
        let sizes = [36.0, 30.0, 24.0, 20.0, 17.0, 15.0];
        let mut y = 750.0;
        for (i, &size) in sizes.iter().enumerate() {
            runs.push(run(&format!("Heading {}", i + 1), 100.0, y, size, false));
            y -= size + 20.0;
        }
        // Add lots of body text to establish 12pt as the body size
        for i in 0..10 {
            runs.push(run("Body text", 100.0, y, 12.0, false));
            y -= 14.0;
        }

        let stats = compute_font_stats(&runs);
        let lines = group_into_lines(&runs);
        let blocks = detect_headings(&lines, &runs, &stats);

        for level in 1..=6u8 {
            let count = blocks
                .iter()
                .filter(|b| matches!(b.role, BlockRole::Heading(l) if l == level))
                .count();
            assert_eq!(count, 1, "Should have exactly one H{level}, got {count}");
        }
    }

    #[test]
    fn block_role_maps_to_standard_role() {
        assert_eq!(
            BlockRole::Heading(1).to_standard_role(),
            Some(crate::accessibility::StandardRole::H(1))
        );
        assert_eq!(
            BlockRole::Paragraph.to_standard_role(),
            Some(crate::accessibility::StandardRole::P)
        );
        assert_eq!(BlockRole::Unknown.to_standard_role(), None);
        assert_eq!(
            BlockRole::ListItem.to_standard_role(),
            Some(crate::accessibility::StandardRole::LI)
        );
    }

    // --- List detection ---

    #[test]
    fn list_bullet_detected() {
        assert!(detect_list_marker("\u{2022} Item one").is_some());
        assert!(detect_list_marker("- Item two").is_some());
        assert!(detect_list_marker("* Item three").is_some());
    }

    #[test]
    fn list_numbered_detected() {
        assert_eq!(detect_list_marker("1. First"), Some("1.".to_string()));
        assert_eq!(detect_list_marker("2) Second"), Some("2)".to_string()));
        assert_eq!(detect_list_marker("12. Twelfth"), Some("12.".to_string()));
    }

    #[test]
    fn list_lettered_detected() {
        assert_eq!(detect_list_marker("a. Item"), Some("a.".to_string()));
        assert_eq!(detect_list_marker("b) Item"), Some("b)".to_string()));
    }

    #[test]
    fn list_roman_detected() {
        assert_eq!(detect_list_marker("i. Item"), Some("i.".to_string()));
        assert_eq!(detect_list_marker("iv. Item"), Some("iv.".to_string()));
    }

    #[test]
    fn list_parenthesized_detected() {
        assert_eq!(detect_list_marker("(1) Item"), Some("(1)".to_string()));
        assert_eq!(detect_list_marker("(a) Item"), Some("(a)".to_string()));
    }

    #[test]
    fn list_normal_text_not_detected() {
        assert!(detect_list_marker("Hello world").is_none());
        assert!(detect_list_marker("The quick brown fox").is_none());
        assert!(detect_list_marker("").is_none());
    }

    // --- Emphasis/code inline classification ---

    #[test]
    fn inline_role_bold() {
        let r = run("Bold text", 100.0, 700.0, 12.0, true);
        assert_eq!(classify_inline_role(&r), InlineRole::Strong);
    }

    #[test]
    fn inline_role_italic() {
        let mut r = run("Italic text", 100.0, 700.0, 12.0, false);
        r.is_italic = true;
        assert_eq!(classify_inline_role(&r), InlineRole::Emphasis);
    }

    #[test]
    fn inline_role_monospaced() {
        let mut r = run("code()", 100.0, 700.0, 10.0, false);
        r.is_monospaced = true;
        assert_eq!(classify_inline_role(&r), InlineRole::Code);
    }

    #[test]
    fn inline_role_bold_italic() {
        let mut r = run("Important", 100.0, 700.0, 12.0, true);
        r.is_italic = true;
        assert_eq!(classify_inline_role(&r), InlineRole::StrongEmphasis);
    }

    #[test]
    fn inline_role_normal() {
        let r = run("Normal text", 100.0, 700.0, 12.0, false);
        assert_eq!(classify_inline_role(&r), InlineRole::Normal);
    }

    // --- Page number detection ---

    #[test]
    fn page_number_at_bottom() {
        let r = run("5", 300.0, 30.0, 10.0, false);
        assert!(is_likely_page_number(&r, 792.0, 612.0));
    }

    #[test]
    fn page_number_at_top() {
        let r = run("12", 300.0, 770.0, 10.0, false);
        assert!(is_likely_page_number(&r, 792.0, 612.0));
    }

    #[test]
    fn page_number_with_dashes() {
        let r = run("- 3 -", 300.0, 30.0, 10.0, false);
        assert!(is_likely_page_number(&r, 792.0, 612.0));
    }

    #[test]
    fn body_text_not_page_number() {
        let r = run("Hello world", 100.0, 400.0, 12.0, false);
        assert!(!is_likely_page_number(&r, 792.0, 612.0));
    }

    #[test]
    fn number_in_body_not_page_number() {
        // Numeric but in the middle of the page
        let r = run("42", 100.0, 400.0, 12.0, false);
        assert!(!is_likely_page_number(&r, 792.0, 612.0));
    }

    // --- classify_blocks pipeline ---

    #[test]
    fn classify_detects_list_items() {
        let runs = vec![
            run("1. First item", 120.0, 700.0, 12.0, false),
            run("2. Second item", 120.0, 686.0, 12.0, false),
            run("Body after list", 100.0, 660.0, 12.0, false),
        ];
        let blocks = classify_blocks(&runs, 612.0, 792.0);

        let lists: Vec<_> = blocks
            .iter()
            .filter(|b| b.role == BlockRole::ListItem)
            .collect();
        assert_eq!(lists.len(), 2, "Should detect two list items");
    }

    #[test]
    fn classify_detects_code_blocks() {
        let mut mono_run = run("fn main() {}", 100.0, 700.0, 10.0, false);
        mono_run.is_monospaced = true;
        mono_run.font_name = "Courier".to_string();

        let runs = vec![
            run("Some text", 100.0, 750.0, 12.0, false),
            run("More text", 100.0, 736.0, 12.0, false),
            mono_run,
            run("After code", 100.0, 680.0, 12.0, false),
        ];
        let blocks = classify_blocks(&runs, 612.0, 792.0);

        let code: Vec<_> = blocks
            .iter()
            .filter(|b| b.role == BlockRole::Code)
            .collect();
        assert_eq!(code.len(), 1, "Should detect one code block");
    }

    #[test]
    fn classify_mixed_content() {
        let runs = vec![
            run("Chapter Title", 100.0, 750.0, 24.0, true),
            run("Body paragraph text here", 100.0, 700.0, 12.0, false),
            run("More body text", 100.0, 686.0, 12.0, false),
            run("1. First list item", 120.0, 660.0, 12.0, false),
            run("2. Second list item", 120.0, 646.0, 12.0, false),
            run("Closing paragraph", 100.0, 620.0, 12.0, false),
        ];
        let blocks = classify_blocks(&runs, 612.0, 792.0);

        let headings = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .count();
        let paragraphs = blocks
            .iter()
            .filter(|b| b.role == BlockRole::Paragraph)
            .count();
        let lists = blocks
            .iter()
            .filter(|b| b.role == BlockRole::ListItem)
            .count();

        assert_eq!(headings, 1, "Should have exactly 1 heading, got {headings}");
        assert_eq!(
            paragraphs, 3,
            "Should have exactly 3 paragraphs, got {paragraphs}"
        );
        assert_eq!(lists, 2, "Should have exactly 2 list items, got {lists}");
    }

    // --- Table detection ---

    #[test]
    fn table_two_columns_three_rows() {
        let blocks = vec![
            // Row 1: two cells at x=100 and x=300
            TextBlock {
                runs: vec![
                    run("Name", 100.0, 700.0, 12.0, true),
                    run("Age", 300.0, 700.0, 12.0, true),
                ],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 700.0,
                    width: 250.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
            // Row 2
            TextBlock {
                runs: vec![
                    run("Alice", 100.0, 686.0, 12.0, false),
                    run("30", 300.0, 686.0, 12.0, false),
                ],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 686.0,
                    width: 250.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
            // Row 3
            TextBlock {
                runs: vec![
                    run("Bob", 100.0, 672.0, 12.0, false),
                    run("25", 300.0, 672.0, 12.0, false),
                ],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 672.0,
                    width: 250.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
        ];
        let tables = detect_tables(&blocks);
        assert_eq!(tables.len(), 1, "Should detect one table");
        assert_eq!(tables[0].rows.len(), 3, "Table should have 3 rows");
        assert_eq!(tables[0].column_xs.len(), 2, "Table should have 2 columns");
    }

    #[test]
    fn table_not_detected_for_single_column() {
        let blocks = vec![
            TextBlock {
                runs: vec![run("Line 1", 100.0, 700.0, 12.0, false)],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 700.0,
                    width: 100.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
            TextBlock {
                runs: vec![run("Line 2", 100.0, 686.0, 12.0, false)],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 686.0,
                    width: 100.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
        ];
        let tables = detect_tables(&blocks);
        assert!(tables.is_empty(), "Single column should not be a table");
    }

    #[test]
    fn table_not_detected_for_misaligned() {
        let blocks = vec![
            TextBlock {
                runs: vec![
                    run("A", 100.0, 700.0, 12.0, false),
                    run("B", 300.0, 700.0, 12.0, false),
                ],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 100.0,
                    y: 700.0,
                    width: 250.0,
                    height: 12.0,
                },
                indent: 100.0,
            },
            TextBlock {
                runs: vec![
                    run("C", 150.0, 686.0, 12.0, false),
                    run("D", 350.0, 686.0, 12.0, false),
                ],
                role: BlockRole::Paragraph,
                bbox: Rect {
                    x: 150.0,
                    y: 686.0,
                    width: 250.0,
                    height: 12.0,
                },
                indent: 150.0,
            },
        ];
        let tables = detect_tables(&blocks);
        assert!(
            tables.is_empty(),
            "Misaligned columns should not be a table"
        );
    }

    // --- Header/footer detection ---

    #[test]
    fn header_detected_across_pages() {
        let page1 = vec![
            run("Company Name", 200.0, 770.0, 10.0, false),
            run("Body text", 100.0, 600.0, 12.0, false),
        ];
        let page2 = vec![
            run("Company Name", 200.0, 770.0, 10.0, false),
            run("More body", 100.0, 600.0, 12.0, false),
        ];
        let results = detect_headers_footers(&[page1, page2], 792.0);
        assert!(!results.is_empty(), "Should detect repeated header");
        assert!(
            results.iter().any(|r| r.is_header),
            "Should be classified as header"
        );
    }

    #[test]
    fn footer_with_page_number() {
        let page1 = vec![
            run("Body", 100.0, 600.0, 12.0, false),
            run("Page 1", 280.0, 30.0, 8.0, false),
        ];
        let page2 = vec![
            run("Body", 100.0, 600.0, 12.0, false),
            run("Page 2", 280.0, 30.0, 8.0, false),
        ];
        let results = detect_headers_footers(&[page1, page2], 792.0);
        assert!(
            !results.is_empty(),
            "Should detect footer with page numbers"
        );
        assert!(results.iter().any(|r| r.is_footer));
    }

    #[test]
    fn no_headers_on_single_page() {
        let page1 = vec![run("Header", 200.0, 770.0, 10.0, false)];
        let results = detect_headers_footers(&[page1], 792.0);
        assert!(
            results.is_empty(),
            "Single page cannot have repeated content"
        );
    }

    // --- Form field label association ---

    #[test]
    fn label_to_left_of_field() {
        let runs = vec![
            run("Name:", 50.0, 500.0, 12.0, false),
            run("Email:", 50.0, 470.0, 12.0, false),
        ];
        let fields = vec![
            ("name".to_string(), 150.0, 494.0, 200.0, 20.0),
            ("email".to_string(), 150.0, 464.0, 200.0, 20.0),
        ];
        let labeled = associate_field_labels(&runs, &fields);
        assert_eq!(labeled.len(), 2);
        assert_eq!(labeled[0].label, "Name:");
        assert_eq!(labeled[1].label, "Email:");
    }

    #[test]
    fn label_above_field() {
        let runs = vec![run("Username", 100.0, 520.0, 12.0, false)];
        let fields = vec![("username".to_string(), 100.0, 494.0, 200.0, 20.0)];
        let labeled = associate_field_labels(&runs, &fields);
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].label, "Username");
    }

    #[test]
    fn no_label_for_distant_field() {
        let runs = vec![run("Far away text", 50.0, 700.0, 12.0, false)];
        let fields = vec![("field1".to_string(), 50.0, 100.0, 200.0, 20.0)];
        let labeled = associate_field_labels(&runs, &fields);
        assert!(labeled.is_empty(), "Distant text should not be associated");
    }

    // --- Figure-caption association ---

    #[test]
    fn caption_below_figure() {
        let runs = vec![
            run("Body text above", 100.0, 600.0, 12.0, false),
            run("Figure 1: A nice chart", 120.0, 380.0, 10.0, false),
            run("Body text below", 100.0, 350.0, 12.0, false),
        ];
        let images = vec![(
            "Im0".to_string(),
            Rect {
                x: 100.0,
                y: 400.0,
                width: 400.0,
                height: 180.0,
            },
        )];
        let stats = compute_font_stats(&runs);
        let captions = associate_figure_captions(&runs, &images, &stats);
        assert_eq!(captions.len(), 1);
        assert!(captions[0].caption.contains("Figure 1"));
    }

    #[test]
    fn no_caption_for_distant_image() {
        let runs = vec![run("Body text", 100.0, 600.0, 12.0, false)];
        let images = vec![(
            "Im0".to_string(),
            Rect {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 100.0,
            },
        )];
        let stats = compute_font_stats(&runs);
        let captions = associate_figure_captions(&runs, &images, &stats);
        assert!(captions.is_empty(), "Distant text should not be caption");
    }

    #[test]
    fn caption_text_detection() {
        assert!(is_caption_text("Figure 1: Description"));
        assert!(is_caption_text("Fig. 3 — Overview"));
        assert!(is_caption_text("Table 2: Results"));
        assert!(is_caption_text("Photo of the building"));
        assert!(!is_caption_text("Normal paragraph text"));
        assert!(!is_caption_text("The quick brown fox"));
    }

    #[test]
    fn normalize_replaces_digits() {
        assert_eq!(normalize_for_repeat("Page 1"), "Page #");
        assert_eq!(normalize_for_repeat("Page 12"), "Page ##");
        assert_eq!(normalize_for_repeat("No numbers"), "No numbers");
    }
}
