//! Adjustments menu (ported from Pinta / Paint.NET).
//!
//! Every adjustment here is a per-pixel colour mapping, so render() only
//! looks at the destination buffer (which starts out as the source pixels)
//! and never depends on tiling. The colour maths runs on straight alpha.

use super::util::{intensity, map_straight, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::{IRect, Surface, TILE, unpremul};
use egui::{Color32, Pos2, Sense, Stroke, StrokeKind, Vec2};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// All adjustments in Pinta's (alphabetical) menu order.
pub fn all() -> Vec<Box<dyn Effect>> {
    vec![
        Box::new(AutoLevel::default()),
        Box::new(BlackAndWhite),
        Box::new(BrightnessContrast::default()),
        Box::new(Curves::default()),
        Box::new(HueSaturation::default()),
        Box::new(InvertColors),
        Box::new(Levels::default()),
        Box::new(Posterize::default()),
        Box::new(Sepia { strength: 100 }),
    ]
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

type Lut = [u8; 256];

const IDENTITY_LUT: Lut = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as u8;
        i += 1;
    }
    t
};

/// Apply per-channel lookup tables (R, G, B order) on straight alpha.
fn apply_luts(dst: &mut [u8], luts: &[Lut; 3]) {
    if luts.iter().all(|l| *l == IDENTITY_LUT) {
        return;
    }
    map_straight(dst, |c| [luts[0][c[0] as usize], luts[1][c[1] as usize], luts[2][c[2] as usize], c[3]]);
}

#[inline]
fn clamp_byte(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Per-channel 256-bin histogram (R, G, B order) of straight-alpha colours.
/// Fully transparent pixels carry no colour and are skipped.
#[derive(Clone)]
struct HistogramRgb {
    bins: [[u64; 256]; 3],
}

impl HistogramRgb {
    fn empty() -> Self {
        HistogramRgb { bins: [[0; 256]; 3] }
    }

    fn compute(src: &Surface, rect: IRect) -> Self {
        let rect = rect.intersect(&src.bounds());
        let tiles = src.tiles_in(rect);
        tiles
            .par_iter()
            .map(|&idx| {
                let mut h = HistogramRgb::empty();
                let Some(tile) = src.tiles[idx].as_deref() else { return h };
                let tr = src.tile_rect(idx);
                let c = tr.intersect(&rect);
                for y in c.y..c.bottom() {
                    let row = (y - tr.y) as usize * TILE;
                    for x in c.x..c.right() {
                        let i = (row + (x - tr.x) as usize) * 4;
                        let p = [tile.px[i], tile.px[i + 1], tile.px[i + 2], tile.px[i + 3]];
                        if p[3] == 0 {
                            continue;
                        }
                        let s = unpremul(p);
                        for ch in 0..3 {
                            h.bins[ch][s[ch] as usize] += 1;
                        }
                    }
                }
                h
            })
            .reduce(HistogramRgb::empty, |mut a, b| {
                for ch in 0..3 {
                    for v in 0..256 {
                        a.bins[ch][v] += b.bins[ch][v];
                    }
                }
                a
            })
    }

    fn mean(&self) -> [f32; 3] {
        let mut out = [0.0; 3];
        for (ch, bins) in self.bins.iter().enumerate() {
            let (mut weighted, mut sum) = (0u64, 0u64);
            for (v, &n) in bins.iter().enumerate() {
                weighted += v as u64 * n;
                sum += n;
            }
            out[ch] = if sum == 0 { 0.0 } else { weighted as f32 / sum as f32 };
        }
        out
    }

    fn mean_color(&self) -> [u8; 3] {
        self.mean().map(|m| (m + 0.5) as u8)
    }

    /// First value whose cumulative count exceeds `fraction` of the total.
    fn percentile_color(&self, fraction: f32) -> [u8; 3] {
        let mut out = [0u8; 3];
        for (ch, bins) in self.bins.iter().enumerate() {
            let sum: u64 = bins.iter().sum();
            let limit = sum as f32 * fraction;
            let mut integral = 0u64;
            for (v, &n) in bins.iter().enumerate() {
                integral += n;
                if integral as f32 > limit {
                    out[ch] = v as u8;
                    break;
                }
            }
        }
        out
    }

    fn make_levels_auto(&self) -> LevelOp {
        LevelOp::auto_from_lo_md_hi(self.percentile_color(0.005), self.mean_color(), self.percentile_color(0.995))
    }

    /// The histogram the image would have after applying `op` (Paint.NET's
    /// "leveled histogram" estimate used for the Levels output preview).
    fn leveled(&self, op: &LevelOp) -> HistogramRgb {
        let mut out = HistogramRgb::empty();
        for c in 0..3 {
            let input = &self.bins[c];
            for v in 0..256usize {
                let (before, slope) = op.unapply(v as u8, c);
                let (lo, hi) = (op.in_lo[c] as usize, op.in_hi[c] as usize);
                out.bins[c][v] = if v as u8 > op.out_hi[c]
                    || (v as u8) < op.out_lo[c]
                    || before.is_nan()
                    || (before.floor() as i32) < 0
                    || (before.ceil() as i32) > 255
                {
                    0
                } else if before <= lo as f32 {
                    input[..=lo].iter().sum()
                } else if before >= hi as f32 {
                    input[hi..].iter().sum()
                } else {
                    let f = before.floor();
                    let a = input[f as usize] as f32;
                    let b = input[before.ceil() as usize] as f32;
                    (slope * (a + (b - a) * (before - f))).max(0.0) as u64
                };
            }
        }
        out
    }
}

/// Paint a histogram: one translucent bar chart per channel.
fn draw_histogram(ui: &mut egui::Ui, hist: Option<&HistogramRgb>, selected: [bool; 3], size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    p.rect_stroke(rect, 2.0, ui.visuals().widgets.noninteractive.bg_stroke, StrokeKind::Inside);
    let Some(h) = hist else { return };
    let max = (0..3).filter(|&c| selected[c]).flat_map(|c| h.bins[c].iter().copied()).max().unwrap_or(0);
    if max == 0 {
        return;
    }
    let colors = [Color32::from_rgba_unmultiplied(230, 40, 40, 110), Color32::from_rgba_unmultiplied(40, 200, 40, 110), Color32::from_rgba_unmultiplied(50, 90, 240, 110)];
    let bw = rect.width() / 256.0;
    for c in 0..3 {
        if !selected[c] {
            continue;
        }
        for v in 0..256 {
            let n = h.bins[c][v];
            if n == 0 {
                continue;
            }
            let x = rect.left() + (v as f32 + 0.5) * bw;
            let top = rect.bottom() - (n as f32 / max as f32) * (rect.height() - 2.0);
            p.line_segment([Pos2::new(x, rect.bottom()), Pos2::new(x, top)], Stroke::new(bw.max(1.0), colors[c]));
        }
    }
}

fn swatch(ui: &mut egui::Ui, rgb: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(22.0, 14.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    ui.painter().rect_stroke(rect, 2.0, ui.visuals().widgets.noninteractive.fg_stroke, StrokeKind::Inside);
}

// ---------------------------------------------------------------------------
// Invert Colors / Black and White / Sepia
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct InvertColors;
impl Effect for InvertColors {
    fn name(&self) -> &'static str {
        "Invert Colors"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-invertcolors-symbolic"
    }
    fn has_dialog(&self) -> bool {
        false
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        map_straight(dst, |c| [255 - c[0], 255 - c[1], 255 - c[2], c[3]]);
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
pub struct BlackAndWhite;
impl Effect for BlackAndWhite {
    fn name(&self) -> &'static str {
        "Black and White"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-blackandwhite-symbolic"
    }
    fn has_dialog(&self) -> bool {
        false
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        map_straight(dst, |c| {
            let i = intensity(c);
            [i, i, i, c[3]]
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

/// Desaturate, then a per-channel gamma level (R 0.8, G 1.0, B 1.2), then
/// blend with the original by strength.
#[derive(Clone)]
pub struct Sepia {
    pub strength: i32,
}
impl Effect for Sepia {
    fn name(&self) -> &'static str {
        "Sepia"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-sepia-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        slider_i(ui, "Strength", &mut self.strength, 0, 100)
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        let mut lut = [[0u8; 256]; 3];
        for (ch, g) in [0.8f32, 1.0, 1.2].iter().enumerate() {
            for v in 0..256 {
                lut[ch][v] = ((v as f32 / 255.0).powf(*g) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        let k = self.strength as f32 / 100.0;
        map_straight(dst, |c| {
            let i = intensity(c) as usize;
            let s = [lut[0][i], lut[1][i], lut[2][i]];
            let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
            [l(c[0], s[0]), l(c[1], s[1]), l(c[2], s[2]), c[3]]
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Levels operation (shared by Auto Level and Levels)
// ---------------------------------------------------------------------------

const MIN_GAMMA: f32 = 0.1;
const MAX_GAMMA: f32 = 10.0;

/// Paint.NET's Level pixel op: per channel input black/white points, gamma
/// and output black/white points. Arrays are in R, G, B order.
#[derive(Clone, Copy, PartialEq, Debug)]
struct LevelOp {
    in_lo: [u8; 3],
    in_hi: [u8; 3],
    out_lo: [u8; 3],
    out_hi: [u8; 3],
    gamma: [f32; 3],
}

impl Default for LevelOp {
    fn default() -> Self {
        LevelOp { in_lo: [0; 3], in_hi: [255; 3], out_lo: [0; 3], out_hi: [255; 3], gamma: [1.0; 3] }
    }
}

impl LevelOp {
    fn auto_from_lo_md_hi(lo: [u8; 3], md: [u8; 3], hi: [u8; 3]) -> Self {
        let mut gamma = [1.0f32; 3];
        for c in 0..3 {
            if lo[c] < md[c] && md[c] < hi[c] {
                let x = (md[c] - lo[c]) as f32 / (hi[c] - lo[c]) as f32;
                gamma[c] = (0.5f32.ln() / x.ln()).clamp(MIN_GAMMA, MAX_GAMMA);
            }
        }
        LevelOp { in_lo: lo, in_hi: hi, out_lo: [0; 3], out_hi: [255; 3], gamma }
    }

    fn is_valid(&self) -> bool {
        (0..3).all(|c| self.out_hi[c] >= self.out_lo[c] && self.in_hi[c] > self.in_lo[c] && self.gamma[c] >= 0.0)
    }

    fn apply_channel(&self, c: usize, x: f32) -> u8 {
        let lo = self.in_lo[c] as f32;
        let v = x - lo;
        if v < 0.0 {
            self.out_lo[c]
        } else if v + lo >= self.in_hi[c] as f32 {
            self.out_hi[c]
        } else {
            let t = (v / (self.in_hi[c] as f32 - lo)) as f64;
            let span = self.out_hi[c] as f64 - self.out_lo[c] as f64;
            (self.out_lo[c] as f64 + span * t.powf(self.gamma[c] as f64)).clamp(0.0, 255.0) as u8
        }
    }

    fn apply_rgb(&self, rgb: [u8; 3]) -> [u8; 3] {
        [0, 1, 2].map(|c| self.apply_channel(c, rgb[c] as f32))
    }

    fn luts(&self) -> [Lut; 3] {
        let mut l = [[0u8; 256]; 3];
        for (c, lut) in l.iter_mut().enumerate() {
            for (v, o) in lut.iter_mut().enumerate() {
                *o = self.apply_channel(c, v as f32);
            }
        }
        l
    }

    /// Inverse mapping for the output histogram: input value and slope.
    fn unapply(&self, after: u8, c: usize) -> (f32, f32) {
        let in_span = self.in_hi[c] as f32 - self.in_lo[c] as f32;
        let out_span = self.out_hi[c] as f32 - self.out_lo[c] as f32;
        let t = (after as f32 - self.out_lo[c] as f32) / out_span;
        let g = self.gamma[c];
        let before = self.in_lo[c] as f32 + in_span * t.powf(1.0 / g);
        let mut slope = in_span / (out_span * g) * t.powf(1.0 / g - 1.0);
        if !slope.is_finite() {
            slope = 0.0;
        }
        (before, slope)
    }

    // Setters keep low < high on every channel, like Paint.NET's properties.
    fn set_in_lo(&mut self, v: [u8; 3]) {
        for c in 0..3 {
            let lo = v[c].min(254);
            if self.in_hi[c] < lo + 1 {
                self.in_hi[c] = lo + 1;
            }
            self.in_lo[c] = lo;
        }
    }
    fn set_in_hi(&mut self, v: [u8; 3]) {
        for c in 0..3 {
            let hi = v[c].max(1);
            if self.in_lo[c] > hi - 1 {
                self.in_lo[c] = hi - 1;
            }
            self.in_hi[c] = hi;
        }
    }
    fn set_out_lo(&mut self, v: [u8; 3]) {
        for c in 0..3 {
            let lo = v[c].min(254);
            if self.out_hi[c] < lo + 1 {
                self.out_hi[c] = lo + 1;
            }
            self.out_lo[c] = lo;
        }
    }
    fn set_out_hi(&mut self, v: [u8; 3]) {
        for c in 0..3 {
            let hi = v[c].max(1);
            if self.out_lo[c] > hi - 1 {
                self.out_lo[c] = hi - 1;
            }
            self.out_hi[c] = hi;
        }
    }
}

// ---------------------------------------------------------------------------
// Auto Level
// ---------------------------------------------------------------------------

/// Levels computed automatically from the whole image's histogram: 0.5% and
/// 99.5% percentiles become the black and white points, and the gamma maps
/// the mean colour to mid-grey.
#[derive(Clone, Default)]
pub struct AutoLevel {
    op: Option<LevelOp>,
}

impl Effect for AutoLevel {
    fn name(&self) -> &'static str {
        "Auto Level"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-autolevel-symbolic"
    }
    fn has_dialog(&self) -> bool {
        false
    }
    fn prepare(&mut self, src: &Surface, _ctx: &RenderCtx) {
        self.op = Some(HistogramRgb::compute(src, src.bounds()).make_levels_auto());
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        if let Some(op) = self.op.filter(LevelOp::is_valid) {
            apply_luts(dst, &op.luts());
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Brightness / Contrast
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct BrightnessContrast {
    pub brightness: i32,
    pub contrast: i32,
}

impl BrightnessContrast {
    /// Per-intensity shift added to each channel, or None for the
    /// contrast = 100 threshold case (which returns a per-intensity grey).
    fn table(&self) -> Result<[i32; 256], Lut> {
        let (b, c) = (self.brightness, self.contrast);
        let (mul, div) = match c {
            c if c < 0 => (c + 100, 100),
            c if c > 0 => (100, 100 - c),
            _ => (1, 1),
        };
        if div == 0 {
            let mut t = [0u8; 256];
            for (i, o) in t.iter_mut().enumerate() {
                *o = if i as i32 + b < 128 { 0 } else { 255 };
            }
            return Err(t);
        }
        let mut shift = [0i32; 256];
        for (i, s) in shift.iter_mut().enumerate() {
            let i = i as i32;
            *s = if div == 100 { (i - 127) * mul / div + 127 - i + b } else { (i - 127 + b) * mul / div + 127 - i };
        }
        Ok(shift)
    }
}

impl Effect for BrightnessContrast {
    fn name(&self) -> &'static str {
        "Brightness / Contrast"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-brightnesscontrast-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Brightness", &mut self.brightness, -100, 100);
        c |= slider_i(ui, "Contrast", &mut self.contrast, -100, 100);
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        match self.table() {
            Ok(shift) => {
                if shift.iter().all(|&s| s == 0) {
                    return;
                }
                map_straight(dst, |c| {
                    let s = shift[intensity(c) as usize];
                    [clamp_byte(c[0] as i32 + s), clamp_byte(c[1] as i32 + s), clamp_byte(c[2] as i32 + s), c[3]]
                });
            }
            Err(grey) => map_straight(dst, |c| {
                let g = grey[intensity(c) as usize];
                [g, g, g, c[3]]
            }),
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/// Natural cubic spline through sorted control points (Numerical Recipes
/// 3.3), as Pinta's SplineInterpolator.
struct Spline {
    xs: Vec<f64>,
    ys: Vec<f64>,
    y2: Vec<f64>,
}

impl Spline {
    fn new(points: &BTreeMap<i32, i32>) -> Self {
        let xs: Vec<f64> = points.keys().map(|&x| x as f64).collect();
        let ys: Vec<f64> = points.values().map(|&y| y as f64).collect();
        let n = xs.len();
        let mut y2 = vec![0.0; n];
        let mut u = vec![0.0; n];
        for i in 1..n.saturating_sub(1) {
            let wx = xs[i + 1] - xs[i - 1];
            let sig = (xs[i] - xs[i - 1]) / wx;
            let p = sig * y2[i - 1] + 2.0;
            y2[i] = (sig - 1.0) / p;
            let dd = (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]) - (ys[i] - ys[i - 1]) / (xs[i] - xs[i - 1]);
            u[i] = (6.0 * dd / wx - sig * u[i - 1]) / p;
        }
        if n > 0 {
            y2[n - 1] = 0.0;
        }
        for i in (0..n.saturating_sub(1)).rev() {
            y2[i] = y2[i] * y2[i + 1] + u[i];
        }
        Spline { xs, ys, y2 }
    }

    fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if n == 0 {
            return x;
        }
        if n == 1 {
            return self.ys[0];
        }
        let (mut lo, mut hi) = (0, n - 1);
        while hi - lo > 1 {
            let k = (hi + lo) >> 1;
            if self.xs[k] > x {
                hi = k;
            } else {
                lo = k;
            }
        }
        let h = self.xs[hi] - self.xs[lo];
        let a = (self.xs[hi] - x) / h;
        let b = (x - self.xs[lo]) / h;
        a * self.ys[lo] + b * self.ys[hi] + ((a * a * a - a) * self.y2[lo] + (b * b * b - b) * self.y2[hi]) * (h * h) / 6.0
    }

    fn lut(points: &BTreeMap<i32, i32>) -> Lut {
        let s = Spline::new(points);
        let mut t = [0u8; 256];
        for (i, o) in t.iter_mut().enumerate() {
            *o = s.eval(i as f64).clamp(0.0, 255.0) as u8;
        }
        t
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransferMode {
    Rgb,
    Luminosity,
}

const CURVE_SIZE: i32 = 256;
const CP_RADIUS: f32 = 6.0;

fn default_curve() -> BTreeMap<i32, i32> {
    BTreeMap::from([(0, 0), (CURVE_SIZE - 1, CURVE_SIZE - 1)])
}

#[derive(Clone)]
pub struct Curves {
    pub mode: TransferMode,
    /// Luminosity transfer curve control points (x -> y, both 0..255).
    pub luminosity: BTreeMap<i32, i32>,
    /// Red, green and blue curves for RGB mode.
    pub rgb: [BTreeMap<i32, i32>; 3],
    /// Which RGB curves are edited (Pinta's Red/Green/Blue check boxes).
    pub edit: [bool; 3],
    // Editor state.
    last_cpx: Option<i32>,
    orig_cps: BTreeSet<i32>,
    mouse: Option<(i32, i32)>,
    last_drag: Option<(i32, i32)>,
    dragging: bool,
}

impl Default for Curves {
    fn default() -> Self {
        Curves {
            mode: TransferMode::Luminosity,
            luminosity: default_curve(),
            rgb: [default_curve(), default_curve(), default_curve()],
            edit: [true; 3],
            last_cpx: None,
            orig_cps: BTreeSet::new(),
            mouse: None,
            last_drag: None,
            dragging: false,
        }
    }
}

impl Curves {
    fn active_indices(&self) -> Vec<usize> {
        match self.mode {
            TransferMode::Luminosity => vec![0],
            TransferMode::Rgb => (0..3).filter(|&i| self.edit[i]).collect(),
        }
    }

    fn curve_mut(&mut self, i: usize) -> &mut BTreeMap<i32, i32> {
        match self.mode {
            TransferMode::Luminosity => &mut self.luminosity,
            TransferMode::Rgb => &mut self.rgb[i],
        }
    }

    fn curves(&self) -> Vec<&BTreeMap<i32, i32>> {
        match self.mode {
            TransferMode::Luminosity => vec![&self.luminosity],
            TransferMode::Rgb => self.rgb.iter().collect(),
        }
    }

    fn near(cp: (i32, i32), pos: (i32, i32)) -> bool {
        let (dx, dy) = ((cp.0 - pos.0) as f32, (cp.1 - pos.1) as f32);
        (dx * dx + dy * dy).sqrt() < CP_RADIUS
    }

    /// Control points in widget space (y down).
    fn widget_points(curve: &BTreeMap<i32, i32>) -> impl Iterator<Item = (i32, i32)> + '_ {
        curve.iter().map(|(&x, &y)| (x, CURVE_SIZE - 1 - y))
    }

    fn add_control_point(&mut self, p: (i32, i32)) {
        for i in self.active_indices() {
            self.curve_mut(i).insert(p.0, CURVE_SIZE - 1 - p.1);
        }
        self.last_cpx = Some(p.0);
    }

    fn press(&mut self, mut pos: (i32, i32)) {
        self.orig_cps.clear();
        for i in self.active_indices() {
            let keys: Vec<i32> = self.curve_mut(i).keys().copied().collect();
            self.orig_cps.extend(keys);
        }
        // Snap onto a nearby control point so it can be dragged.
        'snap: for i in self.active_indices() {
            for cp in Self::widget_points(self.curve_mut(i)).collect::<Vec<_>>() {
                if Self::near(cp, pos) {
                    pos = cp;
                    self.orig_cps.remove(&pos.0);
                    break 'snap;
                }
            }
        }
        self.add_control_point(pos);
    }

    fn drag(&mut self, mut p: (i32, i32)) -> bool {
        if p.0 < 0 || p.0 >= CURVE_SIZE || p.1 < 0 || p.1 >= CURVE_SIZE {
            return false;
        }
        let mut changed = false;
        if let Some(last) = self.last_cpx {
            // End points can only move vertically.
            if last == 0 {
                p.0 = 0;
            } else if last == CURVE_SIZE - 1 {
                p.0 = CURVE_SIZE - 1;
            } else {
                for i in self.active_indices() {
                    changed |= self.curve_mut(i).remove(&last).is_some();
                }
            }
        }
        if !self.orig_cps.contains(&p.0) {
            self.add_control_point(p);
            changed = true;
        } else {
            self.last_cpx = None;
        }
        changed
    }

    fn remove_near(&mut self, pos: (i32, i32)) -> bool {
        let mut changed = false;
        for i in self.active_indices() {
            let curve = self.curve_mut(i);
            let hit = Self::widget_points(curve).find(|&cp| {
                let endpoint = (cp.0 == 0 && cp.1 == CURVE_SIZE - 1) || (cp.0 == CURVE_SIZE - 1 && cp.1 == 0);
                !endpoint && Self::near(cp, pos)
            });
            if let Some(cp) = hit {
                curve.remove(&cp.0);
                changed = true;
            }
        }
        changed
    }

    fn reset(&mut self) {
        match self.mode {
            TransferMode::Luminosity => self.luminosity = default_curve(),
            TransferMode::Rgb => self.rgb = [default_curve(), default_curve(), default_curve()],
        }
    }

    fn graph(&mut self, ui: &mut egui::Ui) -> bool {
        let size = CURVE_SIZE as f32;
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
        let to_px = |p: Pos2| ((p.x - rect.min.x).floor() as i32, (p.y - rect.min.y).floor() as i32);
        let (primary_pressed, secondary_pressed, primary_down, latest) = ui.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
            )
        });
        let mut changed = false;
        let hovered = resp.hovered();
        self.mouse = if hovered || self.dragging { latest.map(to_px) } else { None };
        if hovered && primary_pressed {
            if let Some(p) = latest.map(to_px) {
                self.press(p);
                self.dragging = true;
                self.last_drag = Some(p);
                changed = true;
            }
        } else if self.dragging && primary_down {
            if let Some(p) = latest.map(to_px) {
                if Some(p) != self.last_drag {
                    self.last_drag = Some(p);
                    changed |= self.drag(p);
                }
            }
        }
        if !primary_down {
            self.dragging = false;
        }
        if hovered && secondary_pressed {
            if let Some(p) = latest.map(to_px) {
                changed |= self.remove_near(p);
            }
        }

        // Drawing.
        let painter = ui.painter_at(rect.expand(4.0));
        let fg = ui.visuals().text_color();
        let at = |x: f32, y: f32| Pos2::new(rect.min.x + x, rect.min.y + y);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, fg), StrokeKind::Inside);
        if let Some((mx, my)) = self.mouse.filter(|&(x, y)| (0..CURVE_SIZE).contains(&x) && (0..CURVE_SIZE).contains(&y)) {
            let s = Stroke::new(0.5, fg);
            painter.line_segment([at(mx as f32 + 0.5, 0.0), at(mx as f32 + 0.5, size)], s);
            painter.line_segment([at(0.0, my as f32 + 0.5), at(size, my as f32 + 0.5)], s);
        }
        let grid = Stroke::new(1.0, fg.gamma_multiply(0.35));
        for i in 1..4 {
            let v = i as f32 * size / 4.0;
            painter.extend(egui::Shape::dashed_line(&[at(v, 0.0), at(v, size)], grid, 4.0, 4.0));
            painter.extend(egui::Shape::dashed_line(&[at(0.0, v), at(size, v)], grid, 4.0, 4.0));
        }
        painter.extend(egui::Shape::dashed_line(&[at(0.0, size - 1.0), at(size - 1.0, 0.0)], grid, 4.0, 4.0));

        let infos: Vec<(Color32, bool)> = match self.mode {
            TransferMode::Luminosity => vec![(fg, true)],
            TransferMode::Rgb => vec![
                (Color32::from_rgb(230, 0, 0), self.edit[0]),
                (Color32::from_rgb(0, 230, 0), self.edit[1]),
                (Color32::from_rgb(0, 0, 230), self.edit[2]),
            ],
        };
        for (curve, &(color, active)) in self.curves().into_iter().zip(&infos) {
            let s = Spline::new(curve);
            let pts: Vec<Pos2> = (0..CURVE_SIZE).map(|i| at(i as f32, (size - 1.0 - s.eval(i as f64) as f32).clamp(0.0, size - 1.0))).collect();
            painter.line(pts, Stroke::new(if active { 2.0 } else { 1.0 }, color));
        }
        for (curve, &(color, active)) in self.curves().into_iter().zip(&infos) {
            for cp in Self::widget_points(curve) {
                let c = at(cp.0 as f32, cp.1 as f32);
                if active {
                    if self.mouse.is_some_and(|m| Self::near(cp, m)) {
                        painter.circle_stroke(c, (CP_RADIUS + 2.0) / 2.0, Stroke::new(2.0, Color32::from_gray(51)));
                        painter.circle_filled(c, CP_RADIUS / 2.0, Color32::from_gray(230));
                    } else {
                        painter.circle_stroke(c, CP_RADIUS / 2.0, Stroke::new(2.0, color));
                    }
                }
                painter.circle_filled(c, (CP_RADIUS - 2.0) / 2.0, color);
            }
        }
        if self.dragging || hovered {
            ui.ctx().request_repaint();
        }
        changed
    }
}

impl Effect for Curves {
    fn name(&self) -> &'static str {
        "Curves"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-curves-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Transfer Map");
            let before = self.mode;
            egui::ComboBox::from_id_salt("curves_transfer_map")
                .selected_text(match self.mode {
                    TransferMode::Rgb => "RGB",
                    TransferMode::Luminosity => "Luminosity",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.mode, TransferMode::Rgb, "RGB");
                    ui.selectable_value(&mut self.mode, TransferMode::Luminosity, "Luminosity");
                });
            changed |= before != self.mode;
            if let Some((x, y)) = self.mouse.filter(|&(x, y)| (0..CURVE_SIZE).contains(&x) && (0..CURVE_SIZE).contains(&y)) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("({}, {})", x, CURVE_SIZE - 1 - y));
                });
            }
        });
        changed |= self.graph(ui);
        ui.horizontal(|ui| {
            if self.mode == TransferMode::Rgb {
                ui.checkbox(&mut self.edit[0], "Red");
                ui.checkbox(&mut self.edit[1], "Green");
                ui.checkbox(&mut self.edit[2], "Blue");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reset").clicked() {
                    self.reset();
                    changed = true;
                }
            });
        });
        ui.label("Tip: Right-click to remove control points.");
        changed
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        match self.mode {
            TransferMode::Rgb => apply_luts(dst, &[Spline::lut(&self.rgb[0]), Spline::lut(&self.rgb[1]), Spline::lut(&self.rgb[2])]),
            TransferMode::Luminosity => {
                let curve = Spline::lut(&self.luminosity);
                if curve == IDENTITY_LUT {
                    return;
                }
                map_straight(dst, |c| {
                    let l = intensity(c);
                    let d = curve[l as usize] as i32 - l as i32;
                    [clamp_byte(c[0] as i32 + d), clamp_byte(c[1] as i32 + d), clamp_byte(c[2] as i32 + d), c[3]]
                });
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Hue / Saturation
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct HueSaturation {
    pub hue: i32,
    pub saturation: i32,
    pub lightness: i32,
}

impl Default for HueSaturation {
    fn default() -> Self {
        HueSaturation { hue: 0, saturation: 100, lightness: 0 }
    }
}

/// RGB in 0..1 -> (hue 0..360, sat 0..1, val 0..1).
fn to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if max == 0.0 || d == 0.0 {
        return (0.0, 0.0, max);
    }
    let mut h = if r == max {
        (g - b) / d
    } else if g == max {
        2.0 + (b - r) / d
    } else {
        4.0 + (r - g) / d
    } * 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, d / max, max)
}

fn from_hsv(h: f64, s: f64, v: f64) -> [f64; 3] {
    if s == 0.0 {
        return [v; 3];
    }
    let pos = (h % 360.0) / 60.0;
    let sector = pos.floor();
    let f = pos - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match sector as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        5 => [v, p, q],
        _ => [0.0; 3],
    }
}

impl HueSaturation {
    fn is_default(&self) -> bool {
        self.hue == 0 && self.saturation == 100 && self.lightness == 0
    }

    fn apply(&self, c: [u8; 4], sat_factor: i32, blend: Option<(i32, i32)>) -> [u8; 4] {
        let i = intensity(c) as i32;
        let sat = |v: u8| clamp_byte((i * 1024 + (v as i32 - i) * sat_factor) >> 10);
        let (r, g, b) = (sat(c[0]), sat(c[1]), sat(c[2]));
        let (h, s, v) = to_hsv(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let mut hue = h as i32 + self.hue;
        while hue < 0 {
            hue += 360;
        }
        while hue > 360 {
            hue -= 360;
        }
        let rgb = from_hsv(hue as f64, s, v).map(|x| (x * 255.0).round().clamp(0.0, 255.0) as i32);
        let out = match blend {
            None => rgb,
            Some((value, a)) => rgb.map(|x| (x * (255 - a) + value * a) / 256),
        };
        [out[0] as u8, out[1] as u8, out[2] as u8, c[3]]
    }
}

impl Effect for HueSaturation {
    fn name(&self) -> &'static str {
        "Hue / Saturation"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-huesaturation-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Hue", &mut self.hue, -180, 180);
        c |= slider_i(ui, "Saturation", &mut self.saturation, 0, 200);
        c |= slider_i(ui, "Lightness", &mut self.lightness, -100, 100);
        c
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        if self.is_default() {
            return;
        }
        let sat_factor = self.saturation * 1024 / 100;
        // Lightness blends towards white or black with alpha |l| * 255 / 100.
        let blend = match self.lightness {
            0 => None,
            l if l > 0 => Some((255, l * 255 / 100)),
            l => Some((0, -l * 255 / 100)),
        };
        map_straight(dst, |c| self.apply(c, sat_factor, blend));
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Levels
// ---------------------------------------------------------------------------

/// Histogram of the image being adjusted, filled in by prepare() so the
/// dialog (which edits a different clone of the effect) can show it and
/// compute Auto levels.
type SharedHistogram = Arc<Mutex<Option<Arc<HistogramRgb>>>>;

#[derive(Clone)]
pub struct Levels {
    op: LevelOp,
    /// Channels the dialog edits (R, G, B).
    pub mask: [bool; 3],
    pub in_low: i32,
    pub in_high: i32,
    pub out_low: i32,
    pub out_high: i32,
    pub gamma: f64,
    histogram: SharedHistogram,
}

impl Default for Levels {
    fn default() -> Self {
        Levels {
            op: LevelOp::default(),
            mask: [true; 3],
            in_low: 0,
            in_high: 255,
            out_low: 0,
            out_high: 255,
            gamma: 1.0,
            histogram: Arc::new(Mutex::new(None)),
        }
    }
}

impl Levels {
    fn mask_avg(&self, c: [u8; 3]) -> i32 {
        let v: Vec<i32> = (0..3).filter(|&i| self.mask[i]).map(|i| c[i] as i32).collect();
        if v.is_empty() { 0 } else { v.iter().sum::<i32>() / v.len() as i32 }
    }

    fn mask_gamma(&self) -> f32 {
        let v: Vec<f32> = (0..3).filter(|&i| self.mask[i]).map(|i| self.op.gamma[i]).collect();
        if v.is_empty() { 1.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
    }

    /// Change the masked channels of `before` so their average becomes
    /// `val`: scale them proportionally first, then shift.
    fn update_by_mask(&self, before: [u8; 3], val: u8) -> [u8; 3] {
        if !self.mask.iter().any(|&m| m) {
            return before;
        }
        let val = val as i32;
        let mut after = before;
        let mut average = -1;
        for _ in 0..256 {
            let old = average;
            average = self.mask_avg(after);
            if average == 0 {
                break;
            }
            let factor = val as f32 / average as f32;
            for c in 0..3 {
                if self.mask[c] {
                    after[c] = (after[c] as f32 * factor).clamp(0.0, 255.0) as u8;
                }
            }
            if average == val || old == average {
                break;
            }
        }
        for _ in 0..1024 {
            average = self.mask_avg(after);
            if average == val {
                break;
            }
            let diff = val - average;
            for c in 0..3 {
                if self.mask[c] {
                    after[c] = clamp_byte(after[c] as i32 + diff);
                }
            }
        }
        after
    }

    fn update_gamma_by_mask(&mut self, val: f32) {
        let val = val.clamp(MIN_GAMMA, MAX_GAMMA);
        if !self.mask.iter().any(|&m| m) {
            return;
        }
        for _ in 0..1000 {
            let average = self.mask_gamma();
            let factor = val / average;
            for c in 0..3 {
                if self.mask[c] {
                    self.op.gamma[c] = (factor * self.op.gamma[c]).clamp(MIN_GAMMA, MAX_GAMMA);
                }
            }
            if (val - average).abs() <= 0.001 {
                break;
            }
        }
    }

    /// Push the dialog values into the level op (Pinta's UpdateLevels).
    fn update_levels(&mut self) {
        let v = self.update_by_mask(self.op.out_hi, self.out_high as u8);
        self.op.set_out_hi(v);
        let v = self.update_by_mask(self.op.out_lo, self.out_low as u8);
        self.op.set_out_lo(v);
        self.update_gamma_by_mask(self.gamma as f32);
        let v = self.update_by_mask(self.op.in_hi, self.in_high as u8);
        self.op.set_in_hi(v);
        let v = self.update_by_mask(self.op.in_lo, self.in_low as u8);
        self.op.set_in_lo(v);
    }

    /// Show the op's (masked average) values in the dialog fields.
    fn update_from_op(&mut self) {
        self.in_high = self.mask_avg(self.op.in_hi);
        self.in_low = self.mask_avg(self.op.in_lo);
        self.gamma = self.mask_gamma() as f64;
        self.out_high = self.mask_avg(self.op.out_hi);
        self.out_low = self.mask_avg(self.op.out_lo);
    }

    fn auto(&mut self, hist: &HistogramRgb) {
        self.op = hist.make_levels_auto();
        self.update_from_op();
        self.update_levels();
    }

    fn reset(&mut self) {
        self.in_low = 0;
        self.in_high = 255;
        self.out_low = 0;
        self.out_high = 255;
        self.gamma = 1.0;
        self.update_levels();
    }
}

impl Effect for Levels {
    fn name(&self) -> &'static str {
        "Levels"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-levels-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let hist = self.histogram.lock().ok().and_then(|h| h.clone());
        if hist.is_none() {
            ui.ctx().request_repaint();
        }
        let mut changed = false;
        let mut edited = false;
        let hist_size = Vec2::new(ui.available_width().clamp(200.0, 300.0), 56.0);

        ui.label("Input Histogram");
        draw_histogram(ui, hist.as_deref(), self.mask, hist_size);
        ui.add_space(4.0);
        egui::Grid::new("levels_grid").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.strong("Input");
            ui.label("");
            ui.label("");
            ui.end_row();
            ui.label("White point");
            edited |= ui.add(egui::DragValue::new(&mut self.in_high).range(1..=255)).changed();
            swatch(ui, self.op.in_hi);
            ui.end_row();
            ui.label("Black point");
            edited |= ui.add(egui::DragValue::new(&mut self.in_low).range(0..=254)).changed();
            swatch(ui, self.op.in_lo);
            ui.end_row();

            ui.strong("Output");
            ui.label("");
            ui.label("");
            ui.end_row();
            ui.label("White point");
            edited |= ui.add(egui::DragValue::new(&mut self.out_high).range(2..=255)).changed();
            swatch(ui, self.op.out_hi);
            ui.end_row();
            ui.label("Gamma");
            edited |= ui.add(egui::DragValue::new(&mut self.gamma).range(0.1..=10.0).speed(0.01).fixed_decimals(2)).changed();
            match &hist {
                Some(h) => swatch(ui, self.op.apply_rgb(h.mean_color())),
                None => {
                    ui.label("");
                }
            }
            ui.end_row();
            ui.label("Black point");
            edited |= ui.add(egui::DragValue::new(&mut self.out_low).range(0..=252)).changed();
            swatch(ui, self.op.out_lo);
            ui.end_row();
        });
        if edited {
            self.update_levels();
            changed = true;
        }
        ui.add_space(4.0);
        ui.label("Output Histogram");
        let leveled = hist.as_ref().map(|h| h.leveled(&self.op));
        draw_histogram(ui, leveled.as_ref(), self.mask, hist_size);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.mask[0], "Red");
            ui.checkbox(&mut self.mask[1], "Green");
            ui.checkbox(&mut self.mask[2], "Blue");
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(hist.is_some(), egui::Button::new("Auto")).clicked() {
                if let Some(h) = &hist {
                    self.auto(h);
                    changed = true;
                }
            }
            if ui.button("Reset").clicked() {
                self.reset();
                changed = true;
            }
        });
        changed
    }
    fn prepare(&mut self, src: &Surface, ctx: &RenderCtx) {
        let h = HistogramRgb::compute(src, ctx.sel_bounds);
        if let Ok(mut slot) = self.histogram.lock() {
            *slot = Some(Arc::new(h));
        }
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        if self.op.is_valid() {
            apply_luts(dst, &self.op.luts());
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Posterize
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Posterize {
    pub red: i32,
    pub green: i32,
    pub blue: i32,
    pub linked: bool,
}

impl Default for Posterize {
    fn default() -> Self {
        Posterize { red: 16, green: 16, blue: 16, linked: true }
    }
}

/// Paint.NET's level table: `n` evenly spaced output values, with the input
/// range split into `n` (nearly) equal bands.
fn posterize_levels(n: i32) -> Lut {
    let n = n.clamp(2, 256) as usize;
    let steps: Vec<u8> = (0..n).map(|i| (255 * i / (n - 1)) as u8).collect();
    let mut t = [0u8; 256];
    let (mut j, mut k) = (0usize, 0usize);
    for o in t.iter_mut() {
        *o = steps[j.min(n - 1)];
        k += n;
        if k > 255 {
            k -= 255;
            j += 1;
        }
    }
    t
}

impl Effect for Posterize {
    fn name(&self) -> &'static str {
        "Posterize"
    }
    fn category(&self) -> Category {
        Category::Adjustment
    }
    fn icon(&self) -> &'static str {
        "adjustments-posterize-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let before = [self.red, self.green, self.blue];
        slider_i(ui, "Red", &mut self.red, 2, 64);
        slider_i(ui, "Green", &mut self.green, 2, 64);
        slider_i(ui, "Blue", &mut self.blue, 2, 64);
        ui.checkbox(&mut self.linked, "Linked");
        let now = [self.red, self.green, self.blue];
        if self.linked {
            if let Some(i) = (0..3).find(|&i| now[i] != before[i]) {
                let v = now[i];
                (self.red, self.green, self.blue) = (v, v, v);
            }
        }
        [self.red, self.green, self.blue] != before
    }
    fn render(&self, _src: &Src, dst: &mut [u8], _r: IRect, _ctx: &RenderCtx) {
        apply_luts(dst, &[posterize_levels(self.red), posterize_levels(self.green), posterize_levels(self.blue)]);
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

    fn ctx_for(w: u32, h: u32, sel: IRect) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: sel, primary: [0, 0, 0, 255], secondary: [255; 4] }
    }

    /// 700x500 gradients with a translucent band and a transparent block.
    fn test_image(w: u32, h: u32) -> Surface {
        let mut d = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(2).saturating_sub(1).max(1)) as u8;
                let g = (y * 255 / h.max(2).saturating_sub(1).max(1)) as u8;
                let b = ((x * 3 + y * 7) % 256) as u8;
                let a = if x > w / 2 && y < h / 5 {
                    0
                } else if y > h * 3 / 4 {
                    (40 + (x % 216)) as u8
                } else {
                    255
                };
                d.extend_from_slice(&premul([r, g, b, a]));
            }
        }
        Surface::from_rgba(w, h, &d)
    }

    fn surface_from_straight(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Surface {
        let mut d = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&premul(f(x, y)));
            }
        }
        Surface::from_rgba(w, h, &d)
    }

    fn run(e: &mut dyn Effect, src: &Surface, sel: &Selection) -> Surface {
        let r = sel.bounds(src.width, src.height);
        apply(e, src, sel, &ctx_for(src.width, src.height, r), &Progress::new()).expect("not cancelled")
    }

    /// Render the whole image as a single rectangle.
    fn whole(e: &mut dyn Effect, src: &Surface) -> Vec<u8> {
        let r = src.bounds();
        let ctx = ctx_for(src.width, src.height, r);
        e.prepare(src, &ctx);
        let buf = src.read_rect(r);
        let mut dst = buf.clone();
        e.render(&Src { surface: src, buf: &buf, rect: r }, &mut dst, r, &ctx);
        dst
    }

    /// Run one effect on a 1x1 pixel (straight alpha in and out).
    fn px(e: &mut dyn Effect, c: [u8; 4]) -> [u8; 4] {
        let s = surface_from_straight(1, 1, |_, _| c);
        unpremul(run(e, &s, &Selection::default()).get_pixel(0, 0))
    }

    fn configured() -> Vec<Box<dyn Effect>> {
        let mut curves = Curves::default();
        curves.luminosity.insert(64, 140);
        let mut curves_rgb = Curves { mode: TransferMode::Rgb, ..Curves::default() };
        curves_rgb.rgb[0].insert(128, 200);
        curves_rgb.rgb[2] = BTreeMap::from([(0, 255), (255, 0)]);
        let mut levels = Levels { in_low: 30, in_high: 220, gamma: 1.6, out_low: 10, ..Levels::default() };
        levels.update_levels();
        vec![
            Box::new(AutoLevel::default()),
            Box::new(BlackAndWhite),
            Box::new(BrightnessContrast { brightness: 25, contrast: 40 }),
            Box::new(BrightnessContrast { brightness: -10, contrast: -60 }),
            Box::new(BrightnessContrast { brightness: 5, contrast: 100 }),
            Box::new(curves),
            Box::new(curves_rgb),
            Box::new(HueSaturation { hue: 70, saturation: 150, lightness: -20 }),
            Box::new(InvertColors),
            Box::new(levels),
            Box::new(Posterize { red: 4, green: 6, blue: 3, linked: false }),
            Box::new(Sepia { strength: 100 }),
        ]
    }

    #[test]
    fn adjustments_menu_order() {
        let names: Vec<&str> = all().iter().map(|e| e.name()).collect();
        assert_eq!(
            names,
            ["Auto Level", "Black and White", "Brightness / Contrast", "Curves", "Hue / Saturation", "Invert Colors", "Levels", "Posterize", "Sepia"]
        );
        for e in all() {
            assert_eq!(e.category(), Category::Adjustment);
            assert!(e.icon().starts_with("adjustments-"));
        }
    }

    #[test]
    fn adjustments_change_image_and_tile_independent() {
        let src = test_image(700, 500);
        let input = src.to_rgba();
        for mut e in configured() {
            let tiled = run(e.as_mut(), &src, &Selection::default()).to_rgba();
            assert_ne!(tiled, input, "{} did not change the image", e.name());
            let single = whole(e.as_mut(), &src);
            assert!(tiled == single, "{} depends on tiling", e.name());
            // Fully transparent pixels stay transparent.
            assert_eq!(&tiled[(600 * 4)..(600 * 4 + 4)], &[0, 0, 0, 0], "{}", e.name());
        }
    }

    #[test]
    fn adjustments_defaults_identity() {
        let src = test_image(300, 200);
        let input = src.to_rgba();
        let identity: Vec<Box<dyn Effect>> = vec![
            Box::new(BrightnessContrast::default()),
            Box::new(Curves::default()),
            Box::new(Curves { mode: TransferMode::Rgb, ..Curves::default() }),
            Box::new(HueSaturation::default()),
            Box::new(Levels::default()),
        ];
        for mut e in identity {
            assert!(run(e.as_mut(), &src, &Selection::default()).to_rgba() == input, "{} at defaults changed the image", e.name());
        }
    }

    #[test]
    fn adjustments_one_pixel_and_partial_selection() {
        for mut e in configured() {
            let s = surface_from_straight(1, 1, |_, _| [90, 160, 30, 255]);
            let _ = run(e.as_mut(), &s, &Selection::default());
        }
        let src = test_image(700, 500);
        let input = src.to_rgba();
        let r = IRect::new(130, 270, 300, 140);
        let sel = Selection { mask: Some(Mask::from_rect(700, 500, r)) };
        for mut e in configured() {
            let out = run(e.as_mut(), &src, &sel).to_rgba();
            let mut inside_changed = false;
            for y in 0..500 {
                for x in 0..700 {
                    let i = ((y * 700 + x) * 4) as usize;
                    if r.contains(x, y) {
                        inside_changed |= out[i..i + 4] != input[i..i + 4];
                    } else {
                        assert_eq!(out[i..i + 4], input[i..i + 4], "{} changed ({x},{y}) outside selection", e.name());
                    }
                }
            }
            assert!(inside_changed, "{} did nothing inside the selection", e.name());
        }
    }

    #[test]
    fn adjustments_posterize_values() {
        let two = posterize_levels(2);
        assert!(two[..128].iter().all(|&v| v == 0) && two[128..].iter().all(|&v| v == 255));
        let sixteen = posterize_levels(16);
        // j at index i = floor((16 i - 1) / 255): steps of 17.
        assert_eq!([sixteen[0], sixteen[15], sixteen[16], sixteen[100], sixteen[255]], [0, 0, 17, 102, 255]);
        let mut p = Posterize { red: 2, green: 16, blue: 2, linked: false };
        assert_eq!(px(&mut p, [100, 100, 200, 255]), [0, 102, 255, 255]);
    }

    #[test]
    fn adjustments_brightness_contrast_values() {
        let mut e = BrightnessContrast { brightness: 20, contrast: 0 };
        assert_eq!(px(&mut e, [100, 150, 250, 255]), [120, 170, 255, 255]);
        // contrast 50: shift = I - 127; for greys out = 2 I - 127.
        let mut e = BrightnessContrast { brightness: 0, contrast: 50 };
        assert_eq!(px(&mut e, [200, 200, 200, 255]), [255, 255, 255, 255]);
        assert_eq!(px(&mut e, [100, 100, 100, 255]), [73, 73, 73, 255]);
        // contrast -50: shift = trunc((I - 127) / 2) + 127 - I.
        let mut e = BrightnessContrast { brightness: 0, contrast: -50 };
        assert_eq!(px(&mut e, [100, 100, 100, 255]), [114, 114, 114, 255]);
        // contrast 100 thresholds the intensity.
        let mut e = BrightnessContrast { brightness: 0, contrast: 100 };
        assert_eq!(px(&mut e, [127, 127, 127, 255]), [0, 0, 0, 255]);
        assert_eq!(px(&mut e, [128, 128, 128, 200]), [255, 255, 255, 200]);
    }

    #[test]
    fn adjustments_hue_saturation_values() {
        let mut e = HueSaturation { hue: 180, ..HueSaturation::default() };
        assert_eq!(px(&mut e, [255, 0, 0, 255]), [0, 255, 255, 255]);
        let mut e = HueSaturation { saturation: 0, ..HueSaturation::default() };
        let c = [200, 50, 120, 255];
        let i = intensity(c);
        assert_eq!(px(&mut e, c), [i, i, i, 255]);
        // Lightness 100 blends with white at alpha 255 over 256: 255*255/256.
        let mut e = HueSaturation { lightness: 100, ..HueSaturation::default() };
        assert_eq!(px(&mut e, [10, 20, 30, 255]), [254, 254, 254, 255]);
        let mut e = HueSaturation { lightness: -100, ..HueSaturation::default() };
        assert_eq!(px(&mut e, [10, 20, 30, 255]), [0, 0, 0, 255]);
        // Lightness 50: a = 127, out = (c * 128 + 255 * 127) / 256.
        let mut e = HueSaturation { lightness: 50, ..HueSaturation::default() };
        assert_eq!(px(&mut e, [100, 100, 100, 255]), [176, 176, 176, 255]);
    }

    #[test]
    fn adjustments_curves_values() {
        let mut e = Curves { mode: TransferMode::Rgb, ..Curves::default() };
        e.rgb[1] = BTreeMap::from([(0, 0), (255, 128)]);
        let out = px(&mut e, [10, 255, 77, 255]);
        assert_eq!(out, [10, 128, 77, 255]);
        // Constant luminosity curve at 255 shifts every channel by 255 - L.
        let mut e = Curves { luminosity: BTreeMap::from([(0, 255), (255, 255)]), ..Curves::default() };
        assert_eq!(px(&mut e, [100, 100, 100, 255]), [255, 255, 255, 255]);
        // Three-point natural spline through (0,0), (128,192), (255,255).
        let s = Spline::new(&BTreeMap::from([(0, 0), (128, 192), (255, 255)]));
        assert!((s.eval(128.0) - 192.0).abs() < 1e-9 && s.eval(0.0).abs() < 1e-9);
        assert!(s.eval(64.0) > 96.0 && s.eval(64.0) < 192.0);
    }

    #[test]
    fn adjustments_curves_editor() {
        let mut c = Curves::default();
        c.press((100, 255 - 180));
        assert_eq!(c.luminosity.get(&100), Some(&180));
        assert!(c.drag((110, 255 - 170)));
        assert_eq!(c.luminosity.get(&100), None);
        assert_eq!(c.luminosity.get(&110), Some(&170));
        // End points move vertically only and cannot be removed.
        c.press((1, 254));
        c.drag((40, 200));
        assert_eq!(c.luminosity.get(&0), Some(&55));
        assert!(!Curves::default().remove_near((0, 255)));
        assert!(c.remove_near((110, 255 - 170)));
        assert_eq!(c.luminosity.len(), 2);
    }

    #[test]
    fn adjustments_levels_values() {
        let mut e = Levels { in_low: 50, in_high: 200, ..Levels::default() };
        e.update_levels();
        assert_eq!(px(&mut e, [125, 40, 210, 255]), [127, 0, 255, 255]);
        e.gamma = 2.0;
        e.update_levels();
        assert_eq!(px(&mut e, [125, 125, 125, 255]), [63, 63, 63, 255]);
        // Only the red channel edited.
        let mut e = Levels { mask: [true, false, false], out_high: 128, ..Levels::default() };
        e.update_levels();
        assert_eq!(px(&mut e, [255, 255, 255, 255]), [128, 255, 255, 255]);
    }

    #[test]
    fn adjustments_auto_level_values() {
        // Columns 0..255 of grey: lo = 1, hi = 254, mean 127.5 -> 128.
        let s = surface_from_straight(256, 4, |x, _| [x as u8, x as u8, x as u8, 255]);
        let h = HistogramRgb::compute(&s, s.bounds());
        assert_eq!(h.percentile_color(0.005), [1; 3]);
        assert_eq!(h.percentile_color(0.995), [254; 3]);
        assert_eq!(h.mean_color(), [128; 3]);
        let mut e = AutoLevel::default();
        let out = run(&mut e, &s, &Selection::default());
        let g = (0.5f32.ln() / (127.0f32 / 253.0).ln()) as f64;
        let expect = |v: f32| ((((v - 1.0) / 253.0) as f64).powf(g) * 255.0) as u8;
        for (x, want) in [(0, 0), (1, 0), (254, 255), (255, 255), (100, expect(100.0)), (200, expect(200.0))] {
            assert_eq!(out.get_pixel(x, 0)[0], want, "x={x}");
        }
        // A flat image gives an invalid op and is left alone.
        let flat = surface_from_straight(10, 10, |_, _| [90, 90, 90, 255]);
        assert!(run(&mut e, &flat, &Selection::default()).to_rgba() == flat.to_rgba());
    }

    #[test]
    #[ignore]
    fn adjustments_timing_4000() {
        let src = test_image(4000, 4000);
        for mut e in configured() {
            let t = std::time::Instant::now();
            let _ = run(e.as_mut(), &src, &Selection::default());
            let dt = t.elapsed().as_secs_f32();
            println!("{:<24} {:>7.1} ms", e.name(), dt * 1000.0);
            assert!(dt < 2.0, "{} too slow", e.name());
        }
    }
}
