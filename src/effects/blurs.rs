//! Blurs menu: Fragment, Gaussian Blur, Motion Blur, Radial Blur, Unfocus,
//! Zoom Blur. Ported from Pinta / Paint.NET.

use super::noise::{AlphaHist, sweep_disk};
use super::util::{angle, offset, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![
        Box::new(Fragment::default()),
        Box::new(GaussianBlur::default()),
        Box::new(MotionBlur::default()),
        Box::new(RadialBlur::default()),
        Box::new(Unfocus::default()),
        Box::new(ZoomBlur::default()),
    ]
}

// ---------------------------------------------------------------------------
// Pixel helpers shared by the blurs, noise and artistic menus. They follow
// Pinta's ColorBgra arithmetic exactly (truncating conversions).

/// Premultiplied -> straight alpha, truncating like Pinta's `ToStraightAlpha`.
#[inline]
pub(super) fn to_straight(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    if a == 0 {
        return [0; 4];
    }
    let f = |v: u8| (v as u32 * 255 / a).min(255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

/// Straight -> premultiplied alpha, truncating like Pinta's `ToPremultipliedAlpha`.
#[inline]
pub(super) fn to_premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    let f = |v: u8| (v as u32 * a / 255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

/// Equal-weight average of summed premultiplied colours (Pinta's `ColorBgra.Blender`).
#[inline]
fn blend_avg(sum: [u32; 4], n: u32) -> [u8; 4] {
    if n == 0 {
        return [0; 4];
    }
    [(sum[0] / n).min(255) as u8, (sum[1] / n).min(255) as u8, (sum[2] / n).min(255) as u8, (sum[3] / n).min(255) as u8]
}

#[inline]
fn add_px(sum: &mut [u32; 4], p: [u8; 4]) {
    for c in 0..4 {
        sum[c] += p[c] as u32;
    }
}

#[inline]
fn put(dst: &mut [u8], i: usize, c: [u8; 4]) {
    dst[i..i + 4].copy_from_slice(&c);
}

/// Pixel inside the image (caller guarantees bounds), using the padded
/// buffer when possible and the surface otherwise.
#[inline]
fn px(src: &Src, x: i32, y: i32) -> [u8; 4] {
    if src.rect.contains(x, y) {
        let i = (((y - src.rect.y) * src.rect.w + (x - src.rect.x)) * 4) as usize;
        [src.buf[i], src.buf[i + 1], src.buf[i + 2], src.buf[i + 3]]
    } else {
        src.surface.get_pixel(x, y)
    }
}

/// Centre of radial/zoom blurs in 16.16 fixed point. Pinta measures the
/// offset relative to the image; here it is relative to the selection
/// bounds, which is the image when nothing is selected.
fn centre_fixed(ctx: &RenderCtx, off: [f64; 2]) -> (i64, i64) {
    let b = ctx.sel_bounds;
    let (x, y, w, h) = (b.x as i64, b.y as i64, b.w as i64, b.h as i64);
    let fx = (x << 16) + (w << 15) + (off[0] * (w << 15) as f64) as i64;
    let fy = (y << 16) + (h << 15) + (off[1] * (h << 15) as f64) as i64;
    (fx, fy)
}

// ---------------------------------------------------------------------------
// Gaussian blur (Paint.NET's integer weighted kernel).

/// Row of kernel weights: a triangle `16 * (i + 1)` peaking in the middle.
fn gaussian_row(r: i32) -> Vec<i64> {
    let size = (2 * r + 1) as usize;
    let mut w = vec![0i64; size];
    for i in 0..=r as usize {
        w[i] = 16 * (i as i64 + 1);
        w[size - i - 1] = w[i];
    }
    w
}

/// Paint.NET/Pinta Gaussian blur of `dr` into `dst` (premultiplied, `dr`
/// sized). Colours are averaged in straight alpha weighted by alpha; samples
/// outside the image are skipped. Needs source pixels `rad` around `dr`.
pub(super) fn gaussian_blur(src: &Src, dst: &mut [u8], dr: IRect, rad: i32) {
    if rad <= 0 || dr.is_empty() {
        return;
    }
    let w = gaussian_row(rad);
    let wlen = w.len() as i32;
    let (iw, ih) = (src.width() as i32, src.height() as i32);
    let cx0 = (dr.x - rad).max(0);
    let cx1 = (dr.right() + rad).min(iw);
    let ry0 = (dr.y - rad).max(0);
    let ry1 = (dr.bottom() + rad).min(ih);
    let ncols = (cx1 - cx0) as usize;

    // Straight-alpha copy of the source window: [r, g, b, a, A + (A >> 7)].
    let mut sp = Vec::with_capacity(ncols * (ry1 - ry0) as usize);
    for y in ry0..ry1 {
        for x in cx0..cx1 {
            let c = to_straight(px(src, x, y));
            let a = c[3] as i32;
            sp.push([c[0] as i32, c[1] as i32, c[2] as i32, a, a + (a >> 7)]);
        }
    }

    // Per-column vertical sums: [wa, wc, a, r, g, b].
    let mut cols = vec![[0i64; 6]; ncols];
    for y in dr.y..dr.bottom() {
        cols.iter_mut().for_each(|c| *c = [0; 6]);
        for wy in 0..wlen {
            let sy = y + wy - rad;
            if sy < ry0 || sy >= ry1 {
                continue;
            }
            let wp = w[wy as usize];
            let row = &sp[(sy - ry0) as usize * ncols..][..ncols];
            for (col, p) in cols.iter_mut().zip(row) {
                col[0] += wp;
                let wc = wp * p[4] as i64;
                col[1] += wc;
                let ws = wc >> 8;
                col[2] += ws * p[3] as i64;
                col[3] += ws * p[0] as i64;
                col[4] += ws * p[1] as i64;
                col[5] += ws * p[2] as i64;
            }
        }
        let drow = ((y - dr.y) * dr.w * 4) as usize;
        for x in dr.x..dr.right() {
            let lo = (x - rad).max(cx0);
            let hi = (x + rad + 1).min(cx1);
            let mut s = [0i64; 6];
            for sx in lo..hi {
                let ww = w[(sx - x + rad) as usize];
                let col = &cols[(sx - cx0) as usize];
                for k in 0..6 {
                    s[k] += ww * col[k];
                }
            }
            let wa = s[0];
            let wc = s[1] >> 8;
            let out = if wa == 0 || wc == 0 {
                [0; 4]
            } else {
                let ch = |v: i64, d: i64| (v / d).clamp(0, 255) as u8;
                to_premul([ch(s[3], wc), ch(s[4], wc), ch(s[5], wc), ch(s[2], wa)])
            };
            put(dst, drow + ((x - dr.x) * 4) as usize, out);
        }
    }
}

#[derive(Clone)]
pub struct GaussianBlur {
    pub radius: i32,
}

impl Default for GaussianBlur {
    fn default() -> Self {
        GaussianBlur { radius: 2 }
    }
}

impl Effect for GaussianBlur {
    fn name(&self) -> &'static str {
        "Gaussian Blur"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-gaussianblur-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        slider_i(ui, "Radius", &mut self.radius, 0, 200)
    }
    fn margin(&self) -> i32 {
        self.radius.max(0)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        gaussian_blur(src, dst, r, self.radius);
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Fragment

#[derive(Clone)]
pub struct Fragment {
    pub fragments: i32,
    pub distance: i32,
    /// Degrees.
    pub rotation: f64,
}

impl Default for Fragment {
    fn default() -> Self {
        Fragment { fragments: 4, distance: 8, rotation: 0.0 }
    }
}

impl Fragment {
    /// Offsets of the copies, evenly spaced on a circle of `distance`.
    fn offsets(&self) -> Vec<(i32, i32)> {
        let n = self.fragments.max(1);
        let step = std::f64::consts::TAU / n as f64;
        let start = self.rotation.to_radians() - std::f64::consts::TAU / 4.0;
        let d = self.distance as f64;
        (0..n)
            .map(|i| {
                let a = start + step * i as f64;
                ((d * -a.sin()).round() as i32, (d * -a.cos()).round() as i32)
            })
            .collect()
    }
}

impl Effect for Fragment {
    fn name(&self) -> &'static str {
        "Fragment"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-fragment-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Fragments", &mut self.fragments, 2, 50);
        c |= slider_i(ui, "Distance", &mut self.distance, 0, 100);
        c |= angle(ui, "Rotation", &mut self.rotation);
        c
    }
    fn margin(&self) -> i32 {
        self.distance.max(0) + 1
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let offs = self.offsets();
        let (w, h) = (src.width() as i32, src.height() as i32);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let mut sum = [0u32; 4];
                let mut n = 0;
                for &(ox, oy) in &offs {
                    let (sx, sy) = (x - ox, y - oy);
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        continue;
                    }
                    add_px(&mut sum, px(src, sx, sy));
                    n += 1;
                }
                put(dst, i, blend_avg(sum, n));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Motion blur

#[derive(Clone)]
pub struct MotionBlur {
    /// Degrees.
    pub angle: f64,
    pub distance: i32,
    pub centered: bool,
}

impl Default for MotionBlur {
    fn default() -> Self {
        MotionBlur { angle: 25.0, distance: 10, centered: true }
    }
}

impl MotionBlur {
    /// Sample offsets along the motion line (Pinta's float/double mix kept).
    fn points(&self) -> Vec<(f64, f64)> {
        let theta = self.angle.to_radians() + std::f64::consts::PI;
        let d = self.distance as f64;
        let mut end = ((d as f32) as f64 * theta.cos(), ((-d * theta.sin()) as f32) as f64);
        let mut start = (0.0, 0.0);
        if self.centered {
            start = (-end.0 / 2.0, -end.1 / 2.0);
            end = (end.0 / 2.0, end.1 / 2.0);
        }
        let n = ((1 + self.distance.max(0)) * 3 / 2).max(1) as usize;
        if n == 1 {
            return vec![(0.0, 0.0)];
        }
        (0..n)
            .map(|i| {
                let f = (i as f32 / (n - 1) as f32) as f64;
                (start.0 + f * (end.0 - start.0), start.1 + f * (end.1 - start.1))
            })
            .collect()
    }
}

/// Pinta's `GetBilinearSample`: transparent outside, 8-bit fractions.
#[inline]
fn bilinear_pinta(src: &Src, x: f32, y: f32) -> [u8; 4] {
    let (w, h) = (src.width() as i32, src.height() as i32);
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
        return [0; 4];
    }
    let iu = x.floor() as i32;
    let fx = (256.0 * (x - iu as f32)) as u32;
    let iv = y.floor() as i32;
    let fy = (256.0 * (y - iv as f32)) as u32;
    let (ifx, ify) = (256 - fx, 256 - fy);
    let right = if iu == w - 1 { iu } else { iu + 1 };
    let bottom = if iv == h - 1 { iv } else { iv + 1 };
    let ul = px(src, iu, iv);
    let ur = px(src, right, iv);
    let ll = px(src, iu, bottom);
    let lr = px(src, right, bottom);
    let (wul, wur, wll, wlr) = (ifx * ify, fx * ify, ifx * fy, fx * fy);
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = ((ul[c] as u32 * wul + ur[c] as u32 * wur + ll[c] as u32 * wll + lr[c] as u32 * wlr + 32768) >> 16) as u8;
    }
    out
}

impl Effect for MotionBlur {
    fn name(&self) -> &'static str {
        "Motion Blur"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-motionblur-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = angle(ui, "Angle", &mut self.angle);
        c |= slider_i(ui, "Distance", &mut self.distance, 1, 200);
        c |= ui.checkbox(&mut self.centered, "Centered").changed();
        c
    }
    fn margin(&self) -> i32 {
        self.distance.max(0) + 2
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let pts = self.points();
        let (wm, hm) = ((src.width() - 1) as f64, (src.height() - 1) as f64);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let mut sum = [0u32; 4];
                let mut n = 0;
                for &(px_, py_) in &pts {
                    let (sx, sy) = (px_ + x as f64, py_ + y as f64);
                    if sx < 0.0 || sy < 0.0 || sx > wm || sy > hm {
                        continue;
                    }
                    add_px(&mut sum, bilinear_pinta(src, sx as f32, sy as f32));
                    n += 1;
                }
                put(dst, i, blend_avg(sum, n));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Radial blur

#[derive(Clone)]
pub struct RadialBlur {
    /// Degrees.
    pub angle: f64,
    pub offset: [f64; 2],
    pub quality: i32,
    /// Source margin chosen in prepare(): how far samples travel, when small
    /// enough to be worth buffering. Only affects speed.
    reach: i32,
}

impl Default for RadialBlur {
    fn default() -> Self {
        RadialBlur { angle: 2.0, offset: [0.0, 0.0], quality: 2, reach: 0 }
    }
}

/// Small-angle rotation in 16.16 fixed point (sin x ~ x, cos x ~ 1 - x^2/2),
/// with Pinta's wrapping 32-bit arithmetic.
#[inline]
fn rotated(f: (i32, i32), fr: i32) -> (i32, i32) {
    let fr2 = fr.wrapping_mul(fr) >> 11;
    (
        f.0.wrapping_sub((f.1 >> 8).wrapping_mul(fr) >> 8).wrapping_sub((f.0 >> 14).wrapping_mul(fr2) >> 8),
        f.1.wrapping_add((f.0 >> 8).wrapping_mul(fr) >> 8).wrapping_sub((f.1 >> 14).wrapping_mul(fr2) >> 8),
    )
}

impl Effect for RadialBlur {
    fn name(&self) -> &'static str {
        "Radial Blur"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-radialblur-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = angle(ui, "Angle", &mut self.angle);
        c |= offset(ui, "Offset", &mut self.offset);
        c |= slider_i(ui, "Quality", &mut self.quality, 1, 5);
        ui.weak("Use low quality for previews, small images, and small angles.  Use high quality for final quality, large images, and large angles.");
        c
    }
    fn prepare(&mut self, src: &crate::surface::Surface, ctx: &RenderCtx) {
        let (fcx, fcy) = centre_fixed(ctx, self.offset);
        let (cx, cy) = (fcx as f64 / 65536.0, fcy as f64 / 65536.0);
        let (w, h) = (src.width as f64, src.height as f64);
        let dmax = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
            .iter()
            .map(|&(x, y)| (x - cx).hypot(y - cy))
            .fold(0.0, f64::max);
        let reach = (dmax * self.angle.abs().to_radians() * 1.1).ceil() + 2.0;
        self.reach = if reach <= 256.0 { reach as i32 } else { 0 };
    }
    fn margin(&self) -> i32 {
        self.reach
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        if self.angle == 0.0 {
            return;
        }
        let q = self.quality.clamp(1, 5);
        let n = q * q * (30 + q * q);
        let fr = (self.angle * std::f64::consts::PI * 65536.0 / 181.0) as i32;
        let fsr = fr / n;
        let (fcx, fcy) = centre_fixed(ctx, self.offset);
        let (fcx, fcy) = (fcx as i32, fcy as i32);
        let (w, h) = (src.width() as i32, src.height() as i32);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let mut sum = [0u32; 4];
                add_px(&mut sum, [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]]);
                let mut cnt = 1;
                let f = ((x << 16).wrapping_sub(fcx), (y << 16).wrapping_sub(fcy));
                let (mut o1, mut o2) = (f, f);
                for _ in 0..n {
                    o1 = rotated(o1, fsr);
                    o2 = rotated(o2, -fsr);
                    for o in [o1, o2] {
                        let sx = o.0.wrapping_add(fcx).wrapping_add(32768) >> 16;
                        let sy = o.1.wrapping_add(fcy).wrapping_add(32768) >> 16;
                        if sx > 0 && sy > 0 && sx < w && sy < h {
                            add_px(&mut sum, px(src, sx, sy));
                            cnt += 1;
                        }
                    }
                }
                put(dst, i, blend_avg(sum, cnt));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Unfocus

#[derive(Clone)]
pub struct Unfocus {
    pub radius: i32,
}

impl Default for Unfocus {
    fn default() -> Self {
        Unfocus { radius: 4 }
    }
}

impl Effect for Unfocus {
    fn name(&self) -> &'static str {
        "Unfocus"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-unfocus-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        slider_i(ui, "Radius", &mut self.radius, 1, 200)
    }
    fn margin(&self) -> i32 {
        self.radius.max(1)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let rad = self.radius.max(1);
        let mut hist = AlphaHist::new();
        let (iw, ih) = (src.width() as i32, src.height() as i32);
        sweep_disk(src.buf, src.rect, iw, ih, rad, r, &mut hist, |x, y, h| {
            // Alpha-weighted mean of the premultiplied channels, as Pinta does.
            let div = h.area as i64 * 255;
            let mean = |hc: &[i32; 256]| {
                let s: i64 = hc.iter().enumerate().skip(1).map(|(i, &n)| i as i64 * n as i64).sum();
                (s / div).clamp(0, 255) as u8
            };
            let a = (h.sum / h.area).clamp(0, 255) as u8;
            let i = (((y - r.y) * r.w + (x - r.x)) * 4) as usize;
            put(dst, i, [mean(&h.r), mean(&h.g), mean(&h.b), a]);
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Zoom blur

#[derive(Clone)]
pub struct ZoomBlur {
    pub amount: i32,
    pub offset: [f64; 2],
}

impl Default for ZoomBlur {
    fn default() -> Self {
        ZoomBlur { amount: 10, offset: [0.0, 0.0] }
    }
}

impl Effect for ZoomBlur {
    fn name(&self) -> &'static str {
        "Zoom Blur"
    }
    fn category(&self) -> Category {
        Category::Blurs
    }
    fn icon(&self) -> &'static str {
        "effects-blurs-zoomblur-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Amount", &mut self.amount, 0, 100);
        c |= offset(ui, "Offset", &mut self.offset);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        if self.amount == 0 {
            return;
        }
        const N: usize = 64;
        let fz = self.amount as i64;
        let (fcx, fcy) = centre_fixed(ctx, self.offset);
        let (w, h) = (src.width() as i64, src.height() as i64);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let mut fx = ((x as i64) << 16) - fcx;
                let mut fy = ((y as i64) << 16) - fcy;
                let mut sum = [0u32; 4];
                add_px(&mut sum, [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]]);
                let mut cnt = 1;
                for _ in 0..N {
                    fx -= ((fx >> 4) * fz) >> 10;
                    fy -= ((fy >> 4) * fz) >> 10;
                    let sx = (fx + fcx + 32768) >> 16;
                    let sy = (fy + fcy + 32768) >> 16;
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        continue;
                    }
                    add_px(&mut sum, px(src, sx as i32, sy as i32));
                    cnt += 1;
                }
                put(dst, i, blend_avg(sum, cnt));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------

/// Test helpers shared by the blurs, noise and artistic tests.
#[cfg(test)]
pub(super) mod testutil {
    use super::super::{Effect, Progress, RenderCtx, Src, apply};
    use crate::selection::{Mask, Selection};
    use crate::surface::{IRect, Surface, premul};

    /// Gradients, hard edges, a transparent hole and semi-transparent band.
    pub fn test_image(w: u32, h: u32) -> Surface {
        let mut d = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let mut c = [
                    (x * 255 / w.max(1)) as u8,
                    (y * 255 / h.max(1)) as u8,
                    if (x / 37 + y / 23) % 2 == 0 { 230 } else { 20 },
                    255,
                ];
                if (x as i32 - 300).pow(2) + (y as i32 - 200).pow(2) < 80 * 80 {
                    c[3] = 0;
                } else if (100..140).contains(&y) {
                    c[3] = ((x * 7) % 256) as u8;
                } else if x % 97 < 3 {
                    c = [255, 255, 255, 255];
                }
                d[i..i + 4].copy_from_slice(&premul(c));
            }
        }
        Surface::from_rgba(w, h, &d)
    }

    pub fn ctx(s: &Surface, sel: &Selection) -> RenderCtx {
        RenderCtx {
            width: s.width,
            height: s.height,
            sel_bounds: sel.bounds(s.width, s.height),
            primary: [0, 0, 0, 255],
            secondary: [255, 255, 255, 255],
        }
    }

    pub fn run(e: &mut dyn Effect, s: &Surface, sel: &Selection) -> Vec<u8> {
        let c = ctx(s, sel);
        apply(e, s, sel, &c, &Progress::new()).expect("not cancelled").to_rgba()
    }

    /// Render the whole image with one render() call and a full source buffer.
    pub fn single_call(e: &mut dyn Effect, s: &Surface, c: &RenderCtx) -> Vec<u8> {
        e.prepare(s, c);
        let full = s.bounds();
        let buf = s.read_rect(full);
        let mut dst = buf.clone();
        let src = Src { surface: s, buf: &buf, rect: full };
        e.render(&src, &mut dst, full, c);
        dst
    }

    /// No panic, tiling independence, 1x1 image, partial selection.
    pub fn check(mut e: Box<dyn Effect>) {
        let s = test_image(700, 500);
        let none = Selection::default();
        let tiled = run(e.as_mut(), &s, &none);
        let whole = single_call(e.as_mut(), &s, &ctx(&s, &none));
        assert!(tiled == whole, "{}: tiled render differs from single call", e.name());

        let one = test_image(1, 1);
        run(e.as_mut(), &one, &none);
        let tiny = test_image(3, 2);
        run(e.as_mut(), &tiny, &none);

        let r = IRect::new(130, 90, 300, 280);
        let sel = Selection { mask: Some(Mask::from_rect(s.width, s.height, r)) };
        let part = run(e.as_mut(), &s, &sel);
        let expect = single_call(e.as_mut(), &s, &ctx(&s, &sel));
        let orig = s.to_rgba();
        for y in 0..s.height as i32 {
            for x in 0..s.width as i32 {
                let i = ((y * s.width as i32 + x) * 4) as usize;
                let want = if r.contains(x, y) { &expect[i..i + 4] } else { &orig[i..i + 4] };
                assert_eq!(&part[i..i + 4], want, "{}: selection mismatch at {x},{y}", e.name());
            }
        }
    }

    pub fn assert_identity(mut e: Box<dyn Effect>) {
        let s = test_image(300, 260);
        let out = run(e.as_mut(), &s, &Selection::default());
        assert!(out == s.to_rgba(), "{}: expected identity", e.name());
    }

    /// Time every effect at its defaults on a 4000x4000 image.
    pub fn time_all(effects: Vec<Box<dyn Effect>>) {
        let s = test_image(4000, 4000);
        for mut e in effects {
            let t = std::time::Instant::now();
            run(e.as_mut(), &s, &Selection::default());
            eprintln!("{:<16} {:>8.3} s", e.name(), t.elapsed().as_secs_f64());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::selection::Selection;
    use crate::surface::Surface;

    #[test]
    fn all_effects_tile_independent() {
        for e in all() {
            check(e);
        }
        check(Box::new(GaussianBlur { radius: 17 }));
        check(Box::new(MotionBlur { angle: 200.0, distance: 33, centered: false }));
        check(Box::new(RadialBlur { angle: 40.0, offset: [0.3, -0.5], quality: 1, reach: 0 }));
        check(Box::new(RadialBlur { angle: 9.0, offset: [1.5, 0.2], quality: 3, reach: 0 }));
        check(Box::new(ZoomBlur { amount: 77, offset: [-0.4, 0.6] }));
        check(Box::new(Fragment { fragments: 7, distance: 30, rotation: 33.0 }));
        check(Box::new(Unfocus { radius: 13 }));
    }

    #[test]
    fn identity_settings() {
        assert_identity(Box::new(GaussianBlur { radius: 0 }));
        assert_identity(Box::new(RadialBlur { angle: 0.0, ..Default::default() }));
        assert_identity(Box::new(ZoomBlur { amount: 0, ..Default::default() }));
        assert_identity(Box::new(Fragment { distance: 0, ..Default::default() }));
    }

    #[test]
    fn uniform_image_is_fixed_point() {
        let s = Surface::filled(300, 280, [120, 30, 200, 255]);
        let orig = s.to_rgba();
        for mut e in all() {
            if e.name() == "Motion Blur" {
                // Pinta leaves corner pixels with no in-bounds sample transparent.
                continue;
            }
            let out = run(e.as_mut(), &s, &Selection::default());
            assert!(out == orig, "{} changed a uniform image", e.name());
        }
    }

    /// Direct transcription of Pinta's per-pixel Gaussian formula.
    fn gaussian_naive(s: &Surface, rad: i32) -> Vec<u8> {
        let w = gaussian_row(rad);
        let (iw, ih) = (s.width as i32, s.height as i32);
        let mut out = vec![0u8; (iw * ih * 4) as usize];
        for y in 0..ih {
            for x in 0..iw {
                let (mut wa, mut wc, mut a, mut r, mut g, mut b) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
                for wx in 0..w.len() as i32 {
                    let sx = x + wx - rad;
                    if sx < 0 || sx >= iw {
                        continue;
                    }
                    let (mut cwa, mut cwc, mut ca, mut cr, mut cg, mut cb) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
                    for wy in 0..w.len() as i32 {
                        let sy = y + wy - rad;
                        if sy < 0 || sy >= ih {
                            continue;
                        }
                        let c = to_straight(s.get_pixel(sx, sy));
                        let mut wp = w[wy as usize];
                        cwa += wp;
                        wp *= c[3] as i64 + (c[3] as i64 >> 7);
                        cwc += wp;
                        wp >>= 8;
                        if c[3] > 0 {
                            ca += wp * c[3] as i64;
                            cr += wp * c[0] as i64;
                            cg += wp * c[1] as i64;
                            cb += wp * c[2] as i64;
                        }
                    }
                    let k = w[wx as usize];
                    wa += k * cwa;
                    wc += k * cwc;
                    a += k * ca;
                    r += k * cr;
                    g += k * cg;
                    b += k * cb;
                }
                wc >>= 8;
                let i = ((y * iw + x) * 4) as usize;
                if wa != 0 && wc != 0 {
                    let p = to_premul([(r / wc) as u8, (g / wc) as u8, (b / wc) as u8, (a / wa) as u8]);
                    out[i..i + 4].copy_from_slice(&p);
                }
            }
        }
        out
    }

    #[test]
    fn gaussian_matches_reference() {
        let s = test_image(200, 170);
        for rad in [1, 2, 5] {
            let got = run(&mut GaussianBlur { radius: rad }, &s, &Selection::default());
            assert!(got == gaussian_naive(&s, rad), "radius {rad}");
        }
    }

    #[test]
    #[ignore]
    fn timing_4000() {
        time_all(all());
        time_all(vec![Box::new(GaussianBlur { radius: 50 }), Box::new(GaussianBlur { radius: 200 })]);
    }
}
