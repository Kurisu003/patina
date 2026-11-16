//! Artistic menu: Ink Sketch, Oil Painting, Pencil Sketch. Ported from
//! Pinta / Paint.NET.

use super::blurs::{gaussian_blur, to_premul, to_straight};
use super::util::{intensity, slider_i};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(InkSketch::default()), Box::new(OilPainting::default()), Box::new(PencilSketch::default())]
}

// ---------------------------------------------------------------------------
// Pixel operations (Pinta's UnaryPixelOps / UserBlendOps, premultiplied).

/// Pinta's brightness/contrast pixel op (straight-alpha lookup table).
struct BrightnessContrast {
    divide: i32,
    table: Vec<u8>,
}

impl BrightnessContrast {
    fn new(brightness: i32, contrast: i32) -> Self {
        let (multiply, divide) = match contrast {
            c if c < 0 => (c + 100, 100),
            c if c > 0 => (100, 100 - c),
            _ => (1, 1),
        };
        let mut table = vec![0u8; 65536];
        if divide == 0 {
            for i in 0..256 {
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

    fn apply_straight(&self, c: [u8; 4]) -> [u8; 4] {
        let i = intensity(c) as usize;
        if self.divide == 0 {
            let v = self.table[i];
            return [v, v, v, c[3]];
        }
        let row = &self.table[i * 256..][..256];
        [row[c[0] as usize], row[c[1] as usize], row[c[2] as usize], c[3]]
    }

    fn apply(&self, c: [u8; 4]) -> [u8; 4] {
        match c[3] {
            0 => c,
            255 => self.apply_straight(c),
            _ => to_premul(self.apply_straight(to_straight(c))),
        }
    }
}

/// Grey from the (premultiplied) intensity, alpha kept.
#[inline]
fn desaturate(c: [u8; 4]) -> [u8; 4] {
    let i = intensity(c);
    [i, i, i, c[3]]
}

/// Separable premultiplied blend: `(1-Ab)Ca + (1-Aa)Cb + f(Cb, Ca, Ab, Aa)`,
/// scaled by 255 and rounded, as Pinta's BlendOpHelper.
#[inline]
fn blend_separable(bottom: [u8; 4], top: [u8; 4], f: impl Fn(i32, i32, i32, i32) -> i32) -> [u8; 4] {
    if top[3] == 0 {
        return bottom;
    }
    if bottom[3] == 0 {
        return top;
    }
    let (ab, aa) = (bottom[3] as i32, top[3] as i32);
    let mut out = [0u8; 4];
    for k in 0..3 {
        let (cb, ca) = (bottom[k] as i32, top[k] as i32);
        let v = (255 - ab) * ca + (255 - aa) * cb + f(cb, ca, ab, aa);
        out[k] = ((v + 128) / 255).clamp(0, 255) as u8;
    }
    out[3] = (aa + (ab * (255 - aa) + 128) / 255).clamp(0, 255) as u8;
    out
}

fn screen(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    blend_separable(bottom, top, |cb, ca, ab, aa| aa * cb + ab * ca - ca * cb)
}

fn darken(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    blend_separable(bottom, top, |cb, ca, ab, aa| (ab * ca).min(aa * cb))
}

fn color_dodge(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    blend_separable(bottom, top, |cb, ca, ab, aa| {
        if cb == 0 {
            0
        } else if ca >= aa {
            aa * ab
        } else {
            (aa * ab).min(cb * aa * aa / (aa - ca))
        }
    })
}

#[inline]
fn get(buf: &[u8], i: usize) -> [u8; 4] {
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

// ---------------------------------------------------------------------------
// Ink Sketch

#[derive(Clone)]
pub struct InkSketch {
    pub ink_outline: i32,
    pub coloring: i32,
}

impl Default for InkSketch {
    fn default() -> Self {
        InkSketch { ink_outline: 50, coloring: 50 }
    }
}

const INK_GLOW_RADIUS: i32 = 6;
const INK_KERNEL: [[i32; 5]; 5] =
    [[-1, -1, -1, -1, -1], [-1, -1, -1, -1, -1], [-1, -1, 30, -1, -1], [-1, -1, -1, -1, -1], [-1, -1, -5, -1, -1]];

impl Effect for InkSketch {
    fn name(&self) -> &'static str {
        "Ink Sketch"
    }
    fn category(&self) -> Category {
        Category::Artistic
    }
    fn icon(&self) -> &'static str {
        "effects-artistic-inksketch-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Ink Outline", &mut self.ink_outline, 0, 99);
        c |= slider_i(ui, "Coloring", &mut self.coloring, 0, 100);
        c
    }
    fn margin(&self) -> i32 {
        INK_GLOW_RADIUS
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        // Glow background: blur, brightness/contrast, screen over the source.
        gaussian_blur(src, dst, r, INK_GLOW_RADIUS);
        let bc_amount = -(self.coloring - 50) * 2;
        let bc = BrightnessContrast::new(bc_amount, bc_amount);
        let threshold = self.ink_outline * 255 / 100;
        let (iw, ih) = (src.width() as i32, src.height() as i32);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let glow = screen(src.get(x, y), bc.apply(get(dst, i)));

                // Edge detection on the premultiplied source (window clipped to the image).
                let mut acc = [0i32; 4];
                for v in (y - 2).max(0)..(y + 3).min(ih) {
                    let krow = &INK_KERNEL[(v - y + 2) as usize];
                    for u in (x - 2).max(0)..(x + 3).min(iw) {
                        let w = krow[(u - x + 2) as usize];
                        let p = src.get(u, v);
                        for k in 0..4 {
                            acc[k] += p[k] as i32 * w;
                        }
                    }
                }
                let base = acc.map(|v| v.clamp(0, 255) as u8);
                let grey = desaturate(base);
                let a = grey[3];
                let ink = if to_straight(grey)[0] as i32 > threshold { [a, a, a, a] } else { [0, 0, 0, a] };
                dst[i..i + 4].copy_from_slice(&darken(ink, glow));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Oil Painting

#[derive(Clone)]
pub struct OilPainting {
    pub brush_size: i32,
    pub coarseness: i32,
}

impl Default for OilPainting {
    fn default() -> Self {
        OilPainting { brush_size: 3, coarseness: 50 }
    }
}

impl Effect for OilPainting {
    fn name(&self) -> &'static str {
        "Oil Painting"
    }
    fn category(&self) -> Category {
        Category::Artistic
    }
    fn icon(&self) -> &'static str {
        "effects-artistic-oilpainting-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Brush Size", &mut self.brush_size, 1, 8);
        c |= slider_i(ui, "Coarseness", &mut self.coarseness, 3, 255);
        c
    }
    fn margin(&self) -> i32 {
        self.brush_size.max(1)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let brush = self.brush_size.max(1);
        let levels = self.coarseness.clamp(3, 255) as u32;
        let (iw, ih) = (src.width() as i32, src.height() as i32);
        let sr = src.rect;
        // Intensity bucket of every source pixel in the padded buffer.
        let bucket: Vec<u8> = src
            .buf
            .chunks_exact(4)
            .map(|p| {
                let t = intensity([p[0], p[1], p[2], p[3]]) as u32 * levels + 0x80;
                (((t >> 8) + t) >> 8) as u8
            })
            .collect();
        let n = levels as usize + 1;
        let mut count = [0i32; 256];
        let mut sums = [[0u32; 4]; 256];
        let mut i = 0;
        for y in r.y..r.bottom() {
            let top = (y - brush).max(0);
            let bottom = (y + brush + 1).min(ih);
            for x in r.x..r.right() {
                let left = (x - brush).max(0);
                let right = (x + brush + 1).min(iw);
                count[..n].fill(0);
                sums[..n].fill([0; 4]);
                for v in top..bottom {
                    let row = (v - sr.y) * sr.w - sr.x;
                    for u in left..right {
                        let j = (row + u) as usize;
                        let b = bucket[j] as usize;
                        count[b] += 1;
                        let p = &src.buf[j * 4..j * 4 + 4];
                        let s = &mut sums[b];
                        for k in 0..4 {
                            s[k] += p[k] as u32;
                        }
                    }
                }
                let (mut chosen, mut max) = (0usize, 0i32);
                for (b, &c) in count[..n].iter().enumerate() {
                    if c > max {
                        chosen = b;
                        max = c;
                    }
                }
                let s = sums[chosen];
                let m = max.max(1) as u32;
                dst[i..i + 4].copy_from_slice(&s.map(|v| (v / m) as u8));
                i += 4;
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Pencil Sketch

#[derive(Clone)]
pub struct PencilSketch {
    pub pencil_tip_size: i32,
    pub color_range: i32,
}

impl Default for PencilSketch {
    fn default() -> Self {
        PencilSketch { pencil_tip_size: 2, color_range: 0 }
    }
}

impl Effect for PencilSketch {
    fn name(&self) -> &'static str {
        "Pencil Sketch"
    }
    fn category(&self) -> Category {
        Category::Artistic
    }
    fn icon(&self) -> &'static str {
        "effects-artistic-pencilsketch-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Pencil Tip Size", &mut self.pencil_tip_size, 1, 20);
        c |= slider_i(ui, "Color Range", &mut self.color_range, -20, 20);
        c
    }
    fn margin(&self) -> i32 {
        self.pencil_tip_size.max(1)
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        gaussian_blur(src, dst, r, self.pencil_tip_size.max(1));
        let bc = BrightnessContrast::new(self.color_range, -self.color_range);
        let mut i = 0;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let c = bc.apply(get(dst, i));
                let inverted = [c[3] - c[0], c[3] - c[1], c[3] - c[2], c[3]];
                let top = desaturate(inverted);
                let bottom = desaturate(src.get(x, y));
                dst[i..i + 4].copy_from_slice(&color_dodge(bottom, top));
                i += 4;
            }
        }
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
        check(Box::new(InkSketch { ink_outline: 10, coloring: 0 }));
        check(Box::new(InkSketch { ink_outline: 99, coloring: 100 }));
        check(Box::new(OilPainting { brush_size: 8, coarseness: 255 }));
        check(Box::new(OilPainting { brush_size: 1, coarseness: 3 }));
        check(Box::new(PencilSketch { pencil_tip_size: 20, color_range: -20 }));
        check(Box::new(PencilSketch { pencil_tip_size: 5, color_range: 20 }));
    }

    #[test]
    fn oil_painting_keeps_uniform_image() {
        let s = Surface::filled(300, 280, [120, 30, 200, 255]);
        assert!(run(&mut OilPainting::default(), &s, &Selection::default()) == s.to_rgba());
    }

    #[test]
    fn brightness_contrast_neutral_is_identity() {
        let bc = BrightnessContrast::new(0, 0);
        for c in [[0u8, 0, 0, 255], [10, 200, 30, 255], [0, 0, 0, 0]] {
            assert_eq!(bc.apply(c), c);
        }
    }

    #[test]
    fn blend_ops_basics() {
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        let grey = [100, 100, 100, 255];
        assert_eq!(screen(grey, black), grey);
        assert_eq!(screen(grey, white), white);
        assert_eq!(darken(grey, white), grey);
        assert_eq!(darken(grey, black), black);
        assert_eq!(color_dodge(grey, black), grey);
        assert_eq!(color_dodge(grey, white), white);
    }

    #[test]
    fn pencil_sketch_of_white_is_white() {
        let s = Surface::filled(100, 80, [255, 255, 255, 255]);
        let out = run(&mut PencilSketch::default(), &s, &Selection::default());
        assert!(out == s.to_rgba());
    }

    #[test]
    #[ignore]
    fn timing_4000() {
        time_all(all());
    }
}
