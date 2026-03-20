//! PDF accessibility and tagged PDF support.
//!
//! Implements structure tree parsing and PDF/UA validation per
//! ISO 32000-2:2020 Section 14.7 (Logical Structure) and
//! ISO 14289-1:2014 (PDF/UA).

pub mod auto_tag;
pub mod structure_builder;
mod structure_tree;
mod validation;

pub use structure_tree::{RoleMap, StandardRole, StructElem, StructTree};
pub use validation::{validate_pdf_ua, AccessibilityCheck, AccessibilityReport};
