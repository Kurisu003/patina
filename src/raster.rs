//! Helpers for putting rasterised content onto tiled surfaces while respecting
//! the selection. Mask tiles and surface tiles share the same grid, so tile
//! index `i` lines up in both.

use crate::selection::{MTile, Selection};
use crate::surface::{IRect, Surface, TILE, mul8};
use tiny_skia::Pixmap;

/// Selection coverage tile for tile index `i` (None = fully selected).
#[inline]
pub fn sel_tile<'a>(sel: &'a Selection, i: usize) -> Option<&'a MTile> {
    sel.mask.as_ref().map(|m| &m.tiles[i])
}

#[inline]
pub fn over(dst: &mut [u8], s: [u8; 4], cov: u8) {
    if cov == 0 {
        return;
    }
    let s = if cov == 255 { s } else { [mul8(s[0] as u32, cov as u32), mul8(s[1] as u32, cov as u32), mul8(s[2] as u32, cov as u32), mul8(s[3] as u32, cov as u32)] };
    let inv = 255 - s[3] as u32;
    for c in 0..4 {
        dst[c] = (s[c] as u32 + mul8(dst[c] as u32, inv) as u32).min(255) as u8;
    }
}

/// Linear interpolation between two premultiplied pixels by `t` (0..=255).
#[inline]
pub fn lerp_px(a: &[u8], b: [u8; 4], t: u8) -> [u8; 4] {
    let t = t as u32;
    let it = 255 - t;
    [
        (mul8(a[0] as u32, it) as u32 + mul8(b[0] as u32, t) as u32).min(255) as u8,
        (mul8(a[1] as u32, it) as u32 + mul8(b[1] as u32, t) as u32).min(255) as u8,
        (mul8(a[2] as u32, it) as u32 + mul8(b[2] as u32, t) as u32).min(255) as u8,
        (mul8(a[3] as u32, it) as u32 + mul8(b[3] as u32, t) as u32).min(255) as u8,
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    /// Source-over blending.
    Over,
    /// Replace pixels (still weighted by selection coverage).
    Replace,
}

/// `target` = `base` with `pm` (premultiplied) drawn at `origin`, clipped by
/// the selection. `target` must start as a clone of `base`.
pub fn paste_pixmap(target: &mut Surface, base: &Surface, pm: &Pixmap, origin: (i32, i32), sel: &Selection, mode: PasteMode) {
    let r = IRect::new(origin.0, origin.1, pm.width() as i32, pm.height() as i32);
    paste_rgba(target, base, pm.data(), r, sel, mode);
}

pub fn paste_rgba(target: &mut Surface, base: &Surface, data: &[u8], r: IRect, sel: &Selection, mode: PasteMode) {
    *target = base.clone();
    let stride = r.w as usize * 4;
    let tiles_x = target.tiles_x;
    let alloc = true;
    target.par_tiles_mut(r, alloc, |t, tr| {
        let i = ((tr.y as usize / TILE) as u32 * tiles_x + (tr.x as usize / TILE) as u32) as usize;
        let st = sel_tile(sel, i);
        if matches!(st, Some(MTile::Empty)) {
            return;
        }
        let c = tr.intersect(&r);
        for y in c.y..c.bottom() {
            let ly = (y - tr.y) as usize;
            for x in c.x..c.right() {
                let lx = (x - tr.x) as usize;
                let cov = st.map_or(255, |m| m.get(lx, ly));
                if cov == 0 {
                    continue;
                }
                let si = (y - r.y) as usize * stride + (x - r.x) as usize * 4;
                let s = [data[si], data[si + 1], data[si + 2], data[si + 3]];
                let di = (ly * TILE + lx) * 4;
                let d = &mut t.px[di..di + 4];
                match mode {
                    PasteMode::Over => over(d, s, cov),
                    PasteMode::Replace => {
                        let v = lerp_px(d, s, cov);
                        d.copy_from_slice(&v);
                    }
                }
            }
        }
    });
    target.clear_outside();
}

/// Apply a per-pixel function to all pixels in `r` (and in the selection),
/// writing `base` -> `target`. Coverage blending is done for you.
pub fn map_pixels<F>(target: &mut Surface, base: &Surface, r: IRect, sel: &Selection, f: F)
where
    F: Fn(i32, i32, [u8; 4]) -> [u8; 4] + Sync + Send,
{
    *target = base.clone();
    let tiles_x = target.tiles_x;
    target.par_tiles_mut(r, true, |t, tr| {
        let i = ((tr.y as usize / TILE) as u32 * tiles_x + (tr.x as usize / TILE) as u32) as usize;
        let st = sel_tile(sel, i);
        if matches!(st, Some(MTile::Empty)) {
            return;
        }
        let c = tr.intersect(&r);
        for y in c.y..c.bottom() {
            let ly = (y - tr.y) as usize;
            for x in c.x..c.right() {
                let lx = (x - tr.x) as usize;
                let cov = st.map_or(255, |m| m.get(lx, ly));
                if cov == 0 {
                    continue;
                }
                let di = (ly * TILE + lx) * 4;
                let d = &mut t.px[di..di + 4];
                let v = f(x, y, [d[0], d[1], d[2], d[3]]);
                let v = if cov == 255 { v } else { lerp_px(d, v, cov) };
                d.copy_from_slice(&v);
            }
        }
    });
    target.clear_outside();
}

/// Premultiplied tiny-skia colour from straight RGBA8.
pub fn sk_color(c: [u8; 4]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3])
}
