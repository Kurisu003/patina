//! Render menu: Cells, Clouds, Julia Fractal, Mandelbrot Fractal and Voronoi
//! Diagram, ported from Pinta (fractals and clouds originally from Paint.NET 3.36).
//!
//! Colour schemes, point sets and the seeded random generator follow Pinta.Core
//! so the same settings give the same picture as Pinta.

use super::util;
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::{IRect, Surface};
use std::f64::consts::PI;
use std::sync::Arc;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![
        Box::new(Cells::default()),
        Box::new(Clouds::default()),
        Box::new(JuliaFractal::default()),
        Box::new(MandelbrotFractal::default()),
        Box::new(VoronoiDiagram::default()),
    ]
}

/// Premultiplied RGBA pixel.
type Px = [u8; 4];
const CLEAR: Px = [0; 4];

/// Opaque colour from (r, g, b).
const fn rgb(r: u8, g: u8, b: u8) -> Px {
    [r, g, b, 255]
}
const WHITE: Px = rgb(255, 255, 255);
const BLACK: Px = rgb(0, 0, 0);

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

/// Premultiplied "normal" blend of `top` over `bottom` (Pinta's NormalBlendOp).
#[inline]
fn normal_over(bottom: Px, top: Px) -> Px {
    if top[3] == 255 {
        return top;
    }
    if top[3] == 0 {
        return bottom;
    }
    let ita = 255 - top[3] as i32;
    let iba = 255 - bottom[3] as i32;
    let mut out = [0u8; 4];
    for c in 0..3 {
        let v = iba * top[c] as i32 + ita * bottom[c] as i32 + bottom[3] as i32 * top[c] as i32;
        out[c] = ((v + 128) / 255).clamp(0, 255) as u8;
    }
    out[3] = (top[3] as i32 + (bottom[3] as i32 * ita + 128) / 255).clamp(0, 255) as u8;
    out
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

#[inline]
fn lerp_px(a: Px, b: Px, t: f64) -> Px {
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = lerp(a[c] as f64, b[c] as f64, t).clamp(0.0, 255.0) as u8;
    }
    out
}

// ---------------------------------------------------------------------------
// .NET System.Random (seeded, legacy Knuth subtractive generator)
// ---------------------------------------------------------------------------

/// Bit-exact port of .NET's seeded `System.Random`, so point sets and random
/// colour schemes come out the same as in Pinta for the same seed.
struct DotNetRandom {
    seeds: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl DotNetRandom {
    fn new(seed: i32) -> Self {
        let mut sa = [0i32; 56];
        let sub = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = 161_803_398 - sub;
        sa[55] = mj;
        let mut mk = 1i32;
        let mut ii = 0usize;
        for _ in 1..55 {
            ii += 21;
            if ii >= 55 {
                ii -= 55;
            }
            sa[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(i32::MAX);
            }
            mj = sa[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                let mut n = i + 30;
                if n >= 55 {
                    n -= 55;
                }
                sa[i] = sa[i].wrapping_sub(sa[1 + n]);
                if sa[i] < 0 {
                    sa[i] = sa[i].wrapping_add(i32::MAX);
                }
            }
        }
        DotNetRandom { seeds: sa, inext: 0, inextp: 21 }
    }

    fn internal_sample(&mut self) -> i32 {
        let mut a = self.inext + 1;
        if a >= 56 {
            a = 1;
        }
        let mut b = self.inextp + 1;
        if b >= 56 {
            b = 1;
        }
        let mut r = self.seeds[a].wrapping_sub(self.seeds[b]);
        if r == i32::MAX {
            r -= 1;
        }
        if r < 0 {
            r = r.wrapping_add(i32::MAX);
        }
        self.seeds[a] = r;
        self.inext = a;
        self.inextp = b;
        r
    }

    fn sample(&mut self) -> f64 {
        self.internal_sample() as f64 * (1.0 / i32::MAX as f64)
    }

    /// `Next(min, max)` for ranges that fit in an i32.
    fn next_range(&mut self, min: i32, max: i32) -> i32 {
        let range = max as i64 - min as i64;
        (self.sample() * range as f64) as i32 + min
    }

    /// `RandomColorBgra()`: four random bytes, the first three as B, G, R.
    fn color(&mut self) -> Px {
        let b = self.internal_sample() as u8;
        let g = self.internal_sample() as u8;
        let r = self.internal_sample() as u8;
        let _alpha = self.internal_sample();
        rgb(r, g, b)
    }
}

// ---------------------------------------------------------------------------
// Colour gradients (Pinta.Core ColorGradient + Pinta.Effects GradientHelper)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Gradient {
    start: Px,
    end: Px,
    lo: f64,
    hi: f64,
    /// Sorted by position, strictly inside (lo, hi).
    stops: Vec<(f64, Px)>,
}

impl Gradient {
    fn new(start: Px, end: Px, stops: &[(f64, Px)]) -> Self {
        let mut stops = stops.to_vec();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        Gradient { start, end, lo: 0.0, hi: 1.0, stops }
    }

    fn resized(mut self, lo: f64, hi: f64) -> Self {
        if self.lo == lo && self.hi == hi {
            return self;
        }
        let k = (hi - lo) / (self.hi - self.lo);
        for s in &mut self.stops {
            s.0 = lo + (s.0 - self.lo) * k;
        }
        self.lo = lo;
        self.hi = hi;
        self
    }

    fn reversed(mut self) -> Self {
        std::mem::swap(&mut self.start, &mut self.end);
        for s in &mut self.stops {
            s.0 = self.hi - s.0 + self.lo;
        }
        self.stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        self
    }

    #[inline]
    fn color(&self, pos: f64) -> Px {
        if pos <= self.lo {
            return self.start;
        }
        if pos >= self.hi {
            return self.end;
        }
        let inv = |a: f64, b: f64| (pos - a) / (b - a);
        if self.stops.is_empty() {
            return lerp_px(self.start, self.end, inv(self.lo, self.hi));
        }
        let idx = self.stops.partition_point(|s| s.0 < pos);
        if idx < self.stops.len() && self.stops[idx].0 == pos {
            return self.stops[idx].1;
        }
        if idx == self.stops.len() {
            let (p, c) = self.stops[idx - 1];
            return lerp_px(c, self.end, inv(p, self.hi));
        }
        let (hp, hc) = self.stops[idx];
        if idx == 0 {
            return lerp_px(self.start, hc, inv(self.lo, hp));
        }
        let (lp, lc) = self.stops[idx - 1];
        lerp_px(lc, hc, inv(lp, hp))
    }

    /// Colour for positions possibly outside the range (`GetColorExtended`).
    #[inline]
    fn color_extended(&self, pos: f64, edge: EdgeBehavior, original: Px, primary: Px, secondary: Px) -> Px {
        if pos >= self.lo && pos <= self.hi {
            return self.color(pos);
        }
        let range = self.hi - self.lo;
        match edge {
            EdgeBehavior::Clamp => {
                if pos > self.hi {
                    self.end
                } else if pos < self.lo {
                    self.start
                } else {
                    // NaN
                    self.color(pos)
                }
            }
            EdgeBehavior::Wrap => {
                let m = (pos - self.lo) % range;
                let m = if m < 0.0 { m + range } else { m };
                self.color(m + self.lo)
            }
            EdgeBehavior::Reflect => {
                let m = (pos - self.lo) % (2.0 * range);
                let m = if m < 0.0 { m + 2.0 * range } else { m };
                self.color(if m < range { self.lo + m } else { self.hi - (m - range) })
            }
            EdgeBehavior::Primary => primary,
            EdgeBehavior::Secondary => secondary,
            EdgeBehavior::Transparent => CLEAR,
            EdgeBehavior::Original => original,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSchemeSource {
    PresetGradient,
    SelectedColors,
    Random,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresetGradient {
    BeautifulItaly,
    BlackAndWhite,
    Bonfire,
    CherryBlossom,
    CottonCandy,
    Electric,
    LimeLemon,
    MartianLava,
    PinaColada,
}

impl PresetGradient {
    const ALL: [(PresetGradient, &'static str); 9] = [
        (PresetGradient::BeautifulItaly, "Beautiful Italy"),
        (PresetGradient::BlackAndWhite, "Black and White"),
        (PresetGradient::Bonfire, "Bonfire"),
        (PresetGradient::CherryBlossom, "Cherry Blossom"),
        (PresetGradient::CottonCandy, "Cotton Candy"),
        (PresetGradient::Electric, "Electric"),
        (PresetGradient::LimeLemon, "Lime Lemon"),
        (PresetGradient::MartianLava, "Martian Lava"),
        (PresetGradient::PinaColada, "Piña Colada"),
    ];

    fn gradient(self) -> Gradient {
        const RED: Px = rgb(255, 0, 0);
        const YELLOW: Px = rgb(255, 255, 0);
        const BLUE: Px = rgb(0, 0, 255);
        const CYAN: Px = rgb(0, 255, 255);
        match self {
            PresetGradient::BeautifulItaly => Gradient::new(rgb(0, 146, 70), rgb(206, 43, 55), &[(0.25, WHITE)]),
            PresetGradient::BlackAndWhite => Gradient::new(WHITE, BLACK, &[]),
            PresetGradient::Bonfire => Gradient::new(CLEAR, WHITE, &[(0.25, BLACK), (0.5, RED), (0.75, YELLOW)]),
            PresetGradient::CherryBlossom => Gradient::new(CLEAR, rgb(255, 255, 240), &[(0.25, rgb(135, 206, 235)), (0.75, rgb(255, 182, 193))]),
            PresetGradient::CottonCandy => {
                Gradient::new(WHITE, rgb(214, 235, 242), &[(0.25, rgb(255, 105, 180)), (0.5, rgb(219, 112, 219)), (0.75, rgb(173, 216, 230))])
            }
            PresetGradient::Electric => Gradient::new(CLEAR, WHITE, &[(0.25, BLACK), (0.5, BLUE), (0.75, CYAN)]),
            PresetGradient::LimeLemon => Gradient::new(CLEAR, WHITE, &[(0.25, rgb(0, 128, 0)), (0.5, rgb(0, 255, 0)), (0.75, rgb(255, 255, 0))]),
            PresetGradient::MartianLava => Gradient::new(
                rgb(70, 12, 26),
                rgb(228, 117, 93),
                &[(0.2, rgb(213, 101, 103)), (0.4, rgb(200, 219, 25)), (0.6, rgb(59, 52, 124)), (0.8, rgb(0, 133, 248))],
            ),
            PresetGradient::PinaColada => Gradient::new(rgb(128, 128, 0), rgb(253, 245, 196), &[(0.25, YELLOW)]),
        }
    }
}

/// The colour-scheme settings shared by Cells, Clouds and the fractals.
#[derive(Clone)]
pub struct ColorScheme {
    pub source: ColorSchemeSource,
    pub preset: PresetGradient,
    pub seed: u32,
    pub reverse: bool,
}

impl ColorScheme {
    fn new(source: ColorSchemeSource, preset: PresetGradient) -> Self {
        ColorScheme { source, preset, seed: 0, reverse: false }
    }

    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = combo(
            ui,
            "Color Scheme Source",
            &mut self.source,
            &[
                (ColorSchemeSource::PresetGradient, "Preset Gradient"),
                (ColorSchemeSource::SelectedColors, "Selected Colors"),
                (ColorSchemeSource::Random, "Random"),
            ],
        );
        match self.source {
            ColorSchemeSource::PresetGradient => c |= combo(ui, "Color Scheme", &mut self.preset, &PresetGradient::ALL),
            ColorSchemeSource::Random => c |= seed_row(ui, "Random Color Scheme Seed", &mut self.seed),
            ColorSchemeSource::SelectedColors => {}
        }
        c |= ui.checkbox(&mut self.reverse, "Reverse Color Scheme").changed();
        c
    }

    fn build(&self, ctx: &RenderCtx, lo: f64, hi: f64) -> Gradient {
        let base = match self.source {
            ColorSchemeSource::PresetGradient => self.preset.gradient(),
            ColorSchemeSource::SelectedColors => Gradient::new(premul_trunc(ctx.primary), premul_trunc(ctx.secondary), &[]),
            ColorSchemeSource::Random => {
                let mut rng = DotNetRandom::new(self.seed.min(i32::MAX as u32 - 1) as i32);
                let start = rng.color();
                let end = rng.color();
                let n = rng.next_range(0, 5);
                let stops: Vec<(f64, Px)> = (0..n).map(|i| ((i + 1) as f64 / (n + 1) as f64, rng.color())).collect();
                Gradient::new(start, end, &stops)
            }
        };
        let g = base.resized(lo, hi);
        if self.reverse { g.reversed() } else { g }
    }
}

/// Behaviour for positions outside a colour scheme's range.
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

const EDGE_ITEMS: [(EdgeBehavior, &str); 7] = [
    (EdgeBehavior::Clamp, "Clamp"),
    (EdgeBehavior::Wrap, "Wrap"),
    (EdgeBehavior::Reflect, "Reflect"),
    (EdgeBehavior::Primary, "Primary"),
    (EdgeBehavior::Secondary, "Secondary"),
    (EdgeBehavior::Transparent, "Transparent"),
    (EdgeBehavior::Original, "Original"),
];

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

/// Labelled random seed with a Reseed button (Pinta's RandomSeed editor).
fn seed_row(ui: &mut egui::Ui, label: &str, v: &mut u32) -> bool {
    ui.label(label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::DragValue::new(v).range(0..=i32::MAX as u32 - 1)).changed();
        if ui.button("Reseed").clicked() {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
            *v = util::hash3(*v as i32, 1, t) & 0x7fff_fffe;
            changed = true;
        }
    });
    changed
}

fn color_row(ui: &mut egui::Ui, label: &str, c: &mut [u8; 4]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = ui.color_edit_button_srgba_unmultiplied(c).changed();
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
// Point sets and nearest-point search (Cells, Voronoi)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DistanceMetric {
    Euclidean,
    Manhattan,
    Chebyshev,
}

const METRIC_ITEMS: [(DistanceMetric, &str); 3] =
    [(DistanceMetric::Euclidean, "Euclidean"), (DistanceMetric::Manhattan, "Manhattan"), (DistanceMetric::Chebyshev, "Chebyshev")];

impl DistanceMetric {
    /// Comparable distance from absolute offsets (squared for Euclidean).
    #[inline]
    fn fast(self, dx: f64, dy: f64) -> f64 {
        match self {
            DistanceMetric::Euclidean => dx * dx + dy * dy,
            DistanceMetric::Manhattan => dx.abs() + dy.abs(),
            DistanceMetric::Chebyshev => dx.abs().max(dy.abs()),
        }
    }
    #[inline]
    fn real(self, dx: f64, dy: f64) -> f64 {
        match self {
            DistanceMetric::Euclidean => (dx * dx + dy * dy).sqrt(),
            _ => self.fast(dx, dy),
        }
    }
    fn fast_threshold(self, d: f64) -> f64 {
        match self {
            DistanceMetric::Euclidean => d * d,
            _ => d,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointArrangement {
    Random,
    Circular,
    Phyllotaxis,
}

const ARRANGEMENT_ITEMS: [(PointArrangement, &str); 3] =
    [(PointArrangement::Random, "Random"), (PointArrangement::Circular, "Circular"), (PointArrangement::Phyllotaxis, "Phyllotaxis")];

/// `SpatialPartition.CreateCellControlPoints`.
fn control_points(roi: IRect, count: i32, seed: u32, arrangement: PointArrangement) -> Vec<(f64, f64)> {
    let area = roi.w.max(0) as i64 * roi.h.max(0) as i64;
    let count = (count.max(0) as i64).min(area) as usize;
    if count == 0 {
        return vec![];
    }
    let center = (lerp(roi.x as f64, (roi.right() - 1) as f64, 0.5), lerp(roi.y as f64, (roi.bottom() - 1) as f64, 0.5));
    let half_min = roi.w.min(roi.h) as f64 / 2.0;
    match arrangement {
        PointArrangement::Random => {
            let mut rng = DotNetRandom::new(seed.min(i32::MAX as u32 - 1) as i32);
            let mut seen = std::collections::HashSet::with_capacity(count);
            let mut pts: Vec<(i32, i32)> = Vec::with_capacity(count);
            while pts.len() < count {
                let x = rng.next_range(roi.x, roi.right());
                let y = rng.next_range(roi.y, roi.bottom());
                if seen.insert((x, y)) {
                    pts.push((x, y));
                }
            }
            // Pinta collects the points in an ImmutableHashSet, which enumerates in
            // ascending (signed) hash order of the PointI record struct.
            pts.sort_by_key(|&(x, y)| x.wrapping_mul(-1_521_134_295).wrapping_add(y));
            pts.into_iter().map(|(x, y)| (x as f64 + 0.5, y as f64 + 0.5)).collect()
        }
        PointArrangement::Circular => {
            let step = 2.0 * PI / count as f64;
            (0..count)
                .map(|i| {
                    let a = step * i as f64;
                    (center.0 + half_min * a.cos(), center.1 + half_min * a.sin())
                })
                .collect()
        }
        PointArrangement::Phyllotaxis => {
            let golden = PI * (3.0 - 5f64.sqrt());
            let inv = 1.0 / count as f64;
            (0..count)
                .map(|i| {
                    let r = (i as f64 * inv).sqrt() * half_min;
                    let a = i as f64 * golden;
                    (center.0 + r * a.cos(), center.1 + r * a.sin())
                })
                .collect()
        }
    }
}

/// Supersampling offsets within a pixel (`Sampling.CreateSamplingOffsets`).
fn sampling_offsets(quality: i32) -> Vec<(f64, f64)> {
    let q = quality.max(1);
    let sec = 1.0 / q as f64;
    let init = sec / 2.0;
    let mut v = Vec::with_capacity((q * q) as usize);
    for h in 0..q {
        for k in 0..q {
            v.push((init + sec * h as f64, init + sec * k as f64));
        }
    }
    v
}

/// Indices of the points that can be nearest to some location in the box
/// [x0, x1] x [y0, y1], in ascending index order. Exact: every dropped point is
/// strictly farther than some other point everywhere in the box, so the search
/// result (including Pinta's "last index wins" tie rule) is unchanged.
fn candidates(points: &[(f64, f64)], metric: DistanceMetric, x0: f64, y0: f64, x1: f64, y1: f64, out: &mut Vec<u32>) {
    out.clear();
    let mut bound = f64::INFINITY;
    for &(px, py) in points {
        let dx = (px - x0).abs().max((px - x1).abs());
        let dy = (py - y0).abs().max((py - y1).abs());
        bound = bound.min(metric.fast(dx, dy));
    }
    let bound = bound * (1.0 + 1e-9) + 1e-9;
    for (i, &(px, py)) in points.iter().enumerate() {
        let dx = (x0 - px).max(px - x1).max(0.0);
        let dy = (y0 - py).max(py - y1).max(0.0);
        if metric.fast(dx, dy) <= bound {
            out.push(i as u32);
        }
    }
}

/// Nearest point (fast distance, last index wins ties).
#[inline]
fn nearest(points: &[(f64, f64)], cand: &[u32], metric: DistanceMetric, x: f64, y: f64) -> (usize, f64) {
    let mut best = f64::MAX;
    let mut idx = 0usize;
    for &i in cand {
        let (px, py) = points[i as usize];
        let d = metric.fast(x - px, y - py);
        if d > best {
            continue;
        }
        best = d;
        idx = i as usize;
    }
    (idx, best)
}

const BLOCK: i32 = 16;

/// Render `dst` block by block with a pruned candidate list per block.
fn render_cells(dst: &mut [u8], r: IRect, points: &[(f64, f64)], metric: DistanceMetric, mut px: impl FnMut(&[u32], i32, i32, Px) -> Px) {
    let mut cand = Vec::with_capacity(points.len());
    let mut by = 0;
    while by < r.h {
        let bh = BLOCK.min(r.h - by);
        let mut bx = 0;
        while bx < r.w {
            let bw = BLOCK.min(r.w - bx);
            let (x0, y0) = ((r.x + bx) as f64, (r.y + by) as f64);
            candidates(points, metric, x0, y0, x0 + bw as f64, y0 + bh as f64, &mut cand);
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = ((y * r.w + x) * 4) as usize;
                    let orig = [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]];
                    let out = px(&cand, r.x + x, r.y + y, orig);
                    dst[i..i + 4].copy_from_slice(&out);
                }
            }
            bx += bw;
        }
        by += bh;
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CellsPrepared {
    points: Arc<Vec<(f64, f64)>>,
    gradient: Gradient,
    primary: Px,
    secondary: Px,
}

#[derive(Clone)]
pub struct Cells {
    pub metric: DistanceMetric,
    pub arrangement: PointArrangement,
    pub point_seed: u32,
    pub show_points: bool,
    pub point_size: f64,
    /// Straight-alpha RGBA.
    pub point_color: [u8; 4],
    pub cells: i32,
    pub cell_radius: f64,
    pub scheme: ColorScheme,
    pub scheme_edge: EdgeBehavior,
    pub quality: i32,
    prepared: Option<CellsPrepared>,
}

impl Default for Cells {
    fn default() -> Self {
        Cells {
            metric: DistanceMetric::Euclidean,
            arrangement: PointArrangement::Random,
            point_seed: 0,
            show_points: false,
            point_size: 4.0,
            point_color: [0, 0, 0, 255],
            cells: 100,
            cell_radius: 32.0,
            scheme: ColorScheme::new(ColorSchemeSource::PresetGradient, PresetGradient::BlackAndWhite),
            scheme_edge: EdgeBehavior::Clamp,
            quality: 3,
            prepared: None,
        }
    }
}

impl Effect for Cells {
    fn name(&self) -> &'static str {
        "Cells"
    }
    fn category(&self) -> Category {
        Category::Render
    }
    fn icon(&self) -> &'static str {
        "effects-render-cells-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = combo(ui, "Distance Metric", &mut self.metric, &METRIC_ITEMS);
        c |= combo(ui, "Point Arrangement", &mut self.arrangement, &ARRANGEMENT_ITEMS);
        c |= seed_row(ui, "Random Point Locations", &mut self.point_seed);
        c |= ui.checkbox(&mut self.show_points, "Show Points").changed();
        ui.label("Point Size");
        c |= ui.add(egui::Slider::new(&mut self.point_size, 1.0..=16.0).step_by(1.0)).changed();
        c |= color_row(ui, "Point Color", &mut self.point_color);
        c |= util::slider_i(ui, "Number of Cells", &mut self.cells, 1, 1024);
        ui.label("Cell Radius");
        c |= ui.add(egui::Slider::new(&mut self.cell_radius, 4.0..=100.0).step_by(1.0)).changed();
        c |= self.scheme.ui(ui);
        c |= combo(ui, "Color Scheme Edge Behavior", &mut self.scheme_edge, &EDGE_ITEMS);
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 4);
        c
    }
    fn prepare(&mut self, _src: &Surface, ctx: &RenderCtx) {
        self.prepared = Some(CellsPrepared {
            points: Arc::new(control_points(ctx.sel_bounds, self.cells, self.point_seed, self.arrangement)),
            gradient: self.scheme.build(ctx, 0.0, self.cell_radius),
            primary: premul_trunc(ctx.primary),
            secondary: premul_trunc(ctx.secondary),
        });
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let Some(p) = &self.prepared else { return };
        if p.points.is_empty() {
            return;
        }
        let samples = sampling_offsets(self.quality);
        let metric = self.metric;
        let point_radius = self.point_size / 2.0;
        let point_color = premul_trunc(self.point_color);
        let points = &p.points[..];
        render_cells(dst, r, points, metric, |cand, x, y, orig| {
            let mut acc = Blender::default();
            for &(ox, oy) in &samples {
                let (lx, ly) = (x as f64 + ox, y as f64 + oy);
                let (i, _) = nearest(points, cand, metric, lx, ly);
                let (px, py) = points[i];
                let d = metric.real(lx - px, ly - py);
                let c = p.gradient.color_extended(d, self.scheme_edge, orig, p.primary, p.secondary);
                acc.add(if self.show_points && d <= point_radius { normal_over(c, point_color) } else { c });
            }
            acc.blend()
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Clouds
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

/// Pinta's 2-D Perlin noise for grid cell (gx, gy) and fractional offset (ox, oy).
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

#[derive(Clone)]
pub struct Clouds {
    pub scale: i32,
    pub power: i32,
    pub seed: u32,
    pub scheme: ColorScheme,
}

impl Default for Clouds {
    fn default() -> Self {
        Clouds { scale: 250, power: 50, seed: 0, scheme: ColorScheme::new(ColorSchemeSource::SelectedColors, PresetGradient::BeautifulItaly) }
    }
}

impl Effect for Clouds {
    fn name(&self) -> &'static str {
        "Clouds"
    }
    fn category(&self) -> Category {
        Category::Render
    }
    fn icon(&self) -> &'static str {
        "effects-render-clouds-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Scale", &mut self.scale, 2, 1000);
        c |= util::slider_i(ui, "Power", &mut self.power, 0, 100);
        c |= seed_row(ui, "Random Noise Seed", &mut self.seed);
        c |= self.scheme.ui(ui);
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let gradient = self.scheme.build(ctx, 0.0, 1.0);
        let seed = (self.seed & 255) as usize;
        let power = self.power as f64 / 100.0;
        // Pinta centres the noise on the render area; we use the selection bounds
        // (the whole render area) so the result does not depend on tiling.
        let (bw, bh) = (ctx.sel_bounds.w, ctx.sel_bounds.h);
        for_each_px(dst, r, |x, y, _| {
            let dx = 2 * x - bw;
            let dy = 2 * y - bh;
            let mut val = 0.0;
            let mut mult = 1.0;
            let mut div = self.scale;
            let mut i = 0usize;
            while i < 12 && mult > 0.03 && div > 0 {
                let rx = 65536.0 + dx as f64 / div as f64;
                let ry = 65536.0 + dy as f64 / div as f64;
                let ix = rx as i32;
                let iy = ry as i32;
                let n = perlin_cell((ix & 255) as usize, (iy & 255) as usize, rx - ix as f64, ry - iy as f64, (seed ^ i) & 255);
                val += n * mult;
                div /= 2;
                mult *= power;
                i += 1;
            }
            gradient.color((val + 1.0) / 2.0)
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Julia Fractal
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct JuliaFractal {
    pub factor: i32,
    pub quality: i32,
    pub zoom: f64,
    pub scheme: ColorScheme,
    pub angle: f64,
}

impl Default for JuliaFractal {
    fn default() -> Self {
        JuliaFractal { factor: 4, quality: 2, zoom: 1.0, scheme: ColorScheme::new(ColorSchemeSource::PresetGradient, PresetGradient::Bonfire), angle: 0.0 }
    }
}

impl Effect for JuliaFractal {
    fn name(&self) -> &'static str {
        "Julia Fractal"
    }
    fn category(&self) -> Category {
        Category::Render
    }
    fn icon(&self) -> &'static str {
        "effects-render-juliafractal-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Factor", &mut self.factor, 1, 10);
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 5);
        ui.label("Zoom");
        c |= ui.add(egui::Slider::new(&mut self.zoom, 0.0..=50.0).step_by(0.5)).changed();
        c |= self.scheme.ui(ui);
        c |= util::angle(ui, "Angle", &mut self.angle);
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        const JR: f64 = 0.3125;
        const JI: f64 = 0.03;
        const MAX_SQUARED: f64 = 10_000.0;
        let log_max = MAX_SQUARED.ln();
        let gradient = self.scheme.build(ctx, 0.0, 1023.0);
        // Pinta maps the fractal onto the whole canvas.
        let (w, h) = (ctx.width as f64, ctx.height as f64);
        let inv_h = 1.0 / h;
        let inv_zoom = 1.0 / self.zoom;
        let inv_quality = 1.0 / self.quality as f64;
        let aspect = h / w;
        let count = self.quality * self.quality + 1;
        let inv_count = 1.0 / count as f64;
        let (sin, cos) = self.angle.to_radians().sin_cos();
        let factor = self.factor as f64;
        for_each_px(dst, r, |x, y, _| {
            let bx = 2.0 * x as f64 - w;
            let by = 2.0 * y as f64 - h;
            let mut acc = Blender::default();
            for i in 0..count {
                let tx = (bx + i as f64 * inv_count) * inv_h;
                let ty = (by + (i as f64 * inv_quality) % 1.0) * inv_h;
                let px = tx * cos - ty * sin;
                let py = tx * sin + ty * cos;
                let mut zx = (px - py * aspect) * inv_zoom;
                let mut zy = (py + px * aspect) * inv_zoom;
                let mut c = 0.0;
                while c < 256.0 && zx * zx + zy * zy < MAX_SQUARED {
                    let t = zx;
                    zx = zx * zx - zy * zy + JR;
                    zy = 2.0 * t * zy + JI;
                    c += 1.0;
                }
                let j = c - (2.0 - 2.0 * log_max / (zx * zx + zy * zy).ln());
                acc.add(gradient.color((factor * j).clamp(gradient.lo, gradient.hi)));
            }
            acc.blend()
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Mandelbrot Fractal
// ---------------------------------------------------------------------------

const MANDEL_LANES: usize = 8;

/// Picks a wider-vector build of the lane loop when the CPU has AVX. Rust never
/// fuses multiply-adds on its own, so both builds give identical results.
fn mandelbrot_lanes(cr: &[f64; MANDEL_LANES], ci: &[f64; MANDEL_LANES], factor: i32, out: &mut [f64; MANDEL_LANES]) {
    #[cfg(target_arch = "x86_64")]
    {
        #[target_feature(enable = "avx")]
        fn avx(cr: &[f64; MANDEL_LANES], ci: &[f64; MANDEL_LANES], factor: i32, out: &mut [f64; MANDEL_LANES]) {
            mandelbrot_lanes_impl(cr, ci, factor, out)
        }
        if std::arch::is_x86_feature_detected!("avx") {
            // SAFETY: the CPU supports AVX (checked just above).
            return unsafe { avx(cr, ci, factor, out) };
        }
    }
    mandelbrot_lanes_impl(cr, ci, factor, out)
}

/// Smooth escape value (`Mandelbrot.Compute`) for several points at once.
/// Each lane runs exactly the scalar iteration (masked once it stops), so the
/// results are bit-identical; the independent lanes keep the FPU busy.
#[inline(always)]
fn mandelbrot_lanes_impl(cr: &[f64; MANDEL_LANES], ci: &[f64; MANDEL_LANES], factor: i32, out: &mut [f64; MANDEL_LANES]) {
    const MAX_ITER: i32 = 1024;
    const MAX_SQUARED: f64 = 100_000.0;
    // the iteration count at which a bounded orbit stops
    let last = (MAX_ITER + factor - 1) / factor;
    let mut zx = [0f64; MANDEL_LANES];
    let mut zy = [0f64; MANDEL_LANES];
    let mut c = [0i32; MANDEL_LANES];
    let mut active = [true; MANDEL_LANES];
    for _ in 0..last {
        let mut any = false;
        for l in 0..MANDEL_LANES {
            let a = active[l] & (zx[l] * zx[l] + zy[l] * zy[l] < MAX_SQUARED);
            let nx = zx[l] * zx[l] - zy[l] * zy[l] + cr[l];
            let ny = 2.0 * zx[l] * zy[l] + ci[l];
            zx[l] = if a { nx } else { zx[l] };
            zy[l] = if a { ny } else { zy[l] };
            c[l] += a as i32;
            active[l] = a;
            any |= a;
        }
        if !any {
            break;
        }
    }
    let inv_log_max = 1.0 / MAX_SQUARED.ln();
    for l in 0..MANDEL_LANES {
        out[l] = c[l] as f64 - (zx[l] * zx[l] + zy[l] * zy[l]).ln() * inv_log_max;
    }
}

#[derive(Clone)]
pub struct MandelbrotFractal {
    pub factor: i32,
    pub quality: i32,
    pub zoom: f64,
    pub angle: f64,
    pub scheme: ColorScheme,
    pub invert: bool,
}

impl Default for MandelbrotFractal {
    fn default() -> Self {
        MandelbrotFractal {
            factor: 1,
            quality: 2,
            zoom: 10.0,
            angle: 0.0,
            scheme: ColorScheme::new(ColorSchemeSource::PresetGradient, PresetGradient::Electric),
            invert: false,
        }
    }
}

impl Effect for MandelbrotFractal {
    fn name(&self) -> &'static str {
        "Mandelbrot Fractal"
    }
    fn category(&self) -> Category {
        Category::Render
    }
    fn icon(&self) -> &'static str {
        "effects-render-mandelbrotfractal-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = util::slider_i(ui, "Factor", &mut self.factor, 1, 10);
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 5);
        ui.label("Zoom");
        c |= ui.add(egui::Slider::new(&mut self.zoom, 0.0..=100.0).step_by(0.5)).changed();
        c |= util::angle(ui, "Angle", &mut self.angle);
        c |= self.scheme.ui(ui);
        c |= ui.checkbox(&mut self.invert, "Invert Colors").changed();
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        const OFFSET: (f64, f64) = (-0.7, -0.29);
        let gradient = self.scheme.build(ctx, 0.0, 1023.0);
        let (w, h) = (ctx.width as f64, ctx.height as f64);
        let inv_h = 1.0 / h;
        let inv_zoom = 1.0 / (1.0 + 20.0 * self.zoom);
        let inv_quality = 1.0 / self.quality as f64;
        let count = self.quality * self.quality + 1;
        let inv_count = 1.0 / count as f64;
        let (sin, cos) = self.angle.to_radians().sin_cos();
        let factor = self.factor.max(1);
        let delta_u = inv_count * inv_h;
        let count = count as usize;
        // Escape values for one row: all samples of all pixels, iterated
        // MANDEL_LANES at a time in lockstep for instruction-level parallelism.
        let n = r.w as usize * count;
        let mut cr = vec![0f64; n.next_multiple_of(MANDEL_LANES)];
        let mut ci = vec![0f64; cr.len()];
        let mut m = vec![0f64; cr.len()];
        for y in 0..r.h {
            let bv = (2.0 * (r.y + y) as f64 - h) * inv_h;
            for x in 0..r.w as usize {
                let bu = (2.0 * (r.x + x as i32) as f64 - w) * inv_h;
                for i in 0..count {
                    let u = bu + i as f64 * delta_u;
                    let v = bv + (i as f64 * inv_quality % 1.0) * inv_h;
                    cr[x * count + i] = (u * cos - v * sin) * inv_zoom + OFFSET.0;
                    ci[x * count + i] = (u * sin + v * cos) * inv_zoom + OFFSET.1;
                }
            }
            for ((cr, ci), m) in cr.chunks_exact(MANDEL_LANES).zip(ci.chunks_exact(MANDEL_LANES)).zip(m.chunks_exact_mut(MANDEL_LANES)) {
                mandelbrot_lanes(cr.try_into().unwrap(), ci.try_into().unwrap(), factor, m.try_into().unwrap());
            }
            for x in 0..r.w as usize {
                let mut acc = Blender::default();
                for &mv in &m[x * count..(x + 1) * count] {
                    acc.add(gradient.color((64.0 + factor as f64 * mv).clamp(gradient.lo, gradient.hi)));
                }
                let p = acc.blend();
                let p = if self.invert { [p[3] - p[0], p[3] - p[1], p[3] - p[2], p[3]] } else { p };
                let o = ((y * r.w) as usize + x) * 4;
                dst[o..o + 4].copy_from_slice(&p);
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Voronoi Diagram
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSorting {
    Random,
    HorizontalB,
    HorizontalG,
    HorizontalR,
    VerticalB,
    VerticalG,
    VerticalR,
}

const SORTING_ITEMS: [(ColorSorting, &str); 7] = [
    (ColorSorting::Random, "Random"),
    (ColorSorting::HorizontalB, "Horizontal blue (B)"),
    (ColorSorting::HorizontalG, "Horizontal green (G)"),
    (ColorSorting::HorizontalR, "Horizontal red (R)"),
    (ColorSorting::VerticalB, "Vertical blue (B)"),
    (ColorSorting::VerticalG, "Vertical green (G)"),
    (ColorSorting::VerticalR, "Vertical red (R)"),
];

#[derive(Clone)]
struct VoronoiPrepared {
    points: Arc<Vec<(f64, f64)>>,
    colors: Arc<Vec<Px>>,
}

#[derive(Clone)]
pub struct VoronoiDiagram {
    pub metric: DistanceMetric,
    pub cells: i32,
    pub sorting: ColorSorting,
    pub reverse_sorting: bool,
    pub color_seed: u32,
    pub arrangement: PointArrangement,
    pub point_seed: u32,
    pub show_points: bool,
    pub point_size: f64,
    /// Straight-alpha RGBA.
    pub point_color: [u8; 4],
    pub quality: i32,
    prepared: Option<VoronoiPrepared>,
}

impl Default for VoronoiDiagram {
    fn default() -> Self {
        VoronoiDiagram {
            metric: DistanceMetric::Euclidean,
            cells: 100,
            sorting: ColorSorting::Random,
            reverse_sorting: false,
            color_seed: 0,
            arrangement: PointArrangement::Random,
            point_seed: 0,
            show_points: false,
            point_size: 4.0,
            point_color: [0, 0, 0, 255],
            quality: 3,
            prepared: None,
        }
    }
}

impl Effect for VoronoiDiagram {
    fn name(&self) -> &'static str {
        "Voronoi Diagram"
    }
    fn category(&self) -> Category {
        Category::Render
    }
    fn icon(&self) -> &'static str {
        "effects-render-voronoidiagram-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = combo(ui, "Distance Metric", &mut self.metric, &METRIC_ITEMS);
        c |= util::slider_i(ui, "Number of Cells", &mut self.cells, 1, 1024);
        c |= combo(ui, "Color Sorting", &mut self.sorting, &SORTING_ITEMS);
        c |= ui.checkbox(&mut self.reverse_sorting, "Reverse Color Sorting").changed();
        c |= seed_row(ui, "Random Colors", &mut self.color_seed);
        c |= combo(ui, "Point Arrangement", &mut self.arrangement, &ARRANGEMENT_ITEMS);
        if self.arrangement == PointArrangement::Random {
            c |= seed_row(ui, "Random Point Locations", &mut self.point_seed);
        }
        c |= ui.checkbox(&mut self.show_points, "Show Points").changed();
        if self.show_points {
            ui.label("Point Size");
            c |= ui.add(egui::Slider::new(&mut self.point_size, 1.0..=16.0).step_by(1.0)).changed();
            c |= color_row(ui, "Point Color", &mut self.point_color);
        }
        c |= util::slider_i(ui, "Quality", &mut self.quality, 1, 4);
        c
    }
    fn prepare(&mut self, _src: &Surface, ctx: &RenderCtx) {
        let mut points = control_points(ctx.sel_bounds, self.cells, self.point_seed, self.arrangement);
        use ColorSorting::*;
        match self.sorting {
            Random => {}
            HorizontalB | HorizontalG | HorizontalR => points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))),
            VerticalB | VerticalG | VerticalR => points.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0))),
        }
        // unique random colours, in generation order
        let mut rng = DotNetRandom::new(self.color_seed.min(i32::MAX as u32 - 1) as i32);
        let mut seen = std::collections::HashSet::with_capacity(points.len());
        let mut colors = Vec::with_capacity(points.len());
        while colors.len() < points.len() {
            let c = rng.color();
            if seen.insert(c) {
                colors.push(c);
            }
        }
        match self.sorting {
            Random => {}
            HorizontalB | VerticalB => colors.sort_by_key(|c| c[2]),
            HorizontalG | VerticalG => colors.sort_by_key(|c| c[1]),
            HorizontalR | VerticalR => colors.sort_by_key(|c| c[0]),
        }
        if self.reverse_sorting {
            colors.reverse();
        }
        self.prepared = Some(VoronoiPrepared { points: Arc::new(points), colors: Arc::new(colors) });
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let Some(p) = &self.prepared else { return };
        if p.points.is_empty() {
            return;
        }
        let samples = sampling_offsets(self.quality);
        let metric = self.metric;
        let radius_fast = metric.fast_threshold(self.point_size / 2.0);
        let point_color = premul_trunc(self.point_color);
        let points = &p.points[..];
        render_cells(dst, r, points, metric, |cand, x, y, _| {
            let mut acc = Blender::default();
            for &(ox, oy) in &samples {
                let (i, d) = nearest(points, cand, metric, x as f64 + ox, y as f64 + oy);
                let c = p.colors[i];
                acc.add(if self.show_points && d <= radius_fast { normal_over(c, point_color) } else { c });
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
    use crate::surface::premul;

    fn test_image(w: u32, h: u32) -> Surface {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = if x < w / 6 { 0 } else { 255 - (y * 200 / h.max(1)) as u8 };
                let c = [(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, if (x / 37 + y / 23) % 2 == 0 { 230 } else { 20 }, a];
                data[i..i + 4].copy_from_slice(&premul(c));
            }
        }
        Surface::from_rgba(w, h, &data)
    }

    fn ctx_for(w: u32, h: u32) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: IRect::new(0, 0, w as i32, h as i32), primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255] }
    }

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
        let random = ColorScheme { source: ColorSchemeSource::Random, preset: PresetGradient::Bonfire, seed: 77, reverse: true };
        v.push(Box::new(Cells {
            metric: DistanceMetric::Manhattan,
            show_points: true,
            point_color: [255, 0, 0, 128],
            scheme_edge: EdgeBehavior::Original,
            cell_radius: 8.0,
            ..Cells::default()
        }));
        v.push(Box::new(Cells { arrangement: PointArrangement::Phyllotaxis, scheme: random.clone(), scheme_edge: EdgeBehavior::Reflect, ..Cells::default() }));
        v.push(Box::new(Cells { metric: DistanceMetric::Chebyshev, arrangement: PointArrangement::Circular, cells: 1024, ..Cells::default() }));
        v.push(Box::new(Clouds { scheme: random.clone(), seed: 300, power: 80, scale: 37 }));
        v.push(Box::new(JuliaFractal { factor: 6, quality: 4, zoom: 25.0, angle: 90.0, ..JuliaFractal::default() }));
        v.push(Box::new(MandelbrotFractal { factor: 6, quality: 1, zoom: 0.0, angle: 33.0, invert: true, scheme: random, ..MandelbrotFractal::default() }));
        v.push(Box::new(VoronoiDiagram { metric: DistanceMetric::Chebyshev, sorting: ColorSorting::VerticalG, reverse_sorting: true, cells: 1024, ..VoronoiDiagram::default() }));
        v.push(Box::new(VoronoiDiagram { show_points: true, arrangement: PointArrangement::Phyllotaxis, point_size: 9.0, ..VoronoiDiagram::default() }));
        v
    }

    #[test]
    fn names_in_menu_order() {
        let names: Vec<_> = all().iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Cells", "Clouds", "Julia Fractal", "Mandelbrot Fractal", "Voronoi Diagram"]);
        assert!(all().iter().all(|e| e.category() == Category::Render));
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

    /// The pruned nearest-point search must agree with a brute-force scan.
    #[test]
    fn candidate_pruning_is_exact() {
        for metric in [DistanceMetric::Euclidean, DistanceMetric::Manhattan, DistanceMetric::Chebyshev] {
            for arr in [PointArrangement::Random, PointArrangement::Circular, PointArrangement::Phyllotaxis] {
                let pts = control_points(IRect::new(0, 0, 300, 200), 300, 5, arr);
                let all_idx: Vec<u32> = (0..pts.len() as u32).collect();
                let mut cand = vec![];
                for by in (0..200).step_by(16) {
                    for bx in (0..300).step_by(16) {
                        candidates(&pts, metric, bx as f64, by as f64, bx as f64 + 16.0, by as f64 + 16.0, &mut cand);
                        for k in 0..40 {
                            let x = bx as f64 + (k * 7 % 16) as f64 + 0.37;
                            let y = by as f64 + (k * 3 % 16) as f64 + 0.81;
                            assert_eq!(nearest(&pts, &cand, metric, x, y), nearest(&pts, &all_idx, metric, x, y));
                        }
                    }
                }
            }
        }
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

    #[test]
    fn dotnet_random_matches_reference() {
        // Known outputs of `new System.Random(0).Next()` and `new Random(42).Next()`.
        let mut r = DotNetRandom::new(0);
        assert_eq!([r.internal_sample(), r.internal_sample(), r.internal_sample()], [1559595546, 1755192844, 1649316166]);
        let mut r = DotNetRandom::new(42);
        assert_eq!(r.internal_sample(), 1434747710);
    }

    // --- comparison against Pinta's own test images (when the upstream checkout exists) ---

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../pinta-upstream/tests/Pinta.Effects.Tests/Assets");

    fn load_png(name: &str) -> Option<Surface> {
        let img = image::open(format!("{ASSETS}/{name}")).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let mut data = img.into_raw();
        for p in data.chunks_exact_mut(4) {
            p.copy_from_slice(&premul([p[0], p[1], p[2], p[3]]));
        }
        Some(Surface::from_rgba(w, h, &data))
    }

    /// Fraction of pixels with a channel more than 1 away from Pinta's result
    /// (Pinta's own tests use the same tolerance; PNG round trips shift values by 1).
    fn compare(effect: &mut dyn Effect, expected: &str) -> Option<f64> {
        let input = load_png("input.png")?;
        let exp = load_png(expected)?.to_rgba();
        let ctx = ctx_for(input.width, input.height);
        let out = run(effect, &input, &ctx);
        let bad = out.chunks(4).zip(exp.chunks(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 1)).count();
        Some(bad as f64 / (out.len() / 4) as f64)
    }

    #[test]
    fn matches_pinta_reference_images() {
        let cases: Vec<(Box<dyn Effect>, &str, f64)> = vec![
            (Box::new(Cells::default()), "cells1.png", 0.0),
            (Box::new(Cells { scheme: ColorScheme::new(ColorSchemeSource::PresetGradient, PresetGradient::Electric), ..Cells::default() }), "cells2.png", 0.0),
            (Box::new(Cells { metric: DistanceMetric::Manhattan, ..Cells::default() }), "cells3.png", 0.0),
            (Box::new(Cells { scheme: ColorScheme { reverse: true, ..Cells::default().scheme }, ..Cells::default() }), "cells4.png", 0.0),
            (Box::new(Cells { cell_radius: 16.0, ..Cells::default() }), "cells5.png", 0.0),
            (Box::new(Cells { cells: 50, ..Cells::default() }), "cells6.png", 0.0),
            (Box::new(Cells { cell_radius: 8.0, scheme_edge: EdgeBehavior::Wrap, ..Cells::default() }), "cells7.png", 0.0),
            (Box::new(Cells { cell_radius: 8.0, scheme_edge: EdgeBehavior::Transparent, ..Cells::default() }), "cells8.png", 0.0),
            (Box::new(Cells { cell_radius: 8.0, scheme_edge: EdgeBehavior::Primary, ..Cells::default() }), "cells9.png", 0.0),
            (Box::new(Cells { cell_radius: 8.0, scheme_edge: EdgeBehavior::Original, ..Cells::default() }), "cells10.png", 0.0),
            (Box::new(Cells { show_points: true, ..Cells::default() }), "cells11.png", 0.0),
            (Box::new(Cells { arrangement: PointArrangement::Phyllotaxis, ..Cells::default() }), "cells12.png", 0.0),
            (Box::new(Cells { arrangement: PointArrangement::Circular, ..Cells::default() }), "cells13.png", 0.0),
            (Box::new(Clouds::default()), "clouds1.png", 0.0),
            (Box::new(JuliaFractal::default()), "juliafractal1.png", 0.0),
            (Box::new(JuliaFractal { factor: 6, quality: 4, zoom: 25.0, angle: 90.0, ..JuliaFractal::default() }), "juliafractal2.png", 0.0),
            (Box::new(MandelbrotFractal::default()), "mandelbrotfractal1.png", 0.0),
            (
                Box::new(MandelbrotFractal { factor: 6, quality: 4, zoom: 25.0, angle: 90.0, invert: true, ..MandelbrotFractal::default() }),
                "mandelbrotfractal2.png",
                0.0,
            ),
            (Box::new(VoronoiDiagram::default()), "voronoi1.png", 0.0),
            (Box::new(VoronoiDiagram { cells: 200, ..VoronoiDiagram::default() }), "voronoi2.png", 0.0),
            (Box::new(VoronoiDiagram { metric: DistanceMetric::Manhattan, ..VoronoiDiagram::default() }), "voronoi3.png", 0.0),
            (Box::new(VoronoiDiagram { sorting: ColorSorting::HorizontalB, ..VoronoiDiagram::default() }), "voronoi4.png", 0.0),
            (Box::new(VoronoiDiagram { sorting: ColorSorting::VerticalB, ..VoronoiDiagram::default() }), "voronoi5.png", 0.0),
            (Box::new(VoronoiDiagram { show_points: true, ..VoronoiDiagram::default() }), "voronoi6.png", 0.0),
            (Box::new(VoronoiDiagram { arrangement: PointArrangement::Phyllotaxis, ..VoronoiDiagram::default() }), "voronoi7.png", 0.0),
            // Pinta skips this one as platform dependent (points exactly on cell borders)
            (Box::new(VoronoiDiagram { arrangement: PointArrangement::Circular, ..VoronoiDiagram::default() }), "voronoi8.png", 0.01),
        ];
        let mut failures = vec![];
        for (mut e, name, allowed) in cases {
            let Some(frac) = compare(e.as_mut(), name) else {
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
            eprintln!("{:<20} {:>7.0} ms", e.name(), t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
