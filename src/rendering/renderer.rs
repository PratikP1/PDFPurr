//! Core rendering engine that converts PDF pages to pixel images.

use std::collections::HashMap;

use tiny_skia::{Pixmap, Transform};

use crate::content::{tokenize_content_stream, ContentToken};
use crate::core::objects::{DictExt, Object, PdfStream};
use crate::document::Document;
use crate::error::PdfResult;

use crate::fonts::standard14::Standard14Font;

use super::color_space::RenderColorSpace;
use super::colors::{
    cmyk_from_ops, obj_f32, obj_f64, op_f32, parse_transform, pdf_blend_mode, rgb_from_ops,
    set_color_from_operands, set_gray,
};
use super::glyph::{
    extract_cid_font_program, extract_font_program, CidFontProgram, FontProgram, Type3FontProgram,
};
use super::graphics::RenderStateStack;
use super::image::{render_image, render_inline_image};
use super::path::{paint_path, PathAccumulator};
use super::shading::render_shading;
use super::text::{render_cid_text_string, render_text_string, TextObject};

/// PDF TJ array displacement values are in 1/1000 of text space units.
/// ISO 32000-2:2020, Section 9.4.3.
const TJ_ADJUSTMENT_SCALE: f64 = 1000.0;

/// sRGB luminance coefficients for computing soft mask luminosity.
/// ISO 32000-2:2020, Section 11.5.2; ITU-R BT.709.
const SRGB_LUMA_R: f32 = 0.2126;
const SRGB_LUMA_G: f32 = 0.7152;
const SRGB_LUMA_B: f32 = 0.0722;

/// Groups all font-related state for the current content stream.
///
/// Passed through the operator dispatch pipeline to avoid parameter sprawl.
struct FontState {
    current_font: Option<Standard14Font>,
    current_font_program: Option<String>,
    font_cache: HashMap<String, FontProgram>,
    current_type3_font: Option<String>,
    type3_cache: HashMap<String, Type3FontProgram>,
    current_cid_font: Option<String>,
    cid_font_cache: HashMap<String, CidFontProgram>,
}

impl FontState {
    fn new() -> Self {
        Self {
            current_font: None,
            current_font_program: None,
            font_cache: HashMap::new(),
            current_type3_font: None,
            type3_cache: HashMap::new(),
            current_cid_font: None,
            cid_font_cache: HashMap::new(),
        }
    }
}

/// Options controlling how a page is rendered.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Resolution in dots per inch. Default: `72.0` (1:1 with PDF points).
    pub dpi: f64,
    /// Background color as `[r, g, b, a]` in 0–255. Default: opaque white.
    pub background: [u8; 4],
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            dpi: 72.0,
            background: [255, 255, 255, 255],
        }
    }
}

/// Renders PDF pages to pixel images.
///
/// Interprets content stream operators and paints them onto a
/// [`tiny_skia::Pixmap`] using the document's fonts, images, and
/// color spaces.
///
/// # Example
///
/// ```
/// use pdfpurr::Document;
/// use pdfpurr::rendering::{Renderer, RenderOptions};
///
/// let mut doc = Document::new();
/// doc.add_page(612.0, 792.0).unwrap();
/// let bytes = doc.to_bytes().unwrap();
/// let doc = Document::from_bytes(&bytes).unwrap();
///
/// let renderer = Renderer::new(&doc, RenderOptions::default());
/// let pixmap = renderer.render_page(0).unwrap();
/// assert_eq!(pixmap.width(), 612);
/// ```
pub struct Renderer<'a> {
    doc: &'a Document,
    options: RenderOptions,
}

impl<'a> Renderer<'a> {
    /// Creates a new renderer for the given document.
    pub fn new(doc: &'a Document, options: RenderOptions) -> Self {
        Self { doc, options }
    }

    /// Renders a single page to a pixel image.
    ///
    /// `page_index` is zero-based. Returns an error if the index is out of
    /// bounds or the page cannot be rendered.
    pub fn render_page(&self, page_index: usize) -> PdfResult<Pixmap> {
        let pages = self.doc.pages()?;
        let page_dict = pages.get(page_index).ok_or_else(|| {
            crate::error::PdfError::Other(format!(
                "Page index {} out of range (document has {} pages)",
                page_index,
                pages.len()
            ))
        })?;

        // Read MediaBox, then CropBox (defaults to MediaBox per ISO 32000-2 §7.7.3.3)
        let media_box = self.read_media_box(page_dict)?;
        let crop_box = self.read_box(page_dict, "CropBox").unwrap_or(media_box);
        let (width_pt, height_pt) = (crop_box[2] - crop_box[0], crop_box[3] - crop_box[1]);

        // Handle /Rotate (ISO 32000-2 §7.7.3.3) — must be a multiple of 90
        let rotate = page_dict.get_i64("Rotate").unwrap_or(0) % 360;
        let (px_w_pt, px_h_pt) = if rotate == 90 || rotate == 270 || rotate == -90 || rotate == -270
        {
            (height_pt, width_pt)
        } else {
            (width_pt, height_pt)
        };

        // Convert to pixels
        let scale = self.options.dpi / 72.0;
        let px_w = (px_w_pt * scale).round() as u32;
        let px_h = (px_h_pt * scale).round() as u32;

        let mut pixmap = Pixmap::new(px_w, px_h).ok_or_else(|| {
            crate::error::PdfError::Other(format!(
                "Cannot create {}x{} pixmap (zero or too large)",
                px_w, px_h
            ))
        })?;

        // Fill with background color
        let [r, g, b, a] = self.options.background;
        let bg = tiny_skia::Color::from_rgba8(r, g, b, a);
        pixmap.fill(bg);

        // Set up base transform: PDF bottom-left origin → tiny-skia top-left origin
        // Also apply DPI scaling. Use crop_box origin so content outside CropBox is clipped.
        let s = scale as f32;
        let base_transform = match rotate {
            90 | -270 => {
                // Rotate 90° CW: x' = y, y' = width - x
                Transform::from_row(
                    0.0,
                    -s,
                    s,
                    0.0,
                    -(crop_box[1] * scale) as f32,
                    (crop_box[2] * scale) as f32,
                )
            }
            270 | -90 => {
                // Rotate 270° CW: compose rotation [0,-1,1,0] with Y-flip [1,0,0,-1]
                Transform::from_row(
                    0.0,
                    -s,
                    -s,
                    0.0,
                    (crop_box[3] * scale) as f32,
                    (crop_box[2] * scale) as f32,
                )
            }
            180 | -180 => {
                // Rotate 180°: x' = width - x, y' = height - y
                Transform::from_row(
                    -s,
                    0.0,
                    0.0,
                    s,
                    (crop_box[2] * scale) as f32,
                    -(crop_box[1] * scale) as f32,
                )
            }
            _ => {
                // No rotation (0°)
                Transform::from_row(
                    s,
                    0.0,
                    0.0,
                    -s,
                    -(crop_box[0] * scale) as f32,
                    (crop_box[3] * scale) as f32,
                )
            }
        };

        // Tokenize and interpret the content stream
        if let Some(contents_obj) = page_dict.get_str("Contents") {
            if let Ok(data) = self.doc.resolve_content_data(contents_obj) {
                if let Ok(tokens) = tokenize_content_stream(&data) {
                    self.interpret(&tokens, &mut pixmap, base_transform, page_dict)?;
                }
            }
        }

        // Render annotations on top of page content
        self.render_annotations(page_dict, &mut pixmap, base_transform);

        Ok(pixmap)
    }

    /// Renders visible annotations onto the pixmap.
    ///
    /// Draws annotation rectangles for Link (blue border) and Highlight
    /// (yellow overlay) annotations. Annotations with the Hidden flag or
    /// without a valid rectangle are skipped.
    fn render_annotations(
        &self,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        transform: Transform,
    ) {
        use crate::core::objects::{DictExt, Object};

        let annots_obj = match page_dict.get_str("Annots") {
            Some(obj) => self.doc.resolve(obj).unwrap_or(obj),
            None => return,
        };
        let annots_arr = match annots_obj.as_array() {
            Some(arr) => arr,
            None => return,
        };

        for annot_ref in annots_arr {
            let annot_obj = match annot_ref {
                Object::Reference(r) => match self.doc.get_object(r.id()) {
                    Some(o) => o,
                    None => continue,
                },
                other => other,
            };
            let annot_dict = match annot_obj.as_dict() {
                Some(d) => d,
                None => continue,
            };

            // Check /F flags for Hidden
            let flags = annot_dict.get_i64("F").unwrap_or(0) as u32;
            if flags & 2 != 0 {
                continue; // Hidden
            }

            let rect = match annot_dict.get_str("Rect").and_then(|o| o.parse_rect()) {
                Some(r) => r,
                None => continue,
            };
            let [x1, y1, x2, y2] = rect;
            if x1 == x2 || y1 == y2 {
                continue;
            }

            // Try /AP/N appearance stream first (ISO 32000-2, Section 12.5.5)
            if self.try_render_appearance_stream(annot_dict, rect, pixmap, transform) {
                continue;
            }

            // Fallback: hardcoded rendering for common subtypes
            let subtype = annot_dict.get_name("Subtype").unwrap_or("");
            match subtype {
                "Link" => {
                    // Draw blue border rectangle
                    let rect = match tiny_skia::Rect::from_ltrb(
                        x1 as f32, y1 as f32, x2 as f32, y2 as f32,
                    ) {
                        Some(r) => r,
                        None => continue,
                    };
                    let path = tiny_skia::PathBuilder::from_rect(rect);

                    let mut paint = tiny_skia::Paint::default();
                    paint.set_color_rgba8(0, 0, 255, 64);
                    pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);

                    let mut stroke_paint = tiny_skia::Paint::default();
                    stroke_paint.set_color_rgba8(0, 0, 200, 180);
                    let stroke = tiny_skia::Stroke {
                        width: 1.0,
                        ..Default::default()
                    };
                    pixmap.stroke_path(&path, &stroke_paint, &stroke, transform, None);
                }
                "Highlight" => {
                    let rect = match tiny_skia::Rect::from_ltrb(
                        x1 as f32, y1 as f32, x2 as f32, y2 as f32,
                    ) {
                        Some(r) => r,
                        None => continue,
                    };
                    let color = annot_dict
                        .get_str("C")
                        .and_then(|o| o.as_array())
                        .filter(|a| a.len() >= 3)
                        .map(|a| {
                            (
                                (a[0].as_f64().unwrap_or(1.0) * 255.0) as u8,
                                (a[1].as_f64().unwrap_or(1.0) * 255.0) as u8,
                                (a[2].as_f64().unwrap_or(0.0) * 255.0) as u8,
                            )
                        })
                        .unwrap_or((255, 255, 0));
                    let path = tiny_skia::PathBuilder::from_rect(rect);
                    let mut paint = tiny_skia::Paint::default();
                    paint.set_color_rgba8(color.0, color.1, color.2, 80);
                    pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
                }
                _ => {}
            }
        }
    }

    /// Attempts to render an annotation's appearance stream (/AP/N).
    ///
    /// Returns `true` if an appearance stream was found and rendered,
    /// `false` if the caller should fall back to hardcoded rendering.
    fn try_render_appearance_stream(
        &self,
        annot_dict: &crate::core::objects::Dictionary,
        rect: [f64; 4],
        pixmap: &mut Pixmap,
        base_transform: Transform,
    ) -> bool {
        use crate::core::objects::{DictExt, Object};

        // Get /AP dictionary
        let ap_obj = match annot_dict.get_str("AP") {
            Some(o) => self.doc.resolve(o).unwrap_or(o),
            None => return false,
        };
        let ap_dict = match ap_obj.as_dict() {
            Some(d) => d,
            None => return false,
        };

        // Get /N (normal appearance) — may be a stream or dict of states
        let n_obj = match ap_dict.get_str("N") {
            Some(o) => self.doc.resolve(o).unwrap_or(o),
            None => return false,
        };

        let stream = match n_obj.as_stream() {
            Some(s) => s,
            None => return false,
        };

        // Decode the appearance stream content
        let data = match stream.decode_data() {
            Ok(d) => d,
            Err(_) => return false,
        };

        let tokens = match crate::content::tokenize_content_stream(&data) {
            Ok(t) => t,
            Err(_) => return false,
        };

        // Build transform: map the appearance's BBox to the annotation Rect.
        let bbox = stream
            .dict
            .get_str("BBox")
            .and_then(|o| o.parse_rect())
            .unwrap_or([0.0, 0.0, rect[2] - rect[0], rect[3] - rect[1]]);

        let bw = bbox[2] - bbox[0];
        let bh = bbox[3] - bbox[1];
        let rw = rect[2] - rect[0];
        let rh = rect[3] - rect[1];

        let sx = if bw.abs() > 1e-6 { rw / bw } else { 1.0 };
        let sy = if bh.abs() > 1e-6 { rh / bh } else { 1.0 };

        // Translate to annotation rect origin, scale BBox to Rect
        let ap_transform = base_transform
            .pre_translate(rect[0] as f32, rect[1] as f32)
            .pre_scale(sx as f32, sy as f32)
            .pre_translate(-bbox[0] as f32, -bbox[1] as f32);

        // Render using the existing content stream interpreter
        let mut state_stack = super::graphics::RenderStateStack::new(ap_transform);
        let mut clip_mask: Option<tiny_skia::Mask> = None;
        // Use the stream's /Resources if available, falling back to empty
        let resources_dict = stream
            .dict
            .get_str("Resources")
            .and_then(|o| self.doc.resolve(o))
            .and_then(|o| o.as_dict());

        // Build a temporary page dict with the stream's resources for interpret_tokens
        let page_proxy = if let Some(res) = resources_dict {
            let mut d = crate::core::objects::Dictionary::new();
            d.insert(
                crate::core::objects::PdfName::new("Resources"),
                Object::Dictionary(res.clone()),
            );
            d
        } else {
            crate::core::objects::Dictionary::new()
        };

        self.interpret_tokens(
            &tokens,
            &mut state_stack,
            &page_proxy,
            pixmap,
            &mut clip_mask,
        );
        true
    }

    /// Interprets content stream tokens and renders them onto the pixmap.
    fn interpret(
        &self,
        tokens: &[ContentToken],
        pixmap: &mut Pixmap,
        base_transform: Transform,
        page_dict: &crate::core::objects::Dictionary,
    ) -> PdfResult<()> {
        let mut state_stack = RenderStateStack::new(base_transform);
        let mut clip_mask: Option<tiny_skia::Mask> = None;
        self.interpret_tokens(tokens, &mut state_stack, page_dict, pixmap, &mut clip_mask);
        Ok(())
    }

    /// Core token dispatch loop shared by page rendering, form XObjects, and tiling patterns.
    fn interpret_tokens(
        &self,
        tokens: &[ContentToken],
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        let mut path = PathAccumulator::new();
        let mut text_obj = TextObject::new();
        let mut fonts = FontState::new();
        let mut pending_clip_rule: Option<tiny_skia::FillRule> = None;
        let mut operands: Vec<&Object> = Vec::with_capacity(6);

        for token in tokens {
            match token {
                ContentToken::Operand(obj) => {
                    operands.push(obj);
                }
                ContentToken::Operator(op) => {
                    self.dispatch_operator(
                        op,
                        &operands,
                        state,
                        &mut path,
                        &mut text_obj,
                        &mut fonts,
                        page_dict,
                        pixmap,
                        clip_mask,
                        &mut pending_clip_rule,
                    );
                    operands.clear();
                }
                ContentToken::InlineImage { dict, data } => {
                    render_inline_image(dict, data, state.state(), pixmap);
                    operands.clear();
                }
            }
        }
    }

    /// Dispatches a single operator with its operands.
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn dispatch_operator(
        &self,
        op: &str,
        operands: &[&Object],
        state: &mut RenderStateStack,
        path: &mut PathAccumulator,
        text_obj: &mut TextObject,
        fonts: &mut FontState,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
        pending_clip_rule: &mut Option<tiny_skia::FillRule>,
    ) {
        match op {
            // --- Graphics state ---
            "q" => state.save(),
            "Q" => state.restore(),
            "cm" => {
                if operands.len() == 6 {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                        obj_f32(operands[0]),
                        obj_f32(operands[1]),
                        obj_f32(operands[2]),
                        obj_f32(operands[3]),
                        obj_f32(operands[4]),
                        obj_f32(operands[5]),
                    ) {
                        state.concat_ctm(Transform::from_row(a, b, c, d, e, f));
                    }
                }
            }
            "w" => set_state_f64(operands, state, |s, v| s.line_width = v),
            "J" => set_state_u8(operands, state, |s, v| s.line_cap = v),
            "j" => set_state_u8(operands, state, |s, v| s.line_join = v),
            "M" => set_state_f64(operands, state, |s, v| s.miter_limit = v),
            "d" => {
                if operands.len() == 2 {
                    if let Some(arr) = operands[0].as_array() {
                        let dash: Vec<f32> = arr.iter().filter_map(obj_f32).collect();
                        let phase = obj_f32(operands[1]).unwrap_or(0.0);
                        let s = state.state_mut();
                        s.dash_array = dash;
                        s.dash_phase = phase;
                    }
                }
            }
            "gs" => {
                if let Some(name) = operands.first().and_then(|o| o.as_name()) {
                    self.handle_gs(name, state, page_dict, pixmap);
                }
            }

            // --- Path construction ---
            "m" => path.move_to_ops(operands),
            "l" => path.line_to_ops(operands),
            "c" => path.cubic_to_ops(operands),
            "v" => path.cubic_to_v_ops(operands),
            "y" => path.cubic_to_y_ops(operands),
            "h" => path.close(),
            "re" => path.rect_ops(operands),

            // --- Path painting (fill, stroke, even_odd) ---
            "S" => self.finish_path(
                path,
                state,
                pixmap,
                false,
                true,
                false,
                clip_mask,
                pending_clip_rule,
            ),
            "s" => {
                path.close();
                self.finish_path(
                    path,
                    state,
                    pixmap,
                    false,
                    true,
                    false,
                    clip_mask,
                    pending_clip_rule,
                );
            }
            "f" | "F" => self.finish_path(
                path,
                state,
                pixmap,
                true,
                false,
                false,
                clip_mask,
                pending_clip_rule,
            ),
            "f*" => self.finish_path(
                path,
                state,
                pixmap,
                true,
                false,
                true,
                clip_mask,
                pending_clip_rule,
            ),
            "B" => self.finish_path(
                path,
                state,
                pixmap,
                true,
                true,
                false,
                clip_mask,
                pending_clip_rule,
            ),
            "B*" => self.finish_path(
                path,
                state,
                pixmap,
                true,
                true,
                true,
                clip_mask,
                pending_clip_rule,
            ),
            "b" => {
                path.close();
                self.finish_path(
                    path,
                    state,
                    pixmap,
                    true,
                    true,
                    false,
                    clip_mask,
                    pending_clip_rule,
                );
            }
            "b*" => {
                path.close();
                self.finish_path(
                    path,
                    state,
                    pixmap,
                    true,
                    true,
                    true,
                    clip_mask,
                    pending_clip_rule,
                );
            }
            "n" => {
                if let Some(rule) = pending_clip_rule.take() {
                    if path.has_content() {
                        let acc = std::mem::take(path);
                        if let Some(built_path) = acc.finish() {
                            Self::apply_clip_from_path(&built_path, state, pixmap, clip_mask, rule);
                        }
                        *path = PathAccumulator::new();
                    }
                } else {
                    path.reset();
                }
            }
            "W" => *pending_clip_rule = Some(tiny_skia::FillRule::Winding),
            "W*" => *pending_clip_rule = Some(tiny_skia::FillRule::EvenOdd),

            // --- Color operators (fill = lowercase, stroke = UPPERCASE) ---
            "g" => self.set_gray_color(operands, state, true),
            "G" => self.set_gray_color(operands, state, false),
            "rg" => self.set_rgb_color(operands, state, true),
            "RG" => self.set_rgb_color(operands, state, false),
            "k" => self.set_cmyk_color(operands, state, true),
            "K" => self.set_cmyk_color(operands, state, false),
            "cs" => self.set_color_space(operands, state, page_dict, true),
            "CS" => self.set_color_space(operands, state, page_dict, false),
            "sc" | "scn" => self.set_color_or_pattern(operands, state, page_dict, pixmap, true),
            "SC" | "SCN" => self.set_color_or_pattern(operands, state, page_dict, pixmap, false),

            // --- Text state operators ---
            "BT" => text_obj.begin(),
            "ET" => text_obj.end(),
            "Tf" => {
                if operands.len() == 2 {
                    if let Some(name) = operands[0].as_name() {
                        let resource_name = name.trim_start_matches('/');
                        // Allocate the key string once, reuse across all branches
                        let key = resource_name.to_string();

                        // Reset all font state
                        fonts.current_font = None;
                        fonts.current_font_program = None;
                        fonts.current_type3_font = None;
                        fonts.current_cid_font = None;

                        // Resolve font dict once, branch on subtype
                        let font_dict = self.resolve_font_dict(resource_name, page_dict);
                        let subtype = font_dict.and_then(|d| d.get_name("Subtype"));

                        if subtype == Some("Type0") {
                            if !fonts.cid_font_cache.contains_key(&key) {
                                if let Some(fd) = font_dict {
                                    if let Some(cp) = extract_cid_font_program(fd, self.doc) {
                                        fonts.cid_font_cache.insert(key.clone(), cp);
                                    }
                                }
                            }
                            if fonts.cid_font_cache.contains_key(&key) {
                                fonts.current_cid_font = Some(key.clone());
                            }
                        } else if subtype == Some("Type3") {
                            if !fonts.type3_cache.contains_key(&key) {
                                if let Some(fd) = font_dict {
                                    if let Some(t3) = Type3FontProgram::from_dict(fd, self.doc) {
                                        fonts.type3_cache.insert(key.clone(), t3);
                                    }
                                }
                            }
                            if fonts.type3_cache.contains_key(&key) {
                                fonts.current_type3_font = Some(key.clone());
                            }
                        } else {
                            // Try Standard 14 font by base name first
                            fonts.current_font = Standard14Font::from_name(resource_name);

                            // Try to resolve embedded font from page resources
                            if fonts.current_font.is_none() {
                                if !fonts.font_cache.contains_key(&key) {
                                    if let Some(fd) = font_dict {
                                        if let Some(fp) = extract_font_program(fd, self.doc) {
                                            fonts.font_cache.insert(key.clone(), fp);
                                        }
                                    }
                                }
                                if fonts.font_cache.contains_key(&key) {
                                    fonts.current_font_program = Some(key.clone());
                                }
                            }
                        }

                        state.state_mut().text_state.font_name = Some(key);
                    }
                    if let Some(size) = obj_f64(operands[1]) {
                        state.state_mut().text_state.font_size = size;
                    }
                }
            }
            "Td" => {
                if let (Some(tx), Some(ty)) = (
                    operands.first().and_then(|o| obj_f64(o)),
                    operands.get(1).and_then(|o| obj_f64(o)),
                ) {
                    text_obj.move_text_position(tx, ty);
                }
            }
            "TD" => {
                if let (Some(tx), Some(ty)) = (
                    operands.first().and_then(|o| obj_f64(o)),
                    operands.get(1).and_then(|o| obj_f64(o)),
                ) {
                    text_obj.move_text_position_td(
                        tx,
                        ty,
                        &mut state.state_mut().text_state.leading,
                    );
                }
            }
            "Tm" => {
                if operands.len() == 6 {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                        obj_f64(operands[0]),
                        obj_f64(operands[1]),
                        obj_f64(operands[2]),
                        obj_f64(operands[3]),
                        obj_f64(operands[4]),
                        obj_f64(operands[5]),
                    ) {
                        text_obj.set_text_matrix(a, b, c, d, e, f);
                    }
                }
            }
            "T*" => {
                let leading = state.state().text_state.leading;
                text_obj.next_line(leading);
            }
            "Tc" => set_text_f64(operands, state, |ts, v| ts.character_spacing = v),
            "Tw" => set_text_f64(operands, state, |ts, v| ts.word_spacing = v),
            "TL" => set_text_f64(operands, state, |ts, v| ts.leading = v),
            "Tz" => set_text_f64(operands, state, |ts, v| ts.horizontal_scaling = v),
            "Ts" => set_text_f64(operands, state, |ts, v| ts.rise = v),
            "Tr" => set_text_f64(operands, state, |ts, v| ts.rendering_mode = v as u8),

            // --- Text showing operators ---
            "Tj" => {
                if text_obj.is_active() {
                    if let Some(Object::String(s)) = operands.first() {
                        self.show_text(
                            &s.bytes, text_obj, state, fonts, page_dict, pixmap, clip_mask,
                        );
                    }
                }
            }
            "TJ" => {
                if text_obj.is_active() {
                    if let Some(Object::Array(arr)) = operands.first() {
                        self.show_tj_array(
                            arr, text_obj, state, fonts, page_dict, pixmap, clip_mask,
                        );
                    }
                }
            }
            "'" => {
                // Move to next line, then show string
                if text_obj.is_active() {
                    let leading = state.state().text_state.leading;
                    text_obj.next_line(leading);
                    if let Some(Object::String(s)) = operands.first() {
                        self.show_text(
                            &s.bytes, text_obj, state, fonts, page_dict, pixmap, clip_mask,
                        );
                    }
                }
            }
            "\"" => {
                // Set word/char spacing, move to next line, show string
                if text_obj.is_active() && operands.len() == 3 {
                    if let Some(ws) = obj_f64(operands[0]) {
                        state.state_mut().text_state.word_spacing = ws;
                    }
                    if let Some(cs) = obj_f64(operands[1]) {
                        state.state_mut().text_state.character_spacing = cs;
                    }
                    let leading = state.state().text_state.leading;
                    text_obj.next_line(leading);
                    if let Some(Object::String(s)) = operands.get(2) {
                        self.show_text(
                            &s.bytes, text_obj, state, fonts, page_dict, pixmap, clip_mask,
                        );
                    }
                }
            }

            // --- Image/XObject operators ---
            "Do" => {
                if let Some(name) = operands.first().and_then(|o| o.as_name()) {
                    self.handle_do(name, state, page_dict, pixmap, clip_mask);
                }
            }

            // --- Shading operator ---
            "sh" => {
                if let Some(name) = operands.first().and_then(|o| o.as_name()) {
                    self.handle_shading(name, state, page_dict, pixmap, clip_mask);
                }
            }

            // --- Rendering intent and flatness ---
            "ri" => {
                if let Some(name) = operands.first().and_then(|o| o.as_name()) {
                    state.state_mut().rendering_intent = parse_rendering_intent(name);
                }
            }
            "i" => {
                if let Some(v) = op_f32(operands, 0) {
                    state.state_mut().flatness = v as f64;
                }
            }

            // --- Type 3 glyph operators (consumed, widths come from /Widths) ---
            "d0" | "d1" => {}

            // --- Marked content (structural, no visual effect) ---
            "BMC" | "BDC" | "EMC" | "MP" | "DP" => {}

            _ => {
                tracing::debug!(operator = op, "unknown PDF operator");
            }
        }
    }

    // --- Color operator helpers (fill when `is_fill`, stroke otherwise) ---

    fn set_gray_color(&self, operands: &[&Object], state: &mut RenderStateStack, is_fill: bool) {
        if let Some(g) = op_f32(operands, 0) {
            let s = state.state_mut();
            if is_fill {
                set_gray(&mut s.fill_color, g);
                s.fill_color_space = RenderColorSpace::DeviceGray;
            } else {
                set_gray(&mut s.stroke_color, g);
                s.stroke_color_space = RenderColorSpace::DeviceGray;
            }
        }
    }

    fn set_rgb_color(&self, operands: &[&Object], state: &mut RenderStateStack, is_fill: bool) {
        if let Some(c) = rgb_from_ops(operands) {
            let s = state.state_mut();
            if is_fill {
                s.fill_color = c;
                s.fill_color_space = RenderColorSpace::DeviceRGB;
            } else {
                s.stroke_color = c;
                s.stroke_color_space = RenderColorSpace::DeviceRGB;
            }
        }
    }

    fn set_cmyk_color(&self, operands: &[&Object], state: &mut RenderStateStack, is_fill: bool) {
        if let Some(c) = cmyk_from_ops(operands) {
            let s = state.state_mut();
            if is_fill {
                s.fill_color = c;
                s.fill_color_space = RenderColorSpace::DeviceCMYK;
            } else {
                s.stroke_color = c;
                s.stroke_color_space = RenderColorSpace::DeviceCMYK;
            }
        }
    }

    fn set_color_space(
        &self,
        operands: &[&Object],
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        is_fill: bool,
    ) {
        if let Some(name) = operands.first().and_then(|o| o.as_name()) {
            if let Some(cs) = self.resolve_color_space(name, page_dict) {
                if is_fill {
                    state.state_mut().fill_color_space = cs;
                } else {
                    state.state_mut().stroke_color_space = cs;
                }
            }
        }
    }

    fn set_color_or_pattern(
        &self,
        operands: &[&Object],
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        is_fill: bool,
    ) {
        let cs = if is_fill {
            &state.state().fill_color_space
        } else {
            &state.state().stroke_color_space
        };

        if matches!(cs, RenderColorSpace::Pattern) {
            if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                let pat = self.resolve_pattern(name, state, page_dict, pixmap);
                if is_fill {
                    state.state_mut().fill_pattern = pat;
                } else {
                    state.state_mut().stroke_pattern = pat;
                }
            }
        } else {
            let (components, count) = collect_color_components(operands);
            let resolved = cs.to_color(&components[..count]);
            let s = state.state_mut();
            if let Some(c) = resolved {
                if is_fill {
                    s.fill_color = c;
                } else {
                    s.stroke_color = c;
                }
            } else if is_fill {
                set_color_from_operands(operands, &mut s.fill_color);
            } else {
                set_color_from_operands(operands, &mut s.stroke_color);
            }
        }
    }

    /// Handles the `Do` operator — renders an XObject by name.
    fn handle_do(
        &self,
        name: &str,
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        // Look up XObject in page resources
        let resources = match self.doc.page_resources(page_dict) {
            Some(r) => r,
            None => return,
        };

        let xobjects = match resources
            .get_str("XObject")
            .and_then(|o| self.doc.resolve(o))
        {
            Some(Object::Dictionary(d)) => d,
            _ => return,
        };

        let xobj = match xobjects.get_str(name).and_then(|o| self.doc.resolve(o)) {
            Some(o) => o,
            None => return,
        };

        if let Object::Stream(stream) = xobj {
            let subtype = stream.dict.get_name("Subtype").unwrap_or("");
            match subtype {
                "Image" => {
                    render_image(stream, state.state(), pixmap);
                }
                "Form" => {
                    self.render_form_xobject(stream, state, page_dict, pixmap, clip_mask);
                }
                _ => {}
            }
        }
    }

    /// Handles the `sh` operator — renders a shading pattern.
    fn handle_shading(
        &self,
        name: &str,
        state: &RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        let resources = match self.doc.page_resources(page_dict) {
            Some(r) => r,
            None => return,
        };

        let shadings = match resources
            .get_str("Shading")
            .and_then(|o| self.doc.resolve(o))
        {
            Some(Object::Dictionary(d)) => d,
            _ => return,
        };

        let shading_dict = match shadings.get_str(name).and_then(|o| self.doc.resolve(o)) {
            Some(Object::Dictionary(d)) => d,
            _ => return,
        };

        render_shading(
            shading_dict,
            state.state(),
            pixmap,
            clip_mask.as_ref(),
            self.doc,
        );
    }

    /// Resolves a color space by name from page resources.
    ///
    /// Tries device color space names first, then looks up in `/Resources /ColorSpace`.
    fn resolve_color_space(
        &self,
        name: &str,
        page_dict: &crate::core::objects::Dictionary,
    ) -> Option<RenderColorSpace> {
        // Try device names directly
        if let Some(cs) = RenderColorSpace::from_name(name) {
            return Some(cs);
        }

        // Look up in /Resources /ColorSpace dict
        let resources = self.doc.page_resources(page_dict)?;
        let cs_obj = resources
            .get_str("ColorSpace")
            .and_then(|o| self.doc.resolve(o))?;
        let cs_dict = match cs_obj {
            Object::Dictionary(d) => d,
            _ => return None,
        };
        let cs_entry = cs_dict.get_str(name).and_then(|o| self.doc.resolve(o))?;
        RenderColorSpace::from_object(cs_entry, self.doc)
    }

    /// Resolves a font resource and returns its dictionary.
    fn resolve_font_dict(
        &self,
        resource_name: &str,
        page_dict: &'a crate::core::objects::Dictionary,
    ) -> Option<&'a crate::core::objects::Dictionary> {
        let resources = self.doc.page_resources(page_dict)?;
        let fonts = resources
            .get_str("Font")
            .and_then(|o| self.doc.resolve(o))
            .and_then(|o| match o {
                Object::Dictionary(d) => Some(d),
                _ => None,
            })?;
        fonts
            .get_str(resource_name)
            .and_then(|o| self.doc.resolve(o))
            .and_then(|o| match o {
                Object::Dictionary(d) => Some(d),
                _ => None,
            })
    }

    /// Resolves and renders a fill pattern, storing it in the fill state.
    ///
    /// Dispatches to tiling (PatternType 1) or shading (PatternType 2) patterns.
    /// Resolves a named pattern and returns the rendered pattern data.
    fn resolve_pattern(
        &self,
        name: &str,
        state: &RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &Pixmap,
    ) -> Option<std::sync::Arc<super::graphics::PatternData>> {
        let name = name.trim_start_matches('/');
        let resources = self.doc.page_resources(page_dict)?;
        let patterns = match resources
            .get_str("Pattern")
            .and_then(|o| self.doc.resolve(o))
        {
            Some(Object::Dictionary(d)) => d,
            _ => return None,
        };
        let pat_obj = patterns.get_str(name).and_then(|o| self.doc.resolve(o));
        match pat_obj {
            Some(Object::Stream(s)) => self.render_tiling_pattern(s, page_dict),
            Some(Object::Dictionary(d))
                if matches!(d.get_str("PatternType"), Some(Object::Integer(2))) =>
            {
                self.render_shading_pattern(d, state, pixmap)
            }
            _ => None,
        }
    }

    /// Renders a shading pattern (PatternType 2) and returns the pattern data.
    fn render_shading_pattern(
        &self,
        pat_dict: &crate::core::objects::Dictionary,
        state: &RenderStateStack,
        pixmap: &Pixmap,
    ) -> Option<std::sync::Arc<super::graphics::PatternData>> {
        let shading_dict = match pat_dict
            .get_str("Shading")
            .and_then(|o| self.doc.resolve(o))
        {
            Some(Object::Dictionary(d)) => d,
            _ => return None,
        };

        let pattern_matrix =
            parse_transform(pat_dict.get_str("Matrix")).unwrap_or(Transform::identity());

        let mut pat_state = state.state().clone();
        pat_state.ctm = pat_state.ctm.pre_concat(pattern_matrix);

        let mut temp = Pixmap::new(pixmap.width(), pixmap.height())?;
        render_shading(shading_dict, &pat_state, &mut temp, None, self.doc);

        Some(std::sync::Arc::new(super::graphics::PatternData {
            pixmap: temp,
            x_step: None,
            y_step: None,
            transform: pattern_matrix,
        }))
    }

    /// Renders a tiling pattern (PatternType 1) and returns the pattern data.
    fn render_tiling_pattern(
        &self,
        pat_stream: &PdfStream,
        page_dict: &crate::core::objects::Dictionary,
    ) -> Option<std::sync::Arc<super::graphics::PatternData>> {
        // Read pattern properties
        match pat_stream.dict.get_str("PatternType") {
            Some(Object::Integer(1)) => {}
            _ => return None,
        }

        let bbox = match pat_stream.dict.get_str("BBox").and_then(|o| o.as_array()) {
            Some(arr) if arr.len() >= 4 => {
                let vals: Vec<f64> = arr.iter().filter_map(obj_f64).collect();
                if vals.len() < 4 {
                    return None;
                }
                [vals[0], vals[1], vals[2], vals[3]]
            }
            _ => return None,
        };

        let xstep = pat_stream.dict.get_str("XStep").and_then(obj_f64)?;
        let ystep = pat_stream.dict.get_str("YStep").and_then(obj_f64)?;

        // Render one tile to a pixmap
        let tile_w = xstep.abs().ceil() as u32;
        let tile_h = ystep.abs().ceil() as u32;
        if tile_w == 0 || tile_h == 0 {
            return None;
        }

        let mut tile_pixmap = Pixmap::new(tile_w, tile_h)?;

        // Set up a coordinate system where (0,0) maps to BBox origin
        let base_transform = Transform::from_translate(-bbox[0] as f32, -bbox[1] as f32);
        // Flip Y: tile coords are PDF-style (Y-up), but pixmap is Y-down
        let flip_y = Transform::from_row(1.0, 0.0, 0.0, -1.0, 0.0, tile_h as f32);
        let tile_ctm = flip_y.pre_concat(base_transform);

        let content_data = pat_stream.decode_data().ok()?;

        let mut tile_state = RenderStateStack::new(tile_ctm);
        let mut tile_clip: Option<tiny_skia::Mask> = None;
        let tokens = crate::content::operators::tokenize_content_stream(&content_data).ok()?;

        self.interpret_tokens(
            &tokens,
            &mut tile_state,
            page_dict,
            &mut tile_pixmap,
            &mut tile_clip,
        );

        let pattern_matrix =
            parse_transform(pat_stream.dict.get_str("Matrix")).unwrap_or(Transform::identity());

        Some(std::sync::Arc::new(super::graphics::PatternData {
            pixmap: tile_pixmap,
            x_step: Some(xstep.abs() as f32),
            y_step: Some(ystep.abs() as f32),
            transform: pattern_matrix,
        }))
    }

    /// Renders a Form XObject by interpreting its content stream.
    ///
    /// Saves/restores the graphics state around the form to isolate it.
    /// Applies the form's `/Matrix` (if any) to the CTM.
    fn render_form_xobject(
        &self,
        stream: &PdfStream,
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        // Decode the form's content stream
        let content_data = match stream.decode_data() {
            Ok(d) => d,
            Err(_) => return,
        };

        // Save state, apply form matrix, interpret, restore
        state.save();

        // Apply the form's /Matrix if present
        if let Some(t) = parse_transform(stream.dict.get_str("Matrix")) {
            state.concat_ctm(t);
        }

        // Interpret the form's content stream
        let tokens = match crate::content::operators::tokenize_content_stream(&content_data) {
            Ok(t) => t,
            Err(_) => {
                state.restore();
                return;
            }
        };

        // Apply BBox clipping if present (ISO 32000-2, 8.10.2)
        let bbox_clip = if let Some(Object::Array(arr)) = stream.dict.get_str("BBox") {
            let vals: Vec<f32> = arr.iter().filter_map(obj_f32).collect();
            if vals.len() >= 4 {
                tiny_skia::Rect::from_ltrb(vals[0], vals[1], vals[2], vals[3]).and_then(|r| {
                    let path = tiny_skia::PathBuilder::from_rect(r);
                    let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
                    mask.fill_path(
                        &path,
                        tiny_skia::FillRule::Winding,
                        false,
                        state.state().ctm,
                    );
                    Some(mask)
                })
            } else {
                None
            }
        } else {
            None
        };

        // Use the form's own /Resources if present, falling back to page resources
        let form_dict_ref;
        let effective_dict = if let Some(res_obj) = stream.dict.get_str("Resources") {
            let resolved = self.doc.resolve(res_obj).unwrap_or(res_obj).clone();
            let mut synthetic = crate::core::objects::Dictionary::new();
            synthetic.insert(crate::core::objects::PdfName::new("Resources"), resolved);
            form_dict_ref = synthetic;
            &form_dict_ref
        } else {
            page_dict
        };

        // Check for isolated transparency group
        let isolated = stream
            .dict
            .get_str("Group")
            .and_then(|o| match o {
                Object::Dictionary(d) => Some(d),
                _ => None,
            })
            .is_some_and(|d| {
                d.get_name("S") == Some("Transparency")
                    && matches!(d.get_str("I"), Some(Object::Boolean(true)))
            });

        if isolated {
            // Render to temporary transparent pixmap, then composite
            if let Some(mut temp) = Pixmap::new(pixmap.width(), pixmap.height()) {
                let mut temp_clip: Option<tiny_skia::Mask> = bbox_clip.clone();
                self.interpret_tokens(&tokens, state, effective_dict, &mut temp, &mut temp_clip);
                let paint = tiny_skia::PixmapPaint {
                    blend_mode: state.state().blend_mode,
                    ..tiny_skia::PixmapPaint::default()
                };
                pixmap.draw_pixmap(0, 0, temp.as_ref(), &paint, Transform::identity(), None);
            }
        } else if let Some(bbox_mask) = bbox_clip {
            // Install BBox clip, render, then restore original clip
            let saved_clip = clip_mask.take();
            *clip_mask = Some(bbox_mask);
            self.interpret_tokens(&tokens, state, effective_dict, pixmap, clip_mask);
            *clip_mask = saved_clip;
        } else {
            self.interpret_tokens(&tokens, state, effective_dict, pixmap, clip_mask);
        }

        state.restore();
    }

    /// Shows a text string, dispatching to Type 3 or regular rendering.
    #[allow(clippy::too_many_arguments)]
    fn show_text(
        &self,
        bytes: &[u8],
        text_obj: &mut TextObject,
        state: &mut RenderStateStack,
        fonts: &FontState,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        if let Some(t3) = fonts
            .current_type3_font
            .as_ref()
            .and_then(|k| fonts.type3_cache.get(k))
        {
            self.render_type3_text(bytes, text_obj, state, t3, page_dict, pixmap, clip_mask);
        } else if let Some(cid) = fonts
            .current_cid_font
            .as_ref()
            .and_then(|k| fonts.cid_font_cache.get(k))
        {
            let combined = Self::text_mask_combined(state, clip_mask, pixmap);
            let mask = combined
                .as_ref()
                .or(state.state().soft_mask.as_deref())
                .or(clip_mask.as_ref());
            render_cid_text_string(bytes, text_obj, state.state(), cid, pixmap, mask);
        } else {
            let combined = Self::text_mask_combined(state, clip_mask, pixmap);
            let mask = combined
                .as_ref()
                .or(state.state().soft_mask.as_deref())
                .or(clip_mask.as_ref());
            render_text_string(
                bytes,
                text_obj,
                state.state(),
                fonts.current_font.as_ref(),
                fonts
                    .current_font_program
                    .as_ref()
                    .and_then(|k| fonts.font_cache.get(k)),
                pixmap,
                mask,
            );
        }
    }

    /// Shows a TJ array, dispatching to Type 3 or regular rendering.
    #[allow(clippy::too_many_arguments)]
    fn show_tj_array(
        &self,
        array: &[Object],
        text_obj: &mut TextObject,
        state: &mut RenderStateStack,
        fonts: &FontState,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        let font_size = state.state().text_state.font_size;
        let h_scale = state.state().text_state.horizontal_scaling / 100.0;

        for item in array {
            match item {
                Object::String(s) => {
                    self.show_text(
                        &s.bytes, text_obj, state, fonts, page_dict, pixmap, clip_mask,
                    );
                }
                Object::Integer(adj) => {
                    let displacement = -(*adj as f64) / TJ_ADJUSTMENT_SCALE * font_size * h_scale;
                    text_obj.advance(displacement);
                }
                Object::Real(adj) => {
                    let displacement = -(*adj) / TJ_ADJUSTMENT_SCALE * font_size * h_scale;
                    text_obj.advance(displacement);
                }
                _ => {}
            }
        }
    }

    /// Renders text using a Type 3 font by interpreting CharProc content streams.
    ///
    /// For each byte, looks up the glyph stream, saves graphics state,
    /// applies font matrix + text positioning, interprets the stream,
    /// then restores state and advances the text position.
    #[allow(clippy::too_many_arguments)]
    fn render_type3_text(
        &self,
        text_bytes: &[u8],
        text_obj: &mut TextObject,
        state: &mut RenderStateStack,
        t3: &Type3FontProgram,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &mut Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
    ) {
        let font_size = state.state().text_state.font_size;
        let h_scale = state.state().text_state.horizontal_scaling / 100.0;
        let fm = t3.font_matrix();

        // Hoist reusable allocations outside the per-glyph loop
        let mut glyph_path = PathAccumulator::new();
        let mut glyph_text = TextObject::new();
        let mut glyph_fonts = FontState::new();
        let mut glyph_clip: Option<tiny_skia::FillRule> = None;

        for &byte in text_bytes {
            if let Some(glyph_data) = t3.glyph_stream(byte) {
                let tokens = match tokenize_content_stream(glyph_data) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!("Type3 glyph {byte} tokenization failed: {e}");
                        continue;
                    }
                };

                // Build glyph transform: FontMatrix scaled by font_size, at text position
                let tm = text_obj.text_matrix();
                let font_transform = Transform::from_row(
                    (fm[0] * font_size) as f32,
                    (fm[1] * font_size) as f32,
                    (fm[2] * font_size) as f32,
                    (fm[3] * font_size) as f32,
                    (fm[4] * font_size + tm[4]) as f32,
                    (fm[5] * font_size + tm[5]) as f32,
                );

                state.save();
                let combined = state.state().ctm.pre_concat(font_transform);
                state.state_mut().ctm = combined;

                // operands must be scoped with tokens (borrows token data)
                let mut operands: Vec<&Object> = Vec::with_capacity(6);
                for token in &tokens {
                    match token {
                        ContentToken::Operand(obj) => operands.push(obj),
                        ContentToken::Operator(op) => {
                            self.dispatch_operator(
                                op,
                                &operands,
                                state,
                                &mut glyph_path,
                                &mut glyph_text,
                                &mut glyph_fonts,
                                page_dict,
                                pixmap,
                                clip_mask,
                                &mut glyph_clip,
                            );
                            operands.clear();
                        }
                        ContentToken::InlineImage { .. } => operands.clear(),
                    }
                }

                state.restore();
            }

            // Advance by glyph width (width in glyph space, fm[0] converts to text space)
            let width = t3.glyph_width(byte).unwrap_or(0.0);
            text_obj.advance(width * fm[0] * font_size * h_scale);
        }
    }

    /// Handles the `gs` operator — applies an ExtGState dictionary.
    fn handle_gs(
        &self,
        name: &str,
        state: &mut RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &Pixmap,
    ) {
        let resources = match self.doc.page_resources(page_dict) {
            Some(r) => r,
            None => return,
        };

        let ext_g_states = match resources
            .get_str("ExtGState")
            .and_then(|o| self.doc.resolve(o))
        {
            Some(Object::Dictionary(d)) => d,
            _ => return,
        };

        let gs_dict = match ext_g_states.get_str(name).and_then(|o| self.doc.resolve(o)) {
            Some(Object::Dictionary(d)) => d,
            _ => return,
        };

        // Non-stroking alpha (ca)
        if let Some(alpha) = gs_dict.get_str("ca").and_then(obj_f32) {
            state.state_mut().fill_alpha = alpha;
        }

        // Stroking alpha (CA)
        if let Some(alpha) = gs_dict.get_str("CA").and_then(obj_f32) {
            state.state_mut().stroke_alpha = alpha;
        }

        // Blend mode (BM)
        if let Some(bm) = gs_dict
            .get_str("BM")
            .and_then(|o| o.as_name())
            .and_then(pdf_blend_mode)
        {
            state.state_mut().blend_mode = bm;
        }

        // Dash pattern (D)
        if let Some(Object::Array(d_arr)) = gs_dict.get_str("D") {
            if d_arr.len() == 2 {
                if let Some(dash) = d_arr[0].as_array() {
                    let arr: Vec<f32> = dash.iter().filter_map(obj_f32).collect();
                    let phase = obj_f32(&d_arr[1]).unwrap_or(0.0);
                    state.state_mut().dash_array = arr;
                    state.state_mut().dash_phase = phase;
                }
            }
        }

        // Line width (LW)
        if let Some(lw) = gs_dict.get_str("LW").and_then(obj_f64) {
            state.state_mut().line_width = lw;
        }

        // Line cap style (LC)
        if let Some(&Object::Integer(v)) = gs_dict.get_str("LC") {
            state.state_mut().line_cap = v as u8;
        }

        // Line join style (LJ)
        if let Some(&Object::Integer(v)) = gs_dict.get_str("LJ") {
            state.state_mut().line_join = v as u8;
        }

        // Miter limit (ML)
        if let Some(ml) = gs_dict.get_str("ML").and_then(obj_f64) {
            state.state_mut().miter_limit = ml;
        }

        // Flatness tolerance (FL)
        if let Some(fl) = gs_dict.get_str("FL").and_then(obj_f64) {
            state.state_mut().flatness = fl;
        }

        // Rendering intent (RI)
        if let Some(name) = gs_dict.get_str("RI").and_then(|o| o.as_name()) {
            state.state_mut().rendering_intent = parse_rendering_intent(name);
        }

        // Soft mask (SMask)
        match gs_dict.get_str("SMask") {
            Some(Object::Name(n)) if n.as_str() == "None" => {
                state.state_mut().soft_mask = None;
            }
            Some(smask_obj) => {
                if let Some(mask) = self.build_soft_mask(smask_obj, state, page_dict, pixmap) {
                    state.state_mut().soft_mask = Some(std::sync::Arc::new(mask));
                }
            }
            None => {}
        }
    }

    /// Builds a `tiny_skia::Mask` from an SMask dictionary.
    ///
    /// Renders the SMask's form XObject (`/G`) to a temporary pixmap
    /// and converts it to a luminosity or alpha mask.
    fn build_soft_mask(
        &self,
        smask_obj: &Object,
        state: &RenderStateStack,
        page_dict: &crate::core::objects::Dictionary,
        pixmap: &Pixmap,
    ) -> Option<tiny_skia::Mask> {
        let smask_dict = match self.doc.resolve(smask_obj)? {
            Object::Dictionary(d) => d,
            _ => return None,
        };

        let subtype = smask_dict
            .get_str("S")
            .and_then(|o| o.as_name())
            .unwrap_or("Luminosity");
        let is_alpha = subtype == "Alpha";

        // Get the form XObject stream (/G)
        let form_stream = smask_dict
            .get_str("G")
            .and_then(|o| self.doc.resolve(o))
            .and_then(|o| match o {
                Object::Stream(s) => Some(s),
                _ => None,
            })?;

        // Render the form XObject to a temporary pixmap
        let w = pixmap.width();
        let h = pixmap.height();
        let mut temp = Pixmap::new(w, h)?;

        // Start with black background for luminosity (unmasked areas = transparent)
        // For alpha masks, start with transparent background
        if !is_alpha {
            // Black background: luminosity of black = 0 = fully transparent
            temp.fill(tiny_skia::Color::BLACK);
        }

        let mut temp_state = RenderStateStack::new(state.state().ctm);
        let mut temp_clip: Option<tiny_skia::Mask> = None;
        self.render_form_xobject(
            form_stream,
            &mut temp_state,
            page_dict,
            &mut temp,
            &mut temp_clip,
        );

        // Convert rendered pixmap to a mask
        let mut mask = tiny_skia::Mask::new(w, h)?;
        let mask_data = mask.data_mut();
        let pixels = temp.pixels();

        for (i, pixel) in pixels.iter().enumerate() {
            mask_data[i] = if is_alpha {
                pixel.alpha()
            } else {
                // Luminosity: L = 0.2126 R + 0.7152 G + 0.0722 B (sRGB luminance)
                // pixel values are premultiplied, so demultiply first
                let a = pixel.alpha() as f32 / 255.0;
                if a < 0.001 {
                    0
                } else {
                    let r = pixel.red() as f32 / a;
                    let g = pixel.green() as f32 / a;
                    let b = pixel.blue() as f32 / a;
                    let lum = SRGB_LUMA_R * r + SRGB_LUMA_G * g + SRGB_LUMA_B * b;
                    // Multiply luminosity by alpha to get final mask value
                    (lum * a).round().clamp(0.0, 255.0) as u8
                }
            };
        }

        Some(mask)
    }

    /// Finishes the current path by painting it and resetting the accumulator.
    #[allow(clippy::too_many_arguments)]
    fn finish_path(
        &self,
        path: &mut PathAccumulator,
        state: &RenderStateStack,
        pixmap: &mut Pixmap,
        fill: bool,
        stroke: bool,
        even_odd: bool,
        clip_mask: &mut Option<tiny_skia::Mask>,
        pending_clip_rule: &mut Option<tiny_skia::FillRule>,
    ) {
        if !path.has_content() {
            path.reset();
            return;
        }

        let acc = std::mem::take(path);
        if let Some(built_path) = acc.finish() {
            // If W/W* was set, apply the built path as a clip
            if let Some(rule) = pending_clip_rule.take() {
                Self::apply_clip_from_path(&built_path, state, pixmap, clip_mask, rule);
            }

            let fill_rule = if even_odd {
                tiny_skia::FillRule::EvenOdd
            } else {
                tiny_skia::FillRule::Winding
            };
            // Determine the effective mask: soft mask, clip mask, or both combined
            let combined;
            let paint_mask: Option<&tiny_skia::Mask> =
                match (&state.state().soft_mask, clip_mask.as_ref()) {
                    (Some(soft), Some(clip)) => {
                        combined = Self::combine_masks(soft, clip, pixmap);
                        combined.as_ref()
                    }
                    (Some(soft), None) => Some(soft.as_ref()),
                    (None, clip) => clip,
                };
            paint_path(
                &built_path,
                state.state(),
                pixmap,
                fill,
                stroke,
                fill_rule,
                paint_mask,
            );
        }
        *path = PathAccumulator::new();
    }

    /// Computes the effective mask for text rendering, combining soft mask and
    /// clip mask the same way `finish_path` does for path painting.
    ///
    /// Only allocates when both soft mask and clip mask are present and must
    /// be combined. When only one mask exists, returns `None` (the caller
    /// should use `effective_text_mask` to get a reference).
    fn text_mask_combined(
        state: &RenderStateStack,
        clip_mask: &Option<tiny_skia::Mask>,
        pixmap: &Pixmap,
    ) -> Option<tiny_skia::Mask> {
        match (&state.state().soft_mask, clip_mask.as_ref()) {
            (Some(soft), Some(clip)) => Self::combine_masks(soft, clip, pixmap),
            _ => None,
        }
    }

    /// Combines a soft mask and a clip mask by taking the per-pixel minimum.
    fn combine_masks(
        soft: &tiny_skia::Mask,
        clip: &tiny_skia::Mask,
        pixmap: &Pixmap,
    ) -> Option<tiny_skia::Mask> {
        let mut combined = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
        let data = combined.data_mut();
        let clip_data = clip.data();
        let soft_data = soft.data();
        for i in 0..data.len() {
            data[i] = clip_data[i].min(soft_data[i]);
        }
        Some(combined)
    }

    /// Applies an already-built path as a clipping mask.
    fn apply_clip_from_path(
        built_path: &tiny_skia::Path,
        state: &RenderStateStack,
        pixmap: &Pixmap,
        clip_mask: &mut Option<tiny_skia::Mask>,
        rule: tiny_skia::FillRule,
    ) {
        if let Some(mut mask) = tiny_skia::Mask::new(pixmap.width(), pixmap.height()) {
            mask.fill_path(built_path, rule, true, state.state().ctm);
            *clip_mask = Some(mask);
        }
    }

    /// Reads a named rectangle (MediaBox, CropBox, etc.) from a page dictionary.
    fn read_box(
        &self,
        page_dict: &crate::core::objects::Dictionary,
        key: &str,
    ) -> Option<[f64; 4]> {
        page_dict
            .get_str(key)
            .and_then(|obj| self.doc.resolve(obj))
            .and_then(|obj| obj.parse_rect())
    }

    fn read_media_box(&self, page_dict: &crate::core::objects::Dictionary) -> PdfResult<[f64; 4]> {
        Ok(self
            .read_box(page_dict, "MediaBox")
            .unwrap_or([0.0, 0.0, 612.0, 792.0]))
    }
}

// --- Helper functions ---

/// Collects up to 4 color components from operands into a stack-allocated array.
/// Returns (buffer, count) where count is the number of numeric operands found.
/// Avoids heap allocation for the common case (1–4 components).
/// Sets a text state field from the first operand, if it's a valid f64.
/// Sets a text state field from the first operand, if it's a valid f64.
/// Sets a graphics state f64 field from the first operand.
fn set_state_f64<F>(operands: &[&Object], state: &mut RenderStateStack, setter: F)
where
    F: FnOnce(&mut super::graphics::RenderState, f64),
{
    if let Some(v) = operands.first().and_then(|o| obj_f64(o)) {
        setter(state.state_mut(), v);
    }
}

/// Sets a graphics state u8 field from the first integer operand.
fn set_state_u8<F>(operands: &[&Object], state: &mut RenderStateStack, setter: F)
where
    F: FnOnce(&mut super::graphics::RenderState, u8),
{
    if let Some(&Object::Integer(v)) = operands.first() {
        setter(state.state_mut(), *v as u8);
    }
}

/// Sets a text state field from the first operand, if it's a valid f64.
fn set_text_f64<F>(operands: &[&Object], state: &mut RenderStateStack, setter: F)
where
    F: FnOnce(&mut crate::fonts::graphics_state::TextState, f64),
{
    if let Some(v) = operands.first().and_then(|o| obj_f64(o)) {
        setter(&mut state.state_mut().text_state, v);
    }
}

fn collect_color_components(operands: &[&Object]) -> ([f32; 4], usize) {
    let mut buf = [0.0f32; 4];
    let mut count = 0;
    for op in operands.iter().take(4) {
        if let Some(v) = obj_f32(op) {
            buf[count] = v;
            count += 1;
        }
    }
    (buf, count)
}

/// Maps a PDF rendering intent name to a static string (ISO 32000-2 Table 69).
fn parse_rendering_intent(name: &str) -> &'static str {
    match name.trim_start_matches('/') {
        "AbsoluteColorimetric" => "AbsoluteColorimetric",
        "Saturation" => "Saturation",
        "Perceptual" => "Perceptual",
        _ => "RelativeColorimetric",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{Dictionary, IndirectRef, Object, PdfName, PdfStream};

    /// Creates a document with one page of the given dimensions (in points).
    fn doc_with_page(width: f64, height: f64) -> Document {
        let mut doc = Document::new();

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Creates a document with one 100x100 page and a content stream.
    fn doc_with_content(content: &[u8]) -> Document {
        let mut doc = Document::new();

        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Counts non-white pixels in a pixmap.
    fn count_non_white(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255)
            .count()
    }

    /// Checks if a specific pixel has approximately the expected color.
    fn pixel_approx(pixmap: &Pixmap, x: u32, y: u32, r: u8, g: u8, b: u8, tolerance: u8) -> bool {
        let pixel = pixmap.pixel(x, y).unwrap();
        let dr = (pixel.red() as i16 - r as i16).unsigned_abs() as u8;
        let dg = (pixel.green() as i16 - g as i16).unsigned_abs() as u8;
        let db = (pixel.blue() as i16 - b as i16).unsigned_abs() as u8;
        dr <= tolerance && dg <= tolerance && db <= tolerance
    }

    // --- 10A Tests ---

    #[test]
    fn render_blank_page_dimensions() {
        let doc = doc_with_page(612.0, 792.0);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 612);
        assert_eq!(pixmap.height(), 792);
    }

    #[test]
    fn render_blank_page_white_background() {
        let doc = doc_with_page(100.0, 100.0);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        for pixel in pixmap.pixels() {
            assert_eq!(pixel.red(), 255);
            assert_eq!(pixel.green(), 255);
            assert_eq!(pixel.blue(), 255);
            assert_eq!(pixel.alpha(), 255);
        }
    }

    #[test]
    fn render_blank_page_custom_dpi() {
        let doc = doc_with_page(612.0, 792.0);
        let opts = RenderOptions {
            dpi: 144.0,
            ..Default::default()
        };
        let renderer = Renderer::new(&doc, opts);
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 1224);
        assert_eq!(pixmap.height(), 1584);
    }

    #[test]
    fn render_blank_page_custom_background() {
        let doc = doc_with_page(50.0, 50.0);
        let opts = RenderOptions {
            dpi: 72.0,
            background: [128, 128, 128, 255],
        };
        let renderer = Renderer::new(&doc, opts);
        let pixmap = renderer.render_page(0).unwrap();

        for pixel in pixmap.pixels() {
            assert_eq!(pixel.red(), 128);
            assert_eq!(pixel.green(), 128);
            assert_eq!(pixel.blue(), 128);
        }
    }

    #[test]
    fn render_invalid_page_index_errors() {
        let doc = doc_with_page(612.0, 792.0);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        assert!(renderer.render_page(5).is_err());
    }

    #[test]
    fn document_render_page_convenience() {
        let doc = doc_with_page(612.0, 792.0);
        let pixmap = doc.render_page(0, 72.0).unwrap();
        assert_eq!(pixmap.width(), 612);
        assert_eq!(pixmap.height(), 792);
    }

    // --- 10C Tests: Path Operations ---

    #[test]
    fn render_filled_rect() {
        // Red filled rectangle from (10,10) to (90,90) in 100x100 page
        let doc = doc_with_content(b"1 0 0 rg 10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Center pixel (50,50) → PDF (50,50) → should be red
        // PDF y=50 → pixel y = 100-50 = 50
        assert!(count_non_white(&pixmap) > 100);
        assert!(pixel_approx(&pixmap, 50, 50, 255, 0, 0, 5));
    }

    #[test]
    fn render_stroked_rect() {
        // Blue stroked rectangle
        let doc = doc_with_content(b"0 0 1 RG 2 w 10 10 80 80 re S");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should have blue pixels on the border
        assert!(count_non_white(&pixmap) > 10);
        // Center should still be white (not filled)
        assert!(pixel_approx(&pixmap, 50, 50, 255, 255, 255, 5));
    }

    #[test]
    fn render_line_stroke() {
        // Black line from (10,50) to (90,50)
        let doc = doc_with_content(b"2 w 10 50 m 90 50 l S");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 10);
    }

    #[test]
    fn render_fill_and_stroke() {
        // Green fill, blue stroke
        let doc = doc_with_content(b"0 1 0 rg 0 0 1 RG 2 w 20 20 60 60 re B");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Center should be green (fill)
        assert!(pixel_approx(&pixmap, 50, 50, 0, 255, 0, 5));
        assert!(count_non_white(&pixmap) > 100);
    }

    #[test]
    fn render_close_stroke() {
        // Triangle: move, line, line, close+stroke
        let doc = doc_with_content(b"2 w 50 10 m 90 90 l 10 90 l s");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 10);
    }

    #[test]
    fn render_cubic_curve() {
        // Cubic curve
        let doc = doc_with_content(b"2 w 10 50 m 30 10 70 90 90 50 c S");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 10);
    }

    #[test]
    fn render_path_discard() {
        // Path constructed then discarded (n operator)
        let doc = doc_with_content(b"1 0 0 rg 10 10 80 80 re n");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Nothing should be painted
        assert_eq!(count_non_white(&pixmap), 0);
    }

    #[test]
    fn render_rect_with_transform() {
        // Translate by (20,20) then draw a 40x40 rect at origin
        let doc = doc_with_content(b"1 0 0 rg 1 0 0 1 20 20 cm 0 0 40 40 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Rect should be at PDF (20,20)-(60,60) → pixel center (40, 60) should be red
        assert!(count_non_white(&pixmap) > 100);
        // PDF (40,40) → pixel y=100-40=60
        assert!(pixel_approx(&pixmap, 40, 60, 255, 0, 0, 5));
    }

    #[test]
    fn render_line_width() {
        // Thick line (8pt wide) vs thin line (1pt wide)
        let doc_thick = doc_with_content(b"8 w 10 50 m 90 50 l S");
        let doc_thin = doc_with_content(b"1 w 10 50 m 90 50 l S");

        let px_thick = doc_thick.render_page(0, 72.0).unwrap();
        let px_thin = doc_thin.render_page(0, 72.0).unwrap();

        let thick_count = count_non_white(&px_thick);
        let thin_count = count_non_white(&px_thin);
        assert!(thick_count > thin_count * 2);
    }

    // --- 10D Tests: Color Operators ---

    #[test]
    fn render_gray_fill() {
        let doc = doc_with_content(b"0.5 g 10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Center should be ~50% gray (128, 128, 128)
        assert!(pixel_approx(&pixmap, 50, 50, 128, 128, 128, 5));
    }

    #[test]
    fn render_rgb_fill() {
        let doc = doc_with_content(b"1 0 0 rg 10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(pixel_approx(&pixmap, 50, 50, 255, 0, 0, 5));
    }

    #[test]
    fn render_cmyk_fill() {
        // CMYK (0, 1, 1, 0) → should be red via simple conversion
        let doc = doc_with_content(b"0 1 1 0 k 10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(pixel_approx(&pixmap, 50, 50, 255, 0, 0, 5));
    }

    #[test]
    fn render_stroke_color_separate() {
        // Red fill, blue stroke
        let doc = doc_with_content(b"1 0 0 rg 0 0 1 RG 3 w 20 20 60 60 re B");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Interior → red
        assert!(pixel_approx(&pixmap, 50, 50, 255, 0, 0, 5));
    }

    #[test]
    fn render_black_text_default() {
        // Default colors should be black — fill a rect with defaults
        let doc = doc_with_content(b"10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(pixel_approx(&pixmap, 50, 50, 0, 0, 0, 5));
    }

    #[test]
    fn render_cmyk_to_rgb_conversion() {
        use crate::rendering::colors::cmyk_to_rgb;

        // Verify the conversion formula: C=1,M=0,Y=0,K=0 → R=0,G=1,B=1 (cyan)
        let (r, g, b) = cmyk_to_rgb(1.0, 0.0, 0.0, 0.0);
        assert!((r - 0.0_f32).abs() < 0.01);
        assert!((g - 1.0_f32).abs() < 0.01);
        assert!((b - 1.0_f32).abs() < 0.01);

        // C=0,M=0,Y=0,K=1 → black
        let (r, g, b) = cmyk_to_rgb(0.0, 0.0, 0.0, 1.0);
        assert!((r - 0.0_f32).abs() < 0.01);
        assert!((g - 0.0_f32).abs() < 0.01);
        assert!((b - 0.0_f32).abs() < 0.01);
    }

    // --- 10E Tests: Text Rendering ---

    #[test]
    fn render_text_visible() {
        // BT /Helvetica 12 Tf 10 80 Td (Hello) Tj ET
        let doc = doc_with_content(b"BT /Helvetica 12 Tf 10 80 Td (Hello) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Text should produce non-white pixels
        assert!(count_non_white(&pixmap) > 10);
    }

    #[test]
    fn render_text_position_td() {
        // Text at position (50, 50) — should be visible in the upper-left quadrant area
        let doc = doc_with_content(b"BT /Helvetica 12 Tf 50 50 Td (X) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_text_font_size() {
        // Larger font → more pixels
        let doc_small = doc_with_content(b"BT /Helvetica 6 Tf 10 50 Td (Test) Tj ET");
        let doc_large = doc_with_content(b"BT /Helvetica 24 Tf 10 50 Td (Test) Tj ET");

        let px_small = doc_small.render_page(0, 72.0).unwrap();
        let px_large = doc_large.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&px_large) > count_non_white(&px_small));
    }

    #[test]
    fn render_text_tj_array() {
        // TJ array with spacing adjustments
        let doc = doc_with_content(b"BT /Helvetica 12 Tf 10 50 Td [(H) -100 (ello)] TJ ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 10);
    }

    #[test]
    fn render_text_next_line() {
        // Two lines using TL and T*
        let doc =
            doc_with_content(b"BT /Helvetica 12 Tf 14 TL 10 80 Td (Line1) Tj T* (Line2) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 20);
    }

    #[test]
    fn render_text_matrix_tm() {
        // Set text matrix directly
        let doc = doc_with_content(b"BT /Helvetica 12 Tf 1 0 0 1 50 50 Tm (At50) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_text_colored() {
        // Red text
        let doc = doc_with_content(b"1 0 0 rg BT /Helvetica 12 Tf 10 50 Td (Red) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should have red-ish pixels
        let red_count = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 50 && p.blue() < 50)
            .count();
        assert!(red_count > 5);
    }

    #[test]
    fn render_text_with_transform() {
        // Scale the CTM 2x, then render text
        let doc = doc_with_content(b"2 0 0 2 0 0 cm BT /Helvetica 12 Tf 10 30 Td (Big) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Scaled text produces more pixels
        let doc_normal = doc_with_content(b"BT /Helvetica 12 Tf 10 30 Td (Big) Tj ET");
        let px_normal = doc_normal.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > count_non_white(&px_normal));
    }

    #[test]
    fn render_text_character_spacing() {
        // With character spacing, text should be wider
        let doc_spaced = doc_with_content(b"BT /Helvetica 12 Tf 5 Tc 10 50 Td (ABCD) Tj ET");
        let doc_normal = doc_with_content(b"BT /Helvetica 12 Tf 10 50 Td (ABCD) Tj ET");

        let px_spaced = doc_spaced.render_page(0, 72.0).unwrap();
        let px_normal = doc_normal.render_page(0, 72.0).unwrap();

        // Both should have text, spaced version may span wider
        assert!(count_non_white(&px_spaced) > 0);
        assert!(count_non_white(&px_normal) > 0);
    }

    #[test]
    fn render_text_mode_invisible() {
        // Tr 3 = invisible text: no pixels should be painted
        let doc = doc_with_content(b"BT /Helvetica 12 Tf 3 Tr 10 50 Td (Hidden) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        assert_eq!(count_non_white(&pixmap), 0);
    }

    #[test]
    fn render_text_mode_stroke() {
        // Tr 1 = stroke only: should produce visible pixels using stroke color
        let doc = doc_with_content(b"0 1 0 RG 2 w BT /Helvetica 24 Tf 1 Tr 10 50 Td (S) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        assert!(
            count_non_white(&pixmap) > 0,
            "stroke-mode text should produce visible pixels"
        );
    }

    #[test]
    fn render_text_mode_stroke_no_fill() {
        // Stroke mode should not paint fill color
        // Red fill + green stroke, Tr 1 → should have no red fill interior
        let doc =
            doc_with_content(b"1 0 0 rg 0 1 0 RG 2 w BT /Helvetica 36 Tf 1 Tr 10 50 Td (M) Tj ET");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        // Fill-only mode for comparison
        let doc_fill = doc_with_content(b"1 0 0 rg BT /Helvetica 36 Tf 0 Tr 10 50 Td (M) Tj ET");
        let px_fill = doc_fill.render_page(0, 72.0).unwrap();
        let red_stroke = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 50)
            .count();
        let red_fill = px_fill
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 50)
            .count();
        // Stroke mode should have far fewer red pixels than fill mode
        assert!(
            red_stroke < red_fill,
            "stroke mode ({red_stroke}) should have fewer fill-color pixels than fill mode ({red_fill})"
        );
    }

    #[test]
    fn render_text_mode_fill_stroke() {
        // Tr 2 = fill then stroke: should produce more visible pixels than fill alone
        let doc_both =
            doc_with_content(b"1 0 0 rg 0 0 1 RG 3 w BT /Helvetica 36 Tf 2 Tr 10 50 Td (X) Tj ET");
        let doc_fill = doc_with_content(b"1 0 0 rg BT /Helvetica 24 Tf 0 Tr 10 50 Td (X) Tj ET");
        let px_both = doc_both.render_page(0, 72.0).unwrap();
        let px_fill = doc_fill.render_page(0, 72.0).unwrap();
        // Fill+stroke should produce at least as many non-white pixels
        assert!(count_non_white(&px_both) >= count_non_white(&px_fill));
        // Stroke on top of fill should produce more non-white pixels overall
        // (stroke extends beyond fill boundaries)
        assert!(
            count_non_white(&px_both) > count_non_white(&px_fill),
            "fill+stroke should cover more area than fill alone"
        );
    }

    #[test]
    fn render_text_mode_default_is_fill() {
        // Default Tr 0 = fill: should work same as without Tr
        let doc_default = doc_with_content(b"BT /Helvetica 12 Tf 10 50 Td (A) Tj ET");
        let doc_explicit = doc_with_content(b"BT /Helvetica 12 Tf 0 Tr 10 50 Td (A) Tj ET");
        let px_default = doc_default.render_page(0, 72.0).unwrap();
        let px_explicit = doc_explicit.render_page(0, 72.0).unwrap();
        assert_eq!(count_non_white(&px_default), count_non_white(&px_explicit));
    }

    // --- 10F Tests: Image Rendering ---

    /// Creates a doc with a page that has an image XObject named "Im1".
    fn doc_with_image(
        content: &[u8],
        img_width: u32,
        img_height: u32,
        pixel_data: &[u8],
    ) -> Document {
        let mut doc = Document::new();

        // Create image XObject stream
        let mut img_dict = Dictionary::new();
        img_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        img_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
        img_dict.insert(PdfName::new("Width"), Object::Integer(img_width as i64));
        img_dict.insert(PdfName::new("Height"), Object::Integer(img_height as i64));
        img_dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(8));
        img_dict.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        let img_stream = PdfStream::new(img_dict, pixel_data.to_vec());
        let img_id = doc.add_object(Object::Stream(img_stream));

        // XObject dict
        let mut xobjects = Dictionary::new();
        xobjects.insert(
            PdfName::new("Im1"),
            Object::Reference(IndirectRef::new(img_id.0, img_id.1)),
        );
        let xobj_id = doc.add_object(Object::Dictionary(xobjects));

        // Resources dict
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("XObject"),
            Object::Reference(IndirectRef::new(xobj_id.0, xobj_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let content_stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(content_stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_image_xobject_visible() {
        // 4x4 red image, scaled to 50x50 at position (25, 25)
        let pixel_data: Vec<u8> = vec![255, 0, 0].repeat(16); // 4x4 RGB red
        let doc = doc_with_image(b"q 50 0 0 50 25 25 cm /Im1 Do Q", 4, 4, &pixel_data);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_image_correct_position() {
        // Green image at specific position
        let pixel_data: Vec<u8> = vec![0, 255, 0].repeat(4); // 2x2 green
        let doc = doc_with_image(b"q 40 0 0 40 30 30 cm /Im1 Do Q", 2, 2, &pixel_data);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should have green pixels in the image area
        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_missing_xobject_ignored() {
        // Reference to non-existent XObject should not crash
        let doc = doc_with_content(b"q 50 0 0 50 25 25 cm /NoSuchImage Do Q");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should just be white (no crash)
        assert_eq!(count_non_white(&pixmap), 0);
    }

    /// Creates a doc with a Form XObject and a custom BBox.
    fn doc_with_form_xobject_bbox(
        page_content: &[u8],
        form_content: &[u8],
        bbox: [i64; 4],
    ) -> Document {
        let mut doc = Document::new();

        // Form XObject stream
        let mut form_dict = Dictionary::new();
        form_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        form_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        form_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(bbox[0]),
                Object::Integer(bbox[1]),
                Object::Integer(bbox[2]),
                Object::Integer(bbox[3]),
            ]),
        );
        let form_stream = PdfStream::new(form_dict, form_content.to_vec());
        let form_id = doc.add_object(Object::Stream(form_stream));

        // XObject dict
        let mut xobjects = Dictionary::new();
        xobjects.insert(
            PdfName::new("Fm1"),
            Object::Reference(IndirectRef::new(form_id.0, form_id.1)),
        );
        let xobj_id = doc.add_object(Object::Dictionary(xobjects));

        // Resources dict
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("XObject"),
            Object::Reference(IndirectRef::new(xobj_id.0, xobj_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Page content stream
        let content_stream = PdfStream::new(Dictionary::new(), page_content.to_vec());
        let stream_id = doc.add_object(Object::Stream(content_stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Creates a doc with a Form XObject using the default full-page BBox.
    fn doc_with_form_xobject(page_content: &[u8], form_content: &[u8]) -> Document {
        doc_with_form_xobject_bbox(page_content, form_content, [0, 0, 100, 100])
    }

    #[test]
    fn render_form_xobject_basic() {
        // Form XObject draws a red rectangle
        let doc = doc_with_form_xobject(b"/Fm1 Do", b"1 0 0 rg 10 10 80 80 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();

        let red_count = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 50)
            .count();
        assert!(
            red_count > 0,
            "Form XObject should render its content (red rect)"
        );
    }

    #[test]
    fn render_form_xobject_with_ctm() {
        // Apply CTM before invoking form XObject
        let doc = doc_with_form_xobject(
            b"q 0.5 0 0 0.5 0 0 cm /Fm1 Do Q",
            b"0 1 0 rg 0 0 100 100 re f",
        );
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should have green pixels in the lower-left quadrant only
        let green_count = pixmap
            .pixels()
            .iter()
            .filter(|p| p.green() > 200 && p.red() < 50)
            .count();
        assert!(green_count > 0, "Form XObject should respect CTM");
    }

    #[test]
    fn render_form_xobject_isolates_state() {
        // Form XObject should not leak its graphics state
        let doc = doc_with_form_xobject(
            b"0 0 1 rg 10 10 30 30 re f /Fm1 Do 60 60 30 30 re f",
            b"1 0 0 rg 40 40 20 20 re f",
        );
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Page sets blue fill, then invokes form (sets red), then draws another rect.
        // The second page rect should still be blue (form state isolated).
        let blue_count = pixmap
            .pixels()
            .iter()
            .filter(|p| p.blue() > 200 && p.red() < 50)
            .count();
        assert!(
            blue_count > 0,
            "Graphics state should be isolated after Form XObject"
        );
    }

    #[test]
    fn render_inline_image_rgb() {
        // 2x2 red inline image, scaled to 50x50 via CTM
        // BI /W 2 /H 2 /BPC 8 /CS /RGB ID <data> EI
        let mut content = Vec::new();
        content.extend_from_slice(b"q 50 0 0 50 25 25 cm BI /W 2 /H 2 /BPC 8 /CS /RGB ID ");
        // 2x2 red pixels: 4 pixels * 3 channels (RGB)
        content.extend_from_slice(&[255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0]);
        content.extend_from_slice(b" EI Q");

        let doc = doc_with_content(&content);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        let red_count = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 50)
            .count();
        assert!(red_count > 0, "inline image should render red pixels");
    }

    #[test]
    fn render_inline_image_grayscale() {
        // 2x1 grayscale inline image (black, white)
        let mut content = Vec::new();
        content.extend_from_slice(b"q 50 0 0 25 25 40 cm BI /W 2 /H 1 /BPC 8 /CS /G ID ");
        content.extend_from_slice(&[0, 255]);
        content.extend_from_slice(b" EI Q");

        let doc = doc_with_content(&content);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        assert!(
            count_non_white(&pixmap) > 0,
            "inline grayscale image should produce non-white pixels"
        );
    }

    #[test]
    fn image_to_rgba_grayscale() {
        use crate::images::{ColorSpace, ImageData, PdfImage};

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
        assert_eq!(rgba.len(), 8); // 2 pixels * 4 channels
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]); // black
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]); // white
    }

    #[test]
    fn image_to_rgba_rgb() {
        use crate::images::{ColorSpace, ImageData, PdfImage};

        let img = PdfImage {
            width: 1,
            height: 1,
            bits_per_component: 8,
            color_space: ColorSpace::DeviceRGB,
            data: ImageData::Raw(vec![128, 64, 32]),
            is_image_mask: false,
            decode: None,
        };
        let rgba = img.to_rgba().unwrap();
        assert_eq!(&rgba, &[128, 64, 32, 255]);
    }

    // --- Phase 10G: Clipping, dash, line properties, polish ---

    #[test]
    fn render_dashed_line() {
        // Draw a dashed horizontal line with [6 3] dash pattern
        let doc = doc_with_content(b"[6 3] 0 d 2 w 10 50 m 90 50 l S");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        // Should have some non-white pixels (the dashes)
        let total = count_non_white(&pixmap);
        assert!(total > 0, "dashed line should produce visible pixels");
        // A fully solid line would have more pixels — dashes should have gaps,
        // so count should be less than a solid line. We can't test exact counts,
        // but verify it draws something.
    }

    #[test]
    fn render_clip_rect() {
        // Draw a large filled rect, but clip to a smaller area first
        // Clip to 20,20 40x40, then fill entire page red
        let doc = doc_with_content(b"20 20 40 40 re W n 1 0 0 rg 0 0 100 100 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        let total = count_non_white(&pixmap);
        // Without clipping, the entire 100x100 page would be red (10000 pixels).
        // With clipping, only the 40x40 region should be red (~1600 pixels).
        assert!(total > 0, "clipped fill should have some red pixels");
        assert!(
            total < 5000,
            "clipping should restrict to a subset (got {})",
            total
        );
    }

    #[test]
    fn render_clip_evenodd() {
        // W* uses even-odd clipping rule
        let doc = doc_with_content(b"20 20 40 40 re W* n 1 0 0 rg 0 0 100 100 re f");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        let total = count_non_white(&pixmap);
        assert!(total > 0);
        assert!(total < 5000, "even-odd clipping should restrict drawing");
    }

    #[test]
    fn render_gs_opacity() {
        // Use gs to set fill opacity to 0.5, then draw a rect
        // Build doc with ExtGState in resources
        let doc = doc_with_extgstate(b"0 0 0 rg /GS1 gs 10 10 80 80 re f", 0.5);
        let pixmap = doc.render_page(0, 72.0).unwrap();
        // The filled rect should be semi-transparent (blended with white background)
        // A pixel at (50, 50) should be gray-ish, not pure black
        let idx = (50 * pixmap.width() as usize + 50) * 4;
        let r = pixmap.data()[idx];
        // With 50% opacity black on white, should be ~128
        assert!(
            r > 100 && r < 200,
            "semi-transparent black on white should be ~128, got {}",
            r
        );
    }

    #[test]
    fn render_line_cap_round() {
        // Draw short thick line with round cap
        let doc = doc_with_content(b"1 J 10 w 30 50 m 70 50 l S");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_line_join_round() {
        // Draw a corner with round join
        let doc = doc_with_content(b"1 j 10 w 20 20 m 50 80 l 80 20 l S");
        let pixmap = doc.render_page(0, 72.0).unwrap();
        assert!(count_non_white(&pixmap) > 0);
    }

    #[test]
    fn render_full_page_integration() {
        // Complex content stream: transform, colored rect, text, all together
        let content = b"q 1 0 0 rg 10 10 30 30 re f Q q 0 0 1 rg 50 10 30 30 re f Q BT /F1 12 Tf 10 60 Td (Hello) Tj ET";
        let doc = doc_with_content(content);
        let pixmap = doc.render_page(0, 72.0).unwrap();
        // Should render without crash and have visible content
        assert!(count_non_white(&pixmap) > 100);
    }

    /// Creates a doc with ExtGState resource for opacity testing.
    fn doc_with_extgstate(content: &[u8], fill_alpha: f64) -> Document {
        let mut doc = Document::new();

        // ExtGState dict
        let mut gs_dict = Dictionary::new();
        gs_dict.insert(PdfName::new("ca"), Object::Real(fill_alpha));
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let gs_id = doc.add_object(Object::Dictionary(ext_g_states));

        // Resources
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Creates a doc with an embedded TrueType font from raw TTF data.
    fn doc_with_embedded_font(content: &[u8], font_data: &[u8]) -> Document {
        let mut doc = Document::new();

        // Font stream (FontFile2 = TrueType)
        let font_stream = PdfStream::new(Dictionary::new(), font_data.to_vec());
        let font_stream_id = doc.add_object(Object::Stream(font_stream));

        // Font descriptor
        let mut fd = Dictionary::new();
        fd.insert(
            PdfName::new("Type"),
            Object::Name(PdfName::new("FontDescriptor")),
        );
        fd.insert(
            PdfName::new("FontName"),
            Object::Name(PdfName::new("TestFont")),
        );
        fd.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(font_stream_id.0, font_stream_id.1)),
        );
        fd.insert(PdfName::new("Flags"), Object::Integer(32));
        let fd_id = doc.add_object(Object::Dictionary(fd));

        // Font dictionary
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font_dict.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("TrueType")),
        );
        font_dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("TestFont")),
        );
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("WinAnsiEncoding")),
        );
        font_dict.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(fd_id.0, fd_id.1)),
        );
        let font_id = doc.add_object(Object::Dictionary(font_dict));

        // Font resource dict: /F1 → font
        let mut fonts = Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let fonts_id = doc.add_object(Object::Dictionary(fonts));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(fonts_id.0, fonts_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let content_stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(content_stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(200),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_embedded_font_glyphs() {
        // Load a real TrueType font
        let font_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return, // Skip if font not available
        };

        let doc = doc_with_embedded_font(b"BT /F1 36 Tf 20 100 Td (AHW) Tj ET", &font_data);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Real glyph outlines should produce non-white pixels
        let non_white = count_non_white(&pixmap);
        assert!(
            non_white > 50,
            "embedded font should render visible glyphs, got {non_white}"
        );
    }

    #[test]
    fn render_embedded_font_has_glyph_shapes() {
        // Letter 'O' has a hole in the middle (counter space).
        // Real glyph outlines should produce a ring, not a solid rectangle.
        let font_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let doc = doc_with_embedded_font(b"BT /F1 72 Tf 50 60 Td (O) Tj ET", &font_data);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        let non_white = count_non_white(&pixmap);
        assert!(non_white > 50, "glyph 'O' should be visible");

        // Find the bounding box of non-white pixels
        let w = pixmap.width() as usize;
        let mut min_x = w;
        let mut max_x = 0;
        let mut min_y = pixmap.height() as usize;
        let mut max_y = 0;
        for (i, p) in pixmap.pixels().iter().enumerate() {
            if p.red() < 250 || p.green() < 250 || p.blue() < 250 {
                let x = i % w;
                let y = i / w;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }

        // Check the center of the bounding box — 'O' should have a white hole there
        let cx = (min_x + max_x) / 2;
        let cy = (min_y + max_y) / 2;
        let center_pixel = pixmap.pixels()[cy * w + cx];
        assert!(
            center_pixel.red() > 240 && center_pixel.green() > 240 && center_pixel.blue() > 240,
            "center of 'O' should be white (the counter hole), but got ({}, {}, {})",
            center_pixel.red(),
            center_pixel.green(),
            center_pixel.blue()
        );
    }

    // --- Blend mode tests ---

    /// Creates a doc with ExtGState containing a blend mode.
    fn doc_with_blend_mode(content: &[u8], blend_mode: &str) -> Document {
        let mut doc = Document::new();

        // ExtGState dict with /BM
        let mut gs_dict = Dictionary::new();
        gs_dict.insert(PdfName::new("BM"), Object::Name(PdfName::new(blend_mode)));
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let gs_id = doc.add_object(Object::Dictionary(ext_g_states));

        // Resources
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_blend_mode_multiply() {
        // Draw a red rect, then set Multiply blend and draw a blue rect overlapping.
        // Multiply: result = src * dst. Red(1,0,0) * Blue(0,0,1) = Black(0,0,0) in overlap.
        let content = b"1 0 0 rg 10 10 80 40 re f /GS1 gs 0 0 1 rg 10 30 80 40 re f";
        let doc = doc_with_blend_mode(content, "Multiply");
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        // In the overlap region (y=30..50 in PDF coords, which is y=50..70 in pixel coords),
        // Multiply should produce near-black pixels (red * blue = 0)
        let w = pixmap.width() as usize;
        // Sample the center of the overlap (pixel coords: x=50, y=60)
        let overlap_pixel = pixmap.pixels()[60 * w + 50];
        assert!(
            overlap_pixel.red() < 30 && overlap_pixel.green() < 30 && overlap_pixel.blue() < 30,
            "Multiply blend of red and blue should be near-black, got ({}, {}, {})",
            overlap_pixel.red(),
            overlap_pixel.green(),
            overlap_pixel.blue()
        );
    }

    #[test]
    fn render_blend_mode_screen() {
        // Screen: result = 1 - (1-src)*(1-dst). Red(1,0,0) Screen Blue(0,0,1) = Magenta(1,0,1)
        let content = b"1 0 0 rg 10 10 80 40 re f /GS1 gs 0 0 1 rg 10 30 80 40 re f";
        let doc = doc_with_blend_mode(content, "Screen");
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        let overlap_pixel = pixmap.pixels()[60 * w + 50];
        // Screen of red + blue = magenta (high red, low green, high blue)
        assert!(
            overlap_pixel.red() > 200 && overlap_pixel.green() < 30 && overlap_pixel.blue() > 200,
            "Screen blend of red and blue should be magenta, got ({}, {}, {})",
            overlap_pixel.red(),
            overlap_pixel.green(),
            overlap_pixel.blue()
        );
    }

    #[test]
    fn render_blend_mode_normal_default() {
        // Without any blend mode, the second rect fully covers the overlap
        // (SourceOver with opaque = full replacement)
        let content = b"1 0 0 rg 10 10 80 40 re f 0 0 1 rg 10 30 80 40 re f";
        let doc = doc_with_content(content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        let overlap_pixel = pixmap.pixels()[60 * w + 50];
        // SourceOver with opaque blue on top of red = pure blue
        assert!(
            overlap_pixel.red() < 5 && overlap_pixel.blue() > 250,
            "Default blend (Normal) should show blue on top, got ({}, {}, {})",
            overlap_pixel.red(),
            overlap_pixel.green(),
            overlap_pixel.blue()
        );
    }

    // --- Color space tests ---

    /// Creates a doc with a named color space in /Resources /ColorSpace.
    fn doc_with_color_space(content: &[u8], cs_name: &str, cs_value: Object) -> Document {
        let mut doc = Document::new();

        // ColorSpace dict
        let mut cs_dict = Dictionary::new();
        cs_dict.insert(PdfName::new(cs_name), cs_value);
        let cs_id = doc.add_object(Object::Dictionary(cs_dict));

        // Resources
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ColorSpace"),
            Object::Reference(IndirectRef::new(cs_id.0, cs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_cs_device_rgb_via_cs_operator() {
        // Use cs to switch to DeviceRGB, then sc to set color and fill.
        let content = b"/DeviceRGB cs 0 1 0 sc 10 10 80 80 re f";
        let doc = doc_with_content(content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        // Center pixel (50, 50) should be green
        let w = pixmap.width() as usize;
        let pixel = pixmap.pixels()[50 * w + 50];
        assert!(
            pixel.red() < 10 && pixel.green() > 240 && pixel.blue() < 10,
            "cs /DeviceRGB + sc 0 1 0 should fill green, got ({}, {}, {})",
            pixel.red(),
            pixel.green(),
            pixel.blue()
        );
    }

    #[test]
    fn render_cs_calgray() {
        // CalGray with gamma=2.2, input 0.5.
        // With gamma correction, 0.5^2.2 ≈ 0.218 linear → ~0.498 sRGB
        // Without gamma (DeviceGray), 0.5 stays 0.5.
        // The CalGray result should be different from DeviceGray 0.5.
        let mut calgray_dict = Dictionary::new();
        calgray_dict.insert(PdfName::new("Gamma"), Object::Real(2.2));
        calgray_dict.insert(
            PdfName::new("WhitePoint"),
            Object::Array(vec![
                Object::Real(0.9505),
                Object::Integer(1),
                Object::Real(1.0890),
            ]),
        );
        let cs_value = Object::Array(vec![
            Object::Name(PdfName::new("CalGray")),
            Object::Dictionary(calgray_dict),
        ]);

        // Set CalGray cs, then sc 0.5 and fill
        let content = b"/CG1 cs 0.5 sc 10 10 80 80 re f";
        let doc = doc_with_color_space(content, "CG1", cs_value);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        let pixel = pixmap.pixels()[50 * w + 50];
        let gray = pixel.red();

        // DeviceGray 0.5 → pixel ≈ 128. CalGray gamma 2.2 at 0.5 should be ≈ 127 (almost same sRGB).
        // Actually: 0.5^2.2 = 0.2176 linear → sRGB = 0.498 → pixel ≈ 127.
        // Key test: the pixel should not be white (255) — it was rendered.
        assert!(
            gray < 200 && gray > 50,
            "CalGray 0.5 with gamma 2.2 should render a mid-gray, got {}",
            gray
        );
    }

    #[test]
    fn render_cs_indexed_rgb() {
        // Indexed color space with RGB base and 3-entry palette.
        let lookup_bytes = vec![
            255, 0, 0, // 0 = red
            0, 255, 0, // 1 = green
            0, 0, 255, // 2 = blue
        ];
        let cs_value = Object::Array(vec![
            Object::Name(PdfName::new("Indexed")),
            Object::Name(PdfName::new("DeviceRGB")),
            Object::Integer(2), // hival
            Object::String(crate::core::objects::PdfString::from_bytes(
                lookup_bytes,
                crate::core::objects::StringFormat::Literal,
            )),
        ]);

        // Set indexed cs, then sc 1 (= green) and fill
        let content = b"/Idx cs 1 sc 10 10 80 80 re f";
        let doc = doc_with_color_space(content, "Idx", cs_value);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        let pixel = pixmap.pixels()[50 * w + 50];
        assert!(
            pixel.red() < 10 && pixel.green() > 240 && pixel.blue() < 10,
            "Indexed index 1 should be green, got ({}, {}, {})",
            pixel.red(),
            pixel.green(),
            pixel.blue()
        );
    }

    // --- Shading tests ---

    /// Creates a doc with a Shading resource for the sh operator.
    fn doc_with_shading(content: &[u8], shading_name: &str, shading: Object) -> Document {
        let mut doc = Document::new();

        // Shading dict
        let mut shadings = Dictionary::new();
        shadings.insert(PdfName::new(shading_name), shading);
        let sh_id = doc.add_object(Object::Dictionary(shadings));

        // Resources
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Shading"),
            Object::Reference(IndirectRef::new(sh_id.0, sh_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    /// Builds a Type 2 (axial) shading dict: linear gradient from C0 to C1.
    fn axial_shading(x0: f64, y0: f64, x1: f64, y1: f64, c0: Vec<f64>, c1: Vec<f64>) -> Object {
        let mut func = Dictionary::new();
        func.insert(PdfName::new("FunctionType"), Object::Integer(2));
        func.insert(PdfName::new("N"), Object::Integer(1));
        func.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Integer(0), Object::Integer(1)]),
        );
        func.insert(
            PdfName::new("C0"),
            Object::Array(c0.into_iter().map(Object::Real).collect()),
        );
        func.insert(
            PdfName::new("C1"),
            Object::Array(c1.into_iter().map(Object::Real).collect()),
        );

        let mut shading = Dictionary::new();
        shading.insert(PdfName::new("ShadingType"), Object::Integer(2));
        shading.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        shading.insert(
            PdfName::new("Coords"),
            Object::Array(vec![
                Object::Real(x0),
                Object::Real(y0),
                Object::Real(x1),
                Object::Real(y1),
            ]),
        );
        shading.insert(PdfName::new("Function"), Object::Dictionary(func));
        Object::Dictionary(shading)
    }

    #[test]
    fn render_shading_axial_gradient() {
        // Axial gradient from red (left) to blue (right) across the page.
        let shading = axial_shading(
            0.0,
            50.0,
            100.0,
            50.0,
            vec![1.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0],
        );
        let content = b"/Sh1 sh";
        let doc = doc_with_shading(content, "Sh1", shading);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        // Left side (x=10) should be reddish
        let left = pixmap.pixels()[50 * w + 10];
        assert!(
            left.red() > 200 && left.blue() < 80,
            "Left of gradient should be red, got ({}, {}, {})",
            left.red(),
            left.green(),
            left.blue()
        );

        // Right side (x=90) should be bluish
        let right = pixmap.pixels()[50 * w + 90];
        assert!(
            right.red() < 80 && right.blue() > 200,
            "Right of gradient should be blue, got ({}, {}, {})",
            right.red(),
            right.green(),
            right.blue()
        );
    }

    #[test]
    fn render_shading_radial_gradient() {
        // Radial gradient: red center, blue outer.
        let mut func = Dictionary::new();
        func.insert(PdfName::new("FunctionType"), Object::Integer(2));
        func.insert(PdfName::new("N"), Object::Integer(1));
        func.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Integer(0), Object::Integer(1)]),
        );
        func.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        func.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );

        let mut shading = Dictionary::new();
        shading.insert(PdfName::new("ShadingType"), Object::Integer(3));
        shading.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        // Centered radial: inner circle at (50,50) r=0, outer circle at (50,50) r=50
        shading.insert(
            PdfName::new("Coords"),
            Object::Array(vec![
                Object::Real(50.0),
                Object::Real(50.0),
                Object::Real(0.0),
                Object::Real(50.0),
                Object::Real(50.0),
                Object::Real(50.0),
            ]),
        );
        shading.insert(PdfName::new("Function"), Object::Dictionary(func));

        let content = b"/Sh1 sh";
        let doc = doc_with_shading(content, "Sh1", Object::Dictionary(shading));
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        // Center (50, 50) should be reddish
        let center = pixmap.pixels()[50 * w + 50];
        assert!(
            center.red() > 200,
            "Center of radial gradient should be red, got ({}, {}, {})",
            center.red(),
            center.green(),
            center.blue()
        );

        // Edge pixel (x=5, y=50) should be bluish
        let edge = pixmap.pixels()[50 * w + 5];
        assert!(
            edge.blue() > 150,
            "Edge of radial gradient should be blue, got ({}, {}, {})",
            edge.red(),
            edge.green(),
            edge.blue()
        );
    }

    #[test]
    fn render_shading_function_type1() {
        // Type 1 function-based shading: maps (x,y) → color via a function.
        // Use a Type 2 function that goes from red (C0) to blue (C1) based on x.
        let mut func = Dictionary::new();
        func.insert(PdfName::new("FunctionType"), Object::Integer(2));
        func.insert(PdfName::new("N"), Object::Integer(1));
        func.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Integer(0), Object::Integer(1)]),
        );
        func.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        func.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );

        let mut shading = Dictionary::new();
        shading.insert(PdfName::new("ShadingType"), Object::Integer(1));
        shading.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        shading.insert(
            PdfName::new("Domain"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(1.0),
            ]),
        );
        shading.insert(
            PdfName::new("Matrix"),
            Object::Array(vec![
                Object::Real(100.0),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(100.0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        shading.insert(PdfName::new("Function"), Object::Dictionary(func));

        let content = b"/Sh1 sh";
        let doc = doc_with_shading(content, "Sh1", Object::Dictionary(shading));
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        let w = pixmap.width() as usize;
        // Left side (x=10) should be reddish (domain x≈0.1 → mostly C0=red)
        let left = pixmap.pixels()[50 * w + 10];
        assert!(
            left.red() > 150 && left.blue() < 100,
            "Left side of Type 1 shading should be red, got ({}, {}, {})",
            left.red(),
            left.green(),
            left.blue()
        );

        // Right side (x=90) should be bluish (domain x≈0.9 → mostly C1=blue)
        let right = pixmap.pixels()[50 * w + 90];
        assert!(
            right.blue() > 150 && right.red() < 100,
            "Right side of Type 1 shading should be blue, got ({}, {}, {})",
            right.red(),
            right.green(),
            right.blue()
        );
    }

    // --- Type 3 font tests ---

    /// Creates a document with a Type 3 font.
    ///
    /// The font has glyph "A" (code 65) = filled black rect covering the glyph bbox,
    /// and glyph "B" (code 66) = same.
    /// Creates a document with a Type 3 font.
    ///
    /// `glyphs` maps glyph names to their content streams (PDF operators).
    /// Encoding maps codes starting at 65 ('A') in order of `glyphs`.
    fn doc_with_type3_font(content: &[u8], glyphs: &[(&str, &[u8])]) -> Document {
        let mut doc = Document::new();

        // Add glyph streams and build CharProcs + Encoding
        let mut char_procs = Dictionary::new();
        let mut diff_arr = vec![Object::Integer(65)];

        for &(name, stream_data) in glyphs {
            let stream = PdfStream::new(Dictionary::new(), stream_data.to_vec());
            let id = doc.add_object(Object::Stream(stream));
            char_procs.insert(
                PdfName::new(name),
                Object::Reference(IndirectRef::new(id.0, id.1)),
            );
            diff_arr.push(Object::Name(PdfName::new(name)));
        }
        let cp_id = doc.add_object(Object::Dictionary(char_procs));

        let last_char = 64 + glyphs.len() as i64;
        let widths: Vec<Object> = glyphs.iter().map(|_| Object::Integer(100)).collect();

        let mut enc = Dictionary::new();
        enc.insert(PdfName::new("Type"), Object::Name(PdfName::new("Encoding")));
        enc.insert(PdfName::new("Differences"), Object::Array(diff_arr));
        let enc_id = doc.add_object(Object::Dictionary(enc));

        // Font dict
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type3")));
        font_dict.insert(
            PdfName::new("FontMatrix"),
            Object::Array(vec![
                Object::Real(0.01),
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(0.01),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        font_dict.insert(PdfName::new("FirstChar"), Object::Integer(65));
        font_dict.insert(PdfName::new("LastChar"), Object::Integer(last_char));
        font_dict.insert(PdfName::new("Widths"), Object::Array(widths));
        font_dict.insert(
            PdfName::new("FontBBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        font_dict.insert(
            PdfName::new("CharProcs"),
            Object::Reference(IndirectRef::new(cp_id.0, cp_id.1)),
        );
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Reference(IndirectRef::new(enc_id.0, enc_id.1)),
        );
        let font_id = doc.add_object(Object::Dictionary(font_dict));

        // Font resource dict: /T3F → font
        let mut fonts = Dictionary::new();
        fonts.insert(
            PdfName::new("T3F"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let fonts_id = doc.add_object(Object::Dictionary(fonts));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(fonts_id.0, fonts_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Content stream
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page (200x200 to have room)
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(200),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_type3_simple_glyph() {
        // Render "A" using Type 3 font at position (10, 150), font size 50
        // FontMatrix = [0.01 0 0 0.01 0 0], glyph = 100x100 rect in glyph space
        // So glyph covers 50×50 pixels (font_size * 0.01 * 100 = 50)
        let content = b"BT /T3F 50 Tf 10 50 Td (A) Tj ET";
        let red_rect = b"1 0 0 rg 0 0 100 100 re f".as_slice();
        let doc = doc_with_type3_font(content, &[("A", red_rect), ("B", red_rect)]);

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).expect("render should succeed");

        // The glyph should paint black pixels near position (10, 150)
        // In PDF coords (10,50) = tiny-skia coords (10, 200-50) = (10, 150)
        // The glyph rect is 50pt wide × 50pt tall
        let w = pixmap.width() as usize;

        // Check a pixel inside the glyph area — should be RED (not black placeholder)
        // At PDF (20, 60) → tiny-skia (20, 140)
        let inside = pixmap.pixels()[140 * w + 20];
        assert!(
            inside.red() > 200 && inside.green() < 50 && inside.blue() < 50,
            "Inside Type 3 glyph should be red, got ({}, {}, {})",
            inside.red(),
            inside.green(),
            inside.blue()
        );

        // Check a pixel outside — should be white
        let outside = pixmap.pixels()[10 * w + 180];
        assert!(
            outside.red() > 200,
            "Outside Type 3 glyph should be white, got ({}, {}, {})",
            outside.red(),
            outside.green(),
            outside.blue()
        );
    }

    #[test]
    fn render_type3_two_glyphs() {
        // Render "AB" — glyph A is red, glyph B is blue
        // A at x=10, width=100 glyph units * 0.01 * 50 = 50pt → B starts at x=60
        let content = b"BT /T3F 50 Tf 10 50 Td (AB) Tj ET";
        let doc = doc_with_type3_font(
            content,
            &[
                ("A", b"1 0 0 rg 0 0 100 100 re f"),
                ("B", b"0 0 1 rg 0 0 100 100 re f"),
            ],
        );

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).expect("render should succeed");
        let w = pixmap.width() as usize;

        // Glyph A (red) at x≈20, y≈140
        let glyph_a_pixel = pixmap.pixels()[140 * w + 20];
        assert!(
            glyph_a_pixel.red() > 200 && glyph_a_pixel.blue() < 50,
            "Glyph A should be red, got ({}, {}, {})",
            glyph_a_pixel.red(),
            glyph_a_pixel.green(),
            glyph_a_pixel.blue()
        );

        // Glyph B (blue) at x≈70 (60 + offset into glyph), y≈140
        let glyph_b_pixel = pixmap.pixels()[140 * w + 70];
        assert!(
            glyph_b_pixel.blue() > 200 && glyph_b_pixel.red() < 50,
            "Glyph B should be blue at x=70, got ({}, {}, {})",
            glyph_b_pixel.red(),
            glyph_b_pixel.green(),
            glyph_b_pixel.blue()
        );
    }

    // --- Soft Mask Tests ---

    /// Helper: creates a doc with an ExtGState that has an SMask.
    /// The SMask's /G form XObject draws a white rect on the left half only,
    /// creating a luminosity mask that's opaque on the left, transparent on the right.
    fn doc_with_smask() -> Document {
        let mut doc = Document::new();

        // Form XObject for the SMask /G: draws white rect on left half of 100x100
        // Content: "1 g 0 0 50 100 re f" (white fill on left half)
        let form_content = b"1 g 0 0 50 100 re f";
        let mut form_dict = Dictionary::new();
        form_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        form_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        form_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        let form_stream = PdfStream::new(form_dict, form_content.to_vec());
        let form_id = doc.add_object(Object::Stream(form_stream));

        // SMask dictionary: Luminosity mask using the form XObject
        let mut smask_dict = Dictionary::new();
        smask_dict.insert(PdfName::new("S"), Object::Name(PdfName::new("Luminosity")));
        smask_dict.insert(
            PdfName::new("G"),
            Object::Reference(IndirectRef::new(form_id.0, form_id.1)),
        );
        let smask_id = doc.add_object(Object::Dictionary(smask_dict));

        // ExtGState with the SMask
        let mut gs_dict = Dictionary::new();
        gs_dict.insert(
            PdfName::new("SMask"),
            Object::Reference(IndirectRef::new(smask_id.0, smask_id.1)),
        );
        let gs_id = doc.add_object(Object::Dictionary(gs_dict));

        // ExtGState resource dict
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(
            PdfName::new("GS1"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );

        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("ExtGState"), Object::Dictionary(ext_g_states));

        // Content stream: set ExtGState GS1, then fill whole page red
        let content = b"/GS1 gs 1 0 0 rg 0 0 100 100 re f";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        // Page
        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_smask_luminosity_masks_right_half() {
        let doc = doc_with_smask();
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Left half (x=25, y=50 → PDF y=50) should be red (mask is white = opaque)
        assert!(
            pixel_approx(&pixmap, 25, 50, 255, 0, 0, 10),
            "Left half should be red (opaque through luminosity mask)"
        );

        // Right half (x=75, y=50) should be white (mask is black = transparent)
        assert!(
            pixel_approx(&pixmap, 75, 50, 255, 255, 255, 10),
            "Right half should be white (transparent through luminosity mask), got ({}, {}, {})",
            pixmap.pixel(75, 50).unwrap().red(),
            pixmap.pixel(75, 50).unwrap().green(),
            pixmap.pixel(75, 50).unwrap().blue(),
        );
    }

    // --- Tiling Pattern Tests ---

    /// Creates a document with a tiling pattern that draws a 10×10 red square,
    /// tiled every 20 units. The content stream fills a 100×100 rect with it.
    fn doc_with_tiling_pattern() -> Document {
        let mut doc = Document::new();

        // Pattern stream: draw a 10x10 red square
        let pat_content = b"1 0 0 rg 0 0 10 10 re f";
        let mut pat_dict = Dictionary::new();
        pat_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Pattern")));
        pat_dict.insert(PdfName::new("PatternType"), Object::Integer(1));
        pat_dict.insert(PdfName::new("PaintType"), Object::Integer(1)); // colored
        pat_dict.insert(PdfName::new("TilingType"), Object::Integer(1));
        pat_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        pat_dict.insert(PdfName::new("XStep"), Object::Integer(20));
        pat_dict.insert(PdfName::new("YStep"), Object::Integer(20));
        let pat_stream = PdfStream::new(pat_dict, pat_content.to_vec());
        let pat_id = doc.add_object(Object::Stream(pat_stream));

        // Pattern resource dict
        let mut patterns = Dictionary::new();
        patterns.insert(
            PdfName::new("P1"),
            Object::Reference(IndirectRef::new(pat_id.0, pat_id.1)),
        );

        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("Pattern"), Object::Dictionary(patterns));

        // Content: set Pattern color space, select P1, fill whole page
        let content = b"/Pattern cs /P1 scn 0 0 100 100 re f";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_tiling_pattern_red_squares() {
        let doc = doc_with_tiling_pattern();
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Pattern tiles 10x10 red squares at 20-unit intervals.
        // At PDF (5, 5) → pixel (5, 95) → should be red (inside first tile)
        assert!(
            pixel_approx(&pixmap, 5, 95, 255, 0, 0, 10),
            "Inside tile at (5,95) should be red, got ({}, {}, {})",
            pixmap.pixel(5, 95).unwrap().red(),
            pixmap.pixel(5, 95).unwrap().green(),
            pixmap.pixel(5, 95).unwrap().blue(),
        );

        // At PDF (15, 5) → pixel (15, 95) → should be white (gap between tiles)
        assert!(
            pixel_approx(&pixmap, 15, 95, 255, 255, 255, 10),
            "Gap at (15,95) should be white, got ({}, {}, {})",
            pixmap.pixel(15, 95).unwrap().red(),
            pixmap.pixel(15, 95).unwrap().green(),
            pixmap.pixel(15, 95).unwrap().blue(),
        );

        // At PDF (25, 5) → pixel (25, 95) → should be red (second tile)
        assert!(
            pixel_approx(&pixmap, 25, 95, 255, 0, 0, 10),
            "Inside second tile at (25,95) should be red, got ({}, {}, {})",
            pixmap.pixel(25, 95).unwrap().red(),
            pixmap.pixel(25, 95).unwrap().green(),
            pixmap.pixel(25, 95).unwrap().blue(),
        );
    }

    #[test]
    fn render_smask_none_resets_mask() {
        // Create a doc where the first rect uses SMask, then /SMask /None resets it
        let mut doc = Document::new();

        // Form XObject for the SMask: white rect on left half only
        let mut form_dict = Dictionary::new();
        form_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        form_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        form_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        let form_stream = PdfStream::new(form_dict, b"1 g 0 0 50 100 re f".to_vec());
        let form_id = doc.add_object(Object::Stream(form_stream));

        let mut smask_dict = Dictionary::new();
        smask_dict.insert(PdfName::new("S"), Object::Name(PdfName::new("Luminosity")));
        smask_dict.insert(
            PdfName::new("G"),
            Object::Reference(IndirectRef::new(form_id.0, form_id.1)),
        );
        let smask_id = doc.add_object(Object::Dictionary(smask_dict));

        // GS1: has SMask
        let mut gs1 = Dictionary::new();
        gs1.insert(
            PdfName::new("SMask"),
            Object::Reference(IndirectRef::new(smask_id.0, smask_id.1)),
        );
        let gs1_id = doc.add_object(Object::Dictionary(gs1));

        // GS2: SMask = None (reset)
        let mut gs2 = Dictionary::new();
        gs2.insert(PdfName::new("SMask"), Object::Name(PdfName::new("None")));
        let gs2_id = doc.add_object(Object::Dictionary(gs2));

        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(
            PdfName::new("GS1"),
            Object::Reference(IndirectRef::new(gs1_id.0, gs1_id.1)),
        );
        ext_g_states.insert(
            PdfName::new("GS2"),
            Object::Reference(IndirectRef::new(gs2_id.0, gs2_id.1)),
        );

        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("ExtGState"), Object::Dictionary(ext_g_states));

        // Content: apply mask, draw green on top half, reset mask, draw blue on bottom half
        let content = b"/GS1 gs 0 1 0 rg 0 50 100 50 re f /GS2 gs 0 0 1 rg 0 0 100 50 re f";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Bottom half: blue rect drawn WITHOUT mask (GS2 reset) → should be fully blue
        // PDF y=25 → pixel y=75
        assert!(
            pixel_approx(&pixmap, 75, 75, 0, 0, 255, 10),
            "Bottom-right should be blue after SMask reset, got ({}, {}, {})",
            pixmap.pixel(75, 75).unwrap().red(),
            pixmap.pixel(75, 75).unwrap().green(),
            pixmap.pixel(75, 75).unwrap().blue(),
        );
    }

    #[test]
    fn render_gs_dash_pattern() {
        // Set dash pattern via ExtGState /D and stroke a line
        let mut doc = Document::new();

        // ExtGState with /D [[6 3] 0]
        let mut gs_dict = Dictionary::new();
        gs_dict.insert(
            PdfName::new("D"),
            Object::Array(vec![
                Object::Array(vec![Object::Integer(6), Object::Integer(3)]),
                Object::Integer(0),
            ]),
        );
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let gs_id = doc.add_object(Object::Dictionary(ext_g_states));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Stroke a horizontal line with the GS dash pattern, thick so gaps are visible
        let content = b"2 w /GS1 gs 10 50 m 90 50 l S";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Compare with a solid line (no dash) — dashed should have fewer non-white pixels
        let dashed_count = count_non_white(&pixmap);
        let solid_doc = doc_with_content(b"2 w 10 50 m 90 50 l S");
        let solid_pixmap = solid_doc.render_page(0, 72.0).unwrap();
        let solid_count = count_non_white(&solid_pixmap);

        assert!(dashed_count > 0, "dashed line should have visible pixels");
        assert!(
            dashed_count < solid_count,
            "dashed line ({}) should have fewer pixels than solid ({})",
            dashed_count,
            solid_count
        );
    }

    #[test]
    fn render_image_mask_stencil() {
        // Create an image mask XObject: 8x1, 1-bit, alternating pattern 0xAA = 10101010
        // 0-bits are painted with fill color, 1-bits are transparent
        let mut doc = Document::new();

        let mut img_dict = Dictionary::new();
        img_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        img_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Image")));
        img_dict.insert(PdfName::new("Width"), Object::Integer(8));
        img_dict.insert(PdfName::new("Height"), Object::Integer(1));
        img_dict.insert(PdfName::new("BitsPerComponent"), Object::Integer(1));
        img_dict.insert(PdfName::new("ImageMask"), Object::Boolean(true));
        // Data: 0xAA = 10101010 → bits 1,0,1,0,1,0,1,0
        // Default decode [0 1]: 0-bits painted, 1-bits transparent
        let img_stream = PdfStream::new(img_dict, vec![0xAA]);
        let img_id = doc.add_object(Object::Stream(img_stream));

        let mut xobjects = Dictionary::new();
        xobjects.insert(
            PdfName::new("Im1"),
            Object::Reference(IndirectRef::new(img_id.0, img_id.1)),
        );
        let xobj_id = doc.add_object(Object::Dictionary(xobjects));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("XObject"),
            Object::Reference(IndirectRef::new(xobj_id.0, xobj_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Set fill color to red, then draw the image mask scaled to 80x10
        let content = b"1 0 0 rg q 80 0 0 10 10 45 cm /Im1 Do Q";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // The mask should paint some pixels red and leave gaps
        let non_white = count_non_white(&pixmap);
        assert!(
            non_white > 0,
            "image mask should paint visible pixels in fill color"
        );

        // Check that painted pixels are red (fill color)
        // The mask is 8 pixels wide, scaled to 80px → each mask pixel is 10px wide
        // Pixel 0 (bit 1): transparent, Pixel 1 (bit 0): painted red, etc.
        // At x=15 (second mask pixel, 0-bit = painted), y=50 → should be red
        assert!(
            pixel_approx(&pixmap, 20, 50, 255, 0, 0, 30),
            "painted mask pixel should be red, got ({}, {}, {})",
            pixmap.pixel(20, 50).unwrap().red(),
            pixmap.pixel(20, 50).unwrap().green(),
            pixmap.pixel(20, 50).unwrap().blue(),
        );
    }

    #[test]
    fn render_transparency_group_isolated() {
        // Form XObject with /Group << /S /Transparency /I true >>
        // The form draws a semi-transparent red rect (via ExtGState in page resources).
        // With isolation, it renders to temp pixmap then composites.
        let mut doc = Document::new();

        // Form XObject
        let mut form_dict = Dictionary::new();
        form_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        form_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        form_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        let mut group = Dictionary::new();
        group.insert(
            PdfName::new("S"),
            Object::Name(PdfName::new("Transparency")),
        );
        group.insert(PdfName::new("I"), Object::Boolean(true));
        form_dict.insert(PdfName::new("Group"), Object::Dictionary(group));

        // Form content: use GS1 for 50% alpha, draw red rect
        let form_content = b"/GS1 gs 1 0 0 rg 20 20 60 60 re f";
        let form_stream = PdfStream::new(form_dict, form_content.to_vec());
        let form_id = doc.add_object(Object::Stream(form_stream));

        // ExtGState in page resources (form inherits page resources)
        let mut gs_dict = Dictionary::new();
        gs_dict.insert(PdfName::new("ca"), Object::Real(0.5));
        let mut ext_g = Dictionary::new();
        ext_g.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let ext_g_id = doc.add_object(Object::Dictionary(ext_g));

        let mut xobjects = Dictionary::new();
        xobjects.insert(
            PdfName::new("Fm1"),
            Object::Reference(IndirectRef::new(form_id.0, form_id.1)),
        );
        let xobj_id = doc.add_object(Object::Dictionary(xobjects));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(ext_g_id.0, ext_g_id.1)),
        );
        resources.insert(
            PdfName::new("XObject"),
            Object::Reference(IndirectRef::new(xobj_id.0, xobj_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        let content = b"/Fm1 Do";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Center (50, 50): 50% red on white → pinkish (R≈255, G≈128, B≈128)
        let p = pixmap.pixel(50, 50).unwrap();
        assert!(
            p.red() > 200 && p.green() > 80 && p.green() < 200,
            "Semi-transparent red on white should be pinkish, got ({}, {}, {})",
            p.red(),
            p.green(),
            p.blue()
        );

        // Outside (5, 5) should be white
        assert!(
            pixel_approx(&pixmap, 5, 5, 255, 255, 255, 5),
            "Outside form should be white"
        );
    }

    #[test]
    fn render_type0_text_visible() {
        // Type 0 composite font with Identity-H encoding and embedded TTF.
        // Text is encoded as 2-byte BE CIDs (which equal GIDs under Identity).
        let ttf_data = match std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut doc = Document::new();

        // Embed the TTF as a stream
        let font_stream = PdfStream::new(Dictionary::new(), ttf_data);
        let fs_id = doc.add_object(Object::Stream(font_stream));

        // Font descriptor
        let mut descriptor = Dictionary::new();
        descriptor.insert(
            PdfName::new("Type"),
            Object::Name(PdfName::new("FontDescriptor")),
        );
        descriptor.insert(
            PdfName::new("FontFile2"),
            Object::Reference(IndirectRef::new(fs_id.0, fs_id.1)),
        );
        let desc_id = doc.add_object(Object::Dictionary(descriptor));

        // CIDFont dictionary
        let mut cid_font = Dictionary::new();
        cid_font.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        cid_font.insert(
            PdfName::new("Subtype"),
            Object::Name(PdfName::new("CIDFontType2")),
        );
        cid_font.insert(PdfName::new("DW"), Object::Integer(600));
        cid_font.insert(
            PdfName::new("FontDescriptor"),
            Object::Reference(IndirectRef::new(desc_id.0, desc_id.1)),
        );
        cid_font.insert(
            PdfName::new("CIDToGIDMap"),
            Object::Name(PdfName::new("Identity")),
        );
        let cid_id = doc.add_object(Object::Dictionary(cid_font));

        // Type 0 font dictionary
        let mut font_dict = Dictionary::new();
        font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
        font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type0")));
        font_dict.insert(
            PdfName::new("BaseFont"),
            Object::Name(PdfName::new("DejaVuSans")),
        );
        font_dict.insert(
            PdfName::new("Encoding"),
            Object::Name(PdfName::new("Identity-H")),
        );
        font_dict.insert(
            PdfName::new("DescendantFonts"),
            Object::Array(vec![Object::Reference(IndirectRef::new(
                cid_id.0, cid_id.1,
            ))]),
        );
        let font_id = doc.add_object(Object::Dictionary(font_dict));

        // Font resource dict
        let mut fonts = Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Reference(IndirectRef::new(font_id.0, font_id.1)),
        );
        let fonts_id = doc.add_object(Object::Dictionary(fonts));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("Font"),
            Object::Reference(IndirectRef::new(fonts_id.0, fonts_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // GID 36 = '$' in DejaVu Sans, encode as 2-byte BE: 0x00, 0x24
        // GID 37 = '%', encode as 0x00, 0x25
        let text_hex = "002400250024"; // Three glyphs
        let content = format!("BT /F1 36 Tf 10 50 Td <{}> Tj ET", text_hex);
        let stream = PdfStream::new(Dictionary::new(), content.into_bytes());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Should have non-white pixels near the text position
        let has_ink = (0..200).any(|x| {
            let p = pixmap.pixel(x, 50).unwrap();
            p.red() < 200 || p.green() < 200 || p.blue() < 200
        });
        assert!(has_ink, "Type 0 font should render visible text");
    }

    #[test]
    fn extgstate_sets_line_width() {
        // ExtGState with /LW 8 should produce a thick stroke
        let mut doc = Document::new();

        let mut gs_dict = Dictionary::new();
        gs_dict.insert(PdfName::new("LW"), Object::Real(8.0));
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let gs_id = doc.add_object(Object::Dictionary(ext_g_states));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Stroke a horizontal line using GS1 (which sets LW=8)
        let content = b"0 0 0 RG /GS1 gs 10 50 m 90 50 l S";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // With LW=8, the stroke should cover pixels at y=46..54 (50 ± 4)
        // Check that pixel at y=47 (3 pixels from center) has ink
        let p = pixmap.pixel(50, 47).unwrap();
        assert!(
            p.red() < 200 || p.green() < 200 || p.blue() < 200,
            "ExtGState LW=8 should produce thick stroke covering y=47, got ({}, {}, {})",
            p.red(),
            p.green(),
            p.blue()
        );
    }

    #[test]
    fn extgstate_sets_miter_limit() {
        // ExtGState with /ML 2.0 should be accepted (no crash, no panic)
        let mut doc = Document::new();

        let mut gs_dict = Dictionary::new();
        gs_dict.insert(PdfName::new("ML"), Object::Real(2.0));
        gs_dict.insert(PdfName::new("LJ"), Object::Integer(0)); // miter join
        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(PdfName::new("GS1"), Object::Dictionary(gs_dict));
        let gs_id = doc.add_object(Object::Dictionary(ext_g_states));

        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("ExtGState"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Draw two joined lines with the GS1 miter settings
        let content = b"2 w 0 0 0 RG /GS1 gs 10 10 m 50 90 l 90 10 l S";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        // Should render without panic
        let pixmap = doc.render_page(0, 72.0).unwrap();
        let has_ink = (0..100).any(|x| {
            let p = pixmap.pixel(x, 50).unwrap();
            p.red() < 200
        });
        assert!(has_ink, "ExtGState with ML/LJ should render stroked path");
    }

    // --- Feature 31: Shading Pattern support (PatternType 2) ---

    #[test]
    fn render_shading_pattern_axial() {
        // Define a PatternType 2 (shading pattern) with an axial gradient (black→red).
        // The page content uses Pattern color space and fills a rect.
        let mut doc = Document::new();

        // Type 2 function: interpolates from [0,0,0] to [1,0,0] (black→red)
        let mut func_dict = Dictionary::new();
        func_dict.insert(PdfName::new("FunctionType"), Object::Integer(2));
        func_dict.insert(PdfName::new("N"), Object::Integer(1));
        func_dict.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );
        func_dict.insert(
            PdfName::new("C0"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        func_dict.insert(
            PdfName::new("C1"),
            Object::Array(vec![
                Object::Real(1.0),
                Object::Real(0.0),
                Object::Real(0.0),
            ]),
        );
        let func_id = doc.add_object(Object::Dictionary(func_dict));

        // Shading dict: axial (Type 2), horizontal from x=0 to x=100
        let mut shading = Dictionary::new();
        shading.insert(PdfName::new("ShadingType"), Object::Integer(2));
        shading.insert(
            PdfName::new("ColorSpace"),
            Object::Name(PdfName::new("DeviceRGB")),
        );
        shading.insert(
            PdfName::new("Coords"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(100.0),
                Object::Real(0.0),
            ]),
        );
        shading.insert(
            PdfName::new("Function"),
            Object::Reference(IndirectRef::new(func_id.0, func_id.1)),
        );
        let shading_id = doc.add_object(Object::Dictionary(shading));

        // Pattern dict: PatternType 2 with the shading
        let mut pattern = Dictionary::new();
        pattern.insert(PdfName::new("PatternType"), Object::Integer(2));
        pattern.insert(
            PdfName::new("Shading"),
            Object::Reference(IndirectRef::new(shading_id.0, shading_id.1)),
        );
        let pat_id = doc.add_object(Object::Dictionary(pattern));

        // Pattern resource
        let mut patterns = Dictionary::new();
        patterns.insert(
            PdfName::new("P1"),
            Object::Reference(IndirectRef::new(pat_id.0, pat_id.1)),
        );

        // Resources
        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("Pattern"), Object::Dictionary(patterns));
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Page content: set Pattern cs, fill rect with pattern P1
        let content = b"/Pattern cs /P1 scn 0 0 100 100 re f";
        let content_stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(content_stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Left side should be dark (near black), right side should be red
        let left = pixmap.pixel(5, 50).unwrap();
        let right = pixmap.pixel(95, 50).unwrap();

        assert!(
            left.red() < 100,
            "Left pixel should be dark, got red={}",
            left.red()
        );
        assert!(
            right.red() > 150,
            "Right pixel should be red, got red={}",
            right.red()
        );
        assert!(
            right.red() > left.red() + 50,
            "Right should be redder than left: right.red={}, left.red={}",
            right.red(),
            left.red()
        );
    }

    // --- Feature 28/29: Form XObject BBox clipping and own Resources ---

    #[test]
    fn form_xobject_uses_own_resources() {
        // Form XObject with its own /Resources containing an ExtGState GS1
        // that sets fill alpha to 0.5. The page has NO ExtGState resources.
        // If form resources work, the red fill will be semi-transparent.
        let mut doc = Document::new();

        // ExtGState dict: ca = 0.5 (fill opacity)
        let mut gs = Dictionary::new();
        gs.insert(PdfName::new("ca"), Object::Real(0.5));
        let gs_id = doc.add_object(Object::Dictionary(gs));

        // Form's own resources
        let mut ext_gs_dict = Dictionary::new();
        ext_gs_dict.insert(
            PdfName::new("GS1"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );
        let mut form_resources = Dictionary::new();
        form_resources.insert(PdfName::new("ExtGState"), Object::Dictionary(ext_gs_dict));

        // Form XObject: sets GS1, then fills red
        let mut form_dict = Dictionary::new();
        form_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        form_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        form_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        form_dict.insert(
            PdfName::new("Resources"),
            Object::Dictionary(form_resources),
        );
        let form_stream = PdfStream::new(form_dict, b"/GS1 gs 1 0 0 rg 0 0 100 100 re f".to_vec());
        let form_id = doc.add_object(Object::Stream(form_stream));

        // Page resources: XObject dict only (no ExtGState!)
        let mut xobjects = Dictionary::new();
        xobjects.insert(
            PdfName::new("Fm1"),
            Object::Reference(IndirectRef::new(form_id.0, form_id.1)),
        );
        let xobj_id = doc.add_object(Object::Dictionary(xobjects));
        let mut resources = Dictionary::new();
        resources.insert(
            PdfName::new("XObject"),
            Object::Reference(IndirectRef::new(xobj_id.0, xobj_id.1)),
        );
        let res_id = doc.add_object(Object::Dictionary(resources));

        // Page content: invoke form
        let content_stream = PdfStream::new(Dictionary::new(), b"/Fm1 Do".to_vec());
        let stream_id = doc.add_object(Object::Stream(content_stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(
            PdfName::new("Resources"),
            Object::Reference(IndirectRef::new(res_id.0, res_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let pixmap = doc.render_page(0, 72.0).unwrap();
        let center = pixmap.pixel(50, 50).unwrap();

        // With ca=0.5, red over white background should produce ~(255, 128, 128)
        // Red channel stays high, but green/blue should be noticeably above 0
        // (not pure red 255,0,0 which would mean alpha wasn't applied)
        assert!(
            center.red() > 200,
            "Red channel should be high, got {}",
            center.red()
        );
        assert!(
            center.green() > 80 && center.green() < 200,
            "Green channel should show blending (80-200), got {} — form resources may not be applied",
            center.green()
        );
    }

    #[test]
    fn form_xobject_bbox_clips_content() {
        // Form content draws red over the entire 0 0 100 100 area,
        // but BBox is only [20 20 80 80], so content outside should be clipped.
        let doc =
            doc_with_form_xobject_bbox(b"/Fm1 Do", b"1 0 0 rg 0 0 100 100 re f", [20, 20, 80, 80]);
        let pixmap = doc.render_page(0, 72.0).unwrap();

        // Pixel at (50, 50) should be red (inside BBox, note: PDF y-axis is flipped)
        let center = pixmap.pixel(50, 50).unwrap();
        assert!(
            center.red() > 200 && center.green() < 50,
            "Center pixel should be red (inside BBox)"
        );

        // Pixel at (5, 5) should be white (outside BBox — top-left corner in device space)
        let corner = pixmap.pixel(5, 5).unwrap();
        assert!(
            corner.red() > 200 && corner.green() > 200 && corner.blue() > 200,
            "Corner pixel at (5,5) should be white (outside BBox), got ({},{},{})",
            corner.red(),
            corner.green(),
            corner.blue(),
        );

        // Pixel at (95, 95) should also be white (outside BBox — bottom-right)
        let br = pixmap.pixel(95, 95).unwrap();
        assert!(
            br.red() > 200 && br.green() > 200 && br.blue() > 200,
            "Corner pixel at (95,95) should be white (outside BBox), got ({},{},{})",
            br.red(),
            br.green(),
            br.blue(),
        );
    }

    // ---- Feature 33: CropBox ----

    #[test]
    fn render_page_uses_cropbox() {
        // MediaBox is 200x200, CropBox is [50 50 150 150] → 100x100 visible.
        // Fill entire MediaBox red. Pixmap should be 100x100 with red center.
        let content = b"1 0 0 rg 0 0 200 200 re f";

        let mut doc = Document::new();
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(200),
            ]),
        );
        page.insert(
            PdfName::new("CropBox"),
            Object::Array(vec![
                Object::Integer(50),
                Object::Integer(50),
                Object::Integer(150),
                Object::Integer(150),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        // Pixmap should be 100x100, not 200x200
        assert_eq!(pixmap.width(), 100);
        assert_eq!(pixmap.height(), 100);

        // Center pixel should be red (content fills entire MediaBox including CropBox region)
        let center = pixmap.pixel(50, 50).unwrap();
        assert!(
            center.red() > 200 && center.green() < 50,
            "Center should be red, got ({},{},{})",
            center.red(),
            center.green(),
            center.blue(),
        );
    }

    #[test]
    fn render_page_no_cropbox_uses_mediabox() {
        // No CropBox — should use MediaBox dimensions.
        let content = b"1 0 0 rg 0 0 100 100 re f";
        let doc = doc_with_content(content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 100);
        assert_eq!(pixmap.height(), 100);
    }

    // ---- Feature 34: Page /Rotate ----

    /// Creates a document with one page, optional Rotate, and content.
    fn doc_with_page_rotate(width: f64, height: f64, rotate: i64, content: &[u8]) -> Document {
        let mut doc = Document::new();
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );
        page.insert(PdfName::new("Rotate"), Object::Integer(rotate));
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        doc
    }

    #[test]
    fn render_page_rotate_90() {
        // 100x200 portrait page with Rotate=90 should produce 200x100 landscape pixmap.
        let content = b"1 0 0 rg 0 0 100 200 re f";
        let doc = doc_with_page_rotate(100.0, 200.0, 90, content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 200);
        assert_eq!(pixmap.height(), 100);
    }

    #[test]
    fn render_page_rotate_270() {
        // 100x200 portrait page with Rotate=270 should also produce 200x100.
        let content = b"1 0 0 rg 0 0 100 200 re f";
        let doc = doc_with_page_rotate(100.0, 200.0, 270, content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 200);
        assert_eq!(pixmap.height(), 100);
    }

    #[test]
    fn render_page_no_rotate() {
        // No Rotate — dimensions unchanged.
        let content = b"1 0 0 rg 0 0 100 200 re f";
        let doc = doc_with_page_rotate(100.0, 200.0, 0, content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert_eq!(pixmap.width(), 100);
        assert_eq!(pixmap.height(), 200);
    }

    // ---- Feature 36: ri/i operators and ExtGState FL/RI ----

    #[test]
    fn ri_and_i_operators_accepted() {
        // `i` and `ri` operators should be accepted (no unknown operator debug).
        // Content: set flatness, set rendering intent, draw red rect.
        let content = b"0.5 i /RelativeColorimetric ri 1 0 0 rg 0 0 100 100 re f";
        let doc = doc_with_content(content);
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        // Rect should still be red (operators are no-op for rendering but accepted)
        let center = pixmap.pixel(50, 50).unwrap();
        assert!(
            center.red() > 200 && center.green() < 50,
            "Center should be red, got ({},{},{})",
            center.red(),
            center.green(),
            center.blue(),
        );
    }

    #[test]
    fn extgstate_fl_ri() {
        // ExtGState with /FL and /RI keys should be accepted.
        let mut doc = Document::new();

        let mut gs = Dictionary::new();
        gs.insert(PdfName::new("FL"), Object::Real(0.5));
        gs.insert(
            PdfName::new("RI"),
            Object::Name(PdfName::new("AbsoluteColorimetric")),
        );
        let gs_id = doc.add_object(Object::Dictionary(gs));

        let mut ext_g_states = Dictionary::new();
        ext_g_states.insert(
            PdfName::new("GS1"),
            Object::Reference(IndirectRef::new(gs_id.0, gs_id.1)),
        );

        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("ExtGState"), Object::Dictionary(ext_g_states));

        // Content: apply GS1, draw green rect
        let content = b"/GS1 gs 0 1 0 rg 0 0 100 100 re f";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        // Green rect should render correctly
        let center = pixmap.pixel(50, 50).unwrap();
        assert!(
            center.green() > 200 && center.red() < 50,
            "Center should be green, got ({},{},{})",
            center.red(),
            center.green(),
            center.blue(),
        );
    }

    // ---- Feature 35: SCN stroke pattern ----

    #[test]
    fn stroke_with_pattern() {
        // Set stroke CS to Pattern, use a tiling pattern, stroke a thick horizontal line.
        let mut doc = Document::new();

        // Pattern: 10x10 red square tile at 10-unit intervals
        let pat_content = b"1 0 0 rg 0 0 10 10 re f";
        let mut pat_dict = Dictionary::new();
        pat_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Pattern")));
        pat_dict.insert(PdfName::new("PatternType"), Object::Integer(1));
        pat_dict.insert(PdfName::new("PaintType"), Object::Integer(1));
        pat_dict.insert(PdfName::new("TilingType"), Object::Integer(1));
        pat_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(10),
                Object::Integer(10),
            ]),
        );
        pat_dict.insert(PdfName::new("XStep"), Object::Integer(10));
        pat_dict.insert(PdfName::new("YStep"), Object::Integer(10));
        let pat_stream = PdfStream::new(pat_dict, pat_content.to_vec());
        let pat_id = doc.add_object(Object::Stream(pat_stream));

        let mut patterns = Dictionary::new();
        patterns.insert(
            PdfName::new("P1"),
            Object::Reference(IndirectRef::new(pat_id.0, pat_id.1)),
        );

        let mut resources = Dictionary::new();
        resources.insert(PdfName::new("Pattern"), Object::Dictionary(patterns));

        // Content: set Pattern stroke CS, select P1, draw thick line across center
        let content = b"20 w /Pattern CS /P1 SCN 0 50 m 100 50 l S";
        let stream = PdfStream::new(Dictionary::new(), content.to_vec());
        let stream_id = doc.add_object(Object::Stream(stream));

        let mut page = Dictionary::new();
        page.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        page.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(100),
                Object::Integer(100),
            ]),
        );
        page.insert(
            PdfName::new("Contents"),
            Object::Reference(IndirectRef::new(stream_id.0, stream_id.1)),
        );
        page.insert(PdfName::new("Resources"), Object::Dictionary(resources));
        let page_id = doc.add_object(Object::Dictionary(page));

        if let Some(Object::Dictionary(ref mut pages)) = doc.get_object_mut((2, 0)) {
            pages.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    page_id.0, page_id.1,
                ))]),
            );
            pages.insert(PdfName::new("Count"), Object::Integer(1));
        }

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();

        // The thick line (20pt) at y=50 should have red patterned pixels.
        // At pixel (50, 50) — center of the stroke — should be red from pattern.
        let center = pixmap.pixel(50, 50).unwrap();
        assert!(
            center.red() > 200 && center.green() < 50,
            "Center of stroke should be red from pattern, got ({},{},{})",
            center.red(),
            center.green(),
            center.blue(),
        );

        // Top edge (0, 0) should be white (no stroke there)
        let corner = pixmap.pixel(0, 0).unwrap();
        assert!(
            corner.red() > 200 && corner.green() > 200 && corner.blue() > 200,
            "Corner should be white (no stroke), got ({},{},{})",
            corner.red(),
            corner.green(),
            corner.blue(),
        );
    }

    // --- Feature 38: Device color operators set color space ---

    #[test]
    fn g_operator_resets_fill_color_space_for_scn() {
        // After `rg 1 0 0` (DeviceRGB), `0.5 g` should set space to DeviceGray.
        // Then `0.5 scn` should interpret as gray (1 component, DeviceGray).
        // Fill a rect — should be gray, not red.
        let doc = doc_with_content(b"1 0 0 rg 0.5 g 0.5 scn 10 10 80 80 re f");
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        // Should be ~50% gray (128, 128, 128), not red
        assert!(pixel_approx(&pixmap, 50, 50, 128, 128, 128, 10));
    }

    #[test]
    fn rg_operator_sets_fill_color_space_for_scn() {
        // After `0.5 g` (DeviceGray), `1 0 0 rg` should set space to DeviceRGB.
        // Then `0 1 0 scn` should interpret as RGB green.
        let doc = doc_with_content(b"0.5 g 1 0 0 rg 0 1 0 scn 10 10 80 80 re f");
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        assert!(pixel_approx(&pixmap, 50, 50, 0, 255, 0, 10));
    }

    // --- Feature 37: Text clipping ---

    #[test]
    fn text_clipped_to_rect() {
        // Clip to left half (0-49), then draw text at x=0.
        // Placeholder rectangles should not appear in the right half (x >= 50).
        let doc = doc_with_content(b"q 0 0 50 100 re W n BT /F1 24 Tf 0 40 Td (HELLO) Tj ET Q");
        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).unwrap();
        // Check that no non-white pixels exist at x >= 50
        let has_ink_outside = pixmap.pixels().iter().enumerate().any(|(i, p)| {
            let x = (i as u32) % pixmap.width();
            x >= 50 && (p.red() != 255 || p.green() != 255 || p.blue() != 255) && p.alpha() > 0
        });
        assert!(
            !has_ink_outside,
            "Text should be clipped to the 50-pixel-wide rectangle"
        );
    }

    #[test]
    fn render_link_annotation_draws_blue_rect() {
        use crate::core::objects::*;

        // Build a page with a Link annotation covering (100, 700)-(200, 720)
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Create annotation dictionary
        let mut annot_dict = Dictionary::new();
        annot_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Annot")));
        annot_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Link")));
        annot_dict.insert(
            PdfName::new("Rect"),
            Object::Array(vec![
                Object::Real(100.0),
                Object::Real(700.0),
                Object::Real(300.0),
                Object::Real(720.0),
            ]),
        );
        annot_dict.insert(
            PdfName::new("Border"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
            ]),
        );
        let annot_id = doc.add_object(Object::Dictionary(annot_dict));

        // Add /Annots to the page
        let page_id = {
            // Find the page object ID by scanning objects
            let catalog = doc.catalog().unwrap();
            let pages_ref = catalog.get_str("Pages").unwrap();
            let pages_obj = doc.resolve(pages_ref).unwrap();
            let kids = pages_obj.as_dict().unwrap().get_str("Kids").unwrap();
            let page_ref = kids.as_array().unwrap()[0].as_reference().unwrap();
            page_ref.id()
        };
        if let Some(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
            page.insert(
                PdfName::new("Annots"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    annot_id.0, annot_id.1,
                ))]),
            );
        }

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).expect("render should succeed");

        // Check for blue pixels in the annotation area
        // PDF (200, 710) → tiny-skia (200, 792-710) = (200, 82)
        let w = pixmap.width() as usize;
        let px = pixmap.pixels()[82 * w + 200];
        assert!(
            px.blue() > px.red() && px.blue() > 200,
            "Link annotation area should have blue tint, got ({}, {}, {})",
            px.red(),
            px.green(),
            px.blue()
        );
    }

    #[test]
    fn render_annotation_with_appearance_stream() {
        use crate::core::objects::*;

        // Build a page with an annotation that has a custom /AP/N appearance
        // stream drawing a red filled rectangle.
        let mut doc = Document::new();
        doc.add_page(200.0, 200.0).unwrap();

        // Create the appearance Form XObject: a red rectangle
        let ap_content = b"1 0 0 rg 0 0 50 20 re f";
        let mut ap_dict = Dictionary::new();
        ap_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("XObject")));
        ap_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Form")));
        ap_dict.insert(
            PdfName::new("BBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(50),
                Object::Integer(20),
            ]),
        );
        ap_dict.insert(
            PdfName::new("Length"),
            Object::Integer(ap_content.len() as i64),
        );
        let ap_stream = PdfStream::new(ap_dict, ap_content.to_vec());
        let ap_id = doc.add_object(Object::Stream(ap_stream));

        // Create annotation with /AP/N pointing to the appearance stream
        let mut ap_normal = Dictionary::new();
        ap_normal.insert(
            PdfName::new("N"),
            Object::Reference(IndirectRef::new(ap_id.0, ap_id.1)),
        );

        let mut annot_dict = Dictionary::new();
        annot_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Annot")));
        annot_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Stamp")));
        annot_dict.insert(
            PdfName::new("Rect"),
            Object::Array(vec![
                Object::Real(50.0),
                Object::Real(100.0),
                Object::Real(100.0),
                Object::Real(120.0),
            ]),
        );
        annot_dict.insert(PdfName::new("AP"), Object::Dictionary(ap_normal));
        let annot_id = doc.add_object(Object::Dictionary(annot_dict));

        // Wire annotation to page
        let page_id = {
            let catalog = doc.catalog().unwrap();
            let pages_ref = catalog.get_str("Pages").unwrap();
            let pages_obj = doc.resolve(pages_ref).unwrap();
            let kids = pages_obj.as_dict().unwrap().get_str("Kids").unwrap();
            kids.as_array().unwrap()[0].as_reference().unwrap().id()
        };
        if let Some(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
            page.insert(
                PdfName::new("Annots"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    annot_id.0, annot_id.1,
                ))]),
            );
        }

        let renderer = Renderer::new(&doc, RenderOptions::default());
        let pixmap = renderer.render_page(0).expect("render should succeed");

        // The appearance stream draws a red rect at the annotation's position.
        // PDF (75, 110) -> tiny-skia (75, 200-110) = (75, 90)
        let w = pixmap.width() as usize;
        let px = pixmap.pixels()[90 * w + 75];
        assert!(
            px.red() > 200 && px.green() < 50 && px.blue() < 50,
            "Annotation appearance stream should render red, got ({}, {}, {})",
            px.red(),
            px.green(),
            px.blue()
        );
    }
}
