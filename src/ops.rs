//! Image and layer operations (Image and Layers menus).

use crate::document::{DocState, Document, Layer};
use crate::selection::{Mask, Selection};
use crate::surface::{IRect, Surface, TILE};
use std::sync::Arc;

/// Build a new surface of size (w, h) where each output pixel comes from
/// `f(x, y)` -> source coordinate (or None for transparent). Parallel per tile.
pub fn remap(src: &Surface, w: u32, h: u32, f: impl Fn(i32, i32) -> Option<(i32, i32)> + Sync) -> Surface {
    let mut out = Surface::new(w, h);
    let all = out.bounds();
    out.par_tiles_mut(all, true, |t, tr| {
        let c = tr.intersect(&all);
        for y in c.y..c.bottom() {
            for x in c.x..c.right() {
                if let Some((sx, sy)) = f(x, y) {
                    let p = src.get_pixel(sx, sy);
                    let i = ((y - tr.y) as usize * TILE + (x - tr.x) as usize) * 4;
                    t.px[i..i + 4].copy_from_slice(&p);
                }
            }
        }
    });
    out.compact();
    out
}

fn remap_mask(m: &Mask, w: u32, h: u32, f: impl Fn(i32, i32) -> Option<(i32, i32)> + Sync) -> Mask {
    Mask::from_fn(w, h, IRect::new(0, 0, w as i32, h as i32), |x, y| f(x, y).map_or(0, |(sx, sy)| m.get(sx, sy)))
}

/// Apply the same geometric mapping to every layer and the selection.
fn transform_doc(doc: &mut Document, w: u32, h: u32, f: impl Fn(i32, i32) -> Option<(i32, i32)> + Sync + Copy, label: &str) {
    let st = &mut doc.state;
    for l in &mut st.layers {
        l.surface = remap(&l.surface, w, h, f);
    }
    st.selection = Selection { mask: st.selection.mask.as_ref().map(|m| remap_mask(m, w, h, f)) };
    st.width = w;
    st.height = h;
    doc.commit(label);
}

pub fn flip_image_h(doc: &mut Document) {
    let w = doc.state.width as i32;
    let (ww, hh) = (doc.state.width, doc.state.height);
    transform_doc(doc, ww, hh, move |x, y| Some((w - 1 - x, y)), "Flip Image Horizontal");
}

pub fn flip_image_v(doc: &mut Document) {
    let h = doc.state.height as i32;
    let (ww, hh) = (doc.state.width, doc.state.height);
    transform_doc(doc, ww, hh, move |x, y| Some((x, h - 1 - y)), "Flip Image Vertical");
}

pub fn rotate_cw(doc: &mut Document) {
    let (w, h) = (doc.state.width, doc.state.height);
    let hh = h as i32;
    // new (x, y) <- old (y, H-1-x)
    transform_doc(doc, h, w, move |x, y| Some((y, hh - 1 - x)), "Rotate 90° Clockwise");
    doc.view.fit_pending = true;
}

pub fn rotate_ccw(doc: &mut Document) {
    let (w, h) = (doc.state.width, doc.state.height);
    let ww = w as i32;
    transform_doc(doc, h, w, move |x, y| Some((ww - 1 - y, x)), "Rotate 90° Counter-Clockwise");
    doc.view.fit_pending = true;
}

pub fn rotate_180(doc: &mut Document) {
    let (w, h) = (doc.state.width, doc.state.height);
    let (ww, hh) = (w as i32, h as i32);
    transform_doc(doc, w, h, move |x, y| Some((ww - 1 - x, hh - 1 - y)), "Rotate 180°");
}

pub fn crop(doc: &mut Document, r: IRect, label: &str) {
    let r = r.intersect(&IRect::new(0, 0, doc.state.width as i32, doc.state.height as i32));
    if r.is_empty() {
        return;
    }
    let mask = doc.state.selection.mask.clone();
    let st = &mut doc.state;
    for l in &mut st.layers {
        let src = l.surface.clone();
        let m = mask.clone();
        l.surface = remap(&src, r.w as u32, r.h as u32, move |x, y| {
            let (sx, sy) = (x + r.x, y + r.y);
            // crop to selection clears unselected pixels (non-rectangular selections)
            if let Some(m) = &m {
                if m.get(sx, sy) == 0 {
                    return None;
                }
            }
            Some((sx, sy))
        });
    }
    st.width = r.w as u32;
    st.height = r.h as u32;
    st.selection = Selection::default();
    doc.view.fit_pending = true;
    doc.commit(label);
}

pub fn crop_to_selection(doc: &mut Document) {
    if let Some(b) = doc.state.selection.mask.as_ref().and_then(|m| m.bounds()) {
        crop(doc, b, "Crop to Selection");
    }
}

pub fn auto_crop(doc: &mut Document) {
    let flat = crate::tools::flatten(doc);
    // Pinta auto-crop removes borders matching the top-left colour.
    let c0 = flat.get_pixel(0, 0);
    let (w, h) = (flat.width as i32, flat.height as i32);
    let row_same = |y: i32| (0..w).all(|x| flat.get_pixel(x, y) == c0);
    let col_same = |x: i32, y0: i32, y1: i32| (y0..y1).all(|y| flat.get_pixel(x, y) == c0);
    let mut top = 0;
    while top < h && row_same(top) {
        top += 1;
    }
    if top == h {
        return;
    }
    let mut bottom = h;
    while bottom > top && row_same(bottom - 1) {
        bottom -= 1;
    }
    let mut left = 0;
    while left < w && col_same(left, top, bottom) {
        left += 1;
    }
    let mut right = w;
    while right > left && col_same(right - 1, top, bottom) {
        right -= 1;
    }
    let r = IRect::new(left, top, right - left, bottom - top);
    if r != IRect::new(0, 0, w, h) {
        doc.state.selection = Selection::default();
        crop(doc, r, "Auto Crop");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resample {
    NearestNeighbor,
    Bilinear,
    /// Box-filter area average (best for downscaling).
    Supersampling,
}

pub fn resample(src: &Surface, w: u32, h: u32, mode: Resample) -> Surface {
    let sx = src.width as f32 / w as f32;
    let sy = src.height as f32 / h as f32;
    let mut out = Surface::new(w, h);
    let all = out.bounds();
    let (swi, shi) = (src.width as i32, src.height as i32);
    out.par_tiles_mut(all, true, |t, tr| {
        let c = tr.intersect(&all);
        for y in c.y..c.bottom() {
            for x in c.x..c.right() {
                let p: [u8; 4] = match mode {
                    Resample::NearestNeighbor => src.get_pixel(((x as f32 + 0.5) * sx) as i32, ((y as f32 + 0.5) * sy) as i32),
                    Resample::Bilinear | Resample::Supersampling if mode == Resample::Bilinear || (sx <= 1.0 && sy <= 1.0) => {
                        let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (swi - 1) as f32);
                        let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (shi - 1) as f32);
                        let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
                        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
                        let g = |xx: i32, yy: i32| src.get_pixel(xx.min(swi - 1), yy.min(shi - 1));
                        let (p00, p10, p01, p11) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
                        let mut o = [0u8; 4];
                        for k in 0..4 {
                            let top = p00[k] as f32 * (1.0 - ax) + p10[k] as f32 * ax;
                            let bot = p01[k] as f32 * (1.0 - ax) + p11[k] as f32 * ax;
                            o[k] = (top * (1.0 - ay) + bot * ay).round() as u8;
                        }
                        o
                    }
                    _ => {
                        // area average with fractional edge weights
                        let x0 = x as f32 * sx;
                        let x1 = x0 + sx;
                        let y0 = y as f32 * sy;
                        let y1 = y0 + sy;
                        let mut acc = [0f32; 4];
                        let mut wsum = 0f32;
                        let mut yy = y0.floor() as i32;
                        while (yy as f32) < y1 && yy < shi {
                            let wy = ((yy + 1) as f32).min(y1) - (yy as f32).max(y0);
                            let mut xx = x0.floor() as i32;
                            while (xx as f32) < x1 && xx < swi {
                                let wx = ((xx + 1) as f32).min(x1) - (xx as f32).max(x0);
                                let wgt = wx * wy;
                                let p = src.get_pixel(xx, yy);
                                for k in 0..4 {
                                    acc[k] += p[k] as f32 * wgt;
                                }
                                wsum += wgt;
                                xx += 1;
                            }
                            yy += 1;
                        }
                        let mut o = [0u8; 4];
                        for k in 0..4 {
                            o[k] = (acc[k] / wsum.max(1e-6)).round().clamp(0.0, 255.0) as u8;
                        }
                        o
                    }
                };
                let i = ((y - tr.y) as usize * TILE + (x - tr.x) as usize) * 4;
                t.px[i..i + 4].copy_from_slice(&p);
            }
        }
    });
    out.compact();
    out
}

pub fn resize_image(doc: &mut Document, w: u32, h: u32, mode: Resample) {
    if w == 0 || h == 0 || (w == doc.state.width && h == doc.state.height) {
        return;
    }
    let st = &mut doc.state;
    for l in &mut st.layers {
        l.surface = resample(&l.surface, w, h, mode);
    }
    st.width = w;
    st.height = h;
    st.selection = Selection::default();
    doc.view.fit_pending = true;
    doc.commit("Resize Image");
}

/// Anchor 0..9 in reading order (0 = top-left, 4 = centre).
pub fn resize_canvas(doc: &mut Document, w: u32, h: u32, anchor: u8) {
    if w == 0 || h == 0 {
        return;
    }
    let (ow, oh) = (doc.state.width as i32, doc.state.height as i32);
    let ax = (anchor % 3) as i32;
    let ay = (anchor / 3) as i32;
    let dx = (w as i32 - ow) * ax / 2;
    let dy = (h as i32 - oh) * ay / 2;
    let f = move |x: i32, y: i32| {
        let (sx, sy) = (x - dx, y - dy);
        if sx < 0 || sy < 0 || sx >= ow || sy >= oh { None } else { Some((sx, sy)) }
    };
    transform_doc(doc, w, h, f, "Resize Canvas");
    doc.view.fit_pending = true;
}

pub fn flatten_image(doc: &mut Document) {
    if doc.state.layers.len() < 2 {
        return;
    }
    let flat = crate::tools::flatten(doc);
    let name = doc.state.layers[0].name.clone();
    doc.state.layers = vec![Layer::new(name, flat)];
    doc.state.current = 0;
    doc.commit("Flatten");
}

// ---- layers -----------------------------------------------------------

pub fn add_layer(doc: &mut Document) {
    let st = &mut doc.state;
    let n = st.layers.len() + 1;
    let l = Layer::new(format!("Layer {n}"), Surface::new(st.width, st.height));
    st.layers.insert(st.current + 1, l);
    st.current += 1;
    doc.commit("Add New Layer");
}

pub fn delete_layer(doc: &mut Document) {
    let st = &mut doc.state;
    if st.layers.len() <= 1 {
        return;
    }
    st.layers.remove(st.current);
    st.current = st.current.min(st.layers.len() - 1);
    doc.commit("Delete Layer");
}

pub fn duplicate_layer(doc: &mut Document) {
    let st = &mut doc.state;
    let mut l = st.layers[st.current].clone();
    l.id = crate::document::next_id();
    l.name = format!("{} copy", l.name);
    st.layers.insert(st.current + 1, l);
    st.current += 1;
    doc.commit("Duplicate Layer");
}

pub fn merge_down(doc: &mut Document) {
    let st = &mut doc.state;
    if st.current == 0 {
        return;
    }
    let top = st.layers[st.current].clone();
    let below = &mut st.layers[st.current - 1];
    let op = (top.opacity * 255.0).round() as u8;
    if !top.hidden {
        let bounds = below.surface.bounds();
        let tiles_x = below.surface.tiles_x;
        below.surface.par_tiles_mut(bounds, true, |t, tr| {
            let i = ((tr.y as usize / TILE) as u32 * tiles_x + (tr.x as usize / TILE) as u32) as usize;
            if let Some(src) = top.surface.tiles[i].as_ref() {
                crate::blend::blend_row(&mut t.px[..], &src.px[..], op, top.blend);
            }
        });
        below.surface.compact();
    }
    st.layers.remove(st.current);
    st.current -= 1;
    doc.commit("Merge Layer Down");
}

pub fn move_layer(doc: &mut Document, up: bool) {
    let st = &mut doc.state;
    let i = st.current;
    if up && i + 1 < st.layers.len() {
        st.layers.swap(i, i + 1);
        st.current += 1;
        doc.commit("Move Layer Up");
    } else if !up && i > 0 {
        st.layers.swap(i, i - 1);
        st.current -= 1;
        doc.commit("Move Layer Down");
    }
}

pub fn flip_layer(doc: &mut Document, horizontal: bool) {
    let (w, h) = (doc.state.width as i32, doc.state.height as i32);
    let l = doc.state.layer_mut();
    l.surface = remap(&l.surface, w as u32, h as u32, move |x, y| Some(if horizontal { (w - 1 - x, y) } else { (x, h - 1 - y) }));
    doc.commit(if horizontal { "Flip Layer Horizontal" } else { "Flip Layer Vertical" });
}

/// Add a new layer with pixels (premultiplied RGBA) placed at (x, y).
pub fn import_layer(doc: &mut Document, name: &str, w: u32, h: u32, data: &[u8], at: (i32, i32)) {
    let st = &mut doc.state;
    let mut s = Surface::new(st.width, st.height);
    s.write_rect(IRect::new(at.0, at.1, w as i32, h as i32), data);
    s.compact();
    st.layers.insert(st.current + 1, Layer::new(name, s));
    st.current += 1;
    doc.commit("Import From File");
}

// ---- edit -------------------------------------------------------------

pub fn erase_selection(doc: &mut Document) {
    let sel = doc.state.selection.clone();
    let r = sel.bounds(doc.state.width, doc.state.height);
    let base = doc.state.layer().surface.clone();
    let mut target = base.clone();
    crate::raster::map_pixels(&mut target, &base, r, &sel, |_, _, _| [0, 0, 0, 0]);
    target.compact();
    doc.state.layer_mut().surface = target;
    doc.commit("Erase Selection");
}

pub fn fill_selection(doc: &mut Document, color: [u8; 4]) {
    let sel = doc.state.selection.clone();
    let r = sel.bounds(doc.state.width, doc.state.height);
    let c = crate::surface::premul(color);
    let base = doc.state.layer().surface.clone();
    let mut target = base.clone();
    crate::raster::map_pixels(&mut target, &base, r, &sel, move |_, _, _| c);
    doc.state.layer_mut().surface = target;
    doc.commit("Fill Selection");
}

pub fn select_all(doc: &mut Document) {
    let (w, h) = (doc.state.width, doc.state.height);
    doc.state.selection = Selection { mask: Some(Mask::full(w, h)) };
    doc.commit("Select All");
}

pub fn deselect(doc: &mut Document) {
    if doc.state.selection.is_active() {
        doc.state.selection = Selection::default();
        doc.commit("Deselect All");
    }
}

pub fn invert_selection(doc: &mut Document) {
    let (w, h) = (doc.state.width, doc.state.height);
    let m = doc.state.selection.mask.clone().unwrap_or_else(|| Mask::empty(w, h));
    doc.state.selection = Selection { mask: Some(m.invert()) };
    doc.commit("Invert Selection");
}

/// Selected pixels of the current layer (or the flattened image), cropped to
/// the selection bounds. Returns (w, h, premultiplied rgba).
pub fn copy_selection(doc: &Document, merged: bool) -> Option<(IRect, Vec<u8>)> {
    let st: &DocState = &doc.state;
    let r = st.selection.bounds(st.width, st.height);
    if r.is_empty() {
        return None;
    }
    let src = if merged { crate::tools::flatten(doc) } else { st.layer().surface.clone() };
    let mut data = src.read_rect(r);
    if let Some(m) = &st.selection.mask {
        for y in 0..r.h {
            for x in 0..r.w {
                let c = m.get(r.x + x, r.y + y) as u32;
                if c < 255 {
                    let i = ((y * r.w + x) * 4) as usize;
                    for k in 0..4 {
                        data[i + k] = crate::surface::mul8(data[i + k] as u32, c);
                    }
                }
            }
        }
    }
    Some((r, data))
}

/// Paste premultiplied pixels into the current layer at `at`, selecting them.
pub fn paste(doc: &mut Document, w: u32, h: u32, data: &[u8], at: (i32, i32), new_layer: bool) {
    // Grow the canvas if the pasted image is larger (Pinta asks; we expand).
    let (cw, ch) = (doc.state.width, doc.state.height);
    if w > cw || h > ch {
        let nw = cw.max(w);
        let nh = ch.max(h);
        let st = &mut doc.state;
        for l in &mut st.layers {
            let src = l.surface.clone();
            l.surface = remap(&src, nw, nh, move |x, y| if x < cw as i32 && y < ch as i32 { Some((x, y)) } else { None });
        }
        st.width = nw;
        st.height = nh;
        st.selection = Selection::default();
        doc.view.fit_pending = true;
    }
    let at = (at.0.clamp(0, (doc.state.width - w) as i32), at.1.clamp(0, (doc.state.height - h) as i32));
    let r = IRect::new(at.0, at.1, w as i32, h as i32);
    if new_layer {
        let st = &mut doc.state;
        let n = st.layers.len() + 1;
        st.layers.insert(st.current + 1, Layer::new(format!("Layer {n}"), Surface::new(st.width, st.height)));
        st.current += 1;
    }
    let base = doc.state.layer().surface.clone();
    let mut target = base.clone();
    crate::raster::paste_rgba(&mut target, &base, data, r, &Selection::default(), crate::raster::PasteMode::Over);
    doc.state.layer_mut().surface = target;
    let (dw, dh) = (doc.state.width, doc.state.height);
    doc.state.selection = Selection { mask: Some(Mask::from_rect(dw, dh, r)) };
    doc.commit(if new_layer { "Paste Into New Layer" } else { "Paste" });
}

pub fn layer_thumbnail(s: &Surface, size: u32) -> (u32, u32, Vec<u8>) {
    let scale = (size as f32 / s.width.max(s.height) as f32).min(1.0);
    let tw = ((s.width as f32 * scale).round() as u32).max(1);
    let th = ((s.height as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let sx = ((x as f32 + 0.5) / scale) as i32;
            let sy = ((y as f32 + 0.5) / scale) as i32;
            let p = crate::surface::unpremul(s.get_pixel(sx, sy));
            let i = ((y * tw + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&p);
        }
    }
    let _ = Arc::strong_count(&s.tiles);
    (tw, th, out)
}
