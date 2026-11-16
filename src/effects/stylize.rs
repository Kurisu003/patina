//! Stylize menu: Edge Detect, Emboss, Outline Edge, Relief.
//!
//! Ported from Pinta (MIT) / Paint.NET 3.36. Edge Detect and Relief share
//! Pinta's 3x3 "colour difference" kernel; Outline Edge uses Paint.NET's
//! circular local histogram.

use super::util::{angle, intensity, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(EdgeDetect::default()), Box::new(Emboss::default()), Box::new(OutlineEdge::default()), Box::new(Relief::default())]
}

/// Read-only window into the margin-padded source buffer.
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

/// The directional 3x3 kernel shared by Edge Detect, Emboss and Relief:
/// cosines of the angle rotated in 45° steps around the centre, `center`
/// in the middle.
fn directional_weights(degrees: f64, center: f64) -> [[f64; 3]; 3] {
    let a = degrees.to_radians();
    let d = std::f64::consts::FRAC_PI_4;
    [
        [(a + d).cos(), (a + 2.0 * d).cos(), (a + 3.0 * d).cos()],
        [a.cos(), center, (a + 4.0 * d).cos()],
        [(a - d).cos(), (a - 2.0 * d).cos(), (a - 3.0 * d).cos()],
    ]
}

/// Range of kernel offsets (0..3) that stay inside the image, like Pinta's
/// fStart/fEnd edge handling (out-of-image taps are dropped, not clamped).
#[inline]
fn taps(v: i32, len: i32) -> (usize, usize) {
    let start = if v == 0 { 1 } else { 0 };
    let end = if v == len - 1 { 2 } else { 3 };
    (start, end.max(start))
}

/// Pinta's ColorDifference effect: weighted 3x3 sum per colour channel of the
/// (premultiplied) source, truncated to a byte, output opaque.
fn color_difference(weights: &[[f64; 3]; 3], src: &Src, dst: &mut [u8], r: IRect) {
    let view = View::new(src);
    let (w, h) = (src.width() as i32, src.height() as i32);
    for y in r.y..r.bottom() {
        let (fy0, fy1) = taps(y, h);
        for x in r.x..r.right() {
            let (fx0, fx1) = taps(x, w);
            let mut sum = [0f64; 3];
            for fy in fy0..fy1 {
                for fx in fx0..fx1 {
                    let wt = weights[fy][fx];
                    let c = view.px(x - 1 + fx as i32, y - 1 + fy as i32);
                    sum[0] += wt * c[0] as f64;
                    sum[1] += wt * c[1] as f64;
                    sum[2] += wt * c[2] as f64;
                }
            }
            let o = (((y - r.y) * r.w + (x - r.x)) * 4) as usize;
            for c in 0..3 {
                dst[o + c] = sum[c].clamp(0.0, 255.0) as u8;
            }
            dst[o + 3] = 255;
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct EdgeDetect {
    pub angle: f64,
}

impl Default for EdgeDetect {
    fn default() -> Self {
        EdgeDetect { angle: 45.0 }
    }
}

impl Effect for EdgeDetect {
    fn name(&self) -> &'static str {
        "Edge Detect"
    }
    fn category(&self) -> Category {
        Category::Stylize
    }
    fn icon(&self) -> &'static str {
        "effects-stylize-edgedetect-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        angle(ui, "Angle", &mut self.angle)
    }
    fn margin(&self) -> i32 {
        1
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        color_difference(&directional_weights(self.angle, 0.0), src, dst, r);
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
pub struct Relief {
    pub angle: f64,
}

impl Default for Relief {
    fn default() -> Self {
        Relief { angle: 45.0 }
    }
}

impl Effect for Relief {
    fn name(&self) -> &'static str {
        "Relief"
    }
    fn category(&self) -> Category {
        Category::Stylize
    }
    fn icon(&self) -> &'static str {
        "effects-stylize-relief-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        angle(ui, "Angle", &mut self.angle)
    }
    fn margin(&self) -> i32 {
        1
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        color_difference(&directional_weights(self.angle, 1.0), src, dst, r);
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------

/// Directional kernel on intensity, offset by 128, greyscale and opaque.
#[derive(Clone)]
pub struct Emboss {
    pub angle: f64,
}

impl Default for Emboss {
    fn default() -> Self {
        Emboss { angle: 0.0 }
    }
}

impl Effect for Emboss {
    fn name(&self) -> &'static str {
        "Emboss"
    }
    fn category(&self) -> Category {
        Category::Stylize
    }
    fn icon(&self) -> &'static str {
        "effects-stylize-emboss-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        angle(ui, "Angle", &mut self.angle)
    }
    fn margin(&self) -> i32 {
        1
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let weights = directional_weights(self.angle, 0.0);
        let view = View::new(src);
        let (w, h) = (src.width() as i32, src.height() as i32);
        for y in r.y..r.bottom() {
            let (fy0, fy1) = taps(y, h);
            for x in r.x..r.right() {
                let (fx0, fx1) = taps(x, w);
                let mut sum = 0f64;
                for fy in fy0..fy1 {
                    for fx in fx0..fx1 {
                        sum += weights[fy][fx] * intensity(view.px(x - 1 + fx as i32, y - 1 + fy as i32)) as f64;
                    }
                }
                let v = ((sum as i32) + 128).clamp(0, 255) as u8;
                let o = (((y - r.y) * r.w + (x - r.x)) * 4) as usize;
                dst[o..o + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------

/// Darkens where the spread of channel values in a disc is large.
#[derive(Clone)]
pub struct OutlineEdge {
    pub thickness: i32,
    pub intensity: i32,
}

impl Default for OutlineEdge {
    fn default() -> Self {
        OutlineEdge { thickness: 3, intensity: 50 }
    }
}

/// Straight -> premultiplied with truncation (ColorBgra.ToPremultipliedAlpha).
#[inline]
fn to_premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    let f = |v: u8| (v as u32 * a / 255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

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

/// Lower and upper bins enclosing the middle `intensity`% of a histogram,
/// exactly as Paint.NET's outline walks them.
#[inline]
fn spread(h: &[i32; 256], min1: i32, min2: i32) -> (i32, i32) {
    let mut count = 0;
    let mut v1 = 0usize;
    while v1 < 255 && h[v1] == 0 {
        v1 += 1;
    }
    while v1 < 255 && count < min1 {
        count += h[v1];
        v1 += 1;
    }
    let mut v2 = v1;
    while v2 < 255 && count < min2 {
        count += h[v2];
        v2 += 1;
    }
    (v1 as i32, v2 as i32)
}

impl Effect for OutlineEdge {
    fn name(&self) -> &'static str {
        "Outline Edge"
    }
    fn category(&self) -> Category {
        Category::Stylize
    }
    fn icon(&self) -> &'static str {
        "effects-stylize-outline-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Thickness", &mut self.thickness, 1, 200);
        c |= slider_i(ui, "Intensity", &mut self.intensity, 0, 100);
        c
    }
    fn margin(&self) -> i32 {
        self.thickness.max(0)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let intensity = self.intensity;
        local_histogram(src, r, self.thickness.max(0), dst, |_, area, h| {
            let min1 = area * (100 - intensity) / 200;
            let min2 = area * (100 + intensity) / 200;
            let (r1, r2) = spread(&h[0], min1, min2);
            let (g1, g2) = spread(&h[1], min1, min2);
            let (b1, b2) = spread(&h[2], min1, min2);
            let (_, a2) = spread(&h[3], min1, min2);
            to_premul([(255 - (r2 - r1)) as u8, (255 - (g2 - g1)) as u8, (255 - (b2 - b1)) as u8, a2 as u8])
        });
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{apply, Progress};
    use crate::selection::{Mask, Selection};
    use crate::surface::{premul, Surface};

    fn ctx(w: u32, h: u32, sel: IRect) -> RenderCtx {
        RenderCtx { width: w, height: h, sel_bounds: sel, primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255] }
    }

    fn test_image(w: u32, h: u32) -> Surface {
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

    fn render_whole(e: &mut dyn Effect, src: &Surface, ctx: &RenderCtx) -> Vec<u8> {
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
        if let Some(i) = tiled.iter().zip(&whole).position(|(p, q)| p != q) {
            let px = i / 4;
            panic!("{}: tiled != whole at ({}, {})", e.name(), px as u32 % w, px as u32 / w);
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

    fn variants() -> Vec<Box<dyn Effect>> {
        let mut v = all();
        v.push(Box::new(EdgeDetect { angle: 90.0 }));
        v.push(Box::new(Emboss { angle: 237.0 }));
        v.push(Box::new(OutlineEdge { thickness: 25, intensity: 20 }));
        v.push(Box::new(OutlineEdge { thickness: 1, intensity: 100 }));
        v.push(Box::new(Relief { angle: 300.0 }));
        v
    }

    #[test]
    fn menu_order_and_names() {
        let names: Vec<_> = all().iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Edge Detect", "Emboss", "Outline Edge", "Relief"]);
    }

    #[test]
    fn tiling_independent() {
        let src = test_image(700, 500);
        for e in variants() {
            check(e.as_ref(), &src, None);
        }
    }

    #[test]
    fn partial_selection() {
        let src = test_image(700, 500);
        for e in variants() {
            check(e.as_ref(), &src, Some(IRect::new(250, 70, 300, 410)));
        }
    }

    #[test]
    fn tiny_images() {
        for (w, h) in [(1, 1), (1, 6), (4, 1), (2, 2)] {
            let src = test_image(w, h);
            for e in variants() {
                check(e.as_ref(), &src, None);
            }
        }
    }

    #[test]
    fn flat_image_responses() {
        // Uniform opaque grey: edge detect sums to 0 (cosines cancel), relief
        // keeps the colour, emboss gives mid grey.
        let s = Surface::filled(40, 30, [100, 100, 100, 255]);
        let c = ctx(40, 30, s.bounds());
        let ed = render_whole(&mut EdgeDetect::default(), &s, &c);
        let rl = render_whole(&mut Relief::default(), &s, &c);
        let em = render_whole(&mut Emboss::default(), &s, &c);
        let i = ((15 * 40 + 20) * 4) as usize;
        assert!(ed[i] <= 1 && ed[i + 3] == 255);
        assert!((rl[i] as i32 - 100).abs() <= 1);
        assert!((em[i] as i32 - 128).abs() <= 1);
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
        let dir = std::path::PathBuf::from(std::env::var("PINTA_ASSETS").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../pinta-upstream/tests/Pinta.Effects.Tests/Assets").into()));
        if !dir.exists() {
            return;
        }
        let input = load_png(&dir.join("input.png"));
        let cases: Vec<(&str, Box<dyn Effect>)> = vec![
            ("edgedetect1.png", Box::new(EdgeDetect::default())),
            ("edgedetect2.png", Box::new(EdgeDetect { angle: 90.0 })),
            ("emboss1.png", Box::new(Emboss::default())),
            ("emboss2.png", Box::new(Emboss { angle: 45.0 })),
            ("outline1.png", Box::new(OutlineEdge::default())),
            ("outline2.png", Box::new(OutlineEdge { thickness: 25, intensity: 20 })),
            ("relief1.png", Box::new(Relief::default())),
            ("relief2.png", Box::new(Relief { angle: 90.0 })),
        ];
        let mut bad = vec![];
        for (file, mut e) in cases {
            let expected = load_png(&dir.join(file)).to_rgba();
            let c = ctx(input.width, input.height, input.bounds());
            let out = apply(e.as_mut(), &input, &Selection::default(), &c, &Progress::new()).unwrap().to_rgba();
            let n = out
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .filter(|(p, q)| p.iter().zip(q.iter()).map(|(x, y)| (*x as i32 - *y as i32).pow(2)).sum::<i32>() > 4)
                .count();
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
        let extremes: Vec<Box<dyn Effect>> = vec![Box::new(OutlineEdge { thickness: 50, intensity: 50 }), Box::new(OutlineEdge { thickness: 200, intensity: 50 })];
        for mut e in all().into_iter().chain(extremes) {
            let t = std::time::Instant::now();
            apply(e.as_mut(), &src, &Selection::default(), &c, &Progress::new()).unwrap();
            println!("{:<16} {:>8.3} s", e.name(), t.elapsed().as_secs_f64());
        }
    }
}
