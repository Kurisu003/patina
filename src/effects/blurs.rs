//! Blurs menu.

use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(GaussianBlur { radius: 2 })]
}

/// Separable Gaussian blur on premultiplied pixels.
#[derive(Clone)]
pub struct GaussianBlur {
    pub radius: i32,
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
        super::util::slider_i(ui, "Radius", &mut self.radius, 0, 200)
    }
    fn margin(&self) -> i32 {
        self.radius
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let rad = self.radius;
        if rad <= 0 {
            return;
        }
        let sigma = rad as f32 / 2.0;
        let kernel: Vec<f32> = (-rad..=rad).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
        let ksum: f32 = kernel.iter().sum();
        // horizontal pass over the padded rows
        let sr = IRect::new(r.x, r.y - rad, r.w, r.h + 2 * rad);
        let mut tmp = vec![0f32; (sr.w * sr.h * 4) as usize];
        for y in 0..sr.h {
            for x in 0..sr.w {
                let mut acc = [0f32; 4];
                for (k, w) in kernel.iter().enumerate() {
                    let p = src.get(sr.x + x + k as i32 - rad, sr.y + y);
                    for c in 0..4 {
                        acc[c] += p[c] as f32 * w;
                    }
                }
                let o = ((y * sr.w + x) * 4) as usize;
                for c in 0..4 {
                    tmp[o + c] = acc[c] / ksum;
                }
            }
        }
        // vertical pass
        let h = src.height() as i32;
        for y in 0..r.h {
            for x in 0..r.w {
                let mut acc = [0f32; 4];
                for (k, w) in kernel.iter().enumerate() {
                    // clamp to image edge like the horizontal pass does
                    let iy = (r.y + y + k as i32 - rad).clamp(0, h - 1) - sr.y;
                    let o = ((iy * sr.w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += tmp[o + c] * w;
                    }
                }
                let o = ((y * r.w + x) * 4) as usize;
                for c in 0..4 {
                    dst[o + c] = (acc[c] / ksum).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}
