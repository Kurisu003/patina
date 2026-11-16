//! Photo menu: Glow, Red Eye Removal, Sharpen, Soften Portrait, Vignette.
//!
//! Ported from Pinta (MIT) / Paint.NET 3.36. Pixel math follows Pinta's
//! `ColorBgra` conventions: truncating straight/premultiplied conversions,
//! Paint.NET's integer Gaussian blur and its circular local histogram.

use super::util::{intensity, offset, slider_f, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::{IRect, Surface};

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![
        Box::new(Glow::default()),
        Box::new(RedEyeRemoval::default()),
        Box::new(Sharpen::default()),
        Box::new(SoftenPortrait::default()),
        Box::new(Vignette::default()),
    ]
}

// ---------------------------------------------------------------------------
// Shared pixel helpers (Pinta semantics)
// ---------------------------------------------------------------------------

/// Premultiplied -> straight alpha with truncation (ColorBgra.ToStraightAlpha).
#[inline]
fn to_straight(p: [u8; 4]) -> [u8; 4] {
    let a = p[3] as u32;
    if a == 0 {
        return [0; 4];
    }
    let f = |c: u8| (c as u32 * 255 / a).min(255) as u8;
    [f(p[0]), f(p[1]), f(p[2]), p[3]]
}

/// Straight -> premultiplied with truncation (ColorBgra.ToPremultipliedAlpha).
#[inline]
fn to_premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    let f = |v: u8| (v as u32 * a / 255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

/// Keep premultiplied data valid (colour channels never exceed alpha).
#[inline]
fn clamp_premul(mut p: [u8; 4]) -> [u8; 4] {
    for c in 0..3 {
        p[c] = p[c].min(p[3]);
    }
    p
}

/// Read-only window into the margin-padded source buffer. All in-image pixels
/// within the effect margin of the destination rectangle are inside it.
struct View<'a> {
    buf: &'a [u8],
    rect: IRect,
}

impl<'a> View<'a> {
    fn new(src: &'a Src) -> Self {
        View { buf: src.buf, rect: src.rect }
    }
    #[inline]
    fn px(&self, x: i32, y: i32) -> [u8; 4] {
        debug_assert!(self.rect.contains(x, y));
        let i = (((y - self.rect.y) * self.rect.w + (x - self.rect.x)) * 4) as usize;
        [self.buf[i], self.buf[i + 1], self.buf[i + 2], self.buf[i + 3]]
    }
}

/// Paint.NET's premultiplied blend formula with a replaceable channel blend
/// `f(Cb, Ca, Ab, Aa)` (b = bottom, a = top).
#[inline]
fn blend_premul(bottom: [u8; 4], top: [u8; 4], f: impl Fn(i32, i32, i32, i32) -> i32) -> [u8; 4] {
    if top[3] == 0 {
        return bottom;
    }
    if bottom[3] == 0 {
        return top;
    }
    let ta = top[3] as i32;
    let ba = bottom[3] as i32;
    let inv_t = 255 - ta;
    let inv_b = 255 - ba;
    let mut out = [0u8; 4];
    for c in 0..3 {
        let cb = bottom[c] as i32;
        let ca = top[c] as i32;
        let v = inv_b * ca + inv_t * cb + f(cb, ca, ba, ta);
        out[c] = ((v + 128) / 255).clamp(0, 255) as u8;
    }
    out[3] = (ta + (ba * inv_t + 128) / 255).clamp(0, 255) as u8;
    out
}

#[inline]
fn screen(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    blend_premul(bottom, top, |cb, ca, ab, aa| aa * cb + ab * ca - ca * cb)
}

#[inline]
fn overlay(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    blend_premul(bottom, top, |cb, ca, ab, aa| {
        if 2 * cb <= ab { 2 * cb * ca } else { aa * ab - 2 * (ab - cb) * (aa - ca) }
    })
}

/// Pinta's BrightnessContrastPixelOp (lookup table in straight alpha).
#[derive(Clone)]
struct BrightnessContrast {
    divide: i32,
    table: Vec<u8>,
}

impl BrightnessContrast {
    fn new(brightness: i32, contrast: i32) -> Self {
        let (multiply, divide) = if contrast < 0 {
            (contrast + 100, 100)
        } else if contrast > 0 {
            (100, 100 - contrast)
        } else {
            (1, 1)
        };
        let mut table = vec![0u8; 65536];
        if divide == 0 {
            for i in 0..256i32 {
                table[i as usize] = if i + brightness < 128 { 0 } else { 255 };
            }
        } else {
            for i in 0..256i32 {
                let shift = if divide == 100 {
                    (i - 127) * multiply / divide + 127 - i + brightness
                } else {
                    (i - 127 + brightness) * multiply / divide + 127 - i
                };
                for col in 0..256i32 {
                    table[(i * 256 + col) as usize] = (col + shift).clamp(0, 255) as u8;
                }
            }
        }
        BrightnessContrast { divide, table }
    }

    #[inline]
    fn apply_straight(&self, c: [u8; 4]) -> [u8; 4] {
        let i = intensity(c) as usize;
        if self.divide == 0 {
            let v = self.table[i];
            return [v, v, v, c[3]];
        }
        let row = &self.table[i * 256..i * 256 + 256];
        [row[c[0] as usize], row[c[1] as usize], row[c[2] as usize], c[3]]
    }

    /// Apply to a premultiplied pixel.
    #[inline]
    fn apply(&self, p: [u8; 4]) -> [u8; 4] {
        match p[3] {
            0 => p,
            255 => self.apply_straight(p),
            _ => to_premul(self.apply_straight(to_straight(p))),
        }
    }
}

/// Paint.NET's integer Gaussian blur (triangular weights, alpha weighted),
/// rendered into `out` (premultiplied, `r.w * r.h * 4`). Needs `rad` margin.
fn pdn_gaussian_blur(src: &Src, r: IRect, rad: i32, out: &mut [u8]) {
    let w_img = src.width() as i32;
    let h_img = src.height() as i32;
    let view = View::new(src);
    let size = (2 * rad + 1) as usize;
    let weights: Vec<i64> = (0..size as i32).map(|i| 16 * (i.min(2 * rad - i) as i64 + 1)).collect();

    // Straight-alpha copy of every source pixel the rectangle can reach.
    let cx0 = (r.x - rad).max(0);
    let cx1 = (r.right() + rad).min(w_img);
    let ry0 = (r.y - rad).max(0);
    let ry1 = (r.bottom() + rad).min(h_img);
    let ncol = (cx1 - cx0) as usize;
    let mut straight = vec![[0u8; 4]; ncol * (ry1 - ry0) as usize];
    for y in ry0..ry1 {
        let row = &mut straight[(y - ry0) as usize * ncol..][..ncol];
        for (i, p) in row.iter_mut().enumerate() {
            *p = to_straight(view.px(cx0 + i as i32, y));
        }
    }

    let mut wc = vec![0i64; ncol];
    let mut sa = vec![0i64; ncol];
    let mut sr = vec![0i64; ncol];
    let mut sg = vec![0i64; ncol];
    let mut sb = vec![0i64; ncol];

    for y in r.y..r.bottom() {
        wc.fill(0);
        sa.fill(0);
        sr.fill(0);
        sg.fill(0);
        sb.fill(0);
        let mut wa_col = 0i64;
        for (wy, &wv) in weights.iter().enumerate() {
            let sy = y + wy as i32 - rad;
            if sy < 0 || sy >= h_img {
                continue;
            }
            wa_col += wv;
            let row = &straight[(sy - ry0) as usize * ncol..][..ncol];
            for i in 0..ncol {
                let p = row[i];
                let a = p[3] as i64;
                let mut wp = wv * (a + (a >> 7));
                wc[i] += wp;
                wp >>= 8;
                sa[i] += wp * a;
                sr[i] += wp * p[0] as i64;
                sg[i] += wp * p[1] as i64;
                sb[i] += wp * p[2] as i64;
            }
        }
        let orow = &mut out[((y - r.y) * r.w * 4) as usize..][..(r.w * 4) as usize];
        for x in r.x..r.right() {
            let (mut wa_s, mut wc_s, mut a_s, mut r_s, mut g_s, mut b_s) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
            let lo = (rad - x).max(0) as usize;
            let hi = ((w_img - 1 - x + rad).min(2 * rad)) as usize;
            for wx in lo..=hi {
                let i = (x + wx as i32 - rad - cx0) as usize;
                let ww = weights[wx];
                wa_s += ww * wa_col;
                wc_s += ww * wc[i];
                a_s += ww * sa[i];
                r_s += ww * sr[i];
                g_s += ww * sg[i];
                b_s += ww * sb[i];
            }
            wc_s >>= 8;
            let o = ((x - r.x) * 4) as usize;
            let px = if wa_s == 0 || wc_s == 0 {
                [0u8; 4]
            } else {
                to_premul([(r_s / wc_s) as u8, (g_s / wc_s) as u8, (b_s / wc_s) as u8, (a_s / wa_s) as u8])
            };
            orow[o..o + 4].copy_from_slice(&px);
        }
    }
}

/// Paint.NET's circular local histogram. For every pixel of `r` it calls
/// `apply(src_pixel, area, hist)` where `hist[c][v]` counts straight-alpha
/// channel values (RGBA order) inside a disc of radius `rad`, clipped to the
/// image. Needs `rad` margin.
fn local_histogram(src: &Src, r: IRect, rad: i32, dst: &mut [u8], apply: impl Fn([u8; 4], i32, &[[i32; 256]; 4]) -> [u8; 4]) {
    let w_img = src.width() as i32;
    let h_img = src.height() as i32;
    let view = View::new(src);
    // Straight-alpha copy of the part of the source the discs can reach.
    let reach = r.inflate(rad).intersect(&src.rect);
    let straight: Vec<[u8; 4]> = (reach.y..reach.bottom())
        .flat_map(|y| (reach.x..reach.right()).map(move |x| (x, y)))
        .map(|(x, y)| to_straight(view.px(x, y)))
        .collect();
    let sp = |x: i32, y: i32| straight[((y - reach.y) * reach.w + (x - reach.x)) as usize];

    let cutoff = ((rad * 2 + 1) * (rad * 2 + 1) + 2) / 4;
    let mut edge = vec![0i32; rad as usize + 1];
    for v in 0..=rad {
        for u in 0..=rad {
            if u * u + v * v <= cutoff {
                edge[v as usize] = u;
            }
        }
    }

    let mut hist = Box::new([[0i32; 256]; 4]);
    for y in r.y..r.bottom() {
        for h in hist.iter_mut() {
            h.fill(0);
        }
        let top = -rad.min(y);
        let bottom = rad.min(h_img - 1 - y);
        let mut area = 0i32;
        let x0 = r.x;
        for v in top..=bottom {
            let l = edge[v.unsigned_abs() as usize];
            for u in (-l).max(-x0)..=l.min(w_img - 1 - x0) {
                let p = sp(x0 + u, y + v);
                for c in 0..4 {
                    hist[c][p[c] as usize] += 1;
                }
                area += 1;
            }
        }
        let drow = &mut dst[((y - r.y) * r.w * 4) as usize..][..(r.w * 4) as usize];
        for x in r.x..r.right() {
            let o = ((x - r.x) * 4) as usize;
            let out = apply(view.px(x, y), area, &hist);
            drow[o..o + 4].copy_from_slice(&out);
            if x + 1 >= r.right() {
                break;
            }
            // slide the disc one pixel to the right
            for v in top..=bottom {
                let u = edge[v.unsigned_abs() as usize];
                if x - u >= 0 {
                    let p = sp(x - u, y + v);
                    for c in 0..4 {
                        hist[c][p[c] as usize] -= 1;
                    }
                    area -= 1;
                }
                if x + u + 1 < w_img {
                    let p = sp(x + u + 1, y + v);
                    for c in 0..4 {
                        hist[c][p[c] as usize] += 1;
                    }
                    area += 1;
                }
            }
        }
    }
}

/// LocalHistogram.GetChannelPercentile.
#[inline]
fn channel_percentile(min_count: i32, h: &[i32; 256]) -> u8 {
    let mut c = 0usize;
    while c < 255 && h[c] == 0 {
        c += 1;
    }
    let mut count = 0;
    while c < 255 && count < min_count {
        count += h[c];
        c += 1;
    }
    c as u8
}

// ---------------------------------------------------------------------------
// Glow
// ---------------------------------------------------------------------------

/// Gaussian blur, brightness/contrast on the blur, then screen over the source.
#[derive(Clone)]
pub struct Glow {
    pub radius: i32,
    pub brightness: i32,
    pub contrast: i32,
    bc: Option<BrightnessContrast>,
}

impl Default for Glow {
    fn default() -> Self {
        Glow { radius: 6, brightness: 10, contrast: 10, bc: None }
    }
}

impl Effect for Glow {
    fn name(&self) -> &'static str {
        "Glow"
    }
    fn category(&self) -> Category {
        Category::Photo
    }
    fn icon(&self) -> &'static str {
        "effects-photo-glow-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Radius", &mut self.radius, 1, 20);
        c |= slider_i(ui, "Brightness", &mut self.brightness, -100, 100);
        c |= slider_i(ui, "Contrast", &mut self.contrast, -100, 100);
        c
    }
    fn margin(&self) -> i32 {
        self.radius.max(0)
    }
    fn prepare(&mut self, _src: &Surface, _ctx: &RenderCtx) {
        self.bc = Some(BrightnessContrast::new(self.brightness, self.contrast));
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let local;
        let bc = match &self.bc {
            Some(b) => b,
            None => {
                local = BrightnessContrast::new(self.brightness, self.contrast);
                &local
            }
        };
        let rad = self.radius.max(0);
        if rad > 0 {
            pdn_gaussian_blur(src, r, rad, dst);
        }
        let view = View::new(src);
        for y in 0..r.h {
            for x in 0..r.w {
                let o = ((y * r.w + x) * 4) as usize;
                let blurred = bc.apply([dst[o], dst[o + 1], dst[o + 2], dst[o + 3]]);
                let out = screen(view.px(r.x + x, r.y + y), blurred);
                dst[o..o + 4].copy_from_slice(&out);
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Red Eye Removal
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RedEyeRemoval {
    pub tolerance: i32,
    pub saturation: i32,
}

impl Default for RedEyeRemoval {
    fn default() -> Self {
        RedEyeRemoval { tolerance: 70, saturation: 90 }
    }
}

/// HSV saturation scaled to 0..255, as Pinta's RedEyeRemove op computes it.
#[inline]
fn saturation_255(c: [u8; 4]) -> i32 {
    let r = c[0] as f64 / 255.0;
    let g = c[1] as f64 / 255.0;
    let b = c[2] as f64 / 255.0;
    let min = r.min(g).min(b);
    let max = r.max(g).max(b);
    let delta = max - min;
    let s = if max == 0.0 || delta == 0.0 { 0.0 } else { delta / max };
    (s * 255.0) as i32
}

impl Effect for RedEyeRemoval {
    fn name(&self) -> &'static str {
        "Red Eye Removal"
    }
    fn category(&self) -> Category {
        Category::Photo
    }
    fn icon(&self) -> &'static str {
        "effects-photo-redeyeremove-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Tolerance", &mut self.tolerance, 0, 100);
        c |= slider_i(ui, "Saturation Percentage", &mut self.saturation, 0, 100);
        ui.label(egui::RichText::new("Hint: For best results, first use selection tools to select each eye.").small());
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        let tol = self.tolerance;
        let set_sat = self.saturation as f64 / 100.0;
        for p in dst.chunks_exact_mut(4) {
            let c = [p[0], p[1], p[2], p[3]];
            let difference = c[0] as i32 - c[1].max(c[2]) as i32;
            if difference <= tol || saturation_255(c) <= 100 {
                continue;
            }
            let i = 0.114 * c[2] as f64 + 0.587 * c[1] as f64 + 0.299 * c[0] as f64;
            p[0] = (i * set_sat) as u8;
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Sharpen
// ---------------------------------------------------------------------------

/// Pushes every pixel away from the median of its circular neighbourhood.
#[derive(Clone)]
pub struct Sharpen {
    pub amount: i32,
}

impl Default for Sharpen {
    fn default() -> Self {
        Sharpen { amount: 2 }
    }
}

impl Effect for Sharpen {
    fn name(&self) -> &'static str {
        "Sharpen"
    }
    fn category(&self) -> Category {
        Category::Photo
    }
    fn icon(&self) -> &'static str {
        "effects-photo-sharpen-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        slider_i(ui, "Amount", &mut self.amount, 1, 20)
    }
    fn margin(&self) -> i32 {
        self.amount.max(0)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        local_histogram(src, r, self.amount.max(0), dst, |s, area, h| {
            let min_count = area * 50 / 100;
            let median = to_premul([
                channel_percentile(min_count, &h[0]),
                channel_percentile(min_count, &h[1]),
                channel_percentile(min_count, &h[2]),
                channel_percentile(min_count, &h[3]),
            ]);
            let mut out = [0u8; 4];
            for c in 0..4 {
                let from = s[c] as f32;
                let v = from + -0.5f32 * (median[c] as f32 - from);
                out[c] = v.clamp(0.0, 255.0) as u8;
            }
            clamp_premul(out)
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Soften Portrait
// ---------------------------------------------------------------------------

/// Blur + brightness/contrast, overlaid on a warmed greyscale of the source.
#[derive(Clone)]
pub struct SoftenPortrait {
    pub softness: i32,
    pub lighting: i32,
    pub warmth: i32,
    bc: Option<BrightnessContrast>,
}

impl Default for SoftenPortrait {
    fn default() -> Self {
        SoftenPortrait { softness: 5, lighting: 0, warmth: 10, bc: None }
    }
}

impl SoftenPortrait {
    fn make_bc(&self) -> BrightnessContrast {
        BrightnessContrast::new(self.lighting, -self.lighting / 2)
    }
}

impl Effect for SoftenPortrait {
    fn name(&self) -> &'static str {
        "Soften Portrait"
    }
    fn category(&self) -> Category {
        Category::Photo
    }
    fn icon(&self) -> &'static str {
        "effects-photo-softenportrait-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Softness", &mut self.softness, 0, 10);
        c |= slider_i(ui, "Lighting", &mut self.lighting, -20, 20);
        c |= slider_i(ui, "Warmth", &mut self.warmth, 0, 20);
        c
    }
    fn margin(&self) -> i32 {
        3 * self.softness.max(0)
    }
    fn prepare(&mut self, _src: &Surface, _ctx: &RenderCtx) {
        self.bc = Some(self.make_bc());
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let local;
        let bc = match &self.bc {
            Some(b) => b,
            None => {
                local = self.make_bc();
                &local
            }
        };
        let red_adjust = 1.0f32 + self.warmth as f32 / 100.0;
        let blue_adjust = 1.0f32 - self.warmth as f32 / 100.0;
        let rad = 3 * self.softness.max(0);
        if rad > 0 {
            pdn_gaussian_blur(src, r, rad, dst);
        }
        let view = View::new(src);
        for y in 0..r.h {
            for x in 0..r.w {
                let o = ((y * r.w + x) * 4) as usize;
                let top = bc.apply([dst[o], dst[o + 1], dst[o + 2], dst[o + 3]]);
                let s = view.px(r.x + x, r.y + y);
                let grey = intensity(s);
                let effective = [
                    ((grey as f32 * red_adjust) as i32).clamp(0, 255) as u8,
                    grey,
                    ((grey as f32 * blue_adjust) as i32).clamp(0, 255) as u8,
                    s[3],
                ];
                let out = clamp_premul(overlay(effective, top));
                dst[o..o + 4].copy_from_slice(&out);
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Vignette
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Vignette {
    /// Centre relative to the selection: -1..1 spans its bounds, 0 = middle.
    pub offset: [f64; 2],
    pub radius_percentage: i32,
    pub amount: f64,
}

impl Default for Vignette {
    fn default() -> Self {
        Vignette { offset: [0.0, 0.0], radius_percentage: 50, amount: 1.0 }
    }
}

fn srgb_to_linear(s: f64) -> f64 {
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb_clamped(l: f64) -> f64 {
    if l <= 0.0 {
        0.0
    } else if l >= 1.0 {
        1.0
    } else if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

impl Vignette {
    /// Centre in image pixels (Pinta's point picker maps -1..1 like this).
    fn center(&self, sel: IRect) -> (i32, i32) {
        (
            sel.x + ((self.offset[0] + 1.0) * sel.w as f64 / 2.0) as i32,
            sel.y + ((self.offset[1] + 1.0) * sel.h as f64 / 2.0) as i32,
        )
    }
}

impl Effect for Vignette {
    fn name(&self) -> &'static str {
        "Vignette"
    }
    fn category(&self) -> Category {
        Category::Photo
    }
    fn icon(&self) -> &'static str {
        "effects-photo-vignette-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = offset(ui, "Offset", &mut self.offset);
        c |= slider_i(ui, "Radius Percentage", &mut self.radius_percentage, 10, 400);
        c |= slider_f(ui, "Strength", &mut self.amount, 0.0, 1.0);
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let r1 = ctx.width.max(ctx.height) as f64 * 0.5;
        let r2 = r1 * self.radius_percentage as f64 / 100.0;
        let radius_r = std::f64::consts::PI / (8.0 * r2 * r2);
        let amount = self.amount;
        let amount1 = 1.0 - amount;
        let (cx, cy) = self.center(ctx.sel_bounds);
        let mut lin = [0f64; 256];
        for (i, l) in lin.iter_mut().enumerate() {
            *l = srgb_to_linear(i as f64 / 255.0);
        }
        for y in 0..r.h {
            let dy = (r.y + y - cy) as i64;
            for x in 0..r.w {
                let dx = (r.x + x - cx) as i64;
                let d = (dx * dx + dy * dy) as f64 * radius_r;
                let cos = d.cos();
                let factor = if cos <= 0.0 || d > std::f64::consts::PI {
                    amount1
                } else {
                    let f2 = cos * cos;
                    amount1 + amount * f2 * f2
                };
                let o = ((y * r.w + x) * 4) as usize;
                for c in 0..3 {
                    let v = linear_to_srgb_clamped(lin[dst[o + c] as usize] * factor);
                    dst[o + c] = (0.5 + 255.0 * v) as u8;
                }
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{apply, Progress};
    use crate::selection::{Mask, Selection};
    use crate::surface::premul;

    pub(super) fn ctx(w: u32, h: u32, sel: IRect) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: sel, primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255] }
    }

    /// Gradients, hard edges, a semi-transparent band and a transparent hole.
    pub(super) fn test_image(w: u32, h: u32) -> Surface {
        let mut d = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let checker = ((x / 37 + y / 29) % 2) as u8 * 200;
                let a = if (100..140).contains(&y) {
                    128
                } else if (200..260).contains(&x) && (300..360).contains(&y) {
                    0
                } else {
                    255
                };
                let c = [(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, checker.wrapping_add((x % 13) as u8 * 3), a];
                let i = ((y * w + x) * 4) as usize;
                d[i..i + 4].copy_from_slice(&premul(c));
            }
        }
        Surface::from_rgba(w, h, &d)
    }

    /// prepare() + a single render() call over the selection bounds.
    pub(super) fn render_whole(e: &mut dyn Effect, src: &Surface, ctx: &RenderCtx) -> Vec<u8> {
        e.prepare(src, ctx);
        let full = src.to_rgba();
        let s = Src { surface: src, buf: &full, rect: src.bounds() };
        let r = ctx.sel_bounds.intersect(&src.bounds());
        let mut out = full.clone();
        if r.is_empty() {
            return out;
        }
        let mut dst = src.read_rect(r);
        e.render(&s, &mut dst, r, ctx);
        let w = src.width as i32;
        for y in 0..r.h {
            let o = (((r.y + y) * w + r.x) * 4) as usize;
            out[o..o + (r.w * 4) as usize].copy_from_slice(&dst[(y * r.w * 4) as usize..((y + 1) * r.w * 4) as usize]);
        }
        out
    }

    pub(super) fn check(e: &dyn Effect, src: &Surface, sel: Option<IRect>) {
        let (w, h) = (src.width, src.height);
        let sel_rect = sel.unwrap_or(src.bounds());
        let c = ctx(w, h, sel_rect);
        let selection = match sel {
            Some(r) => Selection { mask: Some(Mask::from_rect(w, h, r)) },
            None => Selection::default(),
        };
        let mut a = e.box_clone();
        let tiled = apply(a.as_mut(), src, &selection, &c, &Progress::new()).unwrap().to_rgba();
        let mut b = e.box_clone();
        let whole = render_whole(b.as_mut(), src, &c);
        if let Some(i) = tiled.iter().zip(&whole).position(|(p, q)| p != q) {
            let px = i / 4;
            panic!("{}: tiled != whole at ({}, {}): {:?} vs {:?}", e.name(), px as u32 % w, px as u32 / w, &tiled[px * 4..px * 4 + 4], &whole[px * 4..px * 4 + 4]);
        }
        if sel.is_some() {
            let orig = src.to_rgba();
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    if !sel_rect.contains(x, y) {
                        let i = ((y * w as i32 + x) * 4) as usize;
                        assert_eq!(&tiled[i..i + 4], &orig[i..i + 4], "{} changed pixel outside selection", e.name());
                    }
                }
            }
        }
    }

    fn effects_nondefault() -> Vec<Box<dyn Effect>> {
        vec![
            Box::new(Glow { radius: 12, brightness: 50, contrast: 50, bc: None }),
            Box::new(Glow { radius: 3, brightness: -20, contrast: 100, bc: None }),
            Box::new(RedEyeRemoval { tolerance: 20, saturation: 20 }),
            Box::new(Sharpen { amount: 16 }),
            Box::new(SoftenPortrait { softness: 8, lighting: -10, warmth: 15, bc: None }),
            Box::new(SoftenPortrait { softness: 0, lighting: 5, warmth: 0, bc: None }),
            Box::new(Vignette { offset: [-0.3, 0.6], radius_percentage: 33, amount: 0.7 }),
        ]
    }

    #[test]
    fn menu_order_and_names() {
        let names: Vec<_> = all().iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Glow", "Red Eye Removal", "Sharpen", "Soften Portrait", "Vignette"]);
        assert!(all().iter().all(|e| e.category() == Category::Photo));
    }

    #[test]
    fn tiling_independent() {
        let src = test_image(700, 500);
        for e in all().into_iter().chain(effects_nondefault()) {
            check(e.as_ref(), &src, None);
        }
    }

    #[test]
    fn partial_selection() {
        let src = test_image(700, 500);
        let sel = IRect::new(130, 90, 333, 287);
        for e in all().into_iter().chain(effects_nondefault()) {
            check(e.as_ref(), &src, Some(sel));
        }
    }

    #[test]
    fn tiny_images() {
        for (w, h) in [(1, 1), (1, 7), (5, 1), (3, 3)] {
            let src = test_image(w, h);
            for e in all().into_iter().chain(effects_nondefault()) {
                check(e.as_ref(), &src, None);
            }
        }
    }

    #[test]
    fn identity_settings() {
        let src = test_image(300, 200);
        let orig = src.to_rgba();
        // Strength 0 vignette leaves (opaque) pixels unchanged.
        let mut v = Vignette { amount: 0.0, ..Default::default() };
        let out = render_whole(&mut v, &src, &ctx(300, 200, src.bounds()));
        for (p, q) in out.chunks_exact(4).zip(orig.chunks_exact(4)) {
            if q[3] == 255 {
                assert_eq!(p, q);
            }
        }
        // Red eye with tolerance 255 can never trigger.
        let mut r = RedEyeRemoval { tolerance: 255, saturation: 50 };
        assert_eq!(render_whole(&mut r, &src, &ctx(300, 200, src.bounds())), orig);
    }

    // ---- comparison against Pinta's reference images ----

    fn pinta_assets() -> Option<std::path::PathBuf> {
        let p = std::path::PathBuf::from(std::env::var("PINTA_ASSETS").unwrap_or_else(|_| "/home/kurisu/src/pinta-upstream/tests/Pinta.Effects.Tests/Assets".into()));
        p.exists().then_some(p)
    }

    pub(super) fn load_png(path: &std::path::Path) -> Surface {
        let img = image::open(path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let mut d = img.into_raw();
        for p in d.chunks_exact_mut(4) {
            let c = premul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
        Surface::from_rgba(w, h, &d)
    }

    /// Number of pixels outside Pinta's test tolerance (sum of squared diffs <= 4).
    pub(super) fn diff_count(a: &[u8], b: &[u8]) -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(p, q)| p.iter().zip(q.iter()).map(|(x, y)| (*x as i32 - *y as i32).pow(2)).sum::<i32>() > 4)
            .count()
    }

    #[test]
    #[ignore]
    fn pinta_reference_images() {
        let Some(dir) = pinta_assets() else { return };
        let input = load_png(&dir.join("input.png"));
        let cases: Vec<(&str, Box<dyn Effect>)> = vec![
            ("glow1.png", Box::new(Glow::default())),
            ("glow2.png", Box::new(Glow { radius: 12, brightness: 50, contrast: 50, bc: None })),
            ("redeyeremove1.png", Box::new(RedEyeRemoval::default())),
            ("redeyeremove2.png", Box::new(RedEyeRemoval { tolerance: 20, saturation: 20 })),
            ("sharpen1.png", Box::new(Sharpen::default())),
            ("sharpen2.png", Box::new(Sharpen { amount: 16 })),
            ("softenportrait1.png", Box::new(SoftenPortrait::default())),
            ("softenportrait2.png", Box::new(SoftenPortrait { softness: 8, lighting: -10, warmth: 15, bc: None })),
            ("vignette1.png", Box::new(Vignette { offset: [-1.0, -1.0], ..Default::default() })),
            ("vignette2.png", Box::new(Vignette::default())),
            ("vignette3.png", Box::new(Vignette { radius_percentage: 33, ..Default::default() })),
        ];
        let mut bad = vec![];
        for (file, mut e) in cases {
            let expected = load_png(&dir.join(file)).to_rgba();
            let c = ctx(input.width, input.height, input.bounds());
            let out = apply(e.as_mut(), &input, &Selection::default(), &c, &Progress::new()).unwrap().to_rgba();
            let n = diff_count(&out, &expected);
            println!("{file}: {n} pixels differ");
            if n > 0 {
                bad.push(file);
            }
        }
        assert!(bad.is_empty(), "mismatching: {bad:?}");
    }

    #[test]
    #[ignore]
    fn timing_4000() {
        let src = test_image(4000, 4000);
        let c = ctx(4000, 4000, src.bounds());
        for mut e in all() {
            let t = std::time::Instant::now();
            apply(e.as_mut(), &src, &Selection::default(), &c, &Progress::new()).unwrap();
            println!("{:<16} {:>8.3} s", e.name(), t.elapsed().as_secs_f64());
        }
    }
}
