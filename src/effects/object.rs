//! Object menu: Align Object, Feather Object, Outline Object.
//!
//! Ported from Pinta (MIT). These effects look at the "object" (pixels that
//! differ from the background / are more opaque than a tolerance) inside the
//! selection bounds. The whole-selection analysis (object bounds, distance to
//! the object's edge) happens in `prepare()`, so tiles render independently.
//!
//! Pinta finds edge pixels and then, for every pixel, loops over all edge
//! pixels within the radius. Here the same "nearest edge pixel" distance comes
//! from an exact Euclidean distance transform, which gives identical results
//! in linear time.

use super::util::slider_i;
use super::{Category, Effect, RenderCtx, Src};
use crate::surface::{IRect, Surface};
use rayon::prelude::*;
use std::sync::Arc;

pub fn all() -> Vec<Box<dyn Effect>> {
    vec![Box::new(AlignObject::default()), Box::new(FeatherObject::default()), Box::new(OutlineObject::default())]
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

/// Straight -> premultiplied with truncation (ColorBgra.ToPremultipliedAlpha).
#[inline]
fn to_premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    let f = |v: u8| (v as u32 * a / 255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

/// ColorBgra.NewAlpha: same straight colour, different alpha.
#[inline]
fn new_alpha(p: [u8; 4], a: u8) -> [u8; 4] {
    let s = to_straight(p);
    to_premul([s[0], s[1], s[2], a])
}

// ---------------------------------------------------------------------------
// Edge distance map shared by Feather Object and Outline Object
// ---------------------------------------------------------------------------

/// Squared distance from every pixel of `area` to the nearest edge pixel
/// inside `area`, saturated at `u16::MAX`.
#[derive(Clone)]
struct EdgeDistance {
    area: IRect,
    d2: Arc<Vec<u16>>,
}

impl EdgeDistance {
    /// Edge pixels are pixels of `area` with alpha <= `tol` that touch (4- or
    /// 8-connected) an in-image pixel with alpha > `tol`; with `canvas_edge`
    /// every pixel on the image border counts as well.
    fn compute(src: &Surface, area: IRect, tol: i32, eight: bool, canvas_edge: bool) -> Self {
        let (w, h) = (area.w as usize, area.h as usize);
        if area.is_empty() {
            return EdgeDistance { area, d2: Arc::new(vec![]) };
        }
        let (iw, ih) = (src.width as i32, src.height as i32);
        // alpha of the area plus a one pixel ring (clipped to the image)
        let ext = area.inflate(1).intersect(&src.bounds());
        let rgba = src.read_rect(ext);
        let alpha: Vec<u8> = rgba.par_chunks_exact(4).map(|p| p[3]).collect();
        let a_at = |x: i32, y: i32| alpha[((y - ext.y) * ext.w + (x - ext.x)) as usize] as i32;

        let mut edge = vec![false; w * h];
        edge.par_chunks_mut(w).enumerate().for_each(|(row, line)| {
            let y = area.y + row as i32;
            for (i, e) in line.iter_mut().enumerate() {
                let x = area.x + i as i32;
                if canvas_edge && (x == 0 || x == iw - 1 || y == 0 || y == ih - 1) {
                    *e = true;
                    continue;
                }
                if a_at(x, y) > tol {
                    continue;
                }
                let mut hit = false;
                let mut probe = |dx: i32, dy: i32| {
                    let (nx, ny) = (x + dx, y + dy);
                    if !hit && nx >= 0 && ny >= 0 && nx < iw && ny < ih && a_at(nx, ny) > tol {
                        hit = true;
                    }
                };
                probe(-1, 0);
                probe(1, 0);
                probe(0, -1);
                probe(0, 1);
                if eight {
                    probe(-1, -1);
                    probe(-1, 1);
                    probe(1, -1);
                    probe(1, 1);
                }
                *e = hit;
            }
        });

        // Column pass: vertical distance to the nearest edge pixel.
        const FAR: u32 = u32::MAX;
        let mut g = vec![FAR; w * h];
        for x in 0..w {
            if edge[x] {
                g[x] = 0;
            }
        }
        for y in 1..h {
            let (prev, cur) = g.split_at_mut(y * w);
            let prev = &prev[(y - 1) * w..];
            let erow = &edge[y * w..(y + 1) * w];
            for x in 0..w {
                cur[x] = if erow[x] { 0 } else { prev[x].saturating_add(1) };
            }
        }
        for y in (0..h.saturating_sub(1)).rev() {
            let (cur, next) = g.split_at_mut((y + 1) * w);
            let cur = &mut cur[y * w..];
            for x in 0..w {
                cur[x] = cur[x].min(next[x].saturating_add(1));
            }
        }

        // Row pass: lower envelope of parabolas (Felzenszwalb & Huttenlocher).
        let mut d2 = vec![u16::MAX; w * h];
        d2.par_chunks_mut(w).zip(g.par_chunks(w)).for_each_init(
            || (vec![0usize; w], vec![0f64; w + 1]),
            |(v, z), (out, grow)| {
                let f = |q: usize| {
                    let gq = grow[q] as f64;
                    gq * gq
                };
                let mut k: isize = -1;
                for q in 0..w {
                    if grow[q] == FAR {
                        continue;
                    }
                    let fq = f(q) + (q * q) as f64;
                    let mut s = f64::NEG_INFINITY;
                    while k >= 0 {
                        let p = v[k as usize];
                        s = (fq - (f(p) + (p * p) as f64)) / (2.0 * (q as f64 - p as f64));
                        if s <= z[k as usize] {
                            k -= 1;
                        } else {
                            break;
                        }
                    }
                    if k < 0 {
                        s = f64::NEG_INFINITY;
                    }
                    k += 1;
                    v[k as usize] = q;
                    z[k as usize] = s;
                }
                if k < 0 {
                    return; // no edge pixel reachable in this row
                }
                let mut j = 0usize;
                for (q, o) in out.iter_mut().enumerate() {
                    while j < k as usize && z[j + 1] < q as f64 {
                        j += 1;
                    }
                    let p = v[j];
                    let dx = q as f64 - p as f64;
                    let d = dx * dx + f(p);
                    *o = d.min(u16::MAX as f64) as u16;
                }
            },
        );
        EdgeDistance { area, d2: Arc::new(d2) }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u16 {
        debug_assert!(self.area.contains(x, y));
        self.d2[((y - self.area.y) * self.area.w + (x - self.area.x)) as usize]
    }
}

// ---------------------------------------------------------------------------
// Align Object
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignPosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl AlignPosition {
    const GRID: [[(AlignPosition, &'static str); 3]; 3] = [
        [(AlignPosition::TopLeft, "Top Left"), (AlignPosition::TopCenter, "Top Center"), (AlignPosition::TopRight, "Top Right")],
        [(AlignPosition::CenterLeft, "Center Left"), (AlignPosition::Center, "Center"), (AlignPosition::CenterRight, "Center Right")],
        [(AlignPosition::BottomLeft, "Bottom Left"), (AlignPosition::BottomCenter, "Bottom Center"), (AlignPosition::BottomRight, "Bottom Right")],
    ];
}

/// Moves the object (everything that differs from the selection's top-left
/// pixel) to a side or corner of the selection, filling the rest with that
/// background colour.
#[derive(Clone)]
pub struct AlignObject {
    pub position: AlignPosition,
    plan: Option<AlignPlan>,
}

#[derive(Clone, Copy)]
struct AlignPlan {
    background: [u8; 4],
    object: IRect,
    target: IRect,
}

impl Default for AlignObject {
    fn default() -> Self {
        AlignObject { position: AlignPosition::Center, plan: None }
    }
}

/// Bounding box of pixels inside `area` that differ from `bg`.
fn object_bounds(src: &Surface, area: IRect, bg: [u8; 4]) -> Option<IRect> {
    let px = src.read_rect(area);
    let w = area.w as usize;
    let rows: Vec<Option<(i32, i32)>> = px
        .par_chunks_exact(w * 4)
        .map(|row| {
            let first = row.chunks_exact(4).position(|p| p != bg)?;
            let last = row.chunks_exact(4).rposition(|p| p != bg)?;
            Some((first as i32, last as i32 + 1))
        })
        .collect();
    let y0 = rows.iter().position(|r| r.is_some())?;
    let y1 = rows.iter().rposition(|r| r.is_some())? + 1;
    let (x0, x1) = rows.iter().flatten().fold((i32::MAX, i32::MIN), |(a, b), &(f, l)| (a.min(f), b.max(l)));
    Some(IRect::new(area.x + x0, area.y + y0 as i32, x1 - x0, (y1 - y0) as i32))
}

impl AlignObject {
    fn plan(&self, src: &Surface, ctx: &RenderCtx) -> Option<AlignPlan> {
        let sel = ctx.sel_bounds.intersect(&src.bounds());
        if sel.is_empty() {
            return None;
        }
        let background = src.get_pixel(sel.x, sel.y);
        let object = object_bounds(src, sel, background)?;
        let (ow, oh) = (object.w, object.h);
        let left = sel.x;
        let hcenter = sel.x + sel.w / 2 - ow / 2;
        let right = sel.right() - ow;
        let top = sel.y;
        let vcenter = sel.y + sel.h / 2 - oh / 2;
        let bottom = sel.bottom() - oh;
        use AlignPosition::*;
        let (x, y) = match self.position {
            TopLeft => (left, top),
            TopCenter => (hcenter, top),
            TopRight => (right, top),
            CenterLeft => (left, vcenter),
            Center => (hcenter, vcenter),
            CenterRight => (right, vcenter),
            BottomLeft => (left, bottom),
            BottomCenter => (hcenter, bottom),
            BottomRight => (right, bottom),
        };
        Some(AlignPlan { background, object, target: IRect::new(x, y, ow, oh) })
    }
}

impl Effect for AlignObject {
    fn name(&self) -> &'static str {
        "Align Object"
    }
    fn category(&self) -> Category {
        Category::Object
    }
    fn icon(&self) -> &'static str {
        "tool-move-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        egui::Grid::new("align_object_grid").num_columns(3).show(ui, |ui| {
            for row in AlignPosition::GRID {
                for (pos, label) in row {
                    changed |= ui.selectable_value(&mut self.position, pos, label).changed();
                }
                ui.end_row();
            }
        });
        changed
    }
    fn prepare(&mut self, src: &Surface, ctx: &RenderCtx) {
        self.plan = self.plan(src, ctx);
    }
    fn render(&self, src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        // Without an object every pixel already equals the background.
        let Some(plan) = self.plan else { return };
        for p in dst.chunks_exact_mut(4) {
            p.copy_from_slice(&plan.background);
        }
        let inside = r.intersect(&plan.target);
        if inside.is_empty() {
            return;
        }
        let (dx, dy) = (plan.object.x - plan.target.x, plan.object.y - plan.target.y);
        let from = IRect::new(inside.x + dx, inside.y + dy, inside.w, inside.h);
        let data = src.surface.read_rect(from);
        let row = (inside.w * 4) as usize;
        for y in 0..inside.h {
            let o = (((inside.y - r.y + y) * r.w + (inside.x - r.x)) * 4) as usize;
            dst[o..o + row].copy_from_slice(&data[y as usize * row..(y as usize + 1) * row]);
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Feather Object
// ---------------------------------------------------------------------------

/// Fades the object's alpha towards its edge.
#[derive(Clone)]
pub struct FeatherObject {
    pub radius: i32,
    pub tolerance: i32,
    pub feather_canvas_edge: bool,
    dist: Option<EdgeDistance>,
}

impl Default for FeatherObject {
    fn default() -> Self {
        FeatherObject { radius: 6, tolerance: 20, feather_canvas_edge: false, dist: None }
    }
}

impl Effect for FeatherObject {
    fn name(&self) -> &'static str {
        "Feather Object"
    }
    fn category(&self) -> Category {
        Category::Object
    }
    fn icon(&self) -> &'static str {
        "effects-object-featherobject-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Radius", &mut self.radius, 1, 100);
        c |= slider_i(ui, "Tolerance", &mut self.tolerance, 0, 255);
        c |= ui.checkbox(&mut self.feather_canvas_edge, "Feather Canvas Edge").changed();
        c
    }
    fn prepare(&mut self, src: &Surface, ctx: &RenderCtx) {
        let area = ctx.sel_bounds.intersect(&src.bounds());
        self.dist = Some(EdgeDistance::compute(src, area, self.tolerance, false, self.feather_canvas_edge));
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, _ctx: &RenderCtx) {
        let Some(dist) = &self.dist else { return };
        let radius = self.radius.max(1);
        let rf = radius as f32;
        let r2 = (radius * radius) as u32;
        for y in 0..r.h {
            for x in 0..r.w {
                let o = ((y * r.w + x) * 4) as usize;
                let a = dst[o + 3];
                if a == 0 {
                    continue;
                }
                let d2 = dist.get(r.x + x, r.y + y) as u32;
                if d2 >= r2 {
                    continue;
                }
                let mult = (d2 as f32).sqrt() / rf;
                let na = (a as f32 * mult) as u8;
                if na < a {
                    let p = new_alpha([dst[o], dst[o + 1], dst[o + 2], a], na);
                    dst[o..o + 4].copy_from_slice(&p);
                }
            }
        }
    }
    fn box_clone(&self) -> Box<dyn Effect> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Outline Object
// ---------------------------------------------------------------------------

/// Draws an outline (primary colour, optionally fading to the secondary)
/// behind the object.
#[derive(Clone)]
pub struct OutlineObject {
    pub radius: i32,
    pub tolerance: i32,
    pub alpha_gradient: bool,
    pub color_gradient: bool,
    pub outline_border: bool,
    pub fill_object_background: bool,
    dist: Option<EdgeDistance>,
}

impl Default for OutlineObject {
    fn default() -> Self {
        OutlineObject {
            radius: 6,
            tolerance: 20,
            alpha_gradient: true,
            color_gradient: true,
            outline_border: false,
            fill_object_background: true,
            dist: None,
        }
    }
}

/// Mathematics.LerpByte.
#[inline]
fn lerp_byte(from: u8, to: u8, frac: u8) -> u8 {
    (from as i32 + (to as i32 - from as i32) * frac as i32 / 255) as u8
}

/// Normal blend of premultiplied `top` over `bottom` (Pinta's NormalBlendOp).
#[inline]
fn normal_blend(bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    if top[3] == 255 {
        return top;
    }
    if top[3] == 0 {
        return bottom;
    }
    let (ta, ba) = (top[3] as i32, bottom[3] as i32);
    let (inv_t, inv_b) = (255 - ta, 255 - ba);
    let mut out = [0u8; 4];
    for c in 0..3 {
        let (cb, ca) = (bottom[c] as i32, top[c] as i32);
        let v = inv_b * ca + inv_t * cb + ba * ca;
        out[c] = ((v + 128) / 255).clamp(0, 255) as u8;
    }
    out[3] = (ta + (ba * inv_t + 128) / 255).clamp(0, 255) as u8;
    out
}

impl OutlineObject {
    /// Pinta treats radius 0 like 1 when measuring distances.
    fn effective_radius(&self) -> i32 {
        self.radius.max(1)
    }
}

impl Effect for OutlineObject {
    fn name(&self) -> &'static str {
        "Outline Object"
    }
    fn category(&self) -> Category {
        Category::Object
    }
    fn icon(&self) -> &'static str {
        "effects-stylize-outline-symbolic"
    }
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut c = slider_i(ui, "Radius", &mut self.radius, 0, 100);
        c |= slider_i(ui, "Tolerance", &mut self.tolerance, 0, 255);
        c |= ui.checkbox(&mut self.alpha_gradient, "Alpha Gradient").changed();
        c |= ui.checkbox(&mut self.color_gradient, "Color Gradient").changed();
        c |= ui.checkbox(&mut self.outline_border, "Outline Border").changed();
        c |= ui.checkbox(&mut self.fill_object_background, "Fill Object Background").changed();
        c
    }
    fn prepare(&mut self, src: &Surface, ctx: &RenderCtx) {
        let area = ctx.sel_bounds.intersect(&src.bounds());
        // Radius 1 also counts diagonal neighbours as touching the object.
        self.dist = Some(EdgeDistance::compute(src, area, self.tolerance, self.radius == 1, self.outline_border));
    }
    fn render(&self, _src: &Src, dst: &mut [u8], r: IRect, ctx: &RenderCtx) {
        let Some(dist) = &self.dist else { return };
        let radius = self.effective_radius();
        let rf = radius as f32;
        let r2 = (radius * radius) as u32;
        let primary = to_premul(ctx.primary);
        let secondary = to_premul(ctx.secondary);
        for y in 0..r.h {
            for x in 0..r.w {
                let o = ((y * r.w + x) * 4) as usize;
                let d = [dst[o], dst[o + 1], dst[o + 2], dst[o + 3]];
                if d[3] == 255 {
                    continue;
                }
                let mut highest = 0u8;
                if self.fill_object_background && d[3] as i32 >= self.tolerance {
                    highest = 255;
                }
                if highest != 255 {
                    let d2 = dist.get(r.x + x, r.y + y) as u32;
                    if d2 < r2 {
                        let mult = 1.0 - (d2 as f32).sqrt() / rf;
                        if mult > 0.0 {
                            highest = highest.max((255.0 * mult) as u8);
                        }
                    }
                }
                let mut color = primary;
                if self.color_gradient {
                    for c in 0..4 {
                        color[c] = lerp_byte(secondary[c], primary[c], highest);
                    }
                }
                if !self.alpha_gradient && highest != 0 {
                    highest = 255;
                }
                let out = normal_blend(new_alpha(color, highest), d);
                dst[o..o + 4].copy_from_slice(&out);
            }
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
        RenderCtx { width: w, height: h, sel_bounds: sel, primary: [200, 30, 10, 255], secondary: [20, 90, 250, 255] }
    }

    /// Opaque blob + soft-edged ring + a separate small object on a
    /// transparent background.
    fn test_image(w: u32, h: u32) -> Surface {
        let mut d = vec![0u8; (w * h * 4) as usize];
        let (cx, cy) = (w as f32 * 0.45, h as f32 * 0.5);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                let a = if r < h as f32 * 0.25 {
                    255
                } else if r < h as f32 * 0.3 {
                    (255.0 * (h as f32 * 0.3 - r) / (h as f32 * 0.05)) as u8
                } else if (x as f32) > w as f32 * 0.8 && (x as f32) < w as f32 * 0.9 && (y as f32) < h as f32 * 0.2 {
                    180
                } else {
                    0
                };
                let c = [(x * 7 % 256) as u8, (y * 3 % 256) as u8, 120, a];
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

    fn check(e: &dyn Effect, src: &Surface, sel: Option<IRect>) -> Vec<u8> {
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
        tiled
    }

    fn variants() -> Vec<Box<dyn Effect>> {
        let mut v = all();
        for p in [AlignPosition::TopLeft, AlignPosition::BottomRight, AlignPosition::CenterRight, AlignPosition::BottomCenter] {
            v.push(Box::new(AlignObject { position: p, plan: None }));
        }
        v.push(Box::new(FeatherObject { radius: 40, tolerance: 100, feather_canvas_edge: true, dist: None }));
        v.push(Box::new(OutlineObject { radius: 0, ..Default::default() }));
        v.push(Box::new(OutlineObject { radius: 1, color_gradient: false, alpha_gradient: false, ..Default::default() }));
        v.push(Box::new(OutlineObject { radius: 37, tolerance: 130, outline_border: true, fill_object_background: false, ..Default::default() }));
        v
    }

    #[test]
    fn menu_order_and_names() {
        let names: Vec<_> = all().iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Align Object", "Feather Object", "Outline Object"]);
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
            check(e.as_ref(), &src, Some(IRect::new(150, 60, 420, 390)));
        }
    }

    #[test]
    fn tiny_and_empty_images() {
        for (w, h) in [(1, 1), (1, 9), (6, 1), (3, 3)] {
            let src = test_image(w, h);
            for e in variants() {
                check(e.as_ref(), &src, None);
            }
        }
        let empty = Surface::new(300, 280);
        for e in variants() {
            let out = check(e.as_ref(), &empty, None);
            if e.name() != "Outline Object" {
                assert!(out.iter().all(|&v| v == 0), "{} changed an empty image", e.name());
            }
        }
    }

    #[test]
    fn align_moves_object_flush() {
        let mut d = vec![0u8; 100 * 80 * 4];
        for y in 30..40 {
            for x in 40..60 {
                let i = ((y * 100 + x) * 4) as usize;
                d[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        let src = Surface::from_rgba(100, 80, &d);
        let mut e = AlignObject { position: AlignPosition::BottomRight, plan: None };
        let out = render_whole(&mut e, &src, &ctx(100, 80, src.bounds()));
        let at = |x: i32, y: i32| out[((y * 100 + x) * 4 + 3) as usize];
        assert_eq!(at(99, 79), 255);
        assert_eq!(at(80, 70), 255);
        assert_eq!(at(79, 79), 0);
        assert_eq!(at(99, 69), 0);
        assert_eq!(at(45, 35), 0);
    }

    #[test]
    fn edge_distance_matches_brute_force() {
        let src = test_image(120, 90);
        let area = IRect::new(10, 5, 100, 80);
        for (tol, eight, canvas) in [(20, false, false), (100, true, true)] {
            let ed = EdgeDistance::compute(&src, area, tol, eight, canvas);
            let a = |x: i32, y: i32| src.get_pixel(x, y)[3] as i32;
            let mut edges = vec![];
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let on_canvas = x == 0 || y == 0 || x == 119 || y == 89;
                    let mut n = vec![(-1, 0), (1, 0), (0, -1), (0, 1)];
                    if eight {
                        n.extend([(-1, -1), (-1, 1), (1, -1), (1, 1)]);
                    }
                    let inimg = |x: i32, y: i32| x >= 0 && y >= 0 && x < 120 && y < 90;
                    if (canvas && on_canvas) || (a(x, y) <= tol && n.iter().any(|&(dx, dy)| inimg(x + dx, y + dy) && a(x + dx, y + dy) > tol)) {
                        edges.push((x, y));
                    }
                }
            }
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let best = edges.iter().map(|&(ex, ey)| ((ex - x).pow(2) + (ey - y).pow(2)) as u32).min().unwrap_or(u32::MAX);
                    assert_eq!(ed.get(x, y) as u32, best.min(u16::MAX as u32), "at {x},{y}");
                }
            }
        }
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
        let oo = |radius, tolerance, alpha_gradient, color_gradient, outline_border, fill_object_background| -> Box<dyn Effect> {
            Box::new(OutlineObject { radius, tolerance, alpha_gradient, color_gradient, outline_border, fill_object_background, dist: None })
        };
        // Pinta bugs we deliberately fix, so these only report differences:
        // GetObjectBounds/AlignObject treat the inclusive Right/Bottom as
        // exclusive (object box one pixel too big, right/bottom alignment
        // short of the edge) and Feather never processes the last row.
        let known_deviation = ["alignobject2.png", "alignobject3.png", "feather1.png", "feather2.png"];
        let cases: Vec<(&str, &str, Box<dyn Effect>)> = vec![
            ("alignobjectinput.png", "alignobject1.png", Box::new(AlignObject { position: AlignPosition::TopLeft, plan: None })),
            ("alignobjectinput.png", "alignobject2.png", Box::new(AlignObject { position: AlignPosition::Center, plan: None })),
            ("alignobjectinput.png", "alignobject3.png", Box::new(AlignObject { position: AlignPosition::BottomRight, plan: None })),
            ("input.png", "feather1.png", Box::new(FeatherObject { radius: 5, tolerance: 20, feather_canvas_edge: true, dist: None })),
            ("input.png", "feather2.png", Box::new(FeatherObject { radius: 10, tolerance: 20, feather_canvas_edge: true, dist: None })),
            ("outlineobjectinput.png", "outlineobject1.png", oo(10, 130, true, false, false, false)),
            ("outlineobjectinput.png", "outlineobject2.png", oo(10, 20, false, true, true, false)),
            ("outlineobjectinput.png", "outlineobject3.png", oo(10, 20, true, true, false, true)),
            ("outlineobjectinput.png", "outlineobject4.png", oo(1, 20, false, false, false, false)),
            ("outlineobjectinput.png", "outlineobject5.png", oo(0, 20, false, false, false, false)),
        ];
        let mut bad = vec![];
        for (input, file, mut e) in cases {
            let input = load_png(&dir.join(input));
            let expected = load_png(&dir.join(file)).to_rgba();
            let mut c = ctx(input.width, input.height, input.bounds());
            c.primary = [0, 0, 0, 255];
            c.secondary = [255, 255, 255, 255];
            let out = apply(e.as_mut(), &input, &Selection::default(), &c, &Progress::new()).unwrap().to_rgba();
            let w = input.width as usize;
            let diffs: Vec<usize> = out
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .enumerate()
                .filter(|(_, (p, q))| p.iter().zip(q.iter()).map(|(x, y)| (*x as i32 - *y as i32).pow(2)).sum::<i32>() > 4)
                .map(|(i, _)| i)
                .collect();
            let rows: std::collections::BTreeSet<usize> = diffs.iter().map(|i| i / w).collect();
            println!("{file}: {} pixels differ, rows {:?}", diffs.len(), rows.iter().take(12).collect::<Vec<_>>());
            if !diffs.is_empty() && !known_deviation.contains(&file) {
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
        for mut e in all() {
            let t = std::time::Instant::now();
            apply(e.as_mut(), &src, &Selection::default(), &c, &Progress::new()).unwrap();
            println!("{:<16} {:>8.3} s", e.name(), t.elapsed().as_secs_f64());
        }
    }
}
