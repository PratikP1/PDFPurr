//! Font handling for PDF text extraction.
//!
//! This module provides:
//! - Character encoding tables (WinAnsi, MacRoman, Standard, MacExpert)
//! - ToUnicode CMap parsing for character code → Unicode mapping
//! - Font loading from PDF font dictionaries
//! - Graphics state tracking for content stream processing

pub mod cidfont;
pub mod cmap;
pub(crate) mod common;
pub mod embedding;
pub mod encoding;
pub mod font;
pub mod graphics_state;
pub mod standard14;

// Re-exports
pub use cmap::ToUnicodeCMap;
pub use encoding::Encoding;
pub use font::{Font, FontSubtype};
pub use graphics_state::{GraphicsState, GraphicsStateStack, TextState};
pub use standard14::Standard14Font;
