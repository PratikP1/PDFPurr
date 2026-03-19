//! Content stream processing.
//!
//! PDF content streams contain a sequence of operators and their operands
//! that describe the visual appearance of a page. This module provides
//! tokenization and interpretation of content streams, as well as a builder
//! for generating new content streams programmatically.

pub mod builder;
pub mod operators;
pub mod text;

pub use builder::{ContentStreamBuilder, TextItem};
pub use operators::{tokenize_content_stream, ContentToken};
pub use text::extract_text_from_content;
