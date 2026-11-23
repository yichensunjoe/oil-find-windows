//! Render a verified local PDF page and map an indexed hit to the drawn glyphs.
use std::borrow::Cow;
use std::ops::Range;
use std::str::CharIndices;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;

use hayro::hayro_interpret::{
    font::{FontQuery, Glyph, GlyphId, GlyphRun, PositionedGlyph},
    hayro_cmap::BfString,
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, InterpreterCache, InterpreterSettings, Paint, RectExt, RenderContent, SoftMask,
    TransformExt,
};
use hayro::hayro_syntax::{object::ObjectIdentifier, Pdf};
use hayro::kurbo::{Affine, BezPath, Rect, Shape};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use lopdf::{Document, LoadOptions, Object};

const MAX_PAGE_SIDE: f32 = 2048.0;
const PAGE_SCALE: f32 = 2.0;
const MAX_PAGE_CONTENT_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// Opaque, top-down pixels in the format expected by a 32-bit Win32 DIB.
    pub bgra: Vec<u8>,
    pub highlight: Option<HighlightBounds>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HighlightBounds {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl HighlightBounds {
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// The page number is one-based, and the match is a UTF-8 byte range in the indexed page text.
pub(super) fn render(
    pdf_bytes: Arc<Vec<u8>>,
    page: usize,
    expected_page_text: &str,
    match_range: Range<usize>,
) -> Result<RenderedPage, String> {
    if expected_page_text
        .get(match_range.clone())
        .is_none_or(str::is_empty)
    {
        return Err("正文命中位置无效".to_owned());
    }
    let page_index = page
        .checked_sub(1)
        .ok_or_else(|| "PDF 页码无效".to_owned())?;
    let (indexed_page, contents) = checked_contents(&pdf_bytes, page_index)
        .ok_or_else(|| "无法完整读取 PDF 页面内容".to_owned())?;
    let pdf = Pdf::new(pdf_bytes).map_err(|_| "无法读取 PDF 原页".to_owned())?;
    let page = pdf
        .pages()
        .get(page_index)
        .filter(|page| page.raw().obj_id() == Some(indexed_page))
        .ok_or_else(|| "无法核对 PDF 页码".to_owned())?;
    if page.page_stream() != Some(contents.as_slice()) {
        return Err("无法完整读取 PDF 页面内容".to_owned());
    }
    drop(contents);

    let crop = page.intersected_crop_box().to_kurbo();
    if !crop.is_finite() || crop.width() <= 0.0 || crop.height() <= 0.0 {
        return Err("PDF 页面尺寸无效".to_owned());
    }
    let (width, height) = page.render_dimensions();
    let scale = PAGE_SCALE.min(MAX_PAGE_SIDE / width.max(height));
    if !width.is_finite() || !height.is_finite() || width * scale < 1.0 || height * scale < 1.0 {
        return Err("PDF 页面尺寸无效".to_owned());
    }
    let incomplete = Arc::new(AtomicBool::new(false));
    let settings = InterpreterSettings {
        // Standard PDF fonts have known equivalents. An unknown font may map a
        // code to an unrelated fallback glyph while preserving its Unicode label.
        font_resolver: Arc::new(|query| match query {
            FontQuery::Standard(font) => Some(font.get_font_data()),
            FontQuery::Fallback(_) => None,
        }),
        warning_sink: Arc::new({
            let incomplete = incomplete.clone();
            move |_| incomplete.store(true, Relaxed)
        }),
        ..Default::default()
    };
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &settings,
        &RenderSettings {
            // Use the same unhinted outlines for the raster and for match geometry.
            hinting: false,
            glyph_caching: false,
            ..Default::default()
        },
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    if incomplete.load(Relaxed) {
        return Err("PDF 原页包含无法呈现的内容".to_owned());
    }
    let width = u32::from(pixmap.width());
    let height = u32::from(pixmap.height());
    let mut bgra = pixmap.data_as_u8_slice().to_vec();
    drop(pixmap);
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    let view = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let transform = Affine::scale(f64::from(scale)) * page.initial_transform(true).to_kurbo();
    let mut context = Context::new(
        transform,
        view,
        &InterpreterCache::new(),
        page.xref(),
        InterpreterSettings {
            render_content: RenderContent::PAGE,
            ..settings
        },
    );
    let mut geometry = MatchGeometry {
        expected: expected_page_text.char_indices(),
        selected: match_range,
        clips: vec![Some(transform.transform_rect_bbox(crop).intersect(view))],
        masked: Vec::new(),
        bounds: Vec::new(),
        valid: true,
    };
    interpret_page(page, &mut context, &mut geometry);
    let highlight = geometry.finish().and_then(|boxes| {
        let highlight = boxes.iter().copied().reduce(HighlightBounds::union)?;
        apply_highlight(&mut bgra, width as usize, &boxes);
        Some(highlight)
    });
    Ok(RenderedPage {
        width,
        height,
        bgra,
        highlight,
    })
}

fn checked_contents(bytes: &[u8], page_index: usize) -> Option<(ObjectIdentifier, Vec<u8>)> {
    // Use the indexer's parser to keep its page slots and reject either parser
    // dropping an unreadable member of a multi-stream Contents array.
    let options = LoadOptions::with_max_decompressed_size(MAX_PAGE_CONTENT_BYTES);
    let pdf = Document::load_mem_with_options(bytes, options).ok()?;
    if pdf.is_encrypted() {
        return None;
    }
    let id = pdf.page_iter().nth(page_index)?;
    let contents = pdf
        .get_dictionary(id)
        .ok()?
        .get_deref(b"Contents", &pdf)
        .ok()?;
    let (streams, separated) = match contents {
        Object::Stream(_) => (std::slice::from_ref(contents), false),
        Object::Array(streams) => (streams.as_slice(), true),
        _ => return None,
    };
    let separators = if separated { streams.len() } else { 0 };
    let mut remaining = MAX_PAGE_CONTENT_BYTES.checked_sub(separators)?;
    let mut complete = Vec::new();
    for member in streams {
        let stream = pdf.dereference(member).ok()?.1.as_stream().ok()?;
        if stream.dict.has(b"F") {
            return None;
        }
        let data = if stream.dict.has(b"Filter") && !stream.filters().ok()?.is_empty() {
            Cow::Owned(stream.decompressed_content_with_limit(remaining).ok()?)
        } else {
            Cow::Borrowed(stream.content.as_slice())
        };
        remaining = remaining.checked_sub(data.len())?;
        complete.extend_from_slice(&data);
        if separated {
            // Hayro appends one space after each member of a Contents array.
            complete.push(b' ');
        }
    }
    Some((ObjectIdentifier::new(id.0, i32::from(id.1)), complete))
}

fn apply_highlight(bgra: &mut [u8], width: usize, boxes: &[HighlightBounds]) {
    let mut mask = vec![false; bgra.len() / 4];
    for bounds in boxes {
        for y in bounds.top..bounds.bottom {
            let row = y as usize * width;
            mask[row + bounds.left as usize..row + bounds.right as usize].fill(true);
        }
    }
    for (pixel, highlight) in bgra.chunks_exact_mut(4).zip(mask) {
        if highlight {
            // Multiply yellow into the page to keep dark glyphs legible.
            pixel[0] = (u16::from(pixel[0]) * 3 / 7) as u8;
            pixel[1] = (u16::from(pixel[1]) * 6 / 7) as u8;
        }
    }
}

struct MatchGeometry<'a> {
    expected: CharIndices<'a>,
    selected: Range<usize>,
    clips: Vec<Option<Rect>>,
    masked: Vec<bool>,
    bounds: Vec<HighlightBounds>,
    valid: bool,
}

fn is_mapped_text(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control()
}

impl MatchGeometry<'_> {
    fn finish(mut self) -> Option<Vec<HighlightBounds>> {
        // Consume the entire page in drawing order, including the suffix after the hit.
        (self.valid && !self.expected.any(|(_, ch)| is_mapped_text(ch))).then_some(self.bounds)
    }

    fn next_char(&mut self, ch: char) -> Option<bool> {
        let (offset, expected) = self.expected.find(|&(_, ch)| is_mapped_text(ch))?;
        (ch == expected).then(|| self.selected.contains(&offset))
    }

    fn is_selected(&mut self, glyph: &PositionedGlyph<'_>) -> Option<bool> {
        // The index can add layout whitespace at text moves or between strings in a TJ array.
        // Align every non-whitespace character and keep the original byte offsets.
        match glyph.as_unicode()? {
            BfString::Char(ch) if is_mapped_text(ch) => self.next_char(ch),
            BfString::Char(_) => Some(false),
            BfString::String(text) => {
                let mut chars = text.chars().filter(|&ch| is_mapped_text(ch));
                let Some(first) = chars.next() else {
                    return Some(false);
                };
                let selected = self.next_char(first)?;
                for ch in chars {
                    // A substring of one shaped glyph has no independently verified bounds.
                    if self.next_char(ch)? != selected {
                        return None;
                    }
                }
                Some(selected)
            }
        }
    }

    fn glyph_bounds(
        &self,
        glyph: &PositionedGlyph<'_>,
        props: &DrawProps<'_>,
        draw_mode: &DrawMode,
    ) -> Option<HighlightBounds> {
        let Glyph::Outline(outline) = &**glyph else {
            return None;
        };
        let clip = self.clips.last().copied().flatten()?;
        let visible_paint =
            matches!(&props.paint, Paint::Color(color) if color.to_rgba().to_rgba8()[3] > 0);
        if matches!(draw_mode, DrawMode::Invisible)
            || outline.glyph_id() == GlyphId::NOTDEF
            || !visible_paint
            || props.soft_mask.is_some()
            || self.masked.last() == Some(&true)
        {
            return None;
        }
        let shape = (props.transform * glyph.transform()) * outline.outline();
        let bounds = shape.bounding_box();
        if !bounds.is_finite()
            || bounds.width() <= 0.0
            || bounds.height() <= 0.0
            || !clip.contains_rect(bounds)
        {
            return None;
        }
        let bounds = bounds.inflate(2.0, 2.0).intersect(clip);
        Some(HighlightBounds {
            left: bounds.x0.floor() as u32,
            top: bounds.y0.floor() as u32,
            right: bounds.x1.ceil() as u32,
            bottom: bounds.y1.ceil() as u32,
        })
    }
}

impl Device<'_> for MatchGeometry<'_> {
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, '_>, props: DrawProps<'_>, mode: &DrawMode) {
        // A fill-and-stroke text command emits the same run twice. Consume its fill pass.
        // Stroke-only text makes the indexed and drawn streams differ and disables the overlay.
        if !self.valid || matches!(mode, DrawMode::Stroke(_)) {
            return;
        }
        for glyph in run.glyphs() {
            let bounds = match self.is_selected(glyph) {
                Some(false) => continue,
                Some(true) => self.glyph_bounds(glyph, &props, mode),
                None => None,
            };
            let Some(bounds) = bounds else {
                self.valid = false;
                break;
            };
            self.bounds.push(bounds);
        }
    }

    fn push_clip_rect(&mut self, rect: &Rect) {
        self.clips.push(
            self.clips
                .last()
                .copied()
                .flatten()
                .map(|clip| clip.intersect(*rect)),
        );
    }

    fn push_clip_path(&mut self, _: &ClipPath) {
        // An arbitrary clip's bounding box cannot establish that it exposes the glyph.
        self.clips.push(None);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'_>>, _: BlendMode) {
        self.masked
            .push(opacity <= 0.0 || mask.is_some() || self.masked.last() == Some(&true));
    }

    fn pop_transparency_group(&mut self) {
        self.masked.pop();
    }

    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'_>, _: &DrawMode) {}
    fn draw_image(&mut self, _: Image<'_, '_>, _: ImageDrawProps<'_>) {}
}

#[cfg(test)]
mod tests;
