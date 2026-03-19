//! AcroForms support for PDF interactive forms.
//!
//! Provides types for parsing and manipulating PDF form fields
//! (text fields, checkboxes, dropdowns, radio buttons, signatures).

pub mod field;

pub use field::{FieldType, FormField};
