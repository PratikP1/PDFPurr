//! PDF parsing and lexical analysis.
//!
//! This module provides parsers for PDF syntax, from low-level tokens
//! to complete object graphs. Built on the `nom` parser combinator library.

pub mod file_structure;
pub mod lexer;
pub mod objects;

pub use file_structure::{
    find_startxref, is_traditional_xref, parse_header, parse_indirect_object, parse_object_stream,
    parse_startxref, parse_trailer, parse_xref_stream, parse_xref_table, IndirectObject,
    PdfVersion, XRefEntry, XRefSubsection, XRefTable,
};
pub use objects::parse_object;
