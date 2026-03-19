//! PDF document structure: outlines, annotations, and metadata.
//!
//! Provides access to bookmarks/outlines, page annotations, and
//! XMP metadata from PDF documents.

mod annotations;
mod metadata;
mod outlines;

pub use annotations::Annotation;
pub use metadata::Metadata;
pub use outlines::Outline;
