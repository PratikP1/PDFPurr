//! PDF page rendering engine.
//!
//! Converts PDF pages to pixel images using [`tiny_skia`]. The entry point
//! is [`Renderer`], which takes a [`Document`](crate::Document) and produces
//! a [`tiny_skia::Pixmap`] for any given page.
//!
//! # Example
//!
//! ```rust,ignore
//! use pdfpurr::Document;
//! use pdfpurr::rendering::{Renderer, RenderOptions};
//!
//! let doc = Document::open("input.pdf")?;
//! let renderer = Renderer::new(&doc, RenderOptions::default());
//! let pixmap = renderer.render_page(0)?;
//! pixmap.save_png("page1.png")?;
//! ```

pub(crate) mod color_space;
pub(crate) mod colors;
pub(crate) mod function;
pub(crate) mod glyph;
pub(crate) mod graphics;
pub(crate) mod image;
pub(crate) mod path;
mod renderer;
pub(crate) mod shading;
pub(crate) mod text;

pub use renderer::{RenderOptions, Renderer};
