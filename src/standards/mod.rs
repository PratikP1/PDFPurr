//! PDF standards compliance validation.
//!
//! Provides validation for:
//! - **PDF/A** (ISO 19005) — archival compliance
//! - **PDF/X** (ISO 15930) — print production compliance
//!
//! Each validator returns a [`StandardsReport`] containing individual
//! [`StandardsCheck`] results, following the same pattern as the
//! accessibility validation in [`crate::accessibility`].

pub mod common;
pub mod pdfa;
pub mod pdfx;

pub use common::{PdfALevel, PdfXLevel, StandardsCheck, StandardsReport};
pub use pdfa::validate_pdfa;
pub use pdfx::validate_pdfx;
