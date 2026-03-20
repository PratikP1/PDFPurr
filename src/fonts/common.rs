//! Shared helpers for font parsing, subsetting, and PDF dictionary generation.
//!
//! Used by both [`super::embedding`] (simple fonts) and [`super::cidfont`]
//! (composite CID fonts) to avoid duplicating the skrifa/allsorts integration.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;

use allsorts::binary::read::ReadScope;
use allsorts::font_data::FontData;
use allsorts::subset::{self, CmapTarget, SubsetProfile};
use skrifa::instance::Size;
use skrifa::metrics::{GlyphMetrics, Metrics};
use skrifa::{FontRef, MetadataProvider};

use crate::core::filters::encode_flate;
use crate::core::objects::{Dictionary, Object, PdfName, PdfStream};
use crate::error::{PdfError, PdfResult};

/// Font metrics extracted from a TrueType/OpenType font via skrifa.
pub(crate) struct ParsedFontMetrics {
    /// PostScript name from the name table.
    pub ps_name: String,
    /// Units per em from the head table.
    pub units_per_em: u16,
    /// Ascent in font units.
    pub ascent: f32,
    /// Descent in font units (typically negative).
    pub descent: f32,
    /// Cap height in font units.
    pub cap_height: f32,
    /// Bounding box [x_min, y_min, x_max, y_max] in font units.
    pub bbox: [f32; 4],
}

/// Parses font metrics from raw TTF/OTF bytes.
pub(crate) fn parse_font_metrics(data: &[u8]) -> PdfResult<ParsedFontMetrics> {
    parse_font_metrics_with_axes(data, &[])
}

/// Parses font metrics at specific variation axis settings.
///
/// Each axis is a `(tag, value)` pair, e.g., `("wght", 700.0)` for bold.
pub(crate) fn parse_font_metrics_with_axes(
    data: &[u8],
    axes: &[(&str, f32)],
) -> PdfResult<ParsedFontMetrics> {
    let font_ref = FontRef::new(data)
        .map_err(|e| PdfError::InvalidFont(format!("Failed to parse font: {}", e)))?;

    let location = build_location(&font_ref, axes);
    let metrics = Metrics::new(&font_ref, Size::unscaled(), &location);

    let ps_name = font_ref
        .localized_strings(skrifa::string::StringId::POSTSCRIPT_NAME)
        .english_or_first()
        .map(|s| s.chars().collect::<String>())
        .unwrap_or_else(|| "UnknownFont".to_string());

    let bbox = metrics
        .bounds
        .map(|b| [b.x_min, b.y_min, b.x_max, b.y_max])
        .unwrap_or([0.0, 0.0, 1000.0, 1000.0]);

    Ok(ParsedFontMetrics {
        ps_name,
        units_per_em: metrics.units_per_em,
        ascent: metrics.ascent,
        descent: metrics.descent,
        cap_height: metrics.cap_height.unwrap_or(metrics.ascent * 0.7),
        bbox,
    })
}

/// Result of glyph collection and font subsetting.
pub(crate) struct SubsetResult {
    /// The subsetted font program bytes.
    pub data: Vec<u8>,
    /// Maps Unicode characters to original glyph IDs.
    pub char_to_gid: BTreeMap<char, u16>,
    /// Maps original glyph IDs to new (sequential) glyph IDs.
    pub old_to_new: BTreeMap<u16, u16>,
    /// Glyph widths indexed by original glyph ID, in font units.
    pub widths: BTreeMap<u16, u16>,
}

/// Builds a skrifa Location from axis tag/value pairs.
///
/// Public within the crate so `embedding::measure_text` can use it.
pub(crate) fn build_location_pub(
    font_ref: &FontRef<'_>,
    axes: &[(&str, f32)],
) -> skrifa::instance::Location {
    build_location(font_ref, axes)
}

/// Builds a skrifa Location from axis tag/value pairs.
fn build_location(font_ref: &FontRef<'_>, axes: &[(&str, f32)]) -> skrifa::instance::Location {
    if axes.is_empty() {
        return skrifa::instance::Location::default();
    }
    let axis_settings: Vec<_> = axes
        .iter()
        .filter_map(|(tag, val)| {
            let tag_bytes = tag.as_bytes();
            if tag_bytes.len() == 4 {
                let arr: [u8; 4] = [tag_bytes[0], tag_bytes[1], tag_bytes[2], tag_bytes[3]];
                let t = skrifa::Tag::new(&arr);
                Some((t, *val))
            } else {
                None
            }
        })
        .collect();
    let axis_collection = font_ref.axes();
    axis_collection.location(axis_settings)
}

/// Collects glyph IDs for the given characters, then subsets the font
/// to include only those glyphs plus `.notdef`.
///
/// Uses skrifa for glyph mapping/metrics and allsorts for subsetting.
pub(crate) fn collect_glyphs_and_subset(
    font_data: &[u8],
    chars: &[char],
) -> PdfResult<SubsetResult> {
    collect_glyphs_and_subset_with_axes(font_data, chars, &[])
}

/// Collects glyph IDs and subsets at specific variation axis settings.
pub(crate) fn collect_glyphs_and_subset_with_axes(
    font_data: &[u8],
    chars: &[char],
    axes: &[(&str, f32)],
) -> PdfResult<SubsetResult> {
    let font_ref = FontRef::new(font_data)
        .map_err(|e| PdfError::InvalidFont(format!("Font parse error: {}", e)))?;

    let charmap = font_ref.charmap();
    let location = build_location(&font_ref, axes);
    let glyph_metrics = GlyphMetrics::new(&font_ref, Size::unscaled(), &location);

    // Collect glyph IDs, char→gid mapping, and widths
    let mut glyph_set: BTreeSet<u16> = BTreeSet::new();
    glyph_set.insert(0); // .notdef always included
    let mut char_to_gid: BTreeMap<char, u16> = BTreeMap::new();
    let mut widths: BTreeMap<u16, u16> = BTreeMap::new();

    // .notdef width
    widths.insert(
        0,
        glyph_metrics
            .advance_width(skrifa::GlyphId::new(0))
            .unwrap_or(0.0) as u16,
    );

    for &ch in chars {
        if let Some(gid) = charmap.map(ch) {
            let gid_u16 = gid.to_u32() as u16;
            if let std::collections::btree_map::Entry::Vacant(e) = char_to_gid.entry(ch) {
                e.insert(gid_u16);
                glyph_set.insert(gid_u16);
                let width = glyph_metrics.advance_width(gid).unwrap_or(0.0) as u16;
                widths.insert(gid_u16, width);
            }
        }
    }

    let glyph_ids: Vec<u16> = glyph_set.into_iter().collect();

    // Subset with allsorts
    let scope = ReadScope::new(font_data);
    let font_data_parsed = scope
        .read::<FontData<'_>>()
        .map_err(|e| PdfError::InvalidFont(format!("Allsorts parse error: {}", e)))?;
    let provider = font_data_parsed
        .table_provider(0)
        .map_err(|e| PdfError::InvalidFont(format!("Allsorts table provider: {}", e)))?;

    let subset_data = subset::subset(
        &provider,
        &glyph_ids,
        &SubsetProfile::Minimal,
        CmapTarget::Unrestricted,
    )
    .map_err(|e| PdfError::InvalidFont(format!("Subsetting failed: {:?}", e)))?;

    // Build old GID → new GID mapping (subset reindexes glyphs sequentially)
    let mut old_to_new: BTreeMap<u16, u16> = BTreeMap::new();
    for (new_idx, &old_gid) in glyph_ids.iter().enumerate() {
        old_to_new.insert(old_gid, new_idx as u16);
    }

    Ok(SubsetResult {
        data: subset_data,
        char_to_gid,
        old_to_new,
        widths,
    })
}

/// Parameters for building a `/FontDescriptor` dictionary.
pub(crate) struct FontDescriptorParams<'a> {
    /// PostScript name.
    pub ps_name: &'a str,
    /// Units per em.
    pub units_per_em: u16,
    /// Ascent in font units.
    pub ascent: f32,
    /// Descent in font units.
    pub descent: f32,
    /// Cap height in font units.
    pub cap_height: f32,
    /// Bounding box in font units.
    pub bbox: [f32; 4],
    /// Font flags: 32 for Nonsymbolic (Latin), 4 for Symbolic (CID).
    pub flags: i64,
    /// Font file dictionary key: `"FontFile2"` for TrueType, `"FontFile3"` for CFF.
    pub font_file_key: &'a str,
    /// Reference to the font file stream object.
    pub font_file_ref: Object,
}

/// Builds a `/FontDescriptor` dictionary.
pub(crate) fn build_font_descriptor(params: FontDescriptorParams<'_>) -> Dictionary {
    let mut dict = Dictionary::new();
    dict.insert(
        PdfName::new("Type"),
        Object::Name(PdfName::new("FontDescriptor")),
    );
    dict.insert(
        PdfName::new("FontName"),
        Object::Name(PdfName::new(params.ps_name)),
    );
    dict.insert(PdfName::new("Flags"), Object::Integer(params.flags));

    let scale = 1000.0 / params.units_per_em as f64;
    dict.insert(
        PdfName::new("Ascent"),
        Object::Integer((params.ascent as f64 * scale).round() as i64),
    );
    dict.insert(
        PdfName::new("Descent"),
        Object::Integer((params.descent as f64 * scale).round() as i64),
    );
    dict.insert(
        PdfName::new("CapHeight"),
        Object::Integer((params.cap_height as f64 * scale).round() as i64),
    );
    dict.insert(
        PdfName::new("FontBBox"),
        Object::Array(
            params
                .bbox
                .iter()
                .map(|&v| Object::Integer((v as f64 * scale).round() as i64))
                .collect(),
        ),
    );

    dict.insert(PdfName::new("ItalicAngle"), Object::Integer(0));
    dict.insert(PdfName::new("StemV"), Object::Integer(80));
    dict.insert(PdfName::new(params.font_file_key), params.font_file_ref);
    dict
}

/// Builds a font program stream (for `/FontFile2` or `/FontFile3`).
pub(crate) fn build_font_stream(data: &[u8]) -> PdfResult<PdfStream> {
    let mut dict = Dictionary::new();
    let (compressed, filter) = encode_flate(data)?;
    dict.insert(PdfName::new("Length1"), Object::Integer(data.len() as i64));
    if let Some(f) = filter {
        dict.insert(PdfName::new("Filter"), Object::Name(PdfName::new(f)));
    }
    Ok(PdfStream::new(dict, compressed))
}

/// Builds a `/ToUnicode` CMap stream.
///
/// `byte_width`: 1 for simple fonts (single-byte codes), 2 for CID fonts.
pub(crate) fn build_to_unicode_cmap(
    char_to_gid: &BTreeMap<char, u16>,
    old_to_new: &BTreeMap<u16, u16>,
    byte_width: usize,
) -> PdfResult<PdfStream> {
    let mut cmap = String::with_capacity(512 + char_to_gid.len() * 30);
    cmap.push_str("/CIDInit /ProcSet findresource begin\n");
    cmap.push_str("12 dict begin\n");
    cmap.push_str("begincmap\n");
    cmap.push_str("/CIDSystemInfo\n");
    cmap.push_str("<< /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    cmap.push_str("/CMapName /Adobe-Identity-UCS def\n");
    cmap.push_str("/CMapType 2 def\n");
    cmap.push_str("1 begincodespacerange\n");
    if byte_width == 1 {
        cmap.push_str("<00> <FF>\n");
    } else {
        cmap.push_str("<0000> <FFFF>\n");
    }
    cmap.push_str("endcodespacerange\n");

    // Collect and sort mappings by new glyph ID
    let hex_width = byte_width * 2;
    let mut mappings: Vec<(u16, char)> = Vec::new();
    for (&ch, &old_gid) in char_to_gid {
        if let Some(&new_gid) = old_to_new.get(&old_gid) {
            mappings.push((new_gid, ch));
        }
    }
    mappings.sort_by_key(|&(gid, _)| gid);

    if !mappings.is_empty() {
        for chunk in mappings.chunks(100) {
            let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
            for &(gid, ch) in chunk {
                let unicode = ch as u32;
                if unicode > 0xFFFF {
                    // Non-BMP: encode as UTF-16 surrogate pair
                    let u = unicode - 0x10000;
                    let high = 0xD800 + (u >> 10);
                    let low = 0xDC00 + (u & 0x3FF);
                    let _ = writeln!(
                        cmap,
                        "<{:0>width$X}> <{:04X}{:04X}>",
                        gid,
                        high,
                        low,
                        width = hex_width
                    );
                } else {
                    let _ = writeln!(
                        cmap,
                        "<{:0>width$X}> <{:04X}>",
                        gid,
                        unicode,
                        width = hex_width
                    );
                }
            }
            cmap.push_str("endbfchar\n");
        }
    }

    cmap.push_str("endcmap\n");
    cmap.push_str("CMapName currentdict /CMap defineresource pop\n");
    cmap.push_str("end\n");
    cmap.push_str("end\n");

    let data = cmap.into_bytes();
    let (compressed, filter) = encode_flate(&data)?;
    let mut dict = Dictionary::new();
    if let Some(f) = filter {
        dict.insert(PdfName::new("Filter"), Object::Name(PdfName::new(f)));
    }
    Ok(PdfStream::new(dict, compressed))
}
