//! Layer blend modes. Formulas follow the W3C Compositing spec, which is what
//! Cairo/pixman (and therefore Pinta) implement.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    ColorBurn,
    ColorDodge,
    Overlay,
    Difference,
    Lighten,
    Darken,
    Screen,
    Xor,
    HardLight,
    SoftLight,
    Color,
    Luminosity,
    Hue,
    Saturation,
}

impl BlendMode {
    pub const ALL: [BlendMode; 16] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::ColorDodge,
        BlendMode::Overlay,
        BlendMode::Difference,
        BlendMode::Lighten,
        BlendMode::Darken,
        BlendMode::Screen,
        BlendMode::Xor,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Color,
        BlendMode::Luminosity,
        BlendMode::Hue,
        BlendMode::Saturation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::Overlay => "Overlay",
            BlendMode::Difference => "Difference",
            BlendMode::Lighten => "Lighten",
            BlendMode::Darken => "Darken",
            BlendMode::Screen => "Screen",
            BlendMode::Xor => "Xor",
            BlendMode::HardLight => "Hard Light",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
        }
    }

    /// Name used in OpenRaster stack.xml (composite-op).
    pub fn ora_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "svg:src-over",
            BlendMode::Multiply => "svg:multiply",
            BlendMode::ColorBurn => "svg:color-burn",
            BlendMode::ColorDodge => "svg:color-dodge",
            BlendMode::Overlay => "svg:overlay",
            BlendMode::Difference => "svg:difference",
            BlendMode::Lighten => "svg:lighten",
            BlendMode::Darken => "svg:darken",
            BlendMode::Screen => "svg:screen",
            BlendMode::Xor => "svg:xor",
            BlendMode::HardLight => "svg:hard-light",
            BlendMode::SoftLight => "svg:soft-light",
            BlendMode::Color => "svg:color",
            BlendMode::Luminosity => "svg:luminosity",
            BlendMode::Hue => "svg:hue",
            BlendMode::Saturation => "svg:saturation",
        }
    }

    pub fn from_ora(s: &str) -> BlendMode {
        BlendMode::ALL.iter().copied().find(|m| m.ora_name() == s).unwrap_or_default()
    }
}

/// Composite `src` over `dst` (both premultiplied RGBA8 slices of equal length)
/// with an extra layer opacity in 0..=255.
pub fn blend_row(dst: &mut [u8], src: &[u8], opacity: u8, mode: BlendMode) {
    if opacity == 0 {
        return;
    }
    match mode {
        BlendMode::Normal => normal_row(dst, src, opacity),
        BlendMode::Xor => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let s = scale(s, opacity);
                let sa = s[3] as f32 / 255.0;
                let da = d[3] as f32 / 255.0;
                for c in 0..4 {
                    let v = s[c] as f32 * (1.0 - da) + d[c] as f32 * (1.0 - sa);
                    d[c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        _ => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                if s[3] == 0 {
                    continue;
                }
                let s = scale(s, opacity);
                let out = blend_px(d, &s, mode);
                d.copy_from_slice(&out);
            }
        }
    }
}

#[inline]
fn scale(s: &[u8], o: u8) -> [u8; 4] {
    if o == 255 {
        return [s[0], s[1], s[2], s[3]];
    }
    let o = o as u32;
    [crate::surface::mul8(s[0] as u32, o), crate::surface::mul8(s[1] as u32, o), crate::surface::mul8(s[2] as u32, o), crate::surface::mul8(s[3] as u32, o)]
}

#[inline]
fn normal_row(dst: &mut [u8], src: &[u8], opacity: u8) {
    use crate::surface::mul8;
    let o = opacity as u32;
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let sa = if o == 255 { s[3] as u32 } else { mul8(s[3] as u32, o) as u32 };
        if sa == 0 {
            continue;
        }
        if sa == 255 {
            d.copy_from_slice(s);
            continue;
        }
        let inv = 255 - sa;
        for c in 0..3 {
            let sc = if o == 255 { s[c] as u32 } else { mul8(s[c] as u32, o) as u32 };
            d[c] = (sc + mul8(d[c] as u32, inv) as u32).min(255) as u8;
        }
        d[3] = (sa + mul8(d[3] as u32, inv) as u32).min(255) as u8;
    }
}

fn blend_px(d: &[u8], s: &[u8; 4], mode: BlendMode) -> [u8; 4] {
    let sa = s[3] as f32 / 255.0;
    let da = d[3] as f32 / 255.0;
    // un-premultiplied colours
    let cs = if sa > 0.0 { [s[0] as f32 / 255.0 / sa, s[1] as f32 / 255.0 / sa, s[2] as f32 / 255.0 / sa] } else { [0.0; 3] };
    let cb = if da > 0.0 { [d[0] as f32 / 255.0 / da, d[1] as f32 / 255.0 / da, d[2] as f32 / 255.0 / da] } else { [0.0; 3] };
    let b = match mode {
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        _ => [sep(mode, cb[0], cs[0]), sep(mode, cb[1], cs[1]), sep(mode, cb[2], cs[2])],
    };
    let ao = sa + da * (1.0 - sa);
    let mut out = [0u8; 4];
    for c in 0..3 {
        let cs_ = (1.0 - da) * cs[c] + da * b[c].clamp(0.0, 1.0);
        let co = sa * cs_ + da * (1.0 - sa) * cb[c];
        out[c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out[3] = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
    out
}

#[inline]
fn sep(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => hard_light(s, b),
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::ColorDodge => {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(b, s),
        BlendMode::SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        BlendMode::Difference => (b - s).abs(),
        _ => s,
    }
}

#[inline]
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 { b * 2.0 * s } else { let s2 = 2.0 * s - 1.0; b + s2 - b * s2 }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap());
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut o = [0.0; 3];
    if c[mx] > c[mn] {
        o[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        o[mx] = s;
    }
    o
}
