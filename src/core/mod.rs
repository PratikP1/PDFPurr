//! Core PDF primitives and object model.
//!
//! This module implements the fundamental PDF object types as defined in
//! ISO 32000-2:2020 (PDF 2.0), Section 7.3. All PDF documents are built
//! from these primitive types.

pub mod filters;
pub mod objects;

pub use objects::{
    decode_utf16be, DictExt, Dictionary, IndirectRef, Object, ObjectId, PdfName, PdfStream,
    PdfString, StringFormat,
};
