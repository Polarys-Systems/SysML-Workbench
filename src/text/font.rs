//! Font registry and CPU text layout (no GPU state).

use super::stb::FontFace;
use std::collections::HashMap;

/// Handle to a font loaded into a [`FontSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId(pub(crate) u16);

/// Vertical metrics in pixels for one font size.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // part of the public metrics even where the demo does not read it
pub struct LineMetrics {
    /// Baseline distance below the top of a line.
    pub ascent: f32,
    /// Distance from the baseline to the bottom of a line (positive).
    pub descent: f32,
    /// Distance between consecutive baselines.
    pub line_height: f32,
}

/// A glyph placed by [`FontSet::layout`], relative to the top-left of the text box.
#[derive(Clone, Copy, Debug)]
pub struct PlacedGlyph {
    pub glyph: u32,
    /// Pen position (unsnapped) in pixels.
    pub x: f32,
    /// Baseline in pixels.
    pub baseline: f32,
}

struct Font {
    face: FontFace,
    glyphs: HashMap<char, u32>,
    advances: HashMap<u32, i32>,
}

impl Font {
    fn glyph(&mut self, ch: char) -> u32 {
        *self.glyphs.entry(ch).or_insert_with(|| self.face.glyph_index(ch))
    }
    fn advance(&mut self, glyph: u32) -> i32 {
        *self.advances.entry(glyph).or_insert_with(|| self.face.advance(glyph))
    }
}

#[derive(Default)]
pub struct FontSet {
    fonts: Vec<Font>,
}

const TAB_SPACES: f32 = 4.0;

impl FontSet {
    pub fn add(&mut self, face: FontFace) -> Option<FontId> {
        let id = u16::try_from(self.fonts.len()).ok()?;
        self.fonts.push(Font { face, glyphs: HashMap::new(), advances: HashMap::new() });
        Some(FontId(id))
    }

    pub fn face(&self, font: FontId) -> &FontFace {
        &self.fonts[font.0 as usize].face
    }

    /// Font-unit to pixel scale for an em size of `size` pixels.
    pub fn scale(&self, font: FontId, size: f32) -> f32 {
        self.face(font).scale_for_em(size)
    }

    pub fn line_metrics(&self, font: FontId, size: f32) -> LineMetrics {
        let face = self.face(font);
        let s = face.scale_for_em(size);
        let m = face.vmetrics;
        LineMetrics {
            ascent: m.ascent as f32 * s,
            descent: -m.descent as f32 * s,
            line_height: (m.ascent - m.descent + m.line_gap) as f32 * s,
        }
    }

    /// Lays out `text` (UTF-8, `\n` breaks lines, `\t` = 4 spaces) and appends the
    /// visible glyphs to `out`. With `max_width`, lines wrap greedily at spaces;
    /// a single word wider than the limit overflows. Returns the box `[width, height]`.
    pub fn layout(
        &mut self,
        font: FontId,
        size: f32,
        text: &str,
        max_width: Option<f32>,
        out: &mut Vec<PlacedGlyph>,
    ) -> [f32; 2] {
        let metrics = self.line_metrics(font, size);
        let font = &mut self.fonts[font.0 as usize];
        let scale = font.face.scale_for_em(size);
        let space = font.glyph(' ');
        let space_advance = font.advance(space) as f32 * scale;

        let mut line = 0u32;
        let mut line_start = out.len();
        let mut pen = 0.0f32;
        let mut width = 0.0f32;
        let mut prev: Option<u32> = None;
        // Last wrap opportunity on this line: (index of the next word's first glyph,
        // pen x where that word starts, line width before the whitespace).
        let mut wrap: Option<(usize, f32, f32)> = None;

        for ch in text.chars() {
            match ch {
                '\r' => continue,
                '\n' => {
                    width = width.max(pen);
                    line += 1;
                    pen = 0.0;
                    prev = None;
                    line_start = out.len();
                    wrap = None;
                    continue;
                }
                _ => {}
            }
            if ch == '\t' || ch == ' ' {
                let before = pen;
                if ch == '\t' {
                    pen += space_advance * TAB_SPACES;
                    prev = None;
                } else {
                    if let Some(p) = prev {
                        pen += font.face.kern(p, space) as f32 * scale;
                    }
                    pen += space_advance;
                    prev = Some(space);
                }
                wrap = Some((out.len(), pen, before));
                continue;
            }

            let glyph = font.glyph(ch);
            if let Some(p) = prev {
                pen += font.face.kern(p, glyph) as f32 * scale;
            }
            let advance = font.advance(glyph) as f32 * scale;
            if let (Some(limit), Some((index, start, before))) = (max_width, wrap)
                && pen + advance > limit
                && index > line_start
            {
                width = width.max(before);
                line += 1;
                for g in &mut out[index..] {
                    g.x -= start;
                    g.baseline += metrics.line_height;
                }
                pen -= start;
                line_start = index;
                wrap = None;
            }
            out.push(PlacedGlyph { glyph, x: pen, baseline: metrics.ascent + line as f32 * metrics.line_height });
            pen += advance;
            prev = Some(glyph);
        }
        width = width.max(pen);
        [width, (line + 1) as f32 * metrics.line_height]
    }
}
