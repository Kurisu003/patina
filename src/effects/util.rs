//! Shared helpers for effects (colour conversions, small RNG, UI widgets).

use crate::surface::{premul, unpremul};

/// Straight-alpha colour from a premultiplied buffer position.
#[inline]
pub fn get_straight(buf: &[u8], i: usize) -> [u8; 4] {
    unpremul([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]])
}

#[inline]
pub fn put_straight(buf: &mut [u8], i: usize, c: [u8; 4]) {
    buf[i..i + 4].copy_from_slice(&premul(c));
}

/// Apply a straight-alpha per-pixel function to every pixel of `dst`.
pub fn map_straight(dst: &mut [u8], f: impl Fn([u8; 4]) -> [u8; 4]) {
    for p in dst.chunks_exact_mut(4) {
        if p[3] == 0 {
            continue;
        }
        let c = unpremul([p[0], p[1], p[2], p[3]]);
        p.copy_from_slice(&premul(f(c)));
    }
}

/// Paint.NET / Pinta intensity: (7471 B + 38470 G + 19595 R) >> 16.
#[inline]
pub fn intensity(c: [u8; 4]) -> u8 {
    ((7471 * c[2] as u32 + 38470 * c[1] as u32 + 19595 * c[0] as u32) >> 16) as u8
}

#[inline]
pub fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// RGB (0..255) -> HSV (h 0..360, s 0..100, v 0..100), as Pinta's HsvColor.
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let r = r as f64 / 255.0;
    let g = g as f64 / 255.0;
    let b = b as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s * 100.0, max * 100.0)
}

pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let s = (s / 100.0).clamp(0.0, 1.0);
    let v = (v / 100.0).clamp(0.0, 1.0);
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (f(r), f(g), f(b))
}

/// Deterministic per-pixel hash RNG so tiles render identically regardless
/// of tiling or thread order.
#[inline]
pub fn hash3(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h
}

/// Uniform float in [0,1) from the hash.
#[inline]
pub fn hash01(x: i32, y: i32, seed: u32) -> f32 {
    (hash3(x, y, seed) >> 8) as f32 / (1u32 << 24) as f32
}

/// Integer slider row like Pinta's effect dialogs. Returns true on change.
pub fn slider_i(ui: &mut egui::Ui, label: &str, v: &mut i32, min: i32, max: i32) -> bool {
    ui.label(label);
    ui.add(egui::Slider::new(v, min..=max)).changed()
}

pub fn slider_f(ui: &mut egui::Ui, label: &str, v: &mut f64, min: f64, max: f64) -> bool {
    ui.label(label);
    ui.add(egui::Slider::new(v, min..=max)).changed()
}

/// Angle picker (degrees).
pub fn angle(ui: &mut egui::Ui, label: &str, v: &mut f64) -> bool {
    ui.label(label);
    ui.add(egui::Slider::new(v, 0.0..=360.0).suffix("°")).changed()
}

/// Random seed row with a "Reseed" button.
pub fn seed(ui: &mut egui::Ui, v: &mut u32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Random seed");
        changed |= ui.add(egui::DragValue::new(v)).changed();
        if ui.button("Reseed").clicked() {
            *v = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
            changed = true;
        }
    });
    changed
}

/// Offset (centre) picker in -1..1 relative coordinates, Pinta's PointPicker
/// equivalent but relative to the selection.
pub fn offset(ui: &mut egui::Ui, label: &str, v: &mut [f64; 2]) -> bool {
    ui.label(label);
    let mut c = false;
    ui.horizontal(|ui| {
        c |= ui.add(egui::DragValue::new(&mut v[0]).speed(0.01).range(-2.0..=2.0).prefix("x ")).changed();
        c |= ui.add(egui::DragValue::new(&mut v[1]).speed(0.01).range(-2.0..=2.0).prefix("y ")).changed();
        if ui.button("Reset").clicked() {
            *v = [0.0, 0.0];
            c = true;
        }
    });
    c
}
