//! Adjustments menu.

use super::util::{intensity, map_straight};
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::IRect;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(BlackAndWhite), Box::new(InvertColors), Box::new(Sepia { strength: 100 })]
}

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
        super::util::slider_i(ui, "Strength", &mut self.strength, 0, 100)
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
