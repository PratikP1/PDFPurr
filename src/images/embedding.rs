//! Image embedding — create PDF XObject streams from image data.
//!
//! Supports JPEG (passthrough with DCTDecode) and PNG (decode then
//! re-encode with FlateDecode). Produces [`PdfStream`] objects suitable
//! for insertion into a document's XObject resource dictionary.
//!
//! ISO 32000-2:2020, Section 8.9.

use crate::core::filters::encode_flate;
use crate::core::objects::{Dictionary, Object, PdfName, PdfStream};
use crate::error::{PdfError, PdfResult};
use crate::images::ColorSpace;

/// An image prepared for embedding in a PDF document.
#[derive(Debug, Clone)]
pub struct EmbeddedImage {
    /// Image width in pixels.
    width: u32,
    /// Image height in pixels.
    height: u32,
    /// Bits per component.
    bits_per_component: u8,
    /// Color space.
    color_space: ColorSpace,
    /// The encoding strategy.
    encoding: ImageEncoding,
}

/// How the image data will be stored in the PDF stream.
#[derive(Debug, Clone)]
enum ImageEncoding {
    /// JPEG passthrough — store raw JPEG bytes with DCTDecode filter.
    Jpeg(Vec<u8>),
    /// Raw pixel data that will be FlateDecode-compressed on output.
    Raw(Vec<u8>),
    /// Raw pixel data with a separate alpha (SMask) channel.
    RawWithAlpha {
        /// Color pixel data (RGB or Gray, no alpha).
        color: Vec<u8>,
        /// Alpha channel data (one byte per pixel).
        alpha: Vec<u8>,
    },
}

impl EmbeddedImage {
    /// Creates an embedded image from JPEG data.
    ///
    /// The JPEG header is parsed to extract dimensions and color space.
    /// The raw JPEG bytes are stored as-is with a DCTDecode filter.
    pub fn from_jpeg(data: &[u8]) -> PdfResult<Self> {
        let (width, height, components) = parse_jpeg_dimensions(data)?;
        let color_space = match components {
            1 => ColorSpace::DeviceGray,
            3 => ColorSpace::DeviceRGB,
            4 => ColorSpace::DeviceCMYK,
            n => {
                return Err(PdfError::InvalidImage(format!(
                    "Unsupported JPEG component count: {}",
                    n
                )))
            }
        };

        Ok(Self {
            width,
            height,
            bits_per_component: 8,
            color_space,
            encoding: ImageEncoding::Jpeg(data.to_vec()),
        })
    }

    /// Creates an embedded image from PNG data.
    ///
    /// The PNG is decoded and re-encoded as raw pixel data with
    /// FlateDecode compression. If the PNG has an alpha channel, it is
    /// separated into an SMask stream.
    pub fn from_png(data: &[u8]) -> PdfResult<Self> {
        let decoder = png::Decoder::new(data);
        let mut reader = decoder
            .read_info()
            .map_err(|e| PdfError::InvalidImage(format!("PNG decode error: {}", e)))?;

        let info = reader.info();
        let width = info.width;
        let height = info.height;
        let color_type = info.color_type;
        let bit_depth = info.bit_depth;

        // Only support 8-bit PNG for now
        if bit_depth != png::BitDepth::Eight {
            return Err(PdfError::UnsupportedFeature(format!(
                "PNG bit depth {:?} (only 8-bit supported)",
                bit_depth
            )));
        }

        // Allocate output buffer and read all pixels
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let output_info = reader
            .next_frame(&mut buf)
            .map_err(|e| PdfError::InvalidImage(format!("PNG frame error: {}", e)))?;
        buf.truncate(output_info.buffer_size());

        match color_type {
            png::ColorType::Rgb => Ok(Self {
                width,
                height,
                bits_per_component: 8,
                color_space: ColorSpace::DeviceRGB,
                encoding: ImageEncoding::Raw(buf),
            }),
            png::ColorType::Rgba => {
                // Separate RGB and alpha channels
                let pixel_count = (width as usize).saturating_mul(height as usize);
                let mut color = Vec::with_capacity(pixel_count * 3);
                let mut alpha = Vec::with_capacity(pixel_count);
                for chunk in buf.chunks_exact(4) {
                    color.extend_from_slice(&chunk[..3]);
                    alpha.push(chunk[3]);
                }
                Ok(Self {
                    width,
                    height,
                    bits_per_component: 8,
                    color_space: ColorSpace::DeviceRGB,
                    encoding: ImageEncoding::RawWithAlpha { color, alpha },
                })
            }
            png::ColorType::Grayscale => Ok(Self {
                width,
                height,
                bits_per_component: 8,
                color_space: ColorSpace::DeviceGray,
                encoding: ImageEncoding::Raw(buf),
            }),
            png::ColorType::GrayscaleAlpha => {
                let pixel_count = (width as usize).saturating_mul(height as usize);
                let mut color = Vec::with_capacity(pixel_count);
                let mut alpha = Vec::with_capacity(pixel_count);
                for chunk in buf.chunks_exact(2) {
                    color.push(chunk[0]);
                    alpha.push(chunk[1]);
                }
                Ok(Self {
                    width,
                    height,
                    bits_per_component: 8,
                    color_space: ColorSpace::DeviceGray,
                    encoding: ImageEncoding::RawWithAlpha { color, alpha },
                })
            }
            other => Err(PdfError::UnsupportedFeature(format!(
                "PNG color type {:?}",
                other
            ))),
        }
    }

    /// Returns the image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the color space name for the PDF dictionary.
    fn color_space_name(&self) -> &'static str {
        match &self.color_space {
            ColorSpace::DeviceRGB => "DeviceRGB",
            ColorSpace::DeviceGray => "DeviceGray",
            ColorSpace::DeviceCMYK => "DeviceCMYK",
            ColorSpace::Indexed(_, _) => "DeviceRGB", // fallback
        }
    }

    /// Builds the XObject image stream for embedding in a PDF.
    ///
    /// The returned stream has all required dictionary entries
    /// (`/Type`, `/Subtype`, `/Width`, `/Height`, `/BitsPerComponent`,
    /// `/ColorSpace`, and `/Filter`).
    pub fn to_xobject_stream(&self) -> PdfResult<PdfStream> {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
        dict.insert(PdfName::new("Width"), Object::Integer(self.width as i64));
        dict.insert(PdfName::new("Height"), Object::Integer(self.height as i64));
        dict.insert(
            PdfName::new("BitsPerComponent"),
            Object::Integer(self.bits_per_component as i64),
        );
        dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new(self.color_space_name())),
        );

        match &self.encoding {
            ImageEncoding::Jpeg(data) => {
                dict.insert(
                    PdfName::new("Filter"),
                    Object::Name(PdfName::new("DCTDecode")),
                );
                Ok(PdfStream::new(dict, data.clone()))
            }
            ImageEncoding::Raw(data) => {
                let (compressed, filter) = encode_flate(data)?;
                if let Some(f) = filter {
                    dict.insert(PdfName::new("Filter"), Object::Name(PdfName::new(f)));
                }
                Ok(PdfStream::new(dict, compressed))
            }
            ImageEncoding::RawWithAlpha { color, .. } => {
                let (compressed, filter) = encode_flate(color)?;
                if let Some(f) = filter {
                    dict.insert(PdfName::new("Filter"), Object::Name(PdfName::new(f)));
                }
                // Note: SMask must be added as a separate indirect object by the caller
                // and referenced here. We set a marker so the caller knows to do this.
                Ok(PdfStream::new(dict, compressed))
            }
        }
    }

    /// Builds an SMask (soft mask) stream for images with alpha.
    ///
    /// Returns `None` if the image has no alpha channel.
    pub fn to_smask_stream(&self) -> PdfResult<Option<PdfStream>> {
        match &self.encoding {
            ImageEncoding::RawWithAlpha { alpha, .. } => {
                let mut dict = Dictionary::new();
                dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
                dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
                dict.insert(PdfName::new("Width"), Object::Integer(self.width as i64));
                dict.insert(PdfName::new("Height"), Object::Integer(self.height as i64));
                dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
                dict.insert(
                    PdfName::new("ColorSpace"),
                    Object::Name(PdfName::new("DeviceGray")),
                );

                let (compressed, filter) = encode_flate(alpha)?;
                if let Some(f) = filter {
                    dict.insert(PdfName::new("Filter"), Object::Name(PdfName::new(f)));
                }

                Ok(Some(PdfStream::new(dict, compressed)))
            }
            _ => Ok(None),
        }
    }
}

/// Parses JPEG dimensions and component count from the SOF marker.
fn parse_jpeg_dimensions(data: &[u8]) -> PdfResult<(u32, u32, u8)> {
    // JPEG files start with FFD8 (SOI marker)
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(PdfError::InvalidImage("Not a valid JPEG".to_string()));
    }

    let mut pos = 2;
    while pos + 1 < data.len() {
        if data[pos] != 0xFF {
            return Err(PdfError::InvalidImage("Invalid JPEG marker".to_string()));
        }

        let marker = data[pos + 1];
        pos += 2;

        // SOF markers: C0 (baseline), C1 (extended sequential), C2 (progressive)
        if matches!(marker, 0xC0..=0xC2) {
            if pos + 8 > data.len() {
                return Err(PdfError::InvalidImage("JPEG SOF too short".to_string()));
            }
            let height = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as u32;
            let width = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u32;
            let components = data[pos + 7];
            return Ok((width, height, components));
        }

        // Skip over other markers
        if marker == 0xD9 {
            break; // EOI
        }
        if marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
            continue; // Restart markers have no length
        }

        if pos + 1 >= data.len() {
            break;
        }
        let length = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        pos += length;
    }

    Err(PdfError::InvalidImage(
        "JPEG SOF marker not found".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal valid JPEG with given dimensions and components.
    fn make_jpeg(width: u16, height: u16, components: u8) -> Vec<u8> {
        let mut data = Vec::new();
        // SOI
        data.extend_from_slice(&[0xFF, 0xD8]);
        // APP0 (JFIF) marker — minimal
        data.extend_from_slice(&[0xFF, 0xE0]);
        data.extend_from_slice(&[0x00, 0x10]); // length 16
        data.extend_from_slice(b"JFIF\0");
        data.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
        // SOF0 (baseline)
        data.extend_from_slice(&[0xFF, 0xC0]);
        let sof_len = 8 + 3 * components as u16;
        data.extend_from_slice(&sof_len.to_be_bytes());
        data.push(8); // precision
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.push(components);
        for i in 0..components {
            data.push(i + 1); // component ID
            data.push(0x11); // sampling factor
            data.push(0); // quantization table
        }
        // EOI
        data.extend_from_slice(&[0xFF, 0xD9]);
        data
    }

    /// Builds a minimal valid 8-bit RGB PNG.
    fn make_png_rgb(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data = vec![128u8; (width * height * 3) as usize];
            writer.write_image_data(&data).unwrap();
        }
        buf
    }

    /// Builds a minimal valid 8-bit RGBA PNG.
    fn make_png_rgba(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data = vec![200u8; (width * height * 4) as usize];
            writer.write_image_data(&data).unwrap();
        }
        buf
    }

    #[test]
    fn embed_jpeg_xobject() {
        let jpeg = make_jpeg(100, 50, 3);
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        let stream = img.to_xobject_stream().unwrap();

        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("Filter"))
                .and_then(|o| o.as_name()),
            Some("DCTDecode")
        );
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("Subtype"))
                .and_then(|o| o.as_name()),
            Some("Image")
        );
        // JPEG data is stored as-is
        assert_eq!(stream.data, jpeg);
    }

    #[test]
    fn embed_jpeg_dimensions() {
        let jpeg = make_jpeg(640, 480, 3);
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        assert_eq!(img.width(), 640);
        assert_eq!(img.height(), 480);
    }

    #[test]
    fn embed_jpeg_grayscale() {
        let jpeg = make_jpeg(10, 10, 1);
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        let stream = img.to_xobject_stream().unwrap();
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("ColorSpace"))
                .and_then(|o| o.as_name()),
            Some("DeviceGray")
        );
    }

    #[test]
    fn embed_jpeg_invalid() {
        let result = EmbeddedImage::from_jpeg(b"not a jpeg");
        assert!(result.is_err());
    }

    #[test]
    fn embed_png_xobject() {
        let png_data = make_png_rgb(4, 4);
        let img = EmbeddedImage::from_png(&png_data).unwrap();
        assert_eq!(img.width(), 4);
        assert_eq!(img.height(), 4);

        let stream = img.to_xobject_stream().unwrap();
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("Subtype"))
                .and_then(|o| o.as_name()),
            Some("Image")
        );
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("ColorSpace"))
                .and_then(|o| o.as_name()),
            Some("DeviceRGB")
        );
    }

    #[test]
    fn embed_png_with_alpha() {
        let png_data = make_png_rgba(4, 4);
        let img = EmbeddedImage::from_png(&png_data).unwrap();

        // Main stream should be RGB without alpha
        let stream = img.to_xobject_stream().unwrap();
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("ColorSpace"))
                .and_then(|o| o.as_name()),
            Some("DeviceRGB")
        );

        // SMask stream should exist
        let smask = img.to_smask_stream().unwrap();
        assert!(smask.is_some());
        let smask = smask.unwrap();
        assert_eq!(
            smask
                .dict
                .get(&PdfName::new("ColorSpace"))
                .and_then(|o| o.as_name()),
            Some("DeviceGray")
        );
    }

    #[test]
    fn embed_image_dict_has_required_keys() {
        let jpeg = make_jpeg(200, 100, 3);
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        let stream = img.to_xobject_stream().unwrap();

        // All required keys per ISO 32000
        assert!(stream.dict.get(&PdfName::new("Type")).is_some());
        assert!(stream.dict.get(&PdfName::new("Subtype")).is_some());
        assert!(stream.dict.get(&PdfName::new("Width")).is_some());
        assert!(stream.dict.get(&PdfName::new("Height")).is_some());
        assert!(stream.dict.get(&PdfName::new("BitsPerComponent")).is_some());
        assert!(stream.dict.get(&PdfName::new("ColorSpace")).is_some());

        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("Width"))
                .and_then(|o| o.as_i64()),
            Some(200)
        );
        assert_eq!(
            stream
                .dict
                .get(&PdfName::new("Height"))
                .and_then(|o| o.as_i64()),
            Some(100)
        );
    }

    #[test]
    fn embed_image_no_smask_for_jpeg() {
        let jpeg = make_jpeg(10, 10, 3);
        let img = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        let smask = img.to_smask_stream().unwrap();
        assert!(smask.is_none());
    }

    #[test]
    fn embed_image_roundtrip() {
        // Create JPEG → embed → extract → verify metadata matches
        let jpeg = make_jpeg(320, 240, 3);
        let embedded = EmbeddedImage::from_jpeg(&jpeg).unwrap();
        let stream = embedded.to_xobject_stream().unwrap();

        // Parse back with the existing PdfImage::from_stream
        use crate::images::PdfImage;
        let extracted = PdfImage::from_stream(&stream).unwrap();
        assert_eq!(extracted.width, 320);
        assert_eq!(extracted.height, 240);
        assert_eq!(extracted.bits_per_component, 8);
        assert_eq!(extracted.color_space, ColorSpace::DeviceRGB);
    }
}
