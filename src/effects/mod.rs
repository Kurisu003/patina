//! Effects and adjustments framework.
//!
//! An effect renders output pixels for one rectangle at a time from a padded
//! source buffer. The runner splits the selection into 256px tiles and renders
//! them in parallel, so effects scale to very large images and can be
//! cancelled mid-way (live preview re-renders when a slider moves).

pub mod adjustments;
pub mod artistic;
pub mod blurs;
pub mod color;
pub mod distort;
pub mod noise;
pub mod object;
pub mod photo;
pub mod render;
pub mod stylize;
pub mod util;

use crate::raster::lerp_px;
use crate::selection::{MTile, Selection};
use crate::surface::{IRect, Surface, TILE};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Adjustment,
    Artistic,
    Blurs,
    Color,
    Distort,
    Noise,
    Object,
    Photo,
    Render,
    Stylize,
}

impl Category {
    pub const EFFECT_MENUS: [Category; 9] = [
        Category::Artistic,
        Category::Blurs,
        Category::Color,
        Category::Distort,
        Category::Noise,
        Category::Object,
        Category::Photo,
        Category::Render,
        Category::Stylize,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Category::Adjustment => "Adjustments",
            Category::Artistic => "Artistic",
            Category::Blurs => "Blurs",
            Category::Color => "Color",
            Category::Distort => "Distort",
            Category::Noise => "Noise",
            Category::Object => "Object",
            Category::Photo => "Photo",
            Category::Render => "Render",
            Category::Stylize => "Stylize",
        }
    }
}

/// Information available to effects while rendering.
#[derive(Clone)]
pub struct RenderCtx {
    pub width: u32,
    pub height: u32,
    /// Bounds of the selection (the whole image when nothing is selected).
    pub sel_bounds: IRect,
    /// Straight-alpha palette colours.
    pub primary: [u8; 4],
    pub secondary: [u8; 4],
}

/// Source pixels for a render call. `buf` holds `rect` (the destination
/// rectangle inflated by the effect's margin, clipped to the image); reads
/// outside it fall back to the full surface with edge clamping.
pub struct Src<'a> {
    pub surface: &'a Surface,
    pub buf: &'a [u8],
    pub rect: IRect,
}

impl<'a> Src<'a> {
    /// Premultiplied pixel with coordinates clamped to the image.
    #[inline]
    pub fn get(&self, x: i32, y: i32) -> [u8; 4] {
        let x = x.clamp(0, self.surface.width as i32 - 1);
        let y = y.clamp(0, self.surface.height as i32 - 1);
        if self.rect.contains(x, y) {
            let i = (((y - self.rect.y) * self.rect.w + (x - self.rect.x)) * 4) as usize;
            [self.buf[i], self.buf[i + 1], self.buf[i + 2], self.buf[i + 3]]
        } else {
            self.surface.get_pixel(x, y)
        }
    }

    /// Pixel or transparent when outside the image (no clamping).
    #[inline]
    pub fn get_or_clear(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.surface.width as i32 || y >= self.surface.height as i32 {
            return [0; 4];
        }
        self.get(x, y)
    }

    /// Bilinear sample (premultiplied), clamped to the image.
    pub fn bilinear(&self, fx: f32, fy: f32) -> [u8; 4] {
        let x0 = fx.floor();
        let y0 = fy.floor();
        let ax = fx - x0;
        let ay = fy - y0;
        let (x0, y0) = (x0 as i32, y0 as i32);
        let p00 = self.get(x0, y0);
        let p10 = self.get(x0 + 1, y0);
        let p01 = self.get(x0, y0 + 1);
        let p11 = self.get(x0 + 1, y0 + 1);
        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = p00[c] as f32 * (1.0 - ax) + p10[c] as f32 * ax;
            let bot = p01[c] as f32 * (1.0 - ax) + p11[c] as f32 * ax;
            out[c] = (top * (1.0 - ay) + bot * ay).round().clamp(0.0, 255.0) as u8;
        }
        out
    }

    pub fn width(&self) -> u32 {
        self.surface.width
    }
    pub fn height(&self) -> u32 {
        self.surface.height
    }
}

pub trait Effect: Send + Sync {
    fn name(&self) -> &'static str;
    fn category(&self) -> Category;
    /// Icon file name (without extension) from assets/icons.
    fn icon(&self) -> &'static str {
        "effects-default-symbolic"
    }
    /// Whether the effect shows a settings dialog before applying.
    fn has_dialog(&self) -> bool {
        true
    }
    /// Draw the settings UI; return true when a value changed.
    fn ui(&mut self, _ui: &mut egui::Ui) -> bool {
        false
    }
    /// Source pixels needed around each output rectangle.
    fn margin(&self) -> i32 {
        0
    }
    /// Whole-image pre-pass (histograms, random tables...). Runs once per render.
    fn prepare(&mut self, _src: &Surface, _ctx: &RenderCtx) {}
    /// Render `dst` (premultiplied RGBA, `dst_rect.w * dst_rect.h * 4`,
    /// pre-filled with the source pixels) for image rectangle `dst_rect`.
    fn render(&self, src: &Src, dst: &mut [u8], dst_rect: IRect, ctx: &RenderCtx);
    fn box_clone(&self) -> Box<dyn Effect>;
}

/// All effects in menu order.
pub fn registry() -> Vec<Box<dyn Effect>> {
    let mut v: Vec<Box<dyn Effect>> = Vec::new();
    v.extend(adjustments::all());
    v.extend(artistic::all());
    v.extend(blurs::all());
    v.extend(color::all());
    v.extend(distort::all());
    v.extend(noise::all());
    v.extend(object::all());
    v.extend(photo::all());
    v.extend(render::all());
    v.extend(stylize::all());
    v
}

pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Progress { done: AtomicUsize::new(0), total: AtomicUsize::new(1), cancel: AtomicBool::new(false) })
    }
    pub fn fraction(&self) -> f32 {
        self.done.load(Ordering::Relaxed) as f32 / self.total.load(Ordering::Relaxed).max(1) as f32
    }
}

/// Render `effect` over the selection of `src`. Returns None if cancelled.
pub fn apply(effect: &mut dyn Effect, src: &Surface, sel: &Selection, ctx: &RenderCtx, progress: &Progress) -> Option<Surface> {
    effect.prepare(src, ctx);
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    let effect: &dyn Effect = effect;
    let r = ctx.sel_bounds.intersect(&src.bounds());
    let mut target = src.clone();
    let n = target.tiles_in(r).len();
    progress.total.store(n.max(1), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let margin = effect.margin();
    let tiles_x = target.tiles_x;
    let img = src.bounds();
    target.par_tiles_mut(r, true, |t, tr| {
        if progress.cancel.load(Ordering::Relaxed) {
            return;
        }
        let i = ((tr.y as usize / TILE) as u32 * tiles_x + (tr.x as usize / TILE) as u32) as usize;
        let st = sel.mask.as_ref().map(|m| &m.tiles[i]);
        if matches!(st, Some(MTile::Empty)) {
            progress.done.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let dr = tr.intersect(&r);
        let srect = dr.inflate(margin).intersect(&img);
        let sbuf = src.read_rect(srect);
        let s = Src { surface: src, buf: &sbuf, rect: srect };
        // destination starts as the source pixels
        let mut dst = vec![0u8; (dr.w * dr.h * 4) as usize];
        for y in 0..dr.h {
            let so = (((dr.y + y - srect.y) * srect.w + (dr.x - srect.x)) * 4) as usize;
            let d0 = (y * dr.w * 4) as usize;
            dst[d0..d0 + dr.w as usize * 4].copy_from_slice(&sbuf[so..so + dr.w as usize * 4]);
        }
        effect.render(&s, &mut dst, dr, ctx);
        for y in 0..dr.h {
            let ly = (dr.y + y - tr.y) as usize;
            for x in 0..dr.w {
                let lx = (dr.x + x - tr.x) as usize;
                let cov = st.map_or(255, |m| m.get(lx, ly));
                if cov == 0 {
                    continue;
                }
                let si = ((y * dr.w + x) * 4) as usize;
                let v = [dst[si], dst[si + 1], dst[si + 2], dst[si + 3]];
                let di = (ly * TILE + lx) * 4;
                let out = if cov == 255 { v } else { lerp_px(&t.px[di..di + 4], v, cov) };
                t.px[di..di + 4].copy_from_slice(&out);
            }
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
    });
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    target.clear_outside();
    Some(target)
}
