//! Distort menu: Bulge, Dents, Frosted Glass, Pixelate, Polar Inversion,
//! Tile Reflection and Twist, ported from Pinta (originally Paint.NET 3.36).
//!
//! Sampling follows Pinta's fixed-point bilinear filters and edge behaviours so
//! results match Pinta's output; everything is keyed by absolute image
//! coordinates so tiles render identically regardless of how they are split.

use super::util;
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;
use std::f64::consts::PI;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![
        Box::new(Bulge::default()),
        Box::new(Dents::default()),
        Box::new(FrostedGlass::default()),
        Box::new(Pixelate::default()),
        Box::new(PolarInversion::default()),
        Box::new(TileReflection::default()),
        Box::new(Twist::default()),
    ]
}

/// Premultiplied RGBA pixel.
type Px = [u8; 4];
const CLEAR: Px = [0; 4];

// ---------------------------------------------------------------------------
// Shared pixel helpers
// ---------------------------------------------------------------------------

/// Straight -> premultiplied with truncation, like Pinta's `ToPremultipliedAlpha`.
#[inline]
fn premul_trunc(c: [u8; 4]) -> Px {
    let a = c[3] as u32;
    [(c[0] as u32 * a / 255) as u8, (c[1] as u32 * a / 255) as u8, (c[2] as u32 * a / 255) as u8, c[3]]
}

/// Equal-weight average of premultiplied colours (integer division, as Pinta's `Blender`).
#[derive(Default)]
struct Blender {
    sum: [u32; 4],
    n: u32,
}

impl Blender {
    #[inline]
    fn add(&mut self, p: Px) {
        for (s, v) in self.sum.iter_mut().zip(p) {
            *s += v as u32;
        }
        self.n += 1;
    }
    #[inline]
    fn blend(&self) -> Px {
        if self.n == 0 {
            return CLEAR;
        }
        [
            (self.sum[0] / self.n) as u8,
            (self.sum[1] / self.n) as u8,
            (self.sum[2] / self.n) as u8,
            (self.sum[3] / self.n) as u8,
        ]
    }
}

/// Four colours weighted by 16.16 fixed-point weights summing to 65536.
#[inline]
fn blend4(c: [Px; 4], w: [u32; 4]) -> Px {
    let mut out = [0u8; 4];
    for (ch, o) in out.iter_mut().enumerate() {
        let v = c[0][ch] as u32 * w[0] + c[1][ch] as u32 * w[1] + c[2][ch] as u32 * w[2] + c[3][ch] as u32 * w[3] + 32768;
        *o = (v >> 16) as u8;
    }
    out
}

#[inline]
fn bilerp(src: &Src, l: i32, r: i32, t: i32, b: i32, fx: u32, fy: u32) -> Px {
    let (ifx, ify) = (256 - fx, 256 - fy);
    blend4([src.get(l, t), src.get(r, t), src.get(l, b), src.get(r, b)], [ifx * ify, fx * ify, ifx * fy, fx * fy])
}

/// Bilinear sample with coordinates clamped to the image (`GetBilinearSampleClamped`).
/// Also used for on-surface samples, where it is identical to the unclamped variant.
#[inline]
fn sample_clamped(src: &Src, x: f32, y: f32) -> Px {
    if !x.is_finite() || !y.is_finite() {
        return CLEAR;
    }
    let w = src.width() as i32;
    let h = src.height() as i32;
    let u = x.clamp(0.0, (w - 1) as f32);
    let v = y.clamp(0.0, (h - 1) as f32);
    let iu = u.floor() as i32;
    let iv = v.floor() as i32;
    let fx = (256.0 * (u - iu as f32)) as u32;
    let fy = (256.0 * (v - iv as f32)) as u32;
    let r = if iu == w - 1 { iu } else { iu + 1 };
    let b = if iv == h - 1 { iv } else { iv + 1 };
    bilerp(src, iu, r, iv, b, fx, fy)
}

/// Bilinear sample that tiles the image (`GetBilinearSampleWrapped`).
#[inline]
fn sample_wrapped(src: &Src, x: f32, y: f32) -> Px {
    if !x.is_finite() || !y.is_finite() {
        return CLEAR;
    }
    let w = src.width() as i32;
    let h = src.height() as i32;
    let iu = x.floor() as i32;
    let iv = y.floor() as i32;
    let fx = (256.0 * (x - iu as f32)).clamp(0.0, 255.0) as u32;
    let fy = (256.0 * (y - iv as f32)).clamp(0.0, 255.0) as u32;
    let wrap = |i: i32, n: i32| {
        if i < 0 {
            n - 1 + (i + 1) % n
        } else if i > n - 1 {
            i % n
        } else {
            i
        }
    };
    let sx = wrap(iu, w);
    let sy = wrap(iv, h);
    let r = if sx == w - 1 { 0 } else { sx + 1 };
    let b = if sy == h - 1 { 0 } else { sy + 1 };
    bilerp(src, sx, r, sy, b, fx, fy)
}

/// Mirror a coordinate back into [0, max] (Pinta's `ReflectCoord`). Steps one
/// image size at a time like Pinta (so f32 rounding matches), but jumps
/// straight there for far-away coordinates so it cannot loop for long.
#[inline]
fn reflect_coord(value: f32, max: i32) -> f32 {
    let m = max as f32;
    let mut v = value;
    let mut flip = false;
    if v.abs() > 64.0 * m {
        let n = (v / m).floor() - 1.0;
        v -= n * m;
        flip = (n as i64) & 1 == 1;
    }
    while v < 0.0 {
        v += m;
        flip = !flip;
    }
    while v > m {
        v -= m;
        flip = !flip;
    }
    if flip { m - v } else { v }
}

#[inline]
fn sample_reflected(src: &Src, x: f32, y: f32) -> Px {
    if !x.is_finite() || !y.is_finite() {
        return CLEAR;
    }
    sample_clamped(src, reflect_coord(x, src.width() as i32), reflect_coord(y, src.height() as i32))
}

/// What a distortion samples when it reaches outside the image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeBehavior {
    Clamp,
    Wrap,
    Reflect,
    Primary,
    Secondary,
    Transparent,
    Original,
}

impl EdgeBehavior {
    const ALL: [EdgeBehavior; 7] = [
        EdgeBehavior::Clamp,
        EdgeBehavior::Wrap,
        EdgeBehavior::Reflect,
        EdgeBehavior::Primary,
        EdgeBehavior::Secondary,
        EdgeBehavior::Transparent,
        EdgeBehavior::Original,
    ];
    fn name(self) -> &'static str {
        match self {
            EdgeBehavior::Clamp => "Clamp",
            EdgeBehavior::Wrap => "Wrap",
            EdgeBehavior::Reflect => "Reflect",
            EdgeBehavior::Primary => "Primary",
            EdgeBehavior::Secondary => "Secondary",
            EdgeBehavior::Transparent => "Transparent",
            EdgeBehavior::Original => "Original",
        }
    }
}

/// Palette colours (premultiplied) needed by the Primary/Secondary edge modes.
#[derive(Clone, Copy)]
struct Edge {
    mode: EdgeBehavior,
    primary: Px,
    secondary: Px,
}

impl Edge {
    fn new(mode: EdgeBehavior, ctx: &RenderCtx) -> Self {
        Edge { mode, primary: premul_trunc(ctx.primary), secondary: premul_trunc(ctx.secondary) }
    }
    /// Sample for a location that is off the image.
    #[inline]
    fn sample(&self, src: &Src, x: f32, y: f32, original: Px) -> Px {
        match self.mode {
            EdgeBehavior::Clamp => sample_clamped(src, x, y),
            EdgeBehavior::Wrap => sample_wrapped(src, x, y),
            EdgeBehavior::Reflect => sample_reflected(src, x, y),
            EdgeBehavior::Primary => self.primary,
            EdgeBehavior::Secondary => self.secondary,
            EdgeBehavior::Transparent => CLEAR,
            EdgeBehavior::Original => original,
        }
    }
}

#[inline]
fn on_surface(src: &Src, u: f32, v: f32) -> bool {
    u >= 0.0 && u <= (src.width() as i32 - 1) as f32 && v >= 0.0 && v <= (src.height() as i32 - 1) as f32
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, label: &str, v: &mut T, items: &[(T, &str)]) -> bool {
    ui.label(label);
    let current = items.iter().find(|(t, _)| *t == *v).map_or("", |(_, n)| *n);
    let mut changed = false;
    egui::ComboBox::from_id_salt(label).selected_text(current).show_ui(ui, |ui| {
        for &(t, n) in items {
            changed |= ui.selectable_value(v, t, n).changed();
        }
    });
    changed
}

fn edge_combo(ui: &mut egui::Ui, v: &mut EdgeBehavior) -> bool {
    let items: Vec<(EdgeBehavior, &str)> = EdgeBehavior::ALL.iter().map(|&e| (e, e.name())).collect();
    combo(ui, "Edge Behavior", v, &items)
}

/// Seed row limited to a byte, like Pinta's 0..255 noise seeds.
fn byte_seed(ui: &mut egui::Ui, label: &str, v: &mut u32) -> bool {
    ui.label(label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::DragValue::new(v).range(0..=255)).changed();
        if ui.button("Reseed").clicked() {
            *v = util::hash3(*v as i32, 0, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1)) % 256;
            changed = true;
        }
    });
    changed
}

/// Run `f(x, y, original) -> new` over every pixel of `dst`.
#[inline]
fn for_each_px(dst: &mut [u8], r: IRect, mut f: impl FnMut(i32, i32, Px) -> Px) {
    for y in 0..r.h {
        for x in 0..r.w {
            let i = ((y * r.w + x) * 4) as usize;
            let orig = [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]];
            let out = f(r.x + x, r.y + y, orig);
            dst[i..i + 4].copy_from_slice(&out);
        }
    }
}

// ---------------------------------------------------------------------------
// Warp base (Dents, Polar Inversion)
// ---------------------------------------------------------------------------

/// Rotated-grid supersampling offsets (`Utility.GetRgssOffsets`).
fn rgss_offsets(quality: i32) -> Vec<(f64, f64)> {
    let n = (quality * quality).max(1);
    if n == 1 {
        return vec![(0.0, 0.0)];
    }
    (0..n)
        .map(|i| {
            let y = (i as f64 + 1.0) / (n as f64 + 1.0);
            let bx = y * quality as f64;
            let x = bx - bx.trunc();
            (x - 0.5, y - 0.5)
        })
        .collect()
}

/// Shared settings for Pinta's inverse-transform warps: centre on the selection,
/// supersample with RGSS offsets and apply the edge behaviour off-image.
struct Warp {
    cx: f64,
    cy: f64,
    aa: Vec<(f64, f64)>,
    edge: Edge,
    default_radius: f64,
    default_radius2: f64,
}

impl Warp {
    fn new(quality: i32, offset: [f64; 2], edge: EdgeBehavior, ctx: &RenderCtx) -> Self {
        let b = ctx.sel_bounds;
        let default_radius = b.w.min(b.h) as f64 * 0.5;
        Warp {
            cx: b.x as f64 + b.w as f64 * (1.0 + offset[0]) * 0.5,
            cy: b.y as f64 + b.h as f64 * (1.0 + offset[1]) * 0.5,
            aa: rgss_offsets(quality),
            edge: Edge::new(edge, ctx),
            default_radius,
            default_radius2: default_radius * default_radius,
        }
    }

    #[inline]
    fn pixel(&self, src: &Src, x: i32, y: i32, original: Px, inverse: impl Fn(f64, f64) -> (f64, f64)) -> Px {
        let rx = x as f64 - self.cx;
        let ry = y as f64 - self.cy;
        let mut acc = Blender::default();
        for &(ax, ay) in &self.aa {
            let (tx, ty) = inverse(rx + ax, ry - ay);
            let u = (tx + self.cx) as f32;
            let v = (ty + self.cy) as f32;
            let s = if on_surface(src, u, v) { sample_clamped(src, u, v) } else { self.edge.sample(src, u, v, original) };
            acc.add(s);
        }
        acc.blend()
    }

    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, inverse: impl Fn(f64, f64) -> (f64, f64)) {
        for_each_px(dst, r, |x, y, orig| self.pixel(src, x, y, orig, &inverse));
    }
}

// ---------------------------------------------------------------------------
// Perlin noise (Pinta.Core PerlinNoise, 2-D variant of Ken Perlin's reference)
// ---------------------------------------------------------------------------

const PERM_BASE: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69, 142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0,
    26, 197, 62, 94, 252, 219, 203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175, 74, 165, 71, 134, 139, 48, 27, 166,
    77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230, 220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76, 132,
    187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173, 186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126,
    255, 82, 85, 212, 207, 206, 59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163, 70, 221, 153, 101, 155, 167, 43,
    172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232, 178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204, 176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93,
    222, 114, 67, 29, 24, 72, 243, 141, 128, 195, 78, 66, 215, 61, 156, 180,
];

#[inline]
fn perm(i: usize) -> usize {
    PERM_BASE[i & 255] as usize
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn grad(hash: usize, x: f64, y: f64) -> f64 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        0.0
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

/// Single-octave noise at grid cell (gx, gy) with fractional offset (ox, oy).
#[inline]
fn perlin_cell(gx: usize, gy: usize, ox: f64, oy: f64, seed: usize) -> f64 {
    let fx = fade(ox);
    let fy = fade(oy);
    let a = perm(gx + seed) + gy;
    let aa = perm(a);
    let ab = perm(a + 1);
    let b = perm(gx + 1 + seed) + gy;
    let ba = perm(b);
    let bb = perm(b + 1);
    let e1 = lerp(grad(perm(aa), ox, oy), grad(perm(ba), ox - 1.0, oy), fx);
    let e2 = lerp(grad(perm(ab), ox, oy - 1.0), grad(perm(bb), ox - 1.0, oy - 1.0), fx);
    lerp(e1, e2, fy)
}

#[inline]
fn perlin_noise(x: f64, y: f64, seed: usize) -> f64 {
    let fx = x.floor();
    let fy = y.floor();
    perlin_cell((fx as i64 & 255) as usize, (fy as i64 & 255) as usize, x - fx, y - fy, seed)
}

/// Fractal (multi-octave) Perlin noise.
struct Fbm {
    rot: [f64; 4],
}

impl Fbm {
    fn new() -> Self {
        let r = 137.2f64.to_radians();
        Fbm { rot: [r.cos(), -r.sin(), r.sin(), r.cos()] }
    }

    #[inline]
    fn compute(&self, mut px: f64, mut py: f64, detail: f64, roughness: f64, seed: usize) -> f64 {
        let mut total = 0.0;
        let mut freq = 1.0;
        let mut amp = 1.0;
        let mut partial = detail;
        let octaves = detail.ceil() as i32;
        for _ in 0..octaves {
            let rx = px * self.rot[0] + py * self.rot[1];
            let ry = px * self.rot[2] + py * self.rot[3];
            let n = amp * perlin_noise(rx * freq, ry * freq, seed);
            total += if partial < 1.0 { n * partial } else { n };
            amp *= roughness;
            if amp < 0.001 {
                break;
            }
            freq += freq;
            partial -= 1.0;
            px = rx + 499.0;
            py = ry + 506.0;
        }
        total
    }
}

// ---------------------------------------------------------------------------
// Bulge
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Bulge {
    pub amount: i32,
    pub offset: [f64; 2],
    pub radius_percentage: i32,
}

impl Default for Bulge {
    fn default() -> Self {
        Bulge { amount: 45, offset: [0.0, 0.0], radius_percentage: 100 }
    }
}

impl Effect for Bulge {
    fn name(&self) -> &'static str {
        "Bulge"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-bulge-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Amount", &mut self.amount, -200, 100);
        c |= util::offset(ui, "Offset", &mut self.offset);
        c |= util::slider_i(ui, "Radius Percentage", &mut self.radius_percentage, 10, 100);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        if self.amount == 0 {
            return;
        }
        // Pinta centres Bulge on the whole image, not the selection.
        let hw = src.width() as f32 / 2.0;
        let hh = src.height() as f32 / 2.0;
        let cx = hw + self.offset[0] as f32 * hw;
        let cy = hh + self.offset[1] as f32 * hh;
        let max_radius = hw.min(hh) * self.radius_percentage as f32 / 100.0;
        let amount = self.amount as f32 / 100.0;
        for_each_px(dst, r, |x, y, orig| {
            let tx = x as f32 - cx;
            let ty = y as f32 - cy;
            let d = (tx * tx + ty * ty).sqrt();
            let s1 = 1.0 - d / max_radius;
            if s1 <= 0.0 {
                return orig;
            }
            let s2 = 1.0 - amount * s1 * s1;
            sample_clamped(src, tx * s2 + cx, ty * s2 + cy)
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Dents
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Dents {
    pub scale: f64,
    pub refraction: f64,
    pub roughness: f64,
    pub tension: f64,
    pub seed: u32,
    pub quality: i32,
    pub center_offset: [f64; 2],
    pub edge: EdgeBehavior,
}

impl Default for Dents {
    fn default() -> Self {
        Dents {
            scale: 25.0,
            refraction: 50.0,
            roughness: 10.0,
            tension: 10.0,
            seed: 0,
            quality: 2,
            center_offset: [0.0, 0.0],
            edge: EdgeBehavior::Wrap,
        }
    }
}

impl Effect for Dents {
    fn name(&self) -> &'static str {
        "Dents"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-dents-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_f(ui, "Scale", &mut self.scale, 1.0, 200.0);
        c |= util::slider_f(ui, "Refraction", &mut self.refraction, 0.0, 200.0);
        c |= util::slider_f(ui, "Roughness", &mut self.roughness, 0.0, 100.0);
        c |= util::slider_f(ui, "Turbulence", &mut self.tension, 0.0, 100.0);
        c |= byte_seed(ui, "Random Noise Seed", &mut self.seed);
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 5);
        c |= util::offset(ui, "Center Offset", &mut self.center_offset);
        c |= edge_combo(ui, &mut self.edge);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let warp = Warp::new(self.quality, self.center_offset, self.edge, ctx);
        let scale_r = 400.0 / warp.default_radius / self.scale;
        let detail = 1.0 + self.roughness / 10.0;
        // keep noise frequencies under the Nyquist limit
        let max_detail = (scale_r.ln() / 0.5f64.ln()).floor();
        let detail = if detail > max_detail && max_detail >= 1.0 { max_detail } else { detail };
        let roughness = self.roughness / 100.0;
        let refraction = self.refraction / 100.0 / scale_r;
        let theta = 2.0 * PI * self.tension / 10.0;
        let seed = self.seed.min(255) as usize;
        let fbm = Fbm::new();
        warp.render(src, dst, r, |x, y| {
            let noise = fbm.compute(x * scale_r, y * scale_r, detail, roughness, seed);
            let bump = theta * noise;
            (x + refraction * (-bump).sin(), y + refraction * bump.cos())
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Frosted Glass
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FrostedGlass {
    pub max_scatter_radius: i32,
    pub min_scatter_radius: i32,
    pub diffusion: f64,
    pub smoothness: i32,
    pub seed: u32,
}

impl Default for FrostedGlass {
    fn default() -> Self {
        FrostedGlass { max_scatter_radius: 3, min_scatter_radius: 0, diffusion: 0.5, smoothness: 2, seed: 0 }
    }
}

/// Schlick's bias: 0.5 is linear, higher values push towards 1.
#[inline]
fn schlick_bias(x: f64, bias: f64) -> f64 {
    x / ((1.0 / bias - 2.0) * (1.0 - x) + 1.0)
}

impl Effect for FrostedGlass {
    fn name(&self) -> &'static str {
        "Frosted Glass"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-frostedglass-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Maximum Scatter Radius", &mut self.max_scatter_radius, 0, 200);
        c |= util::slider_i(ui, "Minimum Scatter Radius", &mut self.min_scatter_radius, 0, 200);
        c |= util::slider_f(ui, "Diffusion", &mut self.diffusion, 0.0, 1.0);
        c |= util::slider_i(ui, "Smoothness", &mut self.smoothness, 1, 16);
        ui.label("Random Noise Seed");
        c |= util::seed(ui, &mut self.seed);
        c
    }
    fn margin(&self) -> i32 {
        self.max_scatter_radius.max(self.min_scatter_radius) + 2
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let min_r = self.min_scatter_radius.min(self.max_scatter_radius) as f64;
        let range = (self.max_scatter_radius - self.min_scatter_radius).max(0) as f64;
        let n = self.smoothness.max(1) as u32;
        let base = self.seed.wrapping_mul(64);
        for_each_px(dst, r, |x, y, _| {
            let mut acc = Blender::default();
            for s in 0..n {
                // per-pixel hash instead of a sequential RNG: independent of tiling
                let angle = util::hash01(x, y, base.wrapping_add(2 * s)) as f64 * PI * 2.0;
                let t = util::hash01(x, y, base.wrapping_add(2 * s + 1)) as f64;
                let dist = min_r + schlick_bias(t, self.diffusion) * range;
                let (sin, cos) = angle.sin_cos();
                acc.add(sample_reflected(src, (x as f64 + dist * cos) as f32, (y as f64 + dist * sin) as f32));
            }
            acc.blend()
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Pixelate
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Pixelate {
    pub cell_size: i32,
}

impl Default for Pixelate {
    fn default() -> Self {
        Pixelate { cell_size: 2 }
    }
}

impl Effect for Pixelate {
    fn name(&self) -> &'static str {
        "Pixelate"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-pixelate-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        util::slider_i(ui, "Cell Size", &mut self.cell_size, 1, 100)
    }
    fn margin(&self) -> i32 {
        self.cell_size
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let cs = self.cell_size.max(1);
        let w = src.width() as i32;
        let h = src.height() as i32;
        let mut y = r.y;
        while y < r.bottom() {
            let cy0 = y - y % cs;
            let y_end = (cy0 + cs).min(r.bottom());
            let bottom = (cy0 + cs).min(h) - 1;
            let mut x = r.x;
            while x < r.right() {
                let cx0 = x - x % cs;
                let x_end = (cx0 + cs).min(r.right());
                let right = (cx0 + cs).min(w) - 1;
                // the cell colour is the average of the cell's four corner pixels
                let c = blend4([src.get(cx0, cy0), src.get(right, cy0), src.get(cx0, bottom), src.get(right, bottom)], [16384; 4]);
                for yy in y..y_end {
                    let row = ((yy - r.y) * r.w) as usize;
                    for xx in x..x_end {
                        let i = (row + (xx - r.x) as usize) * 4;
                        dst[i..i + 4].copy_from_slice(&c);
                    }
                }
                x = x_end;
            }
            y = y_end;
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Polar Inversion
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PolarInversion {
    pub amount: f64,
    pub quality: i32,
    pub center_offset: [f64; 2],
    pub edge: EdgeBehavior,
}

impl Default for PolarInversion {
    fn default() -> Self {
        PolarInversion { amount: 0.0, quality: 2, center_offset: [0.0, 0.0], edge: EdgeBehavior::Reflect }
    }
}

impl Effect for PolarInversion {
    fn name(&self) -> &'static str {
        "Polar Inversion"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-polarinversion-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        ui.label("Amount");
        let mut c = ui.add(egui::Slider::new(&mut self.amount, -4.0..=4.0).step_by(0.1)).changed();
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 5);
        c |= util::offset(ui, "Center Offset", &mut self.center_offset);
        c |= edge_combo(ui, &mut self.edge);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let warp = Warp::new(self.quality, self.center_offset, self.edge, ctx);
        let amount = self.amount;
        let r2 = warp.default_radius2;
        warp.render(src, dst, r, |x, y| {
            // NaN at the exact centre, like Pinta (the sample then falls back to the edge mode)
            let f = lerp(1.0, r2 / (x * x + y * y), amount);
            (x * f, y * f)
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Tile Reflection
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TileType {
    SharpEdges,
    Curved,
}

#[derive(Clone)]
pub struct TileReflection {
    pub rotation: f64,
    pub tile_size: i32,
    pub intensity: i32,
    pub wave: TileType,
    pub edge: EdgeBehavior,
}

impl Default for TileReflection {
    fn default() -> Self {
        TileReflection { rotation: 30.0, tile_size: 40, intensity: 8, wave: TileType::SharpEdges, edge: EdgeBehavior::Wrap }
    }
}

impl Effect for TileReflection {
    fn name(&self) -> &'static str {
        "Tile Reflection"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-tile-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        ui.label("Rotation");
        let mut c = ui.add(egui::Slider::new(&mut self.rotation, -45.0..=45.0).suffix("°")).changed();
        c |= util::slider_i(ui, "Tile Size", &mut self.tile_size, 2, 200);
        c |= util::slider_i(ui, "Intensity", &mut self.intensity, -20, 20);
        c |= combo(ui, "Tile Type", &mut self.wave, &[(TileType::SharpEdges, "Sharp Edges"), (TileType::Curved, "Curved")]);
        c |= edge_combo(ui, &mut self.edge);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        const AA_LEVEL: i32 = 4;
        const AA_SAMPLES: i32 = AA_LEVEL * AA_LEVEL + 1;
        let w = src.width() as i32;
        let hw = w as f32 / 2.0;
        let hh = src.height() as f32 / 2.0;
        let theta = self.rotation.to_radians();
        let sin = (-theta).sin() as f32;
        let cos = (-theta).cos() as f32;
        let scale = std::f32::consts::PI / self.tile_size.max(1) as f32;
        let fi = self.intensity as f32;
        let intensity = fi * fi / 10.0 * (self.intensity.signum() as f32);
        let edge = Edge::new(self.edge, ctx);
        let wave = self.wave;
        let wave_fn = |n: f32| -> f32 {
            match wave {
                TileType::SharpEdges => (n as f64).tan() as f32,
                TileType::Curved => (n as f64).sin() as f32,
            }
        };
        // rotated RGSS offsets
        let mut aa = [(0f32, 0f32); AA_SAMPLES as usize];
        for (i, p) in aa.iter_mut().enumerate() {
            let mut x = i as f64 * AA_LEVEL as f64 / AA_SAMPLES as f64;
            let y = i as f64 / AA_SAMPLES as f64;
            x -= x.trunc();
            *p = ((cos as f64 * x + sin as f64 * y) as f32, (cos as f64 * y - sin as f64 * x) as f32);
        }
        for_each_px(dst, r, |x, y, orig| {
            let i = x as f32 - hw;
            let j = y as f32 - hh;
            let mut acc = Blender::default();
            for &(ax, ay) in &aa {
                let u = i + ax;
                let v = j - ay;
                let s = cos * u + sin * v;
                let t = -sin * u + cos * v;
                let s2 = s + intensity * wave_fn(s * scale);
                let t2 = t + intensity * wave_fn(t * scale);
                let fu = cos * s2 - sin * t2;
                let fv = sin * s2 + cos * t2;
                let px = hw + fu;
                let py = hh + fv;
                let c = if on_surface(src, px, py) { src.get(px as i32, py as i32) } else { edge.sample(src, px, py, orig) };
                acc.add(c);
            }
            acc.blend()
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Twist
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Twist {
    pub amount: i32,
    pub radius_percentage: i32,
    pub antialias: i32,
    pub center_offset: [f64; 2],
    pub edge: EdgeBehavior,
}

impl Default for Twist {
    fn default() -> Self {
        Twist { amount: 30, radius_percentage: 100, antialias: 2, center_offset: [0.0, 0.0], edge: EdgeBehavior::Clamp }
    }
}

impl Effect for Twist {
    fn name(&self) -> &'static str {
        "Twist"
    }
    fn category(&self) -> Category {
        Category::Distort
    }
    fn icon(&self) -> &'static str {
        "effects-distort-twist-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Amount", &mut self.amount, -100, 100);
        c |= util::slider_i(ui, "Radius Percentage", &mut self.radius_percentage, 0, 100);
        c |= util::slider_i(ui, "Antialias", &mut self.antialias, 0, 5);
        c |= util::offset(ui, "Center Offset", &mut self.center_offset);
        c |= edge_combo(ui, &mut self.edge);
        c
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let b = ctx.sel_bounds;
        let hw = b.w as f64 / 2.0;
        let hh = b.h as f64 / 2.0;
        let max_radius = hw.min(hh) * (self.radius_percentage as f64 / 100.0);
        let max_r2 = max_radius * max_radius;
        let threshold2 = (max_radius + 1.0) * (max_radius + 1.0);
        let cx = hw + b.x as f64 + self.center_offset[0] * hw;
        let cy = hh + b.y as f64 + self.center_offset[1] * hh;
        let pre = -(self.amount as f64);
        let twist = pre * pre * pre.signum() / 100.0;
        let level = self.antialias.max(0);
        let n = level * level + 1;
        let aa: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let px = i as f64 * level as f64 / n as f64;
                (px - px.trunc(), i as f64 / n as f64)
            })
            .collect();
        let edge = Edge::new(self.edge, ctx);
        let w = src.width() as i32;
        let h = src.height() as i32;
        for_each_px(dst, r, |x, y, orig| {
            let fx = x as f64 - cx;
            let fy = y as f64 - cy;
            if fx * fx + fy * fy > threshold2 {
                return orig;
            }
            let mut acc = Blender::default();
            for &(ox, oy) in &aa {
                let lx = fx + ox;
                let ly = fy + oy;
                let d2 = lx * lx + ly * ly;
                if d2 > max_r2 {
                    acc.add(orig);
                    continue;
                }
                let rf = 1.0 - d2.sqrt() / max_radius;
                let (sin, cos) = (rf * rf * rf * twist).sin_cos();
                let rx = lx * cos - ly * sin;
                let ry = lx * sin + ly * cos;
                let sx = cx + rx;
                let sy = cy + ry;
                // truncation toward zero, like Pinta's PointD.ToInt()
                let (ix, iy) = (sx as i32, sy as i32);
                let c = if ix >= 0 && ix < w && iy >= 0 && iy < h {
                    src.get(ix, iy)
                } else {
                    edge.sample(src, sx as f32, sy as f32, orig)
                };
                acc.add(c);
            }
            acc.blend()
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Progress, apply};
    use crate::selection::{Mask, Selection};
    use crate::surface::{Surface, premul};

    fn test_image(w: u32, h: u32) -> Surface {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = if x < w / 6 {
                    0
                } else if y < h / 5 {
                    (x * 255 / w.max(1)) as u8
                } else {
                    255
                };
                let c = [(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, if (x / 37 + y / 23) % 2 == 0 { 230 } else { 20 }, a];
                data[i..i + 4].copy_from_slice(&premul(c));
            }
        }
        Surface::from_rgba(w, h, &data)
    }

    fn ctx_for(w: u32, h: u32) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: IRect::new(0, 0, w as i32, h as i32), primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255] }
    }

    /// Render the whole image in a single render() call.
    fn render_whole(effect: &mut dyn Effect, s: &Surface, ctx: &RenderCtx) -> Vec<u8> {
        effect.prepare(s, ctx);
        let r = s.bounds();
        let buf = s.read_rect(r);
        let src = Src { surface: s, buf: &buf, rect: r };
        let mut dst = buf.clone();
        effect.render(&src, &mut dst, r, ctx);
        dst
    }

    fn run(effect: &mut dyn Effect, s: &Surface, ctx: &RenderCtx) -> Vec<u8> {
        apply(effect, s, &Selection::default(), ctx, &Progress::new()).unwrap().to_rgba()
    }

    fn variants() -> Vec<Box<dyn Effect>> {
        let mut v = all();
        v.push(Box::new(Dents { edge: EdgeBehavior::Reflect, quality: 3, center_offset: [0.3, -0.2], ..Dents::default() }));
        v.push(Box::new(PolarInversion { amount: 1.5, edge: EdgeBehavior::Wrap, ..PolarInversion::default() }));
        v.push(Box::new(PolarInversion { amount: -2.0, edge: EdgeBehavior::Clamp, ..PolarInversion::default() }));
        v.push(Box::new(Twist { amount: -80, antialias: 4, center_offset: [-0.5, 0.4], edge: EdgeBehavior::Wrap, ..Twist::default() }));
        v.push(Box::new(Twist { amount: 60, center_offset: [0.7, 0.7], edge: EdgeBehavior::Reflect, ..Twist::default() }));
        v.push(Box::new(TileReflection { wave: TileType::Curved, edge: EdgeBehavior::Reflect, rotation: -20.0, ..TileReflection::default() }));
        v.push(Box::new(TileReflection { edge: EdgeBehavior::Primary, intensity: -15, ..TileReflection::default() }));
        v.push(Box::new(Pixelate { cell_size: 37 }));
        v.push(Box::new(FrostedGlass { min_scatter_radius: 5, max_scatter_radius: 20, smoothness: 7, diffusion: 0.8, seed: 9 }));
        v.push(Box::new(Bulge { amount: -150, offset: [0.4, -0.3], radius_percentage: 40 }));
        v
    }

    #[test]
    fn names_in_menu_order() {
        let names: Vec<_> = all().iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Bulge", "Dents", "Frosted Glass", "Pixelate", "Polar Inversion", "Tile Reflection", "Twist"]);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(all().iter().all(|e| e.category() == Category::Distort));
    }

    #[test]
    fn tiling_independent() {
        let s = test_image(700, 500);
        let ctx = ctx_for(700, 500);
        for mut e in variants() {
            let tiled = run(e.as_mut(), &s, &ctx);
            let whole = render_whole(e.as_mut(), &s, &ctx);
            assert!(tiled == whole, "{} differs between tiled and whole render", e.name());
        }
    }

    #[test]
    fn identity_settings() {
        let s = test_image(700, 500);
        let ctx = ctx_for(700, 500);
        let orig = s.to_rgba();
        let ids: Vec<Box<dyn Effect>> = vec![
            Box::new(Twist { amount: 0, ..Twist::default() }),
            Box::new(Bulge { amount: 0, ..Bulge::default() }),
            Box::new(Pixelate { cell_size: 1 }),
            Box::new(FrostedGlass { max_scatter_radius: 0, min_scatter_radius: 0, ..FrostedGlass::default() }),
        ];
        for mut e in ids {
            assert!(run(e.as_mut(), &s, &ctx) == orig, "{} is not identity", e.name());
        }
        // Polar inversion with amount 0 is identity except at the exact centre (NaN in Pinta too)
        let out = run(&mut PolarInversion { quality: 1, ..PolarInversion::default() }, &s, &ctx);
        let diff = out.chunks(4).zip(orig.chunks(4)).filter(|(a, b)| a != b).count();
        assert!(diff <= 1, "polar inversion identity differs in {diff} pixels");
    }

    #[test]
    fn tiny_images() {
        for (w, h) in [(1, 1), (1, 7), (5, 1), (2, 3)] {
            let s = test_image(w, h);
            let ctx = ctx_for(w, h);
            for mut e in variants() {
                let tiled = run(e.as_mut(), &s, &ctx);
                let whole = render_whole(e.as_mut(), &s, &ctx);
                assert!(tiled == whole, "{} {w}x{h}", e.name());
            }
        }
    }

    #[test]
    fn partial_selection() {
        let s = test_image(700, 500);
        let r = IRect::new(130, 90, 300, 260);
        let sel = Selection { mask: Some(Mask::from_rect(700, 500, r)) };
        let mut ctx = ctx_for(700, 500);
        ctx.sel_bounds = r;
        let orig = s.to_rgba();
        for mut e in variants() {
            let out = apply(e.as_mut(), &s, &sel, &ctx, &Progress::new()).unwrap().to_rgba();
            let whole = render_whole(e.as_mut(), &s, &ctx);
            for y in 0..500 {
                for x in 0..700 {
                    let i = ((y * 700 + x) * 4) as usize;
                    let expect = if r.contains(x, y) { &whole[i..i + 4] } else { &orig[i..i + 4] };
                    assert_eq!(&out[i..i + 4], expect, "{} at {x},{y}", e.name());
                }
            }
        }
    }

    // --- comparison against Pinta's own test images (when the upstream checkout exists) ---

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../pinta-upstream/tests/Pinta.Effects.Tests/Assets");

    fn load_png(name: &str) -> Option<Surface> {
        let img = image::open(format!("{ASSETS}/{name}")).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let mut data = img.into_raw();
        for p in data.chunks_exact_mut(4) {
            // Cairo premultiplies with rounding when Pinta loads the PNG
            p.copy_from_slice(&premul([p[0], p[1], p[2], p[3]]));
        }
        Some(Surface::from_rgba(w, h, &data))
    }

    /// Fraction of pixels whose channels differ by more than `tol` from Pinta's
    /// result. Pinta's own tests use a tolerance of 1: loading its PNGs shifts
    /// many values by one.
    fn compare(effect: &mut dyn Effect, expected: &str, tol: u8) -> Option<f64> {
        let input = load_png("input.png")?;
        let exp = load_png(expected)?.to_rgba();
        let ctx = ctx_for(input.width, input.height);
        let out = run(effect, &input, &ctx);
        let bad = out.chunks(4).zip(exp.chunks(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > tol)).count();
        Some(bad as f64 / (out.len() / 4) as f64)
    }

    #[test]
    fn matches_pinta_reference_images() {
        let cases: Vec<(Box<dyn Effect>, &str, f64)> = vec![
            (Box::new(Bulge { amount: 56, ..Bulge::default() }), "bulge1.png", 0.0),
            (Box::new(Bulge { amount: -59, offset: [-0.184, -0.304], ..Bulge::default() }), "bulge2.png", 0.0),
            (Box::new(Bulge { amount: 56, radius_percentage: 50, ..Bulge::default() }), "bulge3.png", 0.0),
            (Box::new(Dents::default()), "dents1.png", 0.0),
            (Box::new(Dents { scale: 50.0, ..Dents::default() }), "dents2.png", 0.0),
            (Box::new(Dents { roughness: 100.0, ..Dents::default() }), "dents3.png", 0.0),
            (Box::new(Dents { tension: 100.0, ..Dents::default() }), "dents4.png", 0.0),
            (Box::new(Dents { quality: 1, ..Dents::default() }), "dents5.png", 0.0),
            (Box::new(Dents { center_offset: [-1.0, -1.0], ..Dents::default() }), "dents6.png", 0.0),
            (Box::new(Dents { edge: EdgeBehavior::Clamp, ..Dents::default() }), "dents7.png", 0.0),
            (Box::new(Pixelate::default()), "pixelate1.png", 0.0),
            (Box::new(Pixelate { cell_size: 10 }), "pixelate2.png", 0.0),
            (Box::new(PolarInversion { amount: 1.0, ..PolarInversion::default() }), "polarinversion1.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, ..TileReflection::default() }), "tile1.png", 0.0),
            (Box::new(TileReflection { rotation: -90.0, tile_size: 32, intensity: 4, ..TileReflection::default() }), "tile2.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, wave: TileType::Curved, ..TileReflection::default() }), "tile3.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, tile_size: 10, wave: TileType::Curved, ..TileReflection::default() }), "tile4.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Clamp, ..TileReflection::default() }), "tile5.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Reflect, ..TileReflection::default() }), "tile6.png", 0.0002),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Primary, ..TileReflection::default() }), "tile7.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Secondary, ..TileReflection::default() }), "tile8.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Transparent, ..TileReflection::default() }), "tile9.png", 0.0),
            (Box::new(TileReflection { rotation: -30.0, edge: EdgeBehavior::Original, ..TileReflection::default() }), "tile10.png", 0.0),
            (Box::new(Twist { amount: -45, ..Twist::default() }), "twist1.png", 0.0),
            (Box::new(Twist { amount: 20, antialias: 4, ..Twist::default() }), "twist2.png", 0.0),
            (Box::new(Twist::default()), "twist3.png", 0.0),
            (Box::new(Twist { center_offset: [-0.6, -0.6], edge: EdgeBehavior::Clamp, ..Twist::default() }), "twist4.png", 0.0),
            (Box::new(Twist { center_offset: [-0.6, -0.6], edge: EdgeBehavior::Primary, ..Twist::default() }), "twist5.png", 0.0),
        ];
        let mut failures = vec![];
        for (mut e, name, allowed) in cases {
            let Some(frac) = compare(e.as_mut(), name, 1) else {
                eprintln!("skipping Pinta reference comparison: {ASSETS} not found");
                return;
            };
            eprintln!("{name}: {:.4}% pixels differ", frac * 100.0);
            if frac > allowed {
                failures.push(format!("{name}: {:.4}%", frac * 100.0));
            }
        }
        assert!(failures.is_empty(), "differs from Pinta: {failures:?}");
    }

    #[test]
    #[ignore]
    fn timing_4000() {
        let s = test_image(4000, 4000);
        let ctx = ctx_for(4000, 4000);
        for mut e in all() {
            let t = std::time::Instant::now();
            run(e.as_mut(), &s, &ctx);
            eprintln!("{:<16} {:>7.0} ms", e.name(), t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
