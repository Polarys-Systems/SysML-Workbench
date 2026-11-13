//! Minimal safe wrapper over the vendored stb_truetype (see `thirdparty/`).

use std::ffi::{c_float, c_int, c_uchar};
use std::ptr::NonNull;

/// Opaque `stbtt_fontinfo`, allocated by the C shim.
#[repr(C)]
struct FontInfo {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn ev_stbtt_create(data: *const c_uchar, len: c_int, index: c_int) -> *mut FontInfo;
    fn ev_stbtt_destroy(info: *mut FontInfo);
    fn stbtt_FindGlyphIndex(info: *const FontInfo, codepoint: c_int) -> c_int;
    fn stbtt_ScaleForMappingEmToPixels(info: *const FontInfo, pixels: c_float) -> c_float;
    fn stbtt_GetFontVMetrics(info: *const FontInfo, ascent: *mut c_int, descent: *mut c_int, line_gap: *mut c_int);
    fn stbtt_GetGlyphHMetrics(info: *const FontInfo, glyph: c_int, advance: *mut c_int, lsb: *mut c_int);
    fn stbtt_GetGlyphKernAdvance(info: *const FontInfo, glyph1: c_int, glyph2: c_int) -> c_int;
    fn stbtt_GetGlyphBitmapBoxSubpixel(
        info: *const FontInfo,
        glyph: c_int,
        scale_x: c_float,
        scale_y: c_float,
        shift_x: c_float,
        shift_y: c_float,
        x0: *mut c_int,
        y0: *mut c_int,
        x1: *mut c_int,
        y1: *mut c_int,
    );
    fn stbtt_MakeGlyphBitmapSubpixel(
        info: *const FontInfo,
        output: *mut c_uchar,
        out_w: c_int,
        out_h: c_int,
        out_stride: c_int,
        scale_x: c_float,
        scale_y: c_float,
        shift_x: c_float,
        shift_y: c_float,
        glyph: c_int,
    );
}

/// Vertical metrics in font units (y up: `descent` is negative).
#[derive(Clone, Copy, Debug)]
pub struct VMetrics {
    pub ascent: i32,
    pub descent: i32,
    pub line_gap: i32,
}

/// One face of a TrueType/OpenType (glyf) font file.
///
/// stb_truetype does not bounds-check its reads, so only load trusted fonts
/// (system fonts or files shipped with the application).
pub struct FontFace {
    info: NonNull<FontInfo>,
    // Read by `info` through a raw pointer: must outlive it and never move its
    // heap allocation (a boxed slice never reallocates).
    _data: Box<[u8]>,
    pub vmetrics: VMetrics,
}

impl FontFace {
    /// Parses face `index` (0 for plain .ttf/.otf; collections have several).
    pub fn from_bytes(data: Vec<u8>, index: u32) -> Option<Self> {
        let data = data.into_boxed_slice();
        let len = c_int::try_from(data.len()).ok()?;
        let index = c_int::try_from(index).ok()?;
        let info = NonNull::new(unsafe { ev_stbtt_create(data.as_ptr(), len, index) })?;
        let (mut ascent, mut descent, mut line_gap) = (0, 0, 0);
        unsafe { stbtt_GetFontVMetrics(info.as_ptr(), &mut ascent, &mut descent, &mut line_gap) };
        Some(Self { info, _data: data, vmetrics: VMetrics { ascent, descent, line_gap } })
    }

    fn raw(&self) -> *const FontInfo {
        self.info.as_ptr()
    }

    /// Glyph for `ch`; 0 (the .notdef box) when the font lacks it.
    pub fn glyph_index(&self, ch: char) -> u32 {
        unsafe { stbtt_FindGlyphIndex(self.raw(), ch as c_int) as u32 }
    }

    /// Scale from font units to pixels so that one em is `pixels` tall
    /// (the CSS meaning of a font size).
    pub fn scale_for_em(&self, pixels: f32) -> f32 {
        unsafe { stbtt_ScaleForMappingEmToPixels(self.raw(), pixels) }
    }

    /// Advance width in font units.
    pub fn advance(&self, glyph: u32) -> i32 {
        let (mut advance, mut lsb) = (0, 0);
        unsafe { stbtt_GetGlyphHMetrics(self.raw(), glyph as c_int, &mut advance, &mut lsb) };
        advance
    }

    /// Kerning adjustment between two glyphs in font units (kern table or GPOS).
    pub fn kern(&self, left: u32, right: u32) -> i32 {
        unsafe { stbtt_GetGlyphKernAdvance(self.raw(), left as c_int, right as c_int) }
    }

    /// Pixel box `[x0, y0, x1, y1]` of the rasterized glyph relative to the pen
    /// position on the baseline (y down).
    pub fn bitmap_box(&self, glyph: u32, scale: f32, shift_x: f32) -> [i32; 4] {
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, 0, 0);
        unsafe {
            stbtt_GetGlyphBitmapBoxSubpixel(
                self.raw(),
                glyph as c_int,
                scale,
                scale,
                shift_x,
                0.0,
                &mut x0,
                &mut y0,
                &mut x1,
                &mut y1,
            )
        };
        [x0, y0, x1, y1]
    }

    /// Rasterizes 8-bit coverage into the `width`x`height` rectangle at the start
    /// of `out`, whose rows are `stride` bytes apart.
    #[allow(clippy::too_many_arguments)]
    pub fn rasterize(&self, glyph: u32, scale: f32, shift_x: f32, out: &mut [u8], width: u32, height: u32, stride: u32) {
        if width == 0 || height == 0 {
            return;
        }
        assert!(width <= stride, "glyph wider than the destination stride");
        let needed = (height as usize - 1) * stride as usize + width as usize;
        assert!(out.len() >= needed, "glyph destination too small");
        unsafe {
            stbtt_MakeGlyphBitmapSubpixel(
                self.raw(),
                out.as_mut_ptr(),
                width as c_int,
                height as c_int,
                stride as c_int,
                scale,
                scale,
                shift_x,
                0.0,
                glyph as c_int,
            )
        };
    }
}

impl Drop for FontFace {
    fn drop(&mut self) {
        unsafe { ev_stbtt_destroy(self.info.as_ptr()) };
    }
}
