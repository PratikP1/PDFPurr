//! PDF object types as defined in ISO 32000-2:2020, Section 7.3.
//!
//! PDF documents are composed of these fundamental object types:
//! - Boolean (`true` / `false`)
//! - Integer (whole numbers)
//! - Real (floating-point numbers)
//! - String (literal or hexadecimal)
//! - Name (unique identifiers beginning with `/`)
//! - Array (ordered collections)
//! - Dictionary (key-value maps)
//! - Stream (dictionary + binary data)
//! - Null
//! - Indirect Reference (pointer to another object)

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

/// A unique identifier for an indirect object: (object number, generation number).
pub type ObjectId = (u32, u16);

/// A PDF Name object (ISO 32000-2:2020, Section 7.3.5).
///
/// Names are atomic symbols uniquely defined by a sequence of bytes.
/// In PDF syntax they are written as `/Name`. The leading `/` is not
/// part of the name itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PdfName(pub String);

impl PdfName {
    /// Creates a new PDF Name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PdfName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0)
    }
}

impl Borrow<str> for PdfName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PdfName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Indicates whether a PDF string was encoded as literal or hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringFormat {
    /// Literal string: `(Hello World)`
    Literal,
    /// Hexadecimal string: `<48656C6C6F>`
    Hexadecimal,
}

/// A PDF String object (ISO 32000-2:2020, Section 7.3.4).
///
/// PDF strings are sequences of bytes. They can be encoded as
/// literal strings `(...)` or hexadecimal strings `<...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfString {
    /// The raw bytes of the string.
    pub bytes: Vec<u8>,
    /// How the string was originally encoded in the PDF file.
    pub format: StringFormat,
}

impl PdfString {
    /// Creates a new literal PDF string from UTF-8 text.
    pub fn from_literal(text: impl Into<String>) -> Self {
        Self {
            bytes: text.into().into_bytes(),
            format: StringFormat::Literal,
        }
    }

    /// Creates a new hexadecimal PDF string from raw bytes.
    pub fn from_hex(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            format: StringFormat::Hexadecimal,
        }
    }

    /// Creates a PDF string from raw bytes with the given format.
    pub fn from_bytes(bytes: Vec<u8>, format: StringFormat) -> Self {
        Self { bytes, format }
    }

    /// Attempts to interpret the string as UTF-8 text.
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }
}

impl fmt::Display for PdfString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.format {
            StringFormat::Literal => {
                if let Some(text) = self.as_text() {
                    write!(f, "({})", text)
                } else {
                    write!(f, "<")?;
                    write_hex(f, &self.bytes)?;
                    write!(f, ">")
                }
            }
            StringFormat::Hexadecimal => {
                write!(f, "<")?;
                write_hex(f, &self.bytes)?;
                write!(f, ">")
            }
        }
    }
}

/// An indirect reference to another object (ISO 32000-2:2020, Section 7.3.10).
///
/// Written as `{object_number} {generation} R` in PDF syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndirectRef {
    /// The object number.
    pub object_number: u32,
    /// The generation number.
    pub generation: u16,
}

impl IndirectRef {
    /// Creates a new indirect reference.
    pub fn new(object_number: u32, generation: u16) -> Self {
        Self {
            object_number,
            generation,
        }
    }

    /// Returns the ObjectId tuple for this reference.
    pub fn id(&self) -> ObjectId {
        (self.object_number, self.generation)
    }
}

impl fmt::Display for IndirectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.object_number, self.generation)
    }
}

/// A PDF Dictionary (ISO 32000-2:2020, Section 7.3.7).
///
/// An associative map from Name keys to Object values.
/// Uses BTreeMap for deterministic key ordering.
pub type Dictionary = BTreeMap<PdfName, Object>;

/// Extension methods for `Dictionary` to simplify common access patterns.
pub trait DictExt {
    /// Gets a value by string key, avoiding `PdfName::new()` at every call site.
    fn get_str(&self, key: &str) -> Option<&Object>;

    /// Gets a Name value by key, returning the name string.
    fn get_name(&self, key: &str) -> Option<&str>;

    /// Gets an integer value by key.
    fn get_i64(&self, key: &str) -> Option<i64>;

    /// Gets a float value by key (works for both Integer and Real).
    fn get_f64(&self, key: &str) -> Option<f64>;

    /// Gets a text string value by key (extracts UTF-8 text from a String object).
    fn get_text(&self, key: &str) -> Option<String>;
}

impl DictExt for Dictionary {
    fn get_str(&self, key: &str) -> Option<&Object> {
        self.get(key)
    }

    fn get_name(&self, key: &str) -> Option<&str> {
        self.get_str(key)?.as_name()
    }

    fn get_i64(&self, key: &str) -> Option<i64> {
        self.get_str(key)?.as_i64()
    }

    fn get_f64(&self, key: &str) -> Option<f64> {
        self.get_str(key)?.as_f64()
    }

    fn get_text(&self, key: &str) -> Option<String> {
        self.get_str(key)?.as_text_string()
    }
}

/// Decodes a UTF-16BE byte slice into a Unicode string.
///
/// This is the standard encoding for multi-byte strings in PDF.
/// Uses `chunks_exact(2)` to safely handle the byte pairs; any
/// trailing byte is silently ignored.
pub fn decode_utf16be(bytes: &[u8]) -> Option<String> {
    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&u16s).ok()
}

/// A PDF Stream object (ISO 32000-2:2020, Section 7.3.8).
///
/// A stream consists of a dictionary describing the stream's properties
/// (at minimum, the `/Length` entry) followed by the raw stream data.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfStream {
    /// The stream dictionary containing metadata (e.g., `/Length`, `/Filter`).
    pub dict: Dictionary,
    /// The raw (potentially compressed) stream data.
    pub data: Vec<u8>,
}

impl PdfStream {
    /// Creates a new stream with the given dictionary and data.
    pub fn new(dict: Dictionary, data: Vec<u8>) -> Self {
        Self { dict, data }
    }

    /// Returns the value of the `/Filter` entry, if present.
    pub fn filter(&self) -> Option<&Object> {
        self.dict.get(&PdfName::new("Filter"))
    }

    /// Returns the raw data length.
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Decodes the stream data by applying the filter pipeline from the
    /// stream dictionary. Returns the raw data if no filters are present.
    pub fn decode_data(&self) -> crate::error::PdfResult<Vec<u8>> {
        crate::core::filters::decode_stream(&self.data, &self.dict)
    }
}

/// The fundamental PDF object type (ISO 32000-2:2020, Section 7.3).
///
/// Every value in a PDF document is one of these types.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// Boolean value: `true` or `false`
    Boolean(bool),
    /// Integer number
    Integer(i64),
    /// Real (floating-point) number
    Real(f64),
    /// String (literal or hexadecimal)
    String(PdfString),
    /// Name (e.g., `/Type`, `/Pages`)
    Name(PdfName),
    /// Ordered collection of objects
    Array(Vec<Object>),
    /// Key-value map (Name -> Object)
    Dictionary(Dictionary),
    /// Dictionary + binary data
    Stream(PdfStream),
    /// Null object
    Null,
    /// Reference to an indirect object
    Reference(IndirectRef),
}

impl Object {
    // --- Type checking ---

    /// Returns `true` if the object is a Boolean.
    pub fn is_boolean(&self) -> bool {
        matches!(self, Object::Boolean(_))
    }

    /// Returns `true` if the object is an Integer.
    pub fn is_integer(&self) -> bool {
        matches!(self, Object::Integer(_))
    }

    /// Returns `true` if the object is a Real.
    pub fn is_real(&self) -> bool {
        matches!(self, Object::Real(_))
    }

    /// Returns `true` if the object is numeric (Integer or Real).
    pub fn is_number(&self) -> bool {
        matches!(self, Object::Integer(_) | Object::Real(_))
    }

    /// Returns `true` if the object is a String.
    pub fn is_string(&self) -> bool {
        matches!(self, Object::String(_))
    }

    /// Returns `true` if the object is a Name.
    pub fn is_name(&self) -> bool {
        matches!(self, Object::Name(_))
    }

    /// Returns `true` if the object is an Array.
    pub fn is_array(&self) -> bool {
        matches!(self, Object::Array(_))
    }

    /// Returns `true` if the object is a Dictionary.
    pub fn is_dictionary(&self) -> bool {
        matches!(self, Object::Dictionary(_))
    }

    /// Returns `true` if the object is a Stream.
    pub fn is_stream(&self) -> bool {
        matches!(self, Object::Stream(_))
    }

    /// Returns `true` if the object is Null.
    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }

    /// Returns `true` if the object is a Reference.
    pub fn is_reference(&self) -> bool {
        matches!(self, Object::Reference(_))
    }

    // --- Value extraction ---

    /// Extracts the boolean value, if this is a Boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Extracts the integer value, if this is an Integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Extracts a floating-point value from Integer or Real.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Integer(n) => Some(*n as f64),
            Object::Real(n) => Some(*n),
            _ => None,
        }
    }

    /// Extracts the string value, if this is a String.
    pub fn as_pdf_string(&self) -> Option<&PdfString> {
        match self {
            Object::String(s) => Some(s),
            _ => None,
        }
    }

    /// Extracts text from a String object, handling both UTF-8 and UTF-16BE.
    ///
    /// PDF text strings may be encoded as PDFDocEncoding (≈ Latin-1, attempted
    /// as UTF-8 here) or UTF-16BE (prefixed with BOM `0xFE 0xFF`).
    /// Returns `None` if this is not a String or if decoding fails.
    pub fn as_text_string(&self) -> Option<String> {
        let s = self.as_pdf_string()?;
        if s.bytes.starts_with(&[0xFE, 0xFF]) {
            decode_utf16be(&s.bytes[2..])
        } else {
            s.as_text().map(String::from)
        }
    }

    /// Parses a rectangle array `[x1, y1, x2, y2]` from an Array object.
    ///
    /// Returns `None` if this is not an Array, has fewer than 4 elements,
    /// or if any element cannot be converted to `f64`.
    pub fn parse_rect(&self) -> Option<[f64; 4]> {
        let arr = self.as_array()?;
        if arr.len() < 4 {
            return None;
        }
        Some([
            arr[0].as_f64()?,
            arr[1].as_f64()?,
            arr[2].as_f64()?,
            arr[3].as_f64()?,
        ])
    }

    /// Extracts the name value, if this is a Name.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Object::Name(n) => Some(n.as_str()),
            _ => None,
        }
    }

    /// Extracts the array, if this is an Array.
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Extracts the dictionary, if this is a Dictionary.
    pub fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    /// Extracts the stream, if this is a Stream.
    pub fn as_stream(&self) -> Option<&PdfStream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    /// Extracts the indirect reference, if this is a Reference.
    pub fn as_reference(&self) -> Option<&IndirectRef> {
        match self {
            Object::Reference(r) => Some(r),
            _ => None,
        }
    }

    /// Returns a type name string for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Object::Boolean(_) => "Boolean",
            Object::Integer(_) => "Integer",
            Object::Real(_) => "Real",
            Object::String(_) => "String",
            Object::Name(_) => "Name",
            Object::Array(_) => "Array",
            Object::Dictionary(_) => "Dictionary",
            Object::Stream(_) => "Stream",
            Object::Null => "Null",
            Object::Reference(_) => "Reference",
        }
    }

    /// Writes this object in spec-compliant PDF syntax.
    ///
    /// This produces output suitable for inclusion in a PDF file,
    /// unlike `Display` which is for debug/human-readable output.
    pub fn write_pdf(&self, w: &mut dyn Write) -> io::Result<()> {
        match self {
            Object::Boolean(b) => write!(w, "{}", b),
            Object::Integer(n) => write!(w, "{}", n),
            Object::Real(n) => {
                // Avoid trailing zeros but ensure at least one decimal digit
                if *n == n.floor() && n.abs() < 1e15 {
                    write!(w, "{:.1}", n)
                } else {
                    write!(w, "{}", n)
                }
            }
            Object::String(s) => write_pdf_string(w, s),
            Object::Name(n) => write_pdf_name(w, n),
            Object::Array(arr) => {
                w.write_all(b"[")?;
                for (i, obj) in arr.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b" ")?;
                    }
                    obj.write_pdf(w)?;
                }
                w.write_all(b"]")
            }
            Object::Dictionary(dict) => write_pdf_dict(w, dict),
            Object::Stream(stream) => {
                // Write the dictionary with /Length set to data length
                let mut dict = stream.dict.clone();
                dict.insert(
                    PdfName::new("Length"),
                    Object::Integer(stream.data.len() as i64),
                );
                write_pdf_dict(w, &dict)?;
                w.write_all(b"\nstream\r\n")?;
                w.write_all(&stream.data)?;
                w.write_all(b"\r\nendstream")
            }
            Object::Null => w.write_all(b"null"),
            Object::Reference(r) => write!(w, "{} {} R", r.object_number, r.generation),
        }
    }
}

/// Writes a dictionary in PDF syntax.
fn write_pdf_dict(w: &mut dyn Write, dict: &Dictionary) -> io::Result<()> {
    w.write_all(b"<<")?;
    for (key, val) in dict {
        w.write_all(b" ")?;
        write_pdf_name(w, key)?;
        w.write_all(b" ")?;
        val.write_pdf(w)?;
    }
    w.write_all(b" >>")
}

/// Writes a PDF Name, escaping special characters with `#xx`.
fn write_pdf_name(w: &mut dyn Write, name: &PdfName) -> io::Result<()> {
    w.write_all(b"/")?;
    for &b in name.as_str().as_bytes() {
        // Escape delimiter, whitespace, '#', and non-printable bytes
        if !(b'!'..=b'~').contains(&b)
            || b == b'#'
            || b == b'/'
            || b == b'%'
            || b == b'('
            || b == b')'
            || b == b'<'
            || b == b'>'
            || b == b'['
            || b == b']'
            || b == b'{'
            || b == b'}'
        {
            write!(w, "#{:02X}", b)?;
        } else {
            w.write_all(&[b])?;
        }
    }
    Ok(())
}

/// Writes a PDF string with proper escaping.
fn write_pdf_string(w: &mut dyn Write, s: &PdfString) -> io::Result<()> {
    // Use hex encoding for binary data, literal for text
    if s.bytes
        .iter()
        .any(|&b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
        || s.format == StringFormat::Hexadecimal
    {
        w.write_all(b"<")?;
        for &b in &s.bytes {
            write!(w, "{:02X}", b)?;
        }
        w.write_all(b">")
    } else {
        w.write_all(b"(")?;
        for &b in &s.bytes {
            match b {
                b'(' => w.write_all(b"\\(")?,
                b')' => w.write_all(b"\\)")?,
                b'\\' => w.write_all(b"\\\\")?,
                _ => w.write_all(&[b])?,
            }
        }
        w.write_all(b")")
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Object::Boolean(b) => write!(f, "{}", b),
            Object::Integer(n) => write!(f, "{}", n),
            Object::Real(n) => {
                if *n == n.floor() {
                    write!(f, "{:.1}", n)
                } else {
                    write!(f, "{}", n)
                }
            }
            Object::String(s) => write!(f, "{}", s),
            Object::Name(n) => write!(f, "{}", n),
            Object::Array(arr) => {
                write!(f, "[")?;
                for (i, obj) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", obj)?;
                }
                write!(f, "]")
            }
            Object::Dictionary(dict) => {
                write!(f, "<< ")?;
                for (key, val) in dict {
                    write!(f, "{} {} ", key, val)?;
                }
                write!(f, ">>")
            }
            Object::Stream(stream) => {
                write!(f, "<< ")?;
                for (key, val) in &stream.dict {
                    write!(f, "{} {} ", key, val)?;
                }
                write!(f, ">> stream[{} bytes]", stream.data.len())
            }
            Object::Null => write!(f, "null"),
            Object::Reference(r) => write!(f, "{}", r),
        }
    }
}

// --- Convenience From impls ---

impl From<bool> for Object {
    fn from(b: bool) -> Self {
        Object::Boolean(b)
    }
}

impl From<i64> for Object {
    fn from(n: i64) -> Self {
        Object::Integer(n)
    }
}

impl From<i32> for Object {
    fn from(n: i32) -> Self {
        Object::Integer(n as i64)
    }
}

impl From<f64> for Object {
    fn from(n: f64) -> Self {
        Object::Real(n)
    }
}

impl From<PdfName> for Object {
    fn from(name: PdfName) -> Self {
        Object::Name(name)
    }
}

impl From<PdfString> for Object {
    fn from(s: PdfString) -> Self {
        Object::String(s)
    }
}

impl From<Vec<Object>> for Object {
    fn from(arr: Vec<Object>) -> Self {
        Object::Array(arr)
    }
}

impl From<Dictionary> for Object {
    fn from(dict: Dictionary) -> Self {
        Object::Dictionary(dict)
    }
}

impl From<PdfStream> for Object {
    fn from(stream: PdfStream) -> Self {
        Object::Stream(stream)
    }
}

impl From<IndirectRef> for Object {
    fn from(r: IndirectRef) -> Self {
        Object::Reference(r)
    }
}

/// Writes bytes as lowercase hex into a formatter.
fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        write!(f, "{:02x}", b)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- PdfName tests ---

    #[test]
    fn name_creation_and_display() {
        let name = PdfName::new("Type");
        assert_eq!(name.as_str(), "Type");
        assert_eq!(format!("{}", name), "/Type");
    }

    #[test]
    fn name_from_str() {
        let name: PdfName = "Pages".into();
        assert_eq!(name.as_str(), "Pages");
    }

    #[test]
    fn name_ordering_is_deterministic() {
        let a = PdfName::new("A");
        let b = PdfName::new("B");
        assert!(a < b);
    }

    // --- PdfString tests ---

    #[test]
    fn literal_string_creation() {
        let s = PdfString::from_literal("Hello World");
        assert_eq!(s.as_text(), Some("Hello World"));
        assert_eq!(s.format, StringFormat::Literal);
        assert_eq!(format!("{}", s), "(Hello World)");
    }

    #[test]
    fn hex_string_creation() {
        let s = PdfString::from_hex(vec![0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        assert_eq!(s.format, StringFormat::Hexadecimal);
        assert_eq!(format!("{}", s), "<48656c6c6f>");
    }

    #[test]
    fn string_with_non_utf8_bytes() {
        let s = PdfString::from_hex(vec![0xFF, 0xFE]);
        assert_eq!(s.as_text(), None);
    }

    // --- IndirectRef tests ---

    #[test]
    fn indirect_ref_creation() {
        let r = IndirectRef::new(10, 0);
        assert_eq!(r.object_number, 10);
        assert_eq!(r.generation, 0);
        assert_eq!(r.id(), (10, 0));
        assert_eq!(format!("{}", r), "10 0 R");
    }

    // --- PdfStream tests ---

    #[test]
    fn stream_creation() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Length"), Object::Integer(5));
        let stream = PdfStream::new(dict, vec![1, 2, 3, 4, 5]);
        assert_eq!(stream.data_len(), 5);
    }

    #[test]
    fn stream_filter_lookup() {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("FlateDecode")),
        );
        dict.insert(PdfName::new("Length"), Object::Integer(100));
        let stream = PdfStream::new(dict, vec![0; 100]);
        let filter = stream.filter().unwrap();
        assert_eq!(filter.as_name(), Some("FlateDecode"));
    }

    #[test]
    fn stream_without_filter() {
        let dict = Dictionary::new();
        let stream = PdfStream::new(dict, vec![]);
        assert!(stream.filter().is_none());
    }

    // --- Object type checking tests ---

    #[test]
    fn object_type_checks() {
        assert!(Object::Boolean(true).is_boolean());
        assert!(Object::Integer(42).is_integer());
        assert!(Object::Real(3.14).is_real());
        assert!(Object::Integer(42).is_number());
        assert!(Object::Real(3.14).is_number());
        assert!(Object::String(PdfString::from_literal("hi")).is_string());
        assert!(Object::Name(PdfName::new("Type")).is_name());
        assert!(Object::Array(vec![]).is_array());
        assert!(Object::Dictionary(Dictionary::new()).is_dictionary());
        assert!(Object::Stream(PdfStream::new(Dictionary::new(), vec![])).is_stream());
        assert!(Object::Null.is_null());
        assert!(Object::Reference(IndirectRef::new(1, 0)).is_reference());
    }

    #[test]
    fn object_negative_type_checks() {
        let obj = Object::Integer(42);
        assert!(!obj.is_boolean());
        assert!(!obj.is_real());
        assert!(!obj.is_string());
        assert!(!obj.is_name());
        assert!(!obj.is_array());
        assert!(!obj.is_dictionary());
        assert!(!obj.is_stream());
        assert!(!obj.is_null());
        assert!(!obj.is_reference());
    }

    // --- Object value extraction tests ---

    #[test]
    fn extract_bool() {
        assert_eq!(Object::Boolean(true).as_bool(), Some(true));
        assert_eq!(Object::Integer(1).as_bool(), None);
    }

    #[test]
    fn extract_integer() {
        assert_eq!(Object::Integer(42).as_i64(), Some(42));
        assert_eq!(Object::Real(42.0).as_i64(), None);
    }

    #[test]
    fn extract_f64_from_integer_and_real() {
        assert_eq!(Object::Integer(42).as_f64(), Some(42.0));
        assert_eq!(Object::Real(3.14).as_f64(), Some(3.14));
        assert_eq!(Object::Null.as_f64(), None);
    }

    #[test]
    fn extract_name() {
        let obj = Object::Name(PdfName::new("Type"));
        assert_eq!(obj.as_name(), Some("Type"));
        assert_eq!(Object::Null.as_name(), None);
    }

    #[test]
    fn extract_array() {
        let arr = vec![Object::Integer(1), Object::Integer(2)];
        let obj = Object::Array(arr);
        assert_eq!(obj.as_array().unwrap().len(), 2);
        assert_eq!(Object::Null.as_array(), None);
    }

    #[test]
    fn extract_dict() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        let obj = Object::Dictionary(dict);
        assert!(obj.as_dict().is_some());
        assert_eq!(Object::Null.as_dict(), None);
    }

    #[test]
    fn extract_reference() {
        let obj = Object::Reference(IndirectRef::new(5, 0));
        let r = obj.as_reference().unwrap();
        assert_eq!(r.object_number, 5);
        assert_eq!(Object::Null.as_reference(), None);
    }

    // --- Object type_name tests ---

    #[test]
    fn type_names() {
        assert_eq!(Object::Boolean(true).type_name(), "Boolean");
        assert_eq!(Object::Integer(0).type_name(), "Integer");
        assert_eq!(Object::Real(0.0).type_name(), "Real");
        assert_eq!(
            Object::String(PdfString::from_literal("")).type_name(),
            "String"
        );
        assert_eq!(Object::Name(PdfName::new("")).type_name(), "Name");
        assert_eq!(Object::Array(vec![]).type_name(), "Array");
        assert_eq!(
            Object::Dictionary(Dictionary::new()).type_name(),
            "Dictionary"
        );
        assert_eq!(
            Object::Stream(PdfStream::new(Dictionary::new(), vec![])).type_name(),
            "Stream"
        );
        assert_eq!(Object::Null.type_name(), "Null");
        assert_eq!(
            Object::Reference(IndirectRef::new(1, 0)).type_name(),
            "Reference"
        );
    }

    // --- Display tests ---

    #[test]
    fn display_boolean() {
        assert_eq!(format!("{}", Object::Boolean(true)), "true");
        assert_eq!(format!("{}", Object::Boolean(false)), "false");
    }

    #[test]
    fn display_integer() {
        assert_eq!(format!("{}", Object::Integer(42)), "42");
        assert_eq!(format!("{}", Object::Integer(-7)), "-7");
    }

    #[test]
    fn display_real() {
        assert_eq!(format!("{}", Object::Real(3.14)), "3.14");
        assert_eq!(format!("{}", Object::Real(1.0)), "1.0");
    }

    #[test]
    fn display_null() {
        assert_eq!(format!("{}", Object::Null), "null");
    }

    #[test]
    fn display_reference() {
        assert_eq!(
            format!("{}", Object::Reference(IndirectRef::new(10, 2))),
            "10 2 R"
        );
    }

    #[test]
    fn display_array() {
        let arr = Object::Array(vec![
            Object::Integer(1),
            Object::Integer(2),
            Object::Integer(3),
        ]);
        assert_eq!(format!("{}", arr), "[1 2 3]");
    }

    #[test]
    fn display_dictionary() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        let obj = Object::Dictionary(dict);
        assert_eq!(format!("{}", obj), "<< /Type /Page >>");
    }

    // --- write_pdf tests ---

    fn write_pdf_to_string(obj: &Object) -> String {
        let mut buf = Vec::new();
        obj.write_pdf(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn write_pdf_boolean() {
        assert_eq!(write_pdf_to_string(&Object::Boolean(true)), "true");
        assert_eq!(write_pdf_to_string(&Object::Boolean(false)), "false");
    }

    #[test]
    fn write_pdf_integer() {
        assert_eq!(write_pdf_to_string(&Object::Integer(42)), "42");
        assert_eq!(write_pdf_to_string(&Object::Integer(-7)), "-7");
    }

    #[test]
    fn write_pdf_real() {
        assert_eq!(write_pdf_to_string(&Object::Real(3.14)), "3.14");
        assert_eq!(write_pdf_to_string(&Object::Real(1.0)), "1.0");
    }

    #[test]
    fn write_pdf_null() {
        assert_eq!(write_pdf_to_string(&Object::Null), "null");
    }

    #[test]
    fn write_pdf_reference() {
        assert_eq!(
            write_pdf_to_string(&Object::Reference(IndirectRef::new(10, 2))),
            "10 2 R"
        );
    }

    #[test]
    fn write_pdf_name() {
        assert_eq!(
            write_pdf_to_string(&Object::Name(PdfName::new("Type"))),
            "/Type"
        );
    }

    #[test]
    fn write_pdf_string_literal() {
        let s = PdfString::from_literal("Hello World");
        assert_eq!(write_pdf_to_string(&Object::String(s)), "(Hello World)");
    }

    #[test]
    fn write_pdf_string_escaping() {
        let s = PdfString::from_literal("a(b)c\\d");
        assert_eq!(write_pdf_to_string(&Object::String(s)), "(a\\(b\\)c\\\\d)");
    }

    #[test]
    fn write_pdf_string_hex() {
        let s = PdfString::from_hex(vec![0xFF, 0x00, 0xAB]);
        assert_eq!(write_pdf_to_string(&Object::String(s)), "<FF00AB>");
    }

    #[test]
    fn write_pdf_array() {
        let arr = Object::Array(vec![Object::Integer(1), Object::Name(PdfName::new("Foo"))]);
        assert_eq!(write_pdf_to_string(&arr), "[1 /Foo]");
    }

    #[test]
    fn write_pdf_dictionary() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        let obj = Object::Dictionary(dict);
        assert_eq!(write_pdf_to_string(&obj), "<< /Type /Page >>");
    }

    #[test]
    fn write_pdf_stream() {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("FlateDecode")),
        );
        let stream = PdfStream::new(dict, b"raw data".to_vec());
        let output = write_pdf_to_string(&Object::Stream(stream));
        // Should contain stream keyword and endstream
        assert!(output.contains("stream\r\n"));
        assert!(output.contains("\r\nendstream"));
        // Should contain /Length
        assert!(output.contains("/Length 8"));
    }

    // --- as_text_string tests ---

    #[test]
    fn as_text_string_from_literal() {
        let obj = Object::String(PdfString::from_literal("Hello"));
        assert_eq!(obj.as_text_string(), Some("Hello".to_string()));
    }

    #[test]
    fn as_text_string_from_non_string() {
        assert_eq!(Object::Integer(42).as_text_string(), None);
        assert_eq!(Object::Null.as_text_string(), None);
    }

    #[test]
    fn as_text_string_non_utf8() {
        let obj = Object::String(PdfString::from_hex(vec![0xFF, 0xFE]));
        assert_eq!(obj.as_text_string(), None);
    }

    #[test]
    fn as_text_string_utf16be() {
        // UTF-16BE BOM + "Hi" (U+0048, U+0069)
        let bytes = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        let obj = Object::String(PdfString::from_hex(bytes));
        assert_eq!(obj.as_text_string(), Some("Hi".to_string()));
    }

    // --- parse_rect tests ---

    #[test]
    fn parse_rect_from_array() {
        let obj = Object::Array(vec![
            Object::Real(10.0),
            Object::Real(20.0),
            Object::Real(100.0),
            Object::Real(200.0),
        ]);
        assert_eq!(obj.parse_rect(), Some([10.0, 20.0, 100.0, 200.0]));
    }

    #[test]
    fn parse_rect_from_integers() {
        let obj = Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]);
        assert_eq!(obj.parse_rect(), Some([0.0, 0.0, 612.0, 792.0]));
    }

    #[test]
    fn parse_rect_too_short() {
        let obj = Object::Array(vec![Object::Integer(1), Object::Integer(2)]);
        assert_eq!(obj.parse_rect(), None);
    }

    #[test]
    fn parse_rect_non_array() {
        assert_eq!(Object::Integer(42).parse_rect(), None);
    }

    // --- DictExt::get_text tests ---

    #[test]
    fn dict_get_text() {
        let mut dict = Dictionary::new();
        dict.insert(
            PdfName::new("Title"),
            Object::String(PdfString::from_literal("My Doc")),
        );
        assert_eq!(dict.get_text("Title"), Some("My Doc".to_string()));
        assert_eq!(dict.get_text("Author"), None);
    }

    #[test]
    fn dict_get_text_non_string() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        assert_eq!(dict.get_text("Type"), None);
    }

    // --- From impl tests ---

    #[test]
    fn from_impls() {
        let _: Object = true.into();
        let _: Object = 42i64.into();
        let _: Object = 42i32.into();
        let _: Object = 3.14f64.into();
        let _: Object = PdfName::new("Type").into();
        let _: Object = PdfString::from_literal("hi").into();
        let _: Object = vec![Object::Null].into();
        let _: Object = Dictionary::new().into();
        let _: Object = IndirectRef::new(1, 0).into();
    }
}
