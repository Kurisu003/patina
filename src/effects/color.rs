//! Color menu: Dithering.
//!
//! Ported from Pinta (MIT). Error diffusion is inherently sequential, so the
//! whole selection bounds are dithered once in `prepare()` (row-major, exactly
//! like Pinta's single non-tiled pass) and `render()` copies tiles out.

use super::{Category, Effect, RenderCtx, Src};
use crate::surface::{IRect, Surface};
use std::sync::Arc;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(Dithering::default())]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffusionMethod {
    Sierra,
    TwoRowSierra,
    SierraLite,
    Burkes,
    Atkinson,
    Stucki,
    JarvisJudiceNinke,
    FloydSteinberg,
    FalseFloydSteinberg,
}

/// An error diffusion matrix: weights for the current row (starting at the
/// target pixel's column `pixel_column`) and the rows below.
struct DiffusionMatrix {
    rows: &'static [&'static [i32]],
    pixel_column: i32,
    /// Atkinson deliberately diffuses only 6/8 of the error.
    reduction: Option<f64>,
}

impl DiffusionMethod {
    const ALL: [DiffusionMethod; 9] = [
        DiffusionMethod::Sierra,
        DiffusionMethod::TwoRowSierra,
        DiffusionMethod::SierraLite,
        DiffusionMethod::Burkes,
        DiffusionMethod::Atkinson,
        DiffusionMethod::Stucki,
        DiffusionMethod::JarvisJudiceNinke,
        DiffusionMethod::FloydSteinberg,
        DiffusionMethod::FalseFloydSteinberg,
    ];

    fn label(self) -> &'static str {
        match self {
            DiffusionMethod::Sierra => "Sierra",
            DiffusionMethod::TwoRowSierra => "Two-Row Sierra",
            DiffusionMethod::SierraLite => "Sierra Lite",
            DiffusionMethod::Burkes => "Burkes",
            DiffusionMethod::Atkinson => "Atkinson",
            DiffusionMethod::Stucki => "Stucki",
            DiffusionMethod::JarvisJudiceNinke => "Jarvis-Judice-Ninke",
            DiffusionMethod::FloydSteinberg => "Floyd-Steinberg",
            DiffusionMethod::FalseFloydSteinberg => "Floyd-Steinberg Lite",
        }
    }

    fn matrix(self) -> DiffusionMatrix {
        let m = |rows, pixel_column| DiffusionMatrix { rows, pixel_column, reduction: None };
        match self {
            DiffusionMethod::Sierra => m(&[&[0, 0, 0, 5, 3], &[2, 4, 5, 4, 2], &[0, 2, 3, 2, 0]], 2),
            DiffusionMethod::TwoRowSierra => m(&[&[0, 0, 0, 4, 3], &[1, 2, 3, 2, 1]], 2),
            DiffusionMethod::SierraLite => m(&[&[0, 0, 2], &[1, 1, 0]], 1),
            DiffusionMethod::Burkes => m(&[&[0, 0, 0, 8, 4], &[2, 4, 8, 4, 2]], 2),
            DiffusionMethod::Atkinson => DiffusionMatrix {
                rows: &[&[0, 0, 1, 1], &[1, 1, 1, 0], &[0, 1, 0, 0]],
                pixel_column: 1,
                reduction: Some(1.0 / 8.0),
            },
            DiffusionMethod::Stucki => m(&[&[0, 0, 0, 8, 4], &[2, 4, 8, 4, 2], &[1, 2, 4, 2, 1]], 2),
            DiffusionMethod::JarvisJudiceNinke => m(&[&[0, 0, 0, 7, 5], &[3, 5, 7, 5, 3], &[1, 3, 5, 3, 1]], 2),
            DiffusionMethod::FloydSteinberg => m(&[&[0, 0, 7], &[3, 5, 1]], 1),
            DiffusionMethod::FalseFloydSteinberg => m(&[&[0, 3], &[3, 2]], 0),
        }
    }

    /// (dx, dy, factor) for every positive weight.
    fn taps(self) -> Vec<(i32, i32, f64)> {
        let m = self.matrix();
        let total: i32 = m.rows.iter().flat_map(|r| r.iter()).sum();
        let reduction = m.reduction.unwrap_or(1.0 / total as f64);
        let mut v = vec![];
        for (r, row) in m.rows.iter().enumerate() {
            for (c, &w) in row.iter().enumerate() {
                if w > 0 {
                    v.push((c as i32 - m.pixel_column, r as i32, w as f64 * reduction));
                }
            }
        }
        v
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresetPalette {
    BlackWhite,
    OldMsPaint,
    OldWindows16,
    OldWindows20,
    Rgb3Bit,
    Rgb666,
    Rgb6Bit,
    Rgb12Bit,
}

const OLD_WINDOWS_16: [[u8; 3]; 16] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 64, 128],
    [192, 192, 192],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];

const OLD_WINDOWS_20_EXTRA: [[u8; 3]; 4] = [[255, 251, 240], [192, 220, 192], [166, 202, 240], [160, 160, 164]];

const OLD_MS_PAINT: [[u8; 3]; 28] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 128, 128],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
    [128, 64, 0],
    [0, 64, 64],
    [128, 128, 64],
    [255, 128, 64],
    [255, 0, 128],
    [0, 64, 128],
    [0, 255, 128],
    [255, 255, 128],
    [192, 192, 192],
    [128, 0, 255],
    [0, 128, 255],
    [128, 128, 255],
    [128, 255, 255],
];

/// How to find the nearest palette entry.
enum Palette {
    /// Arbitrary list; the first entry with the smallest squared RGB distance wins.
    List(Vec<[u8; 3]>),
    /// Evenly spaced RGB cube with `levels` steps of `step` (odd, so the
    /// per-channel nearest level is unique and equals the brute-force result).
    Cube { levels: u32, step: u32 },
}

impl PresetPalette {
    const ALL: [PresetPalette; 8] = [
        PresetPalette::BlackWhite,
        PresetPalette::OldMsPaint,
        PresetPalette::OldWindows16,
        PresetPalette::OldWindows20,
        PresetPalette::Rgb3Bit,
        PresetPalette::Rgb666,
        PresetPalette::Rgb6Bit,
        PresetPalette::Rgb12Bit,
    ];

    /// Pinta shows the enum member names (they have no captions).
    fn label(self) -> &'static str {
        match self {
            PresetPalette::BlackWhite => "BlackWhite",
            PresetPalette::OldMsPaint => "OldMsPaint",
            PresetPalette::OldWindows16 => "OldWindows16",
            PresetPalette::OldWindows20 => "OldWindows20",
            PresetPalette::Rgb3Bit => "Rgb3Bit",
            PresetPalette::Rgb666 => "Rgb666",
            PresetPalette::Rgb6Bit => "Rgb6Bit",
            PresetPalette::Rgb12Bit => "Rgb12Bit",
        }
    }

    fn palette(self) -> Palette {
        match self {
            PresetPalette::BlackWhite => Palette::List(vec![[0, 0, 0], [255, 255, 255]]),
            PresetPalette::OldMsPaint => Palette::List(OLD_MS_PAINT.to_vec()),
            PresetPalette::OldWindows16 => Palette::List(OLD_WINDOWS_16.to_vec()),
            PresetPalette::OldWindows20 => Palette::List(OLD_WINDOWS_16.iter().chain(OLD_WINDOWS_20_EXTRA.iter()).copied().collect()),
            PresetPalette::Rgb3Bit => Palette::Cube { levels: 2, step: 255 },
            PresetPalette::Rgb666 => Palette::Cube { levels: 6, step: 51 },
            PresetPalette::Rgb6Bit => Palette::Cube { levels: 4, step: 85 },
            PresetPalette::Rgb12Bit => Palette::Cube { levels: 16, step: 17 },
        }
    }
}

impl Palette {
    #[inline]
    fn closest(&self, c: [u8; 3]) -> [u8; 3] {
        match self {
            Palette::List(list) => {
                let mut best = list[0];
                let mut best_d = i32::MAX;
                for &p in list {
                    let d = (0..3).map(|i| (c[i] as i32 - p[i] as i32).pow(2)).sum::<i32>();
                    if d < best_d {
                        best_d = d;
                        best = p;
                    }
                }
                best
            }
            Palette::Cube { levels, step } => {
                let q = |v: u8| (((v as u32 + step / 2) / step).min(levels - 1) * step).min(255) as u8;
                [q(c[0]), q(c[1]), q(c[2])]
            }
        }
    }
}

/// Error-diffusion dithering to a preset palette.
#[derive(Clone)]
pub struct Dithering {
    pub method: DiffusionMethod,
    pub palette: PresetPalette,
    result: Option<(IRect, Arc<Vec<u8>>)>,
}

impl Default for Dithering {
    fn default() -> Self {
        Dithering { method: DiffusionMethod::FloydSteinberg, palette: PresetPalette::OldWindows16, result: None }
    }
}

impl Dithering {
    /// Dither `roi` of `src` (row-major, error only spreads inside `roi`).
    fn dither(&self, src: &Surface, roi: IRect) -> Vec<u8> {
        let mut buf = src.read_rect(roi);
        let palette = self.palette.palette();
        let taps = self.method.taps();
        let (w, h) = (roi.w, roi.h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let orig = [buf[i], buf[i + 1], buf[i + 2]];
                let near = palette.closest(orig);
                buf[i..i + 4].copy_from_slice(&[near[0], near[1], near[2], 255]);
                let err = [orig[0] as i32 - near[0] as i32, orig[1] as i32 - near[1] as i32, orig[2] as i32 - near[2] as i32];
                if err == [0, 0, 0] {
                    continue;
                }
                for &(dx, dy, f) in &taps {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let n = ((ny * w + nx) * 4) as usize;
                    for c in 0..3 {
                        buf[n + c] = (buf[n + c] as i32 + (f * err[c] as f64) as i32).clamp(0, 255) as u8;
                    }
                    buf[n + 3] = 255;
                }
            }
        }
        buf
    }
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, label: &str, value: &mut T, all: &[T], name: impl Fn(T) -> &'static str) -> bool {
    let before = *value;
    ui.label(label);
    egui::ComboBox::from_id_salt(label).selected_text(name(*value)).show_ui(ui, |ui| {
        for &v in all {
            ui.selectable_value(value, v, name(v));
        }
    });
    *value != before
}

impl Effect for Dithering {
    fn name(&self) -> &'static str {
        "Dithering"
    }
    fn category(&self) -> Category {
        Category::Color
    }
    fn icon(&self) -> &'static str {
        "effects-color-dithering-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = combo(ui, "Error Diffusion Method", &mut self.method, &DiffusionMethod::ALL, DiffusionMethod::label);
        c |= combo(ui, "Palette", &mut self.palette, &PresetPalette::ALL, PresetPalette::label);
        c
    }
    fn prepare(&mut self, src: &Surface, ctx: &RenderCtx) {
        let roi = ctx.sel_bounds.intersect(&src.bounds());
        self.result = if roi.is_empty() { None } else { Some((roi, Arc::new(self.dither(src, roi)))) };
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let Some((roi, buf)) = &self.result else { return };
        let c = r.intersect(roi);
        let row = (c.w * 4) as usize;
        for y in c.y..c.bottom() {
            let s = (((y - roi.y) * roi.w + (c.x - roi.x)) * 4) as usize;
            let d = (((y - r.y) * r.w + (c.x - r.x)) * 4) as usize;
            dst[d..d + row].copy_from_slice(&buf[s..s + row]);
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

    fn ctx(w: u32, h: u32, sel: IRect) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: sel, primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255] }
    }

    fn test_image(w: u32, h: u32) -> Surface {
        let mut d = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let a = if (100..140).contains(&y) { 128 } else { 255 };
                let c = [(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, ((x + y) % 256) as u8, a];
                let i = ((y * w + x) * 4) as usize;
                d[i..i + 4].copy_from_slice(&premul(c));
            }
        }
        Surface::from_rgba(w, h, &d)
    }

    fn render_whole(e: &mut dyn Effect, src: &Surface, ctx: &RenderCtx) -> Vec<u8> {
        e.prepare(src, ctx);
        let full = src.to_rgba();
        let s = Src { surface: src, buf: &full, rect: src.bounds() };
        let r = ctx.sel_bounds.intersect(&src.bounds());
        let mut out = full.clone();
        let mut dst = src.read_rect(r);
        e.render(&s, &mut dst, r, ctx);
        let w = src.width as i32;
        for y in 0..r.h {
            let o = (((r.y + y) * w + r.x) * 4) as usize;
            out[o..o + (r.w * 4) as usize].copy_from_slice(&dst[(y * r.w * 4) as usize..((y + 1) * r.w * 4) as usize]);
        }
        out
    }

    fn check(e: &dyn Effect, src: &Surface, sel: Option<IRect>) {
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
        assert!(tiled == whole, "tiled != whole");
        if sel.is_some() {
            let orig = src.to_rgba();
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let i = ((y * w as i32 + x) * 4) as usize;
                    if !sel_rect.contains(x, y) {
                        assert_eq!(&tiled[i..i + 4], &orig[i..i + 4]);
                    }
                }
            }
        }
    }

    fn variants() -> Vec<Box<dyn Effect>> {
        let mut v = all();
        for (i, m) in DiffusionMethod::ALL.iter().enumerate() {
            v.push(Box::new(Dithering { method: *m, palette: PresetPalette::ALL[i % PresetPalette::ALL.len()], result: None }));
        }
        v
    }

    #[test]
    fn tiling_selection_tiny() {
        let src = test_image(700, 500);
        for e in variants() {
            check(e.as_ref(), &src, None);
            check(e.as_ref(), &src, Some(IRect::new(123, 77, 401, 333)));
        }
        for (w, h) in [(1, 1), (1, 5), (7, 1)] {
            let s = test_image(w, h);
            for e in variants() {
                check(e.as_ref(), &s, None);
            }
        }
    }

    #[test]
    fn output_uses_only_palette_colours() {
        let src = test_image(200, 150);
        for p in PresetPalette::ALL {
            let mut e = Dithering { method: DiffusionMethod::Stucki, palette: p, result: None };
            let out = render_whole(&mut e, &src, &ctx(200, 150, src.bounds()));
            let pal = p.palette();
            for px in out.chunks_exact(4) {
                assert_eq!(px[3], 255);
                let c = [px[0], px[1], px[2]];
                assert_eq!(pal.closest(c), c, "{p:?}");
            }
        }
    }

    #[test]
    fn cube_matches_brute_force() {
        for p in [PresetPalette::Rgb3Bit, PresetPalette::Rgb666, PresetPalette::Rgb6Bit, PresetPalette::Rgb12Bit] {
            let Palette::Cube { levels, step } = p.palette() else { unreachable!() };
            let vals: Vec<u8> = (0..levels).map(|i| (i * step).min(255) as u8).collect();
            let mut list = vec![];
            for &r in &vals {
                for &g in &vals {
                    for &b in &vals {
                        list.push([r, g, b]);
                    }
                }
            }
            let brute = Palette::List(list);
            for v in (0..=255u32).step_by(1) {
                let c = [v as u8, (v * 7 % 256) as u8, (255 - v) as u8];
                assert_eq!(p.palette().closest(c), brute.closest(c));
            }
        }
    }

    #[test]
    fn palette_colours_are_fixed_points() {
        // An image made only of palette colours dithers to itself.
        let mut d = vec![0u8; 64 * 4];
        for (i, p) in d.chunks_exact_mut(4).enumerate() {
            let c = OLD_WINDOWS_16[i % 16];
            p.copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        let src = Surface::from_rgba(8, 8, &d);
        let out = render_whole(&mut Dithering::default(), &src, &ctx(8, 8, src.bounds()));
        assert_eq!(out, d);
    }

    fn load_png(path: &std::path::Path) -> Surface {
        let img = image::open(path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let mut d = img.into_raw();
        for p in d.chunks_exact_mut(4) {
            let c = premul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
        Surface::from_rgba(w, h, &d)
    }

    #[test]
    #[ignore]
    fn pinta_reference_images() {
        let dir = std::path::PathBuf::from(std::env::var("PINTA_ASSETS").unwrap_or_else(|_| "/home/kurisu/src/pinta-upstream/tests/Pinta.Effects.Tests/Assets".into()));
        if !dir.exists() {
            return;
        }
        let input = load_png(&dir.join("input.png"));
        let cases = [
            ("dithering1.png", DiffusionMethod::FloydSteinberg, PresetPalette::OldWindows16),
            ("dithering2.png", DiffusionMethod::FloydSteinberg, PresetPalette::BlackWhite),
            ("dithering3.png", DiffusionMethod::Stucki, PresetPalette::OldWindows16),
            ("dithering4.png", DiffusionMethod::Atkinson, PresetPalette::OldMsPaint),
        ];
        let mut bad = vec![];
        for (file, method, palette) in cases {
            let expected = load_png(&dir.join(file)).to_rgba();
            let c = ctx(input.width, input.height, input.bounds());
            let mut e = Dithering { method, palette, result: None };
            let out = apply(&mut e, &input, &Selection::default(), &c, &Progress::new()).unwrap().to_rgba();
            let n = out.chunks_exact(4).zip(expected.chunks_exact(4)).filter(|(p, q)| p != q).count();
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
        for p in [PresetPalette::OldWindows16, PresetPalette::Rgb12Bit] {
            let mut e = Dithering { palette: p, ..Default::default() };
            let t = std::time::Instant::now();
            apply(&mut e, &src, &Selection::default(), &c, &Progress::new()).unwrap();
            println!("Dithering ({p:?}) {:>8.3} s", t.elapsed().as_secs_f64());
        }
    }
}
