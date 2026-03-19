//! PDF image types and extraction logic.

use crate::core::objects::{DictExt, Dictionary, Object, PdfStream};
use crate::error::{PdfError, PdfResult};

/// The color space of a PDF image.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpace {
    /// RGB color (3 components per pixel).
    DeviceRGB,
    /// Grayscale (1 component per pixel).
    DeviceGray,
    /// CMYK color (4 components per pixel).
    DeviceCMYK,
    /// Indexed color space with a base color space and lookup table.
    Indexed(Box<ColorSpace>, Vec<u8>),
}

impl ColorSpace {
    /// Number of components per pixel for this color space.
    pub fn components(&self) -> usize {
        match self {
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::Indexed(_, _) => 1,
        }
    }

    /// Parses a color space from a PDF `/ColorSpace` value.
    pub fn from_object(obj: &Object) -> PdfResult<Self> {
        match obj {
            Object::Name(name) => match name.as_str() {
                "DeviceRGB" | "RGB" => Ok(ColorSpace::DeviceRGB),
                "DeviceGray" | "G" => Ok(ColorSpace::DeviceGray),
                "DeviceCMYK" | "CMYK" => Ok(ColorSpace::DeviceCMYK),
                other => Err(PdfError::UnsupportedFeature(format!(
                    "Color space: {}",
                    other
                ))),
            },
            Object::Array(arr) if !arr.is_empty() => {
                let cs_name = arr[0].as_name().ok_or_else(|| PdfError::TypeError {
                    expected: "Name".to_string(),
                    found: arr[0].type_name().to_string(),
                })?;
                match cs_name {
                    "Indexed" if arr.len() >= 4 => {
                        let base = ColorSpace::from_object(&arr[1])?;
                        // arr[2] = hival (max index), arr[3] = lookup table
                        let lookup = match &arr[3] {
                            Object::String(s) => s.bytes.clone(),
                            Object::Stream(s) => s.decode_data()?,
                            _ => Vec::new(),
                        };
                        Ok(ColorSpace::Indexed(Box::new(base), lookup))
                    }
                    other => Err(PdfError::UnsupportedFeature(format!(
                        "Color space: {}",
                        other
                    ))),
                }
            }
            _ => Err(PdfError::TypeError {
                expected: "Name or Array".to_string(),
                found: obj.type_name().to_string(),
            }),
        }
    }
}

/// The image data payload.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageData {
    /// Decoded raw pixel data (component values in scan order).
    Raw(Vec<u8>),
    /// JPEG data (DCTDecode passthrough — write directly as .jpg).
    Jpeg(Vec<u8>),
    /// JPEG2000 data (JPXDecode passthrough — write directly as .jp2).
    /// Requires the `jpeg2000` feature.
    #[cfg(feature = "jpeg2000")]
    Jpeg2000(Vec<u8>),
}

/// A PDF image extracted from a page.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bits per color component (1, 2, 4, 8, or 16).
    pub bits_per_component: u8,
    /// The image's color space.
    pub color_space: ColorSpace,
    /// The image data (raw pixels or compressed passthrough).
    pub data: ImageData,
    /// Whether this image is a stencil mask (`/ImageMask true`).
    pub is_image_mask: bool,
    /// Decode array for inverting mask interpretation.
    pub decode: Option<Vec<f32>>,
}

impl PdfImage {
    /// Creates a `PdfImage` from an inline image dictionary and raw data.
    ///
    /// Inline images use abbreviated keys: `/W` (Width), `/H` (Height),
    /// `/BPC` (BitsPerComponent), `/CS` (ColorSpace).
    /// ISO 32000-2:2020, Table 90.
    pub fn from_inline(dict: &Dictionary, data: Vec<u8>) -> PdfResult<Self> {
        let width = dict
            .get_i64("W")
            .or_else(|| dict.get_i64("Width"))
            .ok_or_else(|| PdfError::InvalidImage("Inline image missing /W".to_string()))?
            as u32;

        let height = dict
            .get_i64("H")
            .or_else(|| dict.get_i64("Height"))
            .ok_or_else(|| PdfError::InvalidImage("Inline image missing /H".to_string()))?
            as u32;

        let bits_per_component = dict
            .get_i64("BPC")
            .or_else(|| dict.get_i64("BitsPerComponent"))
            .unwrap_or(8) as u8;

        let cs_obj = dict.get_str("CS").or_else(|| dict.get_str("ColorSpace"));

        let color_space = match cs_obj {
            Some(obj) => Self::inline_color_space(obj)?,
            None => ColorSpace::DeviceGray,
        };

        Ok(PdfImage {
            width,
            height,
            bits_per_component,
            color_space,
            data: ImageData::Raw(data),
            is_image_mask: false,
            decode: None,
        })
    }

    /// Resolves an inline image color space, handling abbreviations.
    fn inline_color_space(obj: &Object) -> PdfResult<ColorSpace> {
        if let Some(name) = obj.as_name() {
            return Ok(match name {
                "G" | "DeviceGray" => ColorSpace::DeviceGray,
                "RGB" | "DeviceRGB" => ColorSpace::DeviceRGB,
                "CMYK" | "DeviceCMYK" => ColorSpace::DeviceCMYK,
                _ => ColorSpace::from_object(obj)?,
            });
        }
        ColorSpace::from_object(obj)
    }

    /// Extracts an image from an image XObject stream.
    ///
    /// The stream dictionary must have `/Subtype /Image` along with
    /// `/Width`, `/Height`, and `/BitsPerComponent`.
    pub fn from_stream(stream: &PdfStream) -> PdfResult<Self> {
        let dict = &stream.dict;

        let width = dict
            .get_i64("Width")
            .ok_or_else(|| PdfError::InvalidImage("Image XObject missing /Width".to_string()))?
            as u32;

        let height = dict
            .get_i64("Height")
            .ok_or_else(|| PdfError::InvalidImage("Image XObject missing /Height".to_string()))?
            as u32;

        // BitsPerComponent may be absent for JPXDecode images
        let bits_per_component = dict.get_i64("BitsPerComponent").unwrap_or(8) as u8;

        let color_space = match dict.get_str("ColorSpace") {
            Some(cs_obj) => ColorSpace::from_object(cs_obj)?,
            None => ColorSpace::DeviceGray, // Default for missing CS
        };

        // Determine if this is a passthrough format
        let filter_name = dict
            .get_str("Filter")
            .and_then(|f| f.as_name().map(String::from));

        let data = match filter_name.as_deref() {
            Some("DCTDecode" | "DCT") => ImageData::Jpeg(stream.data.clone()),
            #[cfg(feature = "jpeg2000")]
            Some("JPXDecode") => ImageData::Jpeg2000(stream.data.clone()),
            _ => ImageData::Raw(stream.decode_data()?),
        };

        let is_image_mask = matches!(dict.get_str("ImageMask"), Some(Object::Boolean(true)));

        let decode = dict
            .get_str("Decode")
            .and_then(|o| o.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| match o {
                        Object::Real(f) => Some(*f as f32),
                        Object::Integer(i) => Some(*i as f32),
                        _ => None,
                    })
                    .collect()
            });

        Ok(PdfImage {
            width,
            height,
            bits_per_component,
            color_space,
            data,
            is_image_mask,
            decode,
        })
    }

    /// Converts the image to RGBA pixel data (8 bits per channel).
    ///
    /// JPEG and JPEG2000 passthrough data is decoded using the respective
    /// decoder crates. Raw data is expanded based on the color space.
    pub fn to_rgba(&self) -> PdfResult<Vec<u8>> {
        use std::borrow::Cow;

        let raw: Cow<'_, [u8]> = match &self.data {
            ImageData::Raw(data) => Cow::Borrowed(data),
            ImageData::Jpeg(data) => {
                let mut decoder = jpeg_decoder::Decoder::new(data.as_slice());
                Cow::Owned(
                    decoder
                        .decode()
                        .map_err(|e| PdfError::InvalidImage(format!("JPEG decode: {}", e)))?,
                )
            }
            #[cfg(feature = "jpeg2000")]
            ImageData::Jpeg2000(data) => {
                let jp2 = jpeg2k::Image::from_bytes(data)
                    .map_err(|e| PdfError::InvalidImage(format!("JPEG2000 decode: {}", e)))?;
                Cow::Owned(decode_jpeg2000(&jp2)?)
            }
        };

        let components = self.color_space.components();
        let pixel_count = (self.width as usize)
            .checked_mul(self.height as usize)
            .ok_or_else(|| PdfError::InvalidImage("Image dimensions overflow".to_string()))?;
        let rgba_capacity = pixel_count
            .checked_mul(4)
            .ok_or_else(|| PdfError::InvalidImage("RGBA capacity overflow".to_string()))?;
        let mut rgba = Vec::with_capacity(rgba_capacity.min(256 * 1024 * 1024));

        // Dispatch once by component count to avoid per-pixel branching.
        match components {
            1 => self.convert_gray(&raw, pixel_count, &mut rgba),
            3 => self.convert_rgb(&raw, pixel_count, &mut rgba),
            4 => self.convert_cmyk(&raw, pixel_count, &mut rgba),
            _ => {
                for _ in 0..pixel_count {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                }
            }
        }

        Ok(rgba)
    }

    fn convert_gray(&self, raw: &[u8], pixel_count: usize, rgba: &mut Vec<u8>) {
        for i in 0..pixel_count {
            if i >= raw.len() {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                let g = raw[i];
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
    }

    fn convert_rgb(&self, raw: &[u8], pixel_count: usize, rgba: &mut Vec<u8>) {
        for i in 0..pixel_count {
            let offset = i * 3;
            if offset + 3 > raw.len() {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[raw[offset], raw[offset + 1], raw[offset + 2], 255]);
            }
        }
    }

    fn convert_cmyk(&self, raw: &[u8], pixel_count: usize, rgba: &mut Vec<u8>) {
        use crate::rendering::colors::cmyk_to_rgb;
        for i in 0..pixel_count {
            let offset = i * 4;
            if offset + 4 > raw.len() {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                let (r, g, b) = cmyk_to_rgb(
                    raw[offset] as f32 / 255.0,
                    raw[offset + 1] as f32 / 255.0,
                    raw[offset + 2] as f32 / 255.0,
                    raw[offset + 3] as f32 / 255.0,
                );
                rgba.extend_from_slice(&[
                    (r * 255.0) as u8,
                    (g * 255.0) as u8,
                    (b * 255.0) as u8,
                    255,
                ]);
            }
        }
    }
}

/// Decodes a JPEG2000 image into raw interleaved component bytes.
#[cfg(feature = "jpeg2000")]
fn decode_jpeg2000(jp2: &jpeg2k::Image) -> PdfResult<Vec<u8>> {
    let components = jp2.components();
    let nc = components.len();
    let w = jp2.width() as usize;
    let h = jp2.height() as usize;
    let pixel_count = w
        .checked_mul(h)
        .ok_or_else(|| PdfError::InvalidImage("JPEG2000 dimensions overflow".to_string()))?;
    let buf_size = pixel_count
        .checked_mul(nc)
        .ok_or_else(|| PdfError::InvalidImage("JPEG2000 buffer size overflow".to_string()))?;

    // Guard against absurd allocations (>256MB)
    if buf_size > 256 * 1024 * 1024 {
        return Err(PdfError::InvalidImage(
            "JPEG2000 decoded size exceeds 256MB limit".to_string(),
        ));
    }

    // Pre-fill output buffer so we can write component-major (cache-friendly
    // reads of each contiguous component slice) with strided writes.
    let mut raw = vec![0u8; buf_size];
    for (comp_idx, comp) in components.iter().enumerate() {
        let data = comp.data();
        for pixel_idx in 0..pixel_count {
            let val = data.get(pixel_idx).copied().unwrap_or(0);
            raw[pixel_idx * nc + comp_idx] = val.clamp(0, 255) as u8;
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{Dictionary, PdfName};

    #[test]
    fn color_space_from_name() {
        let obj = Object::Name(PdfName::new("DeviceRGB"));
        let cs = ColorSpace::from_object(&obj).unwrap();
        assert_eq!(cs, ColorSpace::DeviceRGB);
        assert_eq!(cs.components(), 3);
    }

    #[test]
    fn color_space_gray() {
        let cs = ColorSpace::from_object(&Object::Name(PdfName::new("DeviceGray"))).unwrap();
        assert_eq!(cs.components(), 1);
    }

    #[test]
    fn color_space_cmyk() {
        let cs = ColorSpace::from_object(&Object::Name(PdfName::new("DeviceCMYK"))).unwrap();
        assert_eq!(cs.components(), 4);
    }

    #[test]
    fn color_space_unsupported() {
        let result = ColorSpace::from_object(&Object::Name(PdfName::new("CalRGB")));
        assert!(result.is_err());
    }

    #[test]
    fn image_from_stream_raw() {
        // 2x2 RGB image, no filter (raw data)
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
        dict.insert(PdfName::new("Width"), Object::Integer(2));
        dict.insert(PdfName::new("Height"), Object::Integer(2));
        dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
        dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );

        // 2x2 RGB = 12 bytes
        let pixel_data = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128];
        let stream = PdfStream::new(dict, pixel_data.clone());

        let img = PdfImage::from_stream(&stream).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.bits_per_component, 8);
        assert_eq!(img.color_space, ColorSpace::DeviceRGB);
        assert_eq!(img.data, ImageData::Raw(pixel_data));
    }

    #[test]
    fn image_from_stream_jpeg_passthrough() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
        dict.insert(PdfName::new("Width"), Object::Integer(100));
        dict.insert(PdfName::new("Height"), Object::Integer(50));
        dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
        dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("DCTDecode")),
        );

        let jpeg_data = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9];
        let stream = PdfStream::new(dict, jpeg_data.clone());

        let img = PdfImage::from_stream(&stream).unwrap();
        assert_eq!(img.data, ImageData::Jpeg(jpeg_data));
    }

    #[test]
    fn image_from_stream_missing_width() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Height"), Object::Integer(10));
        let stream = PdfStream::new(dict, vec![]);

        let result = PdfImage::from_stream(&stream);
        assert!(result.is_err());
    }

    #[test]
    fn jpeg2000_passthrough_stored() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Width"), Object::Integer(1));
        dict.insert(PdfName::new("Height"), Object::Integer(1));
        dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
        dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        dict.insert(
            PdfName::new("Filter"),
            Object::Name(PdfName::new("JPXDecode")),
        );
        let jp2_data = vec![0x00, 0x00, 0x00, 0x0C]; // invalid JP2
        let stream = PdfStream::new(dict, jp2_data.clone());

        let img = PdfImage::from_stream(&stream).unwrap();
        assert_eq!(img.data, ImageData::Jpeg2000(jp2_data));
    }

    #[test]
    fn jpeg2000_invalid_data_returns_error() {
        let img = PdfImage {
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceRGB,
            data: ImageData::Jpeg2000(vec![0xFF, 0x00]), // invalid
            is_image_mask: false,
            decode: None,
        };
        // Should return an error, not panic
        assert!(img.to_rgba().is_err());
    }

    #[test]
    fn image_from_stream_grayscale() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("Width"), Object::Integer(4));
        dict.insert(PdfName::new("Height"), Object::Integer(1));
        dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
        dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceGray")),
        );

        let pixel_data = vec![0, 85, 170, 255];
        let stream = PdfStream::new(dict, pixel_data.clone());

        let img = PdfImage::from_stream(&stream).unwrap();
        assert_eq!(img.color_space, ColorSpace::DeviceGray);
        assert_eq!(img.data, ImageData::Raw(pixel_data));
    }

    // ---- to_rgba conversion tests ----

    #[test]
    fn to_rgba_gray() {
        let img = PdfImage {
            width: 2,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceGray,
            data: ImageData::Raw(vec![0, 255]),
            is_image_mask: false,
            decode: None,
        };
        let rgba = img.to_rgba().unwrap();
        // Pixel 0: gray 0 → (0, 0, 0, 255)
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        // Pixel 1: gray 255 → (255, 255, 255, 255)
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn to_rgba_rgb() {
        let img = PdfImage {
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceRGB,
            data: ImageData::Raw(vec![255, 128, 0]),
            is_image_mask: false,
            decode: None,
        };
        let rgba = img.to_rgba().unwrap();
        assert_eq!(&rgba[0..4], &[255, 128, 0, 255]);
    }

    #[test]
    fn to_rgba_cmyk() {
        let img = PdfImage {
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceCMYK,
            // CMYK (0, 0, 0, 0) = white
            data: ImageData::Raw(vec![0, 0, 0, 0]),
            is_image_mask: false,
            decode: None,
        };
        let rgba = img.to_rgba().unwrap();
        assert_eq!(&rgba[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn to_rgba_cmyk_black() {
        let img = PdfImage {
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceCMYK,
            // CMYK (0, 0, 0, 255) = black
            data: ImageData::Raw(vec![0, 0, 0, 255]),
            is_image_mask: false,
            decode: None,
        };
        let rgba = img.to_rgba().unwrap();
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
    }
}
