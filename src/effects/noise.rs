//! Noise menu: Add Noise, Median, Reduce Noise. Ported from Pinta / Paint.NET.

use super::blurs::{to_premul, to_straight};
use super::util::{hash3, hash01, seed, slider_f, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;
use std::sync::OnceLock;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(AddNoise::default()), Box::new(Median::default()), Box::new(ReduceNoise::default())]
}

// ---------------------------------------------------------------------------
// Local (disk-shaped) histograms, Paint.NET's LocalHistogramEffect.

/// A running window statistic updated one pixel at a time.
pub(super) trait Window {
    fn reset(&mut self);
    fn add(&mut self, p: &[u8]);
    fn remove(&mut self, p: &[u8]);
}

/// Per-channel histograms of straight-alpha pixels.
pub(super) struct StraightHist {
    pub r: [i32; 256],
    pub g: [i32; 256],
    pub b: [i32; 256],
    pub a: [i32; 256],
    pub area: i32,
}

impl StraightHist {
    pub fn new() -> Self {
        StraightHist { r: [0; 256], g: [0; 256], b: [0; 256], a: [0; 256], area: 0 }
    }
}

impl Window for StraightHist {
    fn reset(&mut self) {
        *self = Self::new();
    }
    #[inline]
    fn add(&mut self, p: &[u8]) {
        self.r[p[0] as usize] += 1;
        self.g[p[1] as usize] += 1;
        self.b[p[2] as usize] += 1;
        self.a[p[3] as usize] += 1;
        self.area += 1;
    }
    #[inline]
    fn remove(&mut self, p: &[u8]) {
        self.r[p[0] as usize] -= 1;
        self.g[p[1] as usize] -= 1;
        self.b[p[2] as usize] -= 1;
        self.a[p[3] as usize] -= 1;
        self.area -= 1;
    }
}

/// Histograms of premultiplied channels weighted by alpha, plus the alpha sum.
pub(super) struct AlphaHist {
    pub r: [i32; 256],
    pub g: [i32; 256],
    pub b: [i32; 256],
    pub sum: i32,
    pub area: i32,
}

impl AlphaHist {
    pub fn new() -> Self {
        AlphaHist { r: [0; 256], g: [0; 256], b: [0; 256], sum: 0, area: 0 }
    }
}

impl Window for AlphaHist {
    fn reset(&mut self) {
        *self = Self::new();
    }
    #[inline]
    fn add(&mut self, p: &[u8]) {
        let w = p[3] as i32;
        self.r[p[0] as usize] += w;
        self.g[p[1] as usize] += w;
        self.b[p[2] as usize] += w;
        self.sum += w;
        self.area += 1;
    }
    #[inline]
    fn remove(&mut self, p: &[u8]) {
        let w = p[3] as i32;
        self.r[p[0] as usize] -= w;
        self.g[p[1] as usize] -= w;
        self.b[p[2] as usize] -= w;
        self.sum -= w;
        self.area -= 1;
    }
}

/// Slide a disk of radius `rad` (cut off at about (rad + 0.5)^2, clipped to
/// the image) across every row of `dr`, calling `emit(x, y, window)` for
/// each pixel. `buf` holds the pixels of `brect`, which must cover
/// `dr.inflate(rad)` clipped to the `img_w` x `img_h` image.
#[allow(clippy::too_many_arguments)]
pub(super) fn sweep_disk<W: Window>(
    buf: &[u8],
    brect: IRect,
    img_w: i32,
    img_h: i32,
    rad: i32,
    dr: IRect,
    win: &mut W,
    mut emit: impl FnMut(i32, i32, &W),
) {
    if dr.is_empty() {
        return;
    }
    let cutoff = ((rad * 2 + 1) * (rad * 2 + 1) + 2) / 4;
    // Half-width of the disk on each row offset |v|.
    let edge: Vec<i32> = (0..=rad).map(|v| (0..=rad).filter(|u| u * u + v * v <= cutoff).max().unwrap_or(0)).collect();
    let at = |x: i32, y: i32| {
        let i = (((y - brect.y) * brect.w + (x - brect.x)) * 4) as usize;
        &buf[i..i + 4]
    };
    for y in dr.y..dr.bottom() {
        win.reset();
        let top = -rad.min(y);
        let bottom = rad.min(img_h - 1 - y);
        let x0 = dr.x;
        for v in top..=bottom {
            let e = edge[v.unsigned_abs() as usize];
            for sx in (x0 - e).max(0)..=(x0 + e).min(img_w - 1) {
                win.add(at(sx, y + v));
            }
        }
        for x in dr.x..dr.right() {
            emit(x, y, win);
            if x + 1 == dr.right() {
                break;
            }
            for v in top..=bottom {
                let e = edge[v.unsigned_abs() as usize];
                if x - e >= 0 {
                    win.remove(at(x - e, y + v));
                }
                if x + e + 1 < img_w {
                    win.add(at(x + e + 1, y + v));
                }
            }
        }
    }
}

/// Copy of the source buffer converted to straight alpha (Pinta rounding).
fn straight_buf(src: &Src) -> Vec<u8> {
    let mut v = src.buf.to_vec();
    for p in v.chunks_exact_mut(4) {
        p.copy_from_slice(&to_straight([p[0], p[1], p[2], p[3]]));
    }
    v
}

/// Smallest value reaching `min_count` samples (Pinta's GetChannelPercentile).
#[inline]
fn channel_percentile(min_count: i32, h: &[i32; 256]) -> u8 {
    let mut c = 0;
    while c < 255 && h[c] == 0 {
        c += 1;
    }
    let mut n = 0;
    while c < 255 && n < min_count {
        n += h[c];
        c += 1;
    }
    c as u8
}

// ---------------------------------------------------------------------------
// Add Noise

const TABLE_SIZE: usize = 16384;

/// Gaussian-distributed noise values (16.16), Paint.NET's lookup table.
fn noise_table() -> &'static [i32] {
    static T: OnceLock<Vec<i32>> = OnceLock::new();
    T.get_or_init(|| {
        let curve = |x: f64, scale: f64| scale * (-x * x / 2.0).exp();
        let at = |i: usize| 16.0 * (i as f64 - (TABLE_SIZE / 2) as f64) / TABLE_SIZE as f64;
        let (mut l, mut r, mut scale) = (5.0f64, 10.0f64, 50.0f64);
        while r - l > 0.0000001 {
            let mut s = 0.0;
            scale = (l + r) * 0.5;
            for i in 0..TABLE_SIZE {
                s += curve(at(i), scale);
                if s > 1000000.0 {
                    break;
                }
            }
            if s > TABLE_SIZE as f64 {
                r = scale;
            } else if s < TABLE_SIZE as f64 {
                l = scale;
            } else {
                break;
            }
        }
        let mut t = vec![0i32; TABLE_SIZE];
        let mut sum = 0.0;
        let mut rounded = 0usize;
        for i in 0..TABLE_SIZE {
            sum += curve(at(i), scale);
            let last = rounded;
            rounded = sum as usize;
            let v = (i as i32 - (TABLE_SIZE / 2) as i32) * 65536 / TABLE_SIZE as i32;
            for slot in t.iter_mut().take(rounded.min(TABLE_SIZE)).skip(last) {
                *slot = v;
            }
        }
        t
    })
}

#[derive(Clone)]
pub struct AddNoise {
    pub intensity: i32,
    pub color_saturation: i32,
    /// Percent of pixels affected.
    pub coverage: f64,
    pub seed: u32,
    pub kind: NoiseKind,
    /// Feature size in pixels (1 = per-pixel grain).
    pub size: f32,
    /// Fractal layers for Perlin / Value noise.
    pub octaves: i32,
}

/// The pattern Add Noise adds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoiseKind {
    /// Paint.NET's per-pixel Gaussian grain.
    Gaussian,
    /// Evenly distributed random values.
    Uniform,
    /// Smooth gradient noise.
    Perlin,
    /// Smoothly interpolated random grid values.
    Value,
    /// Distance to the nearest random feature point (Worley).
    Cellular,
}

impl NoiseKind {
    const ALL: [NoiseKind; 5] = [NoiseKind::Gaussian, NoiseKind::Uniform, NoiseKind::Perlin, NoiseKind::Value, NoiseKind::Cellular];
    fn name(self) -> &'static str {
        match self {
            NoiseKind::Gaussian => "Gaussian (grain)",
            NoiseKind::Uniform => "Uniform",
            NoiseKind::Perlin => "Perlin",
            NoiseKind::Value => "Value",
            NoiseKind::Cellular => "Cellular (Worley)",
        }
    }
    fn fractal(self) -> bool {
        matches!(self, NoiseKind::Perlin | NoiseKind::Value)
    }
}

impl Default for AddNoise {
    fn default() -> Self {
        AddNoise { intensity: 64, color_saturation: 100, coverage: 100.0, seed: 0, kind: NoiseKind::Gaussian, size: 1.0, octaves: 4 }
    }
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Random value in [-1, 1] for a lattice point.
#[inline]
fn lattice(ix: i32, iy: i32, seed: u32) -> f32 {
    (hash3(ix, iy, seed) >> 8) as f32 / (1u32 << 23) as f32 - 1.0
}

/// Gradient noise in roughly [-1, 1].
fn perlin(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (ix, iy) = (x0 as i32, y0 as i32);
    let grad = |gx: i32, gy: i32, dx: f32, dy: f32| {
        let a = (hash3(gx, gy, seed) >> 8) as f32 / (1u32 << 24) as f32 * std::f32::consts::TAU;
        a.cos() * dx + a.sin() * dy
    };
    let n00 = grad(ix, iy, fx, fy);
    let n10 = grad(ix + 1, iy, fx - 1.0, fy);
    let n01 = grad(ix, iy + 1, fx, fy - 1.0);
    let n11 = grad(ix + 1, iy + 1, fx - 1.0, fy - 1.0);
    let (u, v) = (fade(fx), fade(fy));
    let a = n00 + (n10 - n00) * u;
    let b = n01 + (n11 - n01) * u;
    (a + (b - a) * v) * 1.414
}

/// Smoothly interpolated lattice values in [-1, 1].
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (u, v) = (fade(x - x0), fade(y - y0));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let a = lattice(ix, iy, seed) + (lattice(ix + 1, iy, seed) - lattice(ix, iy, seed)) * u;
    let b = lattice(ix, iy + 1, seed) + (lattice(ix + 1, iy + 1, seed) - lattice(ix, iy + 1, seed)) * u;
    a + (b - a) * v
}

/// Distance to the nearest feature point (one per cell), mapped to [-1, 1].
fn cellular(x: f32, y: f32, seed: u32) -> f32 {
    let (cx, cy) = (x.floor() as i32, y.floor() as i32);
    let mut best = f32::MAX;
    for oy in -1..=1 {
        for ox in -1..=1 {
            let (gx, gy) = (cx + ox, cy + oy);
            let h = hash3(gx, gy, seed);
            let px = gx as f32 + (h & 0xffff) as f32 / 65536.0;
            let py = gy as f32 + (h >> 16) as f32 / 65536.0;
            best = best.min((px - x).powi(2) + (py - y).powi(2));
        }
    }
    (best.sqrt() * 1.6).min(1.0) * 2.0 - 1.0
}

/// Sum of `octaves` layers of `f`, each half the size and amplitude.
fn fractal(f: fn(f32, f32, u32) -> f32, x: f32, y: f32, seed: u32, octaves: i32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves.max(1) {
        sum += f(x * freq, y * freq, seed.wrapping_add(o as u32 * 0x9e37)) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

impl AddNoise {
    /// Noise for one pixel (16.16, about 4096 per standard deviation like
    /// the Gaussian table) for the three colour channels.
    fn sample(&self, x: i32, y: i32, table: &[i32], seeds: [u32; 3]) -> (i32, i32, i32) {
        let size = self.size.max(1.0);
        let (fx, fy) = ((x as f32 + 0.5) / size, (y as f32 + 0.5) / size);
        let one = |s: u32| -> i32 {
            match self.kind {
                NoiseKind::Gaussian => {
                    let h = hash3(fx.floor() as i32, fy.floor() as i32, s);
                    table[(h & 0x3fff) as usize]
                }
                NoiseKind::Uniform => (lattice(fx.floor() as i32, fy.floor() as i32, s) * 7000.0) as i32,
                NoiseKind::Perlin => (fractal(perlin, fx, fy, s, self.octaves) * 8192.0) as i32,
                NoiseKind::Value => (fractal(value_noise, fx, fy, s, self.octaves) * 8192.0) as i32,
                NoiseKind::Cellular => (cellular(fx, fy, s) * 8192.0) as i32,
            }
        };
        (one(seeds[0]), one(seeds[1]), one(seeds[2]))
    }
}

impl Effect for AddNoise {
    fn name(&self) -> &'static str {
        "Add Noise"
    }
    fn category(&self) -> Category {
        Category::Noise
    }
    fn icon(&self) -> &'static str {
        "effects-noise-addnoise-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Intensity", &mut self.intensity, 0, 100);
        c |= slider_i(ui, "Color Saturation", &mut self.color_saturation, 0, 400);
        c |= slider_f(ui, "Coverage", &mut self.coverage, 0.0, 100.0);
        ui.label("Type");
        egui::ComboBox::from_id_salt("noise_kind").selected_text(self.kind.name()).show_ui(ui, |ui| {
            for k in NoiseKind::ALL {
                c |= ui.selectable_value(&mut self.kind, k, k.name()).changed();
            }
        });
        ui.label("Size");
        c |= ui.add(egui::Slider::new(&mut self.size, 1.0..=400.0).logarithmic(true).suffix(" px")).changed();
        if self.kind.fractal() {
            c |= slider_i(ui, "Detail (octaves)", &mut self.octaves, 1, 8);
        }
        c |= seed(ui, &mut self.seed);
        c
    }
    fn prepare(&mut self, _src: &crate::surface::Surface, _ctx: &RenderCtx) {
        noise_table();
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let table = noise_table();
        let coverage = 0.01 * self.coverage;
        let dev = self.intensity * self.intensity / 4;
        let sat = self.color_saturation * 4096 / 100;
        let s1 = self.seed;
        let s2 = self.seed ^ 0x9e37_79b9;
        let s3 = self.seed ^ 0x7f4a_7c15;
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let p = &mut dst[i..i + 4];
                i += 4;
                if hash01(x, y, s1) as f64 > coverage {
                    continue;
                }
                let (nr, ng, nb) = if self.kind == NoiseKind::Gaussian && self.size <= 1.0 {
                    // Pinta / Paint.NET grain, bit-exact
                    let h2 = hash3(x, y, s2);
                    let h3 = hash3(x, y, s3);
                    (table[(h2 & 0x3fff) as usize], table[((h2 >> 14) & 0x3fff) as usize], table[(h3 & 0x3fff) as usize])
                } else {
                    self.sample(x, y, table, [s2, s3, s1 ^ 0x5bd1_e995])
                };
                let lum = (4899 * nr + 9618 * ng + 1867 * nb) >> 14;
                let sat_ch = |n: i32| lum + (((n - lum) * sat) >> 12);
                let a = p[3] as i32;
                // Pinta adds noise to the premultiplied channels; keep the
                // result a valid premultiplied colour.
                let add = |c: u8, n: i32| (c as i32 + ((n * dev + 32768) >> 16)).clamp(0, a) as u8;
                p[0] = add(p[0], sat_ch(nr));
                p[1] = add(p[1], sat_ch(ng));
                p[2] = add(p[2], sat_ch(nb));
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Median

#[derive(Clone)]
pub struct Median {
    pub radius: i32,
    pub percentile: i32,
}

impl Default for Median {
    fn default() -> Self {
        Median { radius: 10, percentile: 50 }
    }
}

impl Effect for Median {
    fn name(&self) -> &'static str {
        "Median"
    }
    fn category(&self) -> Category {
        Category::Noise
    }
    fn icon(&self) -> &'static str {
        "effects-noise-median-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Radius", &mut self.radius, 1, 200);
        c |= slider_i(ui, "Percentile", &mut self.percentile, 0, 100);
        c
    }
    fn margin(&self) -> i32 {
        self.radius.max(1)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let rad = self.radius.max(1);
        let sb = straight_buf(src);
        let mut h = StraightHist::new();
        let pct = self.percentile;
        sweep_disk(&sb, src.rect, src.width() as i32, src.height() as i32, rad, r, &mut h, |x, y, h| {
            let min_count = h.area * pct / 100;
            let c = [
                channel_percentile(min_count, &h.r),
                channel_percentile(min_count, &h.g),
                channel_percentile(min_count, &h.b),
                channel_percentile(min_count, &h.a),
            ];
            let i = (((y - r.y) * r.w + (x - r.x)) * 4) as usize;
            dst[i..i + 4].copy_from_slice(&to_premul(c));
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Reduce Noise

#[derive(Clone)]
pub struct ReduceNoise {
    pub radius: i32,
    pub strength: f64,
}

impl Default for ReduceNoise {
    fn default() -> Self {
        ReduceNoise { radius: 6, strength: 0.4 }
    }
}

/// Fraction (0..255) of the window darker than `v`.
#[inline]
fn rank(h: &[i32; 256], area: i32, v: u8) -> u8 {
    let below: i32 = h[..v as usize].iter().sum();
    (below * 255 / area) as u8
}

impl Effect for ReduceNoise {
    fn name(&self) -> &'static str {
        "Reduce Noise"
    }
    fn category(&self) -> Category {
        Category::Noise
    }
    fn icon(&self) -> &'static str {
        "effects-noise-reducenoise-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Radius", &mut self.radius, 1, 200);
        ui.label("Strength");
        c |= ui.add(egui::Slider::new(&mut self.strength, 0.0..=1.0).step_by(0.01).fixed_decimals(2)).changed();
        c
    }
    fn margin(&self) -> i32 {
        self.radius.max(1)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let rad = self.radius.max(1);
        let strength = -0.2 * self.strength;
        let sb = straight_buf(src);
        let mut h = StraightHist::new();
        sweep_disk(&sb, src.rect, src.width() as i32, src.height() as i32, rad, r, &mut h, |x, y, h| {
            let i = (((y - r.y) * r.w + (x - r.x)) * 4) as usize;
            let c = [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]];
            let s = to_straight(c);
            let norm = to_premul([rank(&h.r, h.area, s[0]), rank(&h.g, h.area, s[1]), rank(&h.b, h.area, s[2]), s[3]]);
            let lum = (0.114 * c[2] as f64 + 0.587 * c[1] as f64 + 0.299 * c[0] as f64) / 255.0;
            let t = strength * (1.0 - 0.75 * lum);
            for k in 0..4 {
                let v = c[k] as f64 + t * (norm[k] as f64 - c[k] as f64);
                dst[i + k] = v.clamp(0.0, 255.0) as u8;
            }
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::blurs::testutil::*;
    use super::*;
    use crate::selection::Selection;
    use crate::surface::Surface;

    #[test]
    fn all_effects_tile_independent() {
        for e in all() {
            check(e);
        }
        check(Box::new(AddNoise { intensity: 100, color_saturation: 400, coverage: 37.5, seed: 1234, ..Default::default() }));
        for kind in NoiseKind::ALL {
            check(Box::new(AddNoise { kind, size: 23.0, octaves: 3, ..Default::default() }));
        }
        check(Box::new(Median { radius: 3, percentile: 90 }));
        check(Box::new(Median { radius: 25, percentile: 0 }));
        check(Box::new(ReduceNoise { radius: 15, strength: 1.0 }));
    }

    #[test]
    fn identity_settings() {
        assert_identity(Box::new(AddNoise { intensity: 0, ..Default::default() }));
        assert_identity(Box::new(AddNoise { coverage: 0.0, ..Default::default() }));
        assert_identity(Box::new(ReduceNoise { strength: 0.0, ..Default::default() }));
    }

    #[test]
    fn uniform_image_is_fixed_point() {
        let s = Surface::filled(300, 280, [120, 30, 200, 255]);
        assert!(run(&mut Median { radius: 4, percentile: 0 }, &s, &Selection::default()) == s.to_rgba());
        // Pinta's percentile search stops one bucket past the match, so a
        // uniform image brightens by one level at percentiles above 0.
        let out = run(&mut Median::default(), &s, &Selection::default());
        assert!(out.chunks_exact(4).all(|p| p == [121, 31, 201, 255]));
    }

    #[test]
    fn noise_table_is_symmetric_gaussian() {
        let t = noise_table();
        assert_eq!(t.len(), TABLE_SIZE);
        let mean: f64 = t.iter().map(|&v| v as f64).sum::<f64>() / TABLE_SIZE as f64;
        assert!(mean.abs() < 200.0, "mean {mean}");
        assert!(t[0] < 0 && t[TABLE_SIZE - 2] > 0);
    }

    #[test]
    fn noise_changes_pixels_and_seed_matters() {
        let s = test_image(200, 150);
        let a = run(&mut AddNoise::default(), &s, &Selection::default());
        let b = run(&mut AddNoise { seed: 99, ..Default::default() }, &s, &Selection::default());
        assert!(a != s.to_rgba());
        assert!(a != b);
    }

    /// Median computed by brute force over the clipped disk.
    #[test]
    fn median_matches_brute_force() {
        let s = test_image(120, 90);
        let (rad, pct) = (4, 50);
        let got = run(&mut Median { radius: rad, percentile: pct }, &s, &Selection::default());
        let cutoff = ((rad * 2 + 1) * (rad * 2 + 1) + 2) / 4;
        for y in 0..90 {
            for x in 0..120 {
                let mut h = StraightHist::new();
                for v in -rad..=rad {
                    for u in -rad..=rad {
                        let (sx, sy) = (x + u, y + v);
                        if u * u + v * v <= cutoff && sx >= 0 && sy >= 0 && sx < 120 && sy < 90 {
                            h.add(&to_straight(s.get_pixel(sx, sy)));
                        }
                    }
                }
                let m = h.area * pct / 100;
                let want = to_premul([
                    channel_percentile(m, &h.r),
                    channel_percentile(m, &h.g),
                    channel_percentile(m, &h.b),
                    channel_percentile(m, &h.a),
                ]);
                let i = ((y * 120 + x) * 4) as usize;
                assert_eq!(&got[i..i + 4], &want, "at {x},{y}");
            }
        }
    }

    #[test]
    #[ignore]
    fn timing_4000() {
        time_all(all());
    }
}
