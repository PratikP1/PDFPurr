//! # PDFPurr 🐱📄
//!
//! **The Ultimate Rust PDF Library**
//!
//! PDFPurr is a comprehensive, standards-compliant, and accessibility-focused PDF library
//! written in pure Rust. It provides everything you need to work with PDF files:
//! reading, writing, editing, and rendering.
//!
//! ## Features
//!
//! - **Comprehensive**: Full-featured PDF manipulation
//! - **Standards-Compliant**: Support for PDF 2.0, PDF/A, PDF/UA, PDF/X
//! - **Accessible**: First-class support for accessibility (PDF/UA)
//! - **Memory-Safe**: Pure Rust implementation
//! - **High-Performance**: Optimized for speed and low memory usage
//!
//! ## Project Status
//!
//! ⚠️ **Early Development**: PDFPurr is currently in the foundation phase.
//! The API is not stable and breaking changes are expected.
//!
//! ## Quick Start
//!
//! ```
//! use pdfpurr::Document;
//!
//! // Create a new PDF document
//! let mut doc = Document::new();
//! doc.add_page(612.0, 792.0).unwrap();
//! let bytes = doc.to_bytes().unwrap();
//!
//! // Parse it back
//! let doc = Document::from_bytes(&bytes).unwrap();
//! assert_eq!(doc.page_count().unwrap(), 1);
//! ```
//!
//! ## Architecture
//!
//! PDFPurr is organized into several core modules:
//!
//! - [`core`]: Low-level PDF primitives and object model
//! - [`parser`]: PDF file parsing and lexical analysis
//! - [`content`]: Content stream processing
//! - [`fonts`]: Font handling and embedding
//! - [`images`]: Image processing and extraction
//! - [`encryption`]: Security and encryption
//! - [`forms`]: Form handling (AcroForms)
//! - [`structure`]: Outlines, annotations, and metadata
//! - [`accessibility`]: PDF/UA and tagged PDF support
//! - [`standards`]: Standards compliance (PDF/A, PDF/X, etc.)
//!
//! ## Examples
//!
//! See the `examples/` directory for comprehensive examples.
//!
//! ## Documentation
//!
//! For more information, see:
//! - [README.md](https://github.com/PratikP1/Research-Private/blob/main/PDFPurr/README.md)
//! - [RESEARCH_REPORT.md](https://github.com/PratikP1/Research-Private/blob/main/PDFPurr/RESEARCH_REPORT.md)

#![deny(missing_docs)]
#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

// Core modules (always available)
pub mod accessibility;
pub mod content;
pub mod core;
pub mod document;
pub mod encryption;
pub mod error;
pub mod fonts;
pub mod forms;
pub mod images;
pub mod page_builder;
pub mod parser;
pub mod signatures;
pub mod standards;
pub mod structure;

pub mod rendering;

// Re-exports for convenience — most users should only need `use pdfpurr::*`.
pub use document::Document;
pub use error::{PdfError, PdfResult};

// Core objects
pub use crate::core::{
    DictExt, Dictionary, IndirectRef, Object, ObjectId, PdfName, PdfStream, PdfString, StringFormat,
};
pub use crate::parser::PdfVersion;

// Rendering (only with feature)
pub use crate::rendering::{RenderOptions, Renderer};

// Structure: annotations, outlines, metadata
pub use crate::structure::{Annotation, Metadata, Outline};

// Forms
pub use crate::forms::{FieldType, FormField};

// Fonts (embedding requires "fonts" feature)
pub use crate::fonts::cidfont::CidFont;
pub use crate::fonts::embedding::EmbeddedFont;

// Accessibility
pub use crate::accessibility::{
    validate_pdf_ua, AccessibilityCheck, AccessibilityReport, StructElem, StructTree,
};

// Standards compliance
pub use crate::standards::{PdfALevel, PdfXLevel, StandardsCheck, StandardsReport};

// Signatures
pub use crate::signatures::{SignatureInfo, SignatureValidity, SubFilter};

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Library name
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Shared test utilities, available to all test modules in the crate.
#[cfg(test)]
pub(crate) mod test_utils {
    use crate::core::objects::{Dictionary, Object, PdfName, PdfString};

    /// Builds a `Dictionary` from a list of `(&str, Object)` pairs.
    pub fn make_dict(entries: Vec<(&str, Object)>) -> Dictionary {
        let mut dict = Dictionary::new();
        for (k, v) in entries {
            dict.insert(PdfName::new(k), v);
        }
        dict
    }

    /// Creates a literal `Object::String` from a `&str`.
    pub fn str_obj(s: &str) -> Object {
        Object::String(PdfString::from_literal(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
        assert_eq!(NAME, "pdfpurr");
    }
}
