//! High-level page builder for creating PDF pages with content.
//!
//! [`PageBuilder`] provides an ergonomic API for adding text, images,
//! and shapes to a new page. It manages content streams, font resources,
//! and image XObjects automatically.
//!
//! # Example
//!
//! ```rust,ignore
//! use pdfpurr::{Document, fonts::Standard14Font};
//! use pdfpurr::page_builder::PageBuilder;
//!
//! let mut doc = Document::new();
//! let font = Standard14Font::from_name("Helvetica").unwrap();
//! let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
//! builder.add_text("Hello World", &font, 12.0, 72.0, 720.0);
//! builder.finish().unwrap();
//! ```

use crate::content::ContentStreamBuilder;
use crate::core::objects::{Dictionary, IndirectRef, Object, PdfName, PdfStream};
use crate::document::Document;
use crate::error::{PdfError, PdfResult};
use crate::fonts::standard14::Standard14Font;
use crate::images::EmbeddedImage;

/// A reference to a font registered with the page builder.
#[derive(Debug, Clone, Copy)]
pub struct FontRef {
    /// The resource name (e.g. "F1", "F2").
    name_index: usize,
}

/// A reference to an image registered with the page builder.
#[derive(Debug, Clone, Copy)]
pub struct ImageRef {
    /// The resource name index (e.g. "Im1", "Im2").
    name_index: usize,
}

/// Registered font: either a standard 14 or an embedded font.
enum RegisteredFont {
    Standard14(Standard14Font),
}

/// Registered image.
struct RegisteredImage {
    image: EmbeddedImage,
}

/// Builds a PDF page with content, fonts, and images.
///
/// Automatically manages font and image resources, content streams,
/// and page dictionary construction.
pub struct PageBuilder<'a> {
    doc: &'a mut Document,
    width: f64,
    height: f64,
    content: ContentStreamBuilder,
    fonts: Vec<RegisteredFont>,
    images: Vec<RegisteredImage>,
}

impl<'a> PageBuilder<'a> {
    /// Creates a new page builder for the given document and page dimensions.
    ///
    /// Dimensions are in points. Standard US Letter is `(612.0, 792.0)`.
    pub fn new(doc: &'a mut Document, width: f64, height: f64) -> Self {
        Self {
            doc,
            width,
            height,
            content: ContentStreamBuilder::new(),
            fonts: Vec::new(),
            images: Vec::new(),
        }
    }

    /// Registers a standard 14 font and returns a reference to it.
    pub fn add_standard_font(&mut self, font: Standard14Font) -> FontRef {
        let idx = self.fonts.len();
        self.fonts.push(RegisteredFont::Standard14(font));
        FontRef { name_index: idx }
    }

    /// Registers an image and returns a reference to it.
    pub fn add_image(&mut self, image: EmbeddedImage) -> ImageRef {
        let idx = self.images.len();
        self.images.push(RegisteredImage { image });
        ImageRef { name_index: idx }
    }

    /// Adds text at the given position using a registered font.
    pub fn add_text(
        &mut self,
        text: &str,
        font_ref: FontRef,
        size: f64,
        x: f64,
        y: f64,
    ) -> &mut Self {
        let font_name = format!("F{}", font_ref.name_index + 1);
        self.content
            .begin_text()
            .set_font(&font_name, size)
            .move_to(x, y)
            .show_text(text)
            .end_text();
        self
    }

    /// Draws a registered image at the given position and size.
    pub fn draw_image(
        &mut self,
        image_ref: ImageRef,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> &mut Self {
        let image_name = format!("Im{}", image_ref.name_index + 1);
        self.content
            .save_state()
            .set_transform(width, 0.0, 0.0, height, x, y)
            .draw_image(&image_name)
            .restore_state();
        self
    }

    /// Draws a filled rectangle.
    pub fn add_rect(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        color: [f64; 3],
    ) -> &mut Self {
        self.content
            .set_fill_color_rgb(color[0], color[1], color[2])
            .rect(x, y, width, height)
            .fill();
        self
    }

    /// Returns a mutable reference to the underlying content stream builder
    /// for low-level content stream operations.
    pub fn content_builder(&mut self) -> &mut ContentStreamBuilder {
        &mut self.content
    }

    /// Finishes building the page and adds it to the document.
    ///
    /// Returns the zero-based page index.
    pub fn finish(self) -> PdfResult<usize> {
        let doc = self.doc;
        let pages_id = doc.pages_id()?;

        // Build content stream
        let content_data = self.content.build();
        let content_stream = PdfStream::new(Dictionary::new(), content_data);
        let content_id = doc.add_object(Object::Stream(content_stream));

        // Build font resources
        let mut font_dict = Dictionary::new();
        for (i, registered_font) in self.fonts.iter().enumerate() {
            let font_name = format!("F{}", i + 1);
            match registered_font {
                RegisteredFont::Standard14(std_font) => {
                    let fd = std_font.to_font_dictionary();
                    let font_id = doc.add_object(Object::Dictionary(fd));
                    font_dict.insert(
                        PdfName::new(&font_name),
                        Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
                    );
                }
            }
        }

        // Build image resources
        let mut xobject_dict = Dictionary::new();
        for (i, registered_image) in self.images.iter().enumerate() {
            let image_name = format!("Im{}", i + 1);
            let stream = registered_image.image.to_xobject_stream()?;
            let image_id = doc.add_object(Object::Stream(stream));

            // Handle SMask if present
            if let Ok(Some(smask_stream)) = registered_image.image.to_smask_stream() {
                let smask_id = doc.add_object(Object::Stream(smask_stream));
                // Add SMask reference to the image XObject
                if let Some(Object::Stream(ref mut img_stream)) = doc.get_object_mut(image_id) {
                    img_stream.dict.insert(
                        PdfName::new("SMask"),
                        Object::Reference(IndirectRef::new(smask_id.0, smask_id.1)),
                    );
                }
            }

            xobject_dict.insert(
                PdfName::new(&image_name),
                Object::Reference(IndirectRef::new(image_id.0, image_id.1)),
            );
        }

        // Build resources dictionary
        let mut resources = Dictionary::new();
        if !font_dict.is_empty() {
            resources.insert(PdfName::new("Font"), Object::Dictionary(font_dict));
        }
        if !xobject_dict.is_empty() {
            resources.insert(PdfName::new("XObject"), Object::Dictionary(xobject_dict));
        }

        // Build page dictionary
        let mut page_dict = Dictionary::new();
        page_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page_dict.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(pages_id.0, pages_id.1)),
        );
        page_dict.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(self.width),
                Object::Real(self.height),
            ]),
        );
        page_dict.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(content_id.0, content_id.1)),
        );
        if !resources.is_empty() {
            resources.insert(
                PdfName::new("ProcSet"),
                Object::Array(vec![
                    Object::Name(PdfName::new("PDF")),
                    Object::Name(PdfName::new("Text")),
                    Object::Name(PdfName::new("ImageC")),
                ]),
            );
            page_dict.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        }

        let page_id = doc.add_object(Object::Dictionary(page_dict));

        // Update /Pages: append to /Kids and increment /Count
        let pages = doc
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        let kids = match pages.get_mut(&PdfName::new("Kids")) {
            Some(Object::Array(arr)) => arr,
            _ => {
                return Err(PdfError::InvalidStructure(
                    "/Pages missing /Kids array".to_string(),
                ))
            }
        };
        kids.push(Object::Reference(IndirectRef::new(page_id.0, page_id.1)));

        // Re-borrow pages after the mutable borrow of kids is done
        let pages = doc
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        if let Some(Object::Integer(count)) = pages.get_mut(&PdfName::new("Count")) {
            let idx = *count as usize;
            *count += 1;
            Ok(idx)
        } else {
            Err(PdfError::InvalidStructure(
                "No /Count in /Pages".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_builder_empty() {
        let mut doc = Document::new();
        let builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let idx = builder.finish().unwrap();
        assert_eq!(idx, 0);
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn page_builder_add_text() {
        let mut doc = Document::new();
        let font = Standard14Font::from_name("Helvetica").unwrap();
        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let fref = builder.add_standard_font(font);
        builder.add_text("Hello World", fref, 12.0, 72.0, 720.0);
        let idx = builder.finish().unwrap();
        assert_eq!(idx, 0);

        // Roundtrip: serialize → parse → extract text
        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        let text = doc2.extract_page_text(0).unwrap();
        assert!(text.contains("Hello World"));
    }

    #[test]
    fn page_builder_add_text_position() {
        let mut doc = Document::new();
        let font = Standard14Font::from_name("Courier").unwrap();
        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let fref = builder.add_standard_font(font);
        builder.add_text("At Position", fref, 10.0, 100.0, 500.0);
        builder.finish().unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        let text = doc2.extract_page_text(0).unwrap();
        assert!(text.contains("At Position"));
    }

    #[test]
    fn page_builder_multiple_fonts() {
        let mut doc = Document::new();
        let helv = Standard14Font::from_name("Helvetica").unwrap();
        let courier = Standard14Font::from_name("Courier").unwrap();

        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let f1 = builder.add_standard_font(helv);
        let f2 = builder.add_standard_font(courier);
        builder.add_text("Helvetica Text", f1, 12.0, 72.0, 720.0);
        builder.add_text("Courier Text", f2, 12.0, 72.0, 700.0);
        builder.finish().unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        let text = doc2.extract_page_text(0).unwrap();
        assert!(text.contains("Helvetica Text"));
        assert!(text.contains("Courier Text"));
    }

    #[test]
    fn page_builder_add_image() {
        // Create a minimal JPEG for testing
        fn make_jpeg() -> Vec<u8> {
            let mut data = Vec::new();
            data.extend_from_slice(&[0xFF, 0xD8]);
            data.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
            data.extend_from_slice(b"JFIF\0");
            data.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
            data.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 8]);
            data.extend_from_slice(&10u16.to_be_bytes());
            data.extend_from_slice(&10u16.to_be_bytes());
            data.push(3);
            data.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
            data.extend_from_slice(&[0xFF, 0xD9]);
            data
        }

        let jpeg = make_jpeg();
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();

        let mut doc = Document::new();
        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let iref = builder.add_image(img);
        builder.draw_image(iref, 72.0, 500.0, 200.0, 150.0);
        builder.finish().unwrap();

        // Verify page exists and has content
        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 1);
    }

    #[test]
    fn page_builder_add_rect() {
        let mut doc = Document::new();
        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        builder.add_rect(50.0, 50.0, 100.0, 100.0, [1.0, 0.0, 0.0]);
        builder.finish().unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 1);
    }

    #[test]
    fn page_builder_text_and_image() {
        fn make_jpeg() -> Vec<u8> {
            let mut data = Vec::new();
            data.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
            data.extend_from_slice(b"JFIF\0");
            data.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
            data.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 8]);
            data.extend_from_slice(&20u16.to_be_bytes());
            data.extend_from_slice(&20u16.to_be_bytes());
            data.push(3);
            data.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
            data.extend_from_slice(&[0xFF, 0xD9]);
            data
        }

        let mut doc = Document::new();
        let font = Standard14Font::from_name("Helvetica").unwrap();
        let img = EmbeddedImage::from_jpeg(&make_jpeg()).unwrap();

        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let fref = builder.add_standard_font(font);
        let iref = builder.add_image(img);
        builder.add_text("Caption", fref, 12.0, 72.0, 720.0);
        builder.draw_image(iref, 72.0, 500.0, 200.0, 150.0);
        builder.finish().unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        let text = doc2.extract_page_text(0).unwrap();
        assert!(text.contains("Caption"));
    }

    #[test]
    fn page_builder_full_roundtrip() {
        let mut doc = Document::new();
        let font = Standard14Font::from_name("Times-Roman").unwrap();

        // Page 1
        let mut b1 = PageBuilder::new(&mut doc, 612.0, 792.0);
        let f1 = b1.add_standard_font(font);
        b1.add_text("Page One", f1, 14.0, 72.0, 720.0);
        b1.finish().unwrap();

        // Page 2
        let font2 = Standard14Font::from_name("Helvetica-Bold").unwrap();
        let mut b2 = PageBuilder::new(&mut doc, 595.0, 842.0);
        let f2 = b2.add_standard_font(font2);
        b2.add_text("Page Two", f2, 16.0, 72.0, 770.0);
        b2.add_rect(50.0, 50.0, 200.0, 100.0, [0.0, 0.0, 1.0]);
        b2.finish().unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 2);

        let text1 = doc2.extract_page_text(0).unwrap();
        assert!(text1.contains("Page One"));

        let text2 = doc2.extract_page_text(1).unwrap();
        assert!(text2.contains("Page Two"));
    }

    #[test]
    fn page_builder_missing_kids_errors() {
        let mut doc = Document::new();
        // Corrupt the /Pages dict by removing /Kids
        let pages_id = doc.pages_id().unwrap();
        if let Some(Object::Dictionary(pages)) = doc.get_object_mut(pages_id) {
            pages.remove(&PdfName::new("Kids"));
        }

        let builder = PageBuilder::new(&mut doc, 612.0, 792.0);
        let result = builder.finish();
        assert!(result.is_err(), "Should error when /Kids is missing");
    }

    #[test]
    fn page_builder_content_builder_access() {
        let mut doc = Document::new();
        let mut builder = PageBuilder::new(&mut doc, 612.0, 792.0);

        // Use low-level content builder directly
        builder
            .content_builder()
            .save_state()
            .set_fill_color_rgb(0.5, 0.5, 0.5)
            .rect(0.0, 0.0, 612.0, 792.0)
            .fill()
            .restore_state();

        builder.finish().unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }
}
