//! Content stream builder for generating PDF page content.
//!
//! The builder produces raw bytes in PDF content stream syntax — the inverse
//! of [`tokenize_content_stream`](super::tokenize_content_stream).
//! Content streams use postfix notation: operands precede the operator.
//!
//! # Example
//!
//! ```rust
//! use pdfpurr::content::ContentStreamBuilder;
//!
//! let mut b = ContentStreamBuilder::new();
//! b.begin_text()
//!     .set_font("F1", 12.0)
//!     .move_to(100.0, 700.0)
//!     .show_text("Hello World")
//!     .end_text();
//! let content = b.build();
//! ```
//!
//! ISO 32000-2:2020, Section 7.8.2.

use std::fmt::Write as FmtWrite;

/// An item in a TJ (show text with adjustments) array.
#[derive(Debug, Clone, PartialEq)]
pub enum TextItem<'a> {
    /// A text string to display.
    Text(&'a str),
    /// A spacing adjustment in thousandths of a unit of text space.
    /// Negative values move the next glyph closer (kerning).
    Spacing(i64),
}

/// Builds PDF content stream bytes programmatically.
///
/// Each method appends operands and an operator to an internal buffer.
/// Call [`build`](Self::build) to produce the final byte sequence.
pub struct ContentStreamBuilder {
    buf: String,
}

impl ContentStreamBuilder {
    /// Creates a new, empty content stream builder.
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    // --- Text object operators ---

    /// Begins a text object (`BT`).
    pub fn begin_text(&mut self) -> &mut Self {
        self.buf.push_str("BT\n");
        self
    }

    /// Ends a text object (`ET`).
    pub fn end_text(&mut self) -> &mut Self {
        self.buf.push_str("ET\n");
        self
    }

    /// Sets the font and size (`Tf`).
    pub fn set_font(&mut self, name: &str, size: f64) -> &mut Self {
        self.buf.push('/');
        write_name(&mut self.buf, name);
        self.buf.push(' ');
        write_number(&mut self.buf, size);
        self.buf.push_str(" Tf\n");
        self
    }

    /// Moves to a text position (`Td`).
    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        write_number(&mut self.buf, x);
        self.buf.push(' ');
        write_number(&mut self.buf, y);
        self.buf.push_str(" Td\n");
        self
    }

    /// Shows a text string (`Tj`).
    pub fn show_text(&mut self, text: &str) -> &mut Self {
        self.buf.push('(');
        write_literal_string(&mut self.buf, text);
        self.buf.push_str(") Tj\n");
        self
    }

    /// Shows text with individual glyph positioning (`TJ`).
    pub fn show_text_adjusted(&mut self, items: &[TextItem<'_>]) -> &mut Self {
        self.buf.push('[');
        for item in items {
            match item {
                TextItem::Text(s) => {
                    self.buf.push('(');
                    write_literal_string(&mut self.buf, s);
                    self.buf.push(')');
                }
                TextItem::Spacing(n) => {
                    let _ = write!(self.buf, "{}", n);
                }
            }
            self.buf.push(' ');
        }
        // Remove trailing space before closing bracket
        if self.buf.ends_with(' ') {
            self.buf.pop();
        }
        self.buf.push_str("] TJ\n");
        self
    }

    /// Sets the text matrix (`Tm`).
    pub fn set_text_matrix(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> &mut Self {
        write_number(&mut self.buf, a);
        self.buf.push(' ');
        write_number(&mut self.buf, b);
        self.buf.push(' ');
        write_number(&mut self.buf, c);
        self.buf.push(' ');
        write_number(&mut self.buf, d);
        self.buf.push(' ');
        write_number(&mut self.buf, e);
        self.buf.push(' ');
        write_number(&mut self.buf, f);
        self.buf.push_str(" Tm\n");
        self
    }

    /// Sets character spacing (`Tc`).
    pub fn set_character_spacing(&mut self, spacing: f64) -> &mut Self {
        write_number(&mut self.buf, spacing);
        self.buf.push_str(" Tc\n");
        self
    }

    /// Sets word spacing (`Tw`).
    pub fn set_word_spacing(&mut self, spacing: f64) -> &mut Self {
        write_number(&mut self.buf, spacing);
        self.buf.push_str(" Tw\n");
        self
    }

    /// Sets text leading (`TL`).
    pub fn set_leading(&mut self, leading: f64) -> &mut Self {
        write_number(&mut self.buf, leading);
        self.buf.push_str(" TL\n");
        self
    }

    /// Moves to the next line (`T*`).
    pub fn next_line(&mut self) -> &mut Self {
        self.buf.push_str("T*\n");
        self
    }

    // --- Graphics state operators ---

    /// Saves the graphics state (`q`).
    pub fn save_state(&mut self) -> &mut Self {
        self.buf.push_str("q\n");
        self
    }

    /// Restores the graphics state (`Q`).
    pub fn restore_state(&mut self) -> &mut Self {
        self.buf.push_str("Q\n");
        self
    }

    /// Sets the transformation matrix (`cm`).
    pub fn set_transform(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> &mut Self {
        write_number(&mut self.buf, a);
        self.buf.push(' ');
        write_number(&mut self.buf, b);
        self.buf.push(' ');
        write_number(&mut self.buf, c);
        self.buf.push(' ');
        write_number(&mut self.buf, d);
        self.buf.push(' ');
        write_number(&mut self.buf, e);
        self.buf.push(' ');
        write_number(&mut self.buf, f);
        self.buf.push_str(" cm\n");
        self
    }

    /// Sets the line width (`w`).
    pub fn set_line_width(&mut self, width: f64) -> &mut Self {
        write_number(&mut self.buf, width);
        self.buf.push_str(" w\n");
        self
    }

    // --- Color operators ---

    /// Sets the fill color in DeviceRGB (`rg`).
    pub fn set_fill_color_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Self {
        write_number(&mut self.buf, r);
        self.buf.push(' ');
        write_number(&mut self.buf, g);
        self.buf.push(' ');
        write_number(&mut self.buf, b);
        self.buf.push_str(" rg\n");
        self
    }

    /// Sets the stroke color in DeviceRGB (`RG`).
    pub fn set_stroke_color_rgb(&mut self, r: f64, g: f64, b: f64) -> &mut Self {
        write_number(&mut self.buf, r);
        self.buf.push(' ');
        write_number(&mut self.buf, g);
        self.buf.push(' ');
        write_number(&mut self.buf, b);
        self.buf.push_str(" RG\n");
        self
    }

    /// Sets the fill color in DeviceGray (`g`).
    pub fn set_fill_color_gray(&mut self, gray: f64) -> &mut Self {
        write_number(&mut self.buf, gray);
        self.buf.push_str(" g\n");
        self
    }

    /// Sets the stroke color in DeviceGray (`G`).
    pub fn set_stroke_color_gray(&mut self, gray: f64) -> &mut Self {
        write_number(&mut self.buf, gray);
        self.buf.push_str(" G\n");
        self
    }

    // --- Path construction operators ---

    /// Begins a new subpath at `(x, y)` (`m`).
    pub fn move_line_to(&mut self, x: f64, y: f64) -> &mut Self {
        write_number(&mut self.buf, x);
        self.buf.push(' ');
        write_number(&mut self.buf, y);
        self.buf.push_str(" m\n");
        self
    }

    /// Appends a straight line to `(x, y)` (`l`).
    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        write_number(&mut self.buf, x);
        self.buf.push(' ');
        write_number(&mut self.buf, y);
        self.buf.push_str(" l\n");
        self
    }

    /// Appends a rectangle (`re`).
    pub fn rect(&mut self, x: f64, y: f64, width: f64, height: f64) -> &mut Self {
        write_number(&mut self.buf, x);
        self.buf.push(' ');
        write_number(&mut self.buf, y);
        self.buf.push(' ');
        write_number(&mut self.buf, width);
        self.buf.push(' ');
        write_number(&mut self.buf, height);
        self.buf.push_str(" re\n");
        self
    }

    // --- Path painting operators ---

    /// Strokes the path (`S`).
    pub fn stroke(&mut self) -> &mut Self {
        self.buf.push_str("S\n");
        self
    }

    /// Fills the path using the nonzero winding rule (`f`).
    pub fn fill(&mut self) -> &mut Self {
        self.buf.push_str("f\n");
        self
    }

    /// Fills and then strokes the path (`B`).
    pub fn fill_and_stroke(&mut self) -> &mut Self {
        self.buf.push_str("B\n");
        self
    }

    /// Closes the current subpath (`h`).
    pub fn close_path(&mut self) -> &mut Self {
        self.buf.push_str("h\n");
        self
    }

    // --- XObject operators ---

    /// Paints an XObject (image or form) by name (`Do`).
    pub fn draw_image(&mut self, name: &str) -> &mut Self {
        self.buf.push('/');
        write_name(&mut self.buf, name);
        self.buf.push_str(" Do\n");
        self
    }

    // --- Build ---

    /// Consumes the builder and returns the content stream bytes.
    pub fn build(self) -> Vec<u8> {
        self.buf.into_bytes()
    }
}

impl Default for ContentStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes a number in PDF-compatible format.
///
/// Integer-valued floats are written without a decimal point (e.g. `12` not `12.0`)
/// so the tokenizer parses them as `Object::Integer`. Actual fractional values
/// are written with their natural decimal representation.
fn write_number(buf: &mut String, n: f64) {
    if n == n.floor() && n.abs() < 1e15 {
        let _ = write!(buf, "{}", n as i64);
    } else {
        let _ = write!(buf, "{}", n);
    }
}

/// Writes a PDF name, escaping special characters with `#xx`.
fn write_name(buf: &mut String, name: &str) {
    for &b in name.as_bytes() {
        if !(0x21..=0x7E).contains(&b)
            || matches!(
                b,
                b'#' | b'/' | b'%' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}'
            )
        {
            let _ = write!(buf, "#{:02X}", b);
        } else {
            buf.push(b as char);
        }
    }
}

/// Writes a literal PDF string, escaping `(`, `)`, and `\`.
fn write_literal_string(buf: &mut String, text: &str) {
    for &b in text.as_bytes() {
        match b {
            b'(' => buf.push_str("\\("),
            b')' => buf.push_str("\\)"),
            b'\\' => buf.push_str("\\\\"),
            _ => buf.push(b as char),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{extract_text_from_content, tokenize_content_stream, ContentToken};
    use crate::core::objects::{Object, PdfName};

    /// Helper: build a content stream from a closure that configures the builder.
    fn build<F: FnOnce(&mut ContentStreamBuilder)>(f: F) -> Vec<u8> {
        let mut b = ContentStreamBuilder::new();
        f(&mut b);
        b.build()
    }

    #[test]
    fn builder_empty_stream() {
        let data = ContentStreamBuilder::new().build();
        assert!(data.is_empty());
    }

    #[test]
    fn builder_begin_end_text() {
        let data = build(|b| {
            b.begin_text().end_text();
        });
        assert_eq!(data, b"BT\nET\n");
    }

    #[test]
    fn builder_set_font() {
        let data = build(|b| {
            b.set_font("F1", 12.0);
        });
        assert_eq!(data, b"/F1 12 Tf\n");
    }

    #[test]
    fn builder_text_position() {
        let data = build(|b| {
            b.move_to(100.0, 700.0);
        });
        assert_eq!(data, b"100 700 Td\n");
    }

    #[test]
    fn builder_show_text() {
        let data = build(|b| {
            b.show_text("Hello");
        });
        assert_eq!(data, b"(Hello) Tj\n");
    }

    #[test]
    fn builder_show_text_escaping() {
        let data = build(|b| {
            b.show_text("a(b)c\\d");
        });
        assert_eq!(data, b"(a\\(b\\)c\\\\d) Tj\n");
    }

    #[test]
    fn builder_show_text_adjusted() {
        let data = build(|b| {
            b.show_text_adjusted(&[
                TextItem::Text("Hello"),
                TextItem::Spacing(-100),
                TextItem::Text("World"),
            ]);
        });
        assert_eq!(data, b"[(Hello) -100 (World)] TJ\n");
    }

    #[test]
    fn builder_save_restore() {
        let data = build(|b| {
            b.save_state().restore_state();
        });
        assert_eq!(data, b"q\nQ\n");
    }

    #[test]
    fn builder_set_transform() {
        let data = build(|b| {
            b.set_transform(1.0, 0.0, 0.0, 1.0, 50.0, 50.0);
        });
        assert_eq!(data, b"1 0 0 1 50 50 cm\n");
    }

    #[test]
    fn builder_fill_color_rgb() {
        let data = build(|b| {
            b.set_fill_color_rgb(1.0, 0.0, 0.0);
        });
        assert_eq!(data, b"1 0 0 rg\n");
    }

    #[test]
    fn builder_stroke_color_rgb() {
        let data = build(|b| {
            b.set_stroke_color_rgb(1.0, 0.0, 0.0);
        });
        assert_eq!(data, b"1 0 0 RG\n");
    }

    #[test]
    fn builder_fill_color_gray() {
        let data = build(|b| {
            b.set_fill_color_gray(0.5);
        });
        assert_eq!(data, b"0.5 g\n");
    }

    #[test]
    fn builder_stroke_color_gray() {
        let data = build(|b| {
            b.set_stroke_color_gray(0.5);
        });
        assert_eq!(data, b"0.5 G\n");
    }

    #[test]
    fn builder_rect() {
        let data = build(|b| {
            b.rect(10.0, 20.0, 100.0, 50.0);
        });
        assert_eq!(data, b"10 20 100 50 re\n");
    }

    #[test]
    fn builder_line_path() {
        let data = build(|b| {
            b.move_line_to(0.0, 0.0)
                .line_to(100.0, 100.0)
                .close_path()
                .stroke();
        });
        assert_eq!(data, b"0 0 m\n100 100 l\nh\nS\n");
    }

    #[test]
    fn builder_draw_image() {
        let data = build(|b| {
            b.draw_image("Im1");
        });
        assert_eq!(data, b"/Im1 Do\n");
    }

    #[test]
    fn builder_set_line_width() {
        let data = build(|b| {
            b.set_line_width(2.0);
        });
        assert_eq!(data, b"2 w\n");
    }

    #[test]
    fn builder_roundtrip() {
        let data = build(|b| {
            b.save_state()
                .set_transform(1.0, 0.0, 0.0, 1.0, 50.0, 50.0)
                .begin_text()
                .set_font("F1", 12.0)
                .move_to(100.0, 700.0)
                .show_text("Hello World")
                .end_text()
                .restore_state();
        });

        let tokens = tokenize_content_stream(&data).unwrap();

        // q
        assert_eq!(tokens[0], ContentToken::Operator("q".to_string()));
        // 1 0 0 1 50 50 cm
        assert_eq!(tokens[1], ContentToken::Operand(Object::Integer(1)));
        assert_eq!(tokens[7], ContentToken::Operator("cm".to_string()));
        // BT
        assert_eq!(tokens[8], ContentToken::Operator("BT".to_string()));
        // /F1 12 Tf
        assert_eq!(
            tokens[9],
            ContentToken::Operand(Object::Name(PdfName::new("F1")))
        );
        assert_eq!(tokens[10], ContentToken::Operand(Object::Integer(12)));
        assert_eq!(tokens[11], ContentToken::Operator("Tf".to_string()));
        // 100 700 Td
        assert_eq!(tokens[12], ContentToken::Operand(Object::Integer(100)));
        assert_eq!(tokens[13], ContentToken::Operand(Object::Integer(700)));
        assert_eq!(tokens[14], ContentToken::Operator("Td".to_string()));
        // (Hello World) Tj
        assert_eq!(tokens[16], ContentToken::Operator("Tj".to_string()));
        // ET
        assert_eq!(tokens[17], ContentToken::Operator("ET".to_string()));
        // Q
        assert_eq!(tokens[18], ContentToken::Operator("Q".to_string()));
    }

    #[test]
    fn builder_full_page_content() {
        let data = build(|b| {
            b.begin_text()
                .set_font("F1", 12.0)
                .move_to(72.0, 720.0)
                .show_text("Hello World")
                .end_text();
        });

        let extracted = extract_text_from_content(&data).unwrap();
        assert!(extracted.contains("Hello World"));
    }

    #[test]
    fn builder_fractional_numbers() {
        let data = build(|b| {
            b.move_to(72.5, 100.25);
        });
        assert_eq!(data, b"72.5 100.25 Td\n");
    }

    #[test]
    fn builder_negative_numbers() {
        let data = build(|b| {
            b.move_to(-10.0, -20.0);
        });
        assert_eq!(data, b"-10 -20 Td\n");
    }

    #[test]
    fn builder_fill_operator() {
        let data = build(|b| {
            b.rect(0.0, 0.0, 100.0, 100.0).fill();
        });
        assert_eq!(data, b"0 0 100 100 re\nf\n");
    }

    #[test]
    fn builder_fill_and_stroke() {
        let data = build(|b| {
            b.rect(0.0, 0.0, 100.0, 100.0).fill_and_stroke();
        });
        assert_eq!(data, b"0 0 100 100 re\nB\n");
    }

    #[test]
    fn builder_text_state_operators() {
        let data = build(|b| {
            b.set_character_spacing(0.5)
                .set_word_spacing(1.0)
                .set_leading(14.0);
        });
        assert_eq!(data, b"0.5 Tc\n1 Tw\n14 TL\n");
    }

    #[test]
    fn builder_next_line() {
        let data = build(|b| {
            b.next_line();
        });
        assert_eq!(data, b"T*\n");
    }

    #[test]
    fn builder_text_matrix() {
        let data = build(|b| {
            b.set_text_matrix(1.0, 0.0, 0.0, 1.0, 72.0, 720.0);
        });
        assert_eq!(data, b"1 0 0 1 72 720 Tm\n");
    }

    #[test]
    fn builder_name_escaping() {
        let data = build(|b| {
            b.set_font("F#1", 12.0);
        });
        assert_eq!(data, b"/F#231 12 Tf\n");
    }
}
