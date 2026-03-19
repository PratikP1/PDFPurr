//! PDF image extraction and representation.
//!
//! Extracts image XObjects from PDF pages and provides access to image
//! metadata and pixel data. Supports DCTDecode (JPEG) and JPXDecode
//! (JPEG2000) passthrough as well as decoded raw pixel data.
//!
//! ISO 32000-2:2020, Section 8.9.

pub mod embedding;
mod image;

pub use embedding::EmbeddedImage;
pub use image::{ColorSpace, ImageData, PdfImage};
