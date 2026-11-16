//! Pinta's tool set. Tools edit `doc.state` live and commit a history step
//! when the gesture ends.

use crate::document::Document;
use crate::flood::fill_mask;
use crate::raster::{PasteMode, lerp_px, map_pixels, over, paste_pixmap, paste_rgba, sel_tile, sk_color};
use crate::selection::{CombineMode, MTile, Mask, Selection};
use crate::surface::{IRect, Surface, TILE, TILE_PX, premul, unpremul};
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke as SkStroke, Transform};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ToolKind {
    MoveSelected,
    MoveSelection,
    Zoom,
    Pan,
    RectangleSelect,
    EllipseSelect,
    LassoSelect,
    MagicWand,
    Paintbrush,
    Pencil,
    Eraser,
    PaintBucket,
    Gradient,
    ColorPicker,
    Text,
    LineCurve,
    Rectangle,
    RoundedRectangle,
    Ellipse,
    FreeformShape,
    CloneStamp,
    Recolor,
}

impl ToolKind {
    /// Order matches Pinta's tool box.
    pub const ALL: [ToolKind; 22] = [
        ToolKind::MoveSelected,
        ToolKind::MoveSelection,
        ToolKind::Zoom,
        ToolKind::Pan,
        ToolKind::RectangleSelect,
        ToolKind::EllipseSelect,
        ToolKind::LassoSelect,
        ToolKind::MagicWand,
        ToolKind::Paintbrush,
        ToolKind::Pencil,
        ToolKind::Eraser,
        ToolKind::PaintBucket,
        ToolKind::Gradient,
        ToolKind::ColorPicker,
        ToolKind::Text,
        ToolKind::LineCurve,
        ToolKind::Rectangle,
        ToolKind::RoundedRectangle,
        ToolKind::Ellipse,
        ToolKind::FreeformShape,
        ToolKind::CloneStamp,
        ToolKind::Recolor,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ToolKind::MoveSelected => "Move Selected",
            ToolKind::MoveSelection => "Move Selection",
            ToolKind::Zoom => "Zoom",
            ToolKind::Pan => "Pan",
            ToolKind::RectangleSelect => "Rectangle Select",
            ToolKind::EllipseSelect => "Ellipse Select",
            ToolKind::LassoSelect => "Lasso Select",
            ToolKind::MagicWand => "Magic Wand Select",
            ToolKind::Paintbrush => "Paintbrush",
            ToolKind::Pencil => "Pencil",
            ToolKind::Eraser => "Eraser",
            ToolKind::PaintBucket => "Paint Bucket",
            ToolKind::Gradient => "Gradient",
            ToolKind::ColorPicker => "Color Picker",
            ToolKind::Text => "Text",
            ToolKind::LineCurve => "Line/Curve",
            ToolKind::Rectangle => "Rectangle",
            ToolKind::RoundedRectangle => "Rounded Rectangle",
            ToolKind::Ellipse => "Ellipse",
            ToolKind::FreeformShape => "Freeform Shape",
            ToolKind::CloneStamp => "Clone Stamp",
            ToolKind::Recolor => "Recolor",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ToolKind::MoveSelected => "tool-move",
            ToolKind::MoveSelection => "tool-move-selection",
            ToolKind::Zoom => "tool-zoom",
            ToolKind::Pan => "tool-pan",
            ToolKind::RectangleSelect => "tool-select-rectangle",
            ToolKind::EllipseSelect => "tool-select-ellipse",
            ToolKind::LassoSelect => "tool-select-lasso",
            ToolKind::MagicWand => "tool-select-magicwand",
            ToolKind::Paintbrush => "tool-paintbrush",
            ToolKind::Pencil => "tool-pencil",
            ToolKind::Eraser => "tool-eraser",
            ToolKind::PaintBucket => "tool-paintbucket",
            ToolKind::Gradient => "tool-gradient",
            ToolKind::ColorPicker => "tool-colorpicker",
            ToolKind::Text => "tool-text",
            ToolKind::LineCurve => "tool-line",
            ToolKind::Rectangle => "tool-rectangle",
            ToolKind::RoundedRectangle => "tool-rectangle-rounded",
            ToolKind::Ellipse => "tool-ellipse",
            ToolKind::FreeformShape => "tool-freeformshape",
            ToolKind::CloneStamp => "tool-clonestamp",
            ToolKind::Recolor => "tool-recolor",
        }
    }

    pub fn shortcut(self) -> Option<char> {
        Some(match self {
            ToolKind::MoveSelected | ToolKind::MoveSelection => 'M',
            ToolKind::Zoom => 'Z',
            ToolKind::Pan => 'H',
            ToolKind::RectangleSelect | ToolKind::EllipseSelect | ToolKind::LassoSelect | ToolKind::MagicWand => 'S',
            ToolKind::Paintbrush => 'B',
            ToolKind::Pencil => 'P',
            ToolKind::Eraser => 'E',
            ToolKind::PaintBucket => 'F',
            ToolKind::Gradient => 'G',
            ToolKind::ColorPicker => 'K',
            ToolKind::Text => 'T',
            ToolKind::LineCurve | ToolKind::Rectangle | ToolKind::RoundedRectangle | ToolKind::Ellipse | ToolKind::FreeformShape => 'O',
            ToolKind::CloneStamp => 'L',
            ToolKind::Recolor => 'R',
        })
    }

    pub fn uses_brush_width(self) -> bool {
        matches!(
            self,
            ToolKind::Paintbrush
                | ToolKind::Eraser
                | ToolKind::LineCurve
                | ToolKind::Rectangle
                | ToolKind::RoundedRectangle
                | ToolKind::Ellipse
                | ToolKind::FreeformShape
                | ToolKind::CloneStamp
                | ToolKind::Recolor
        )
    }
    pub fn is_shape(self) -> bool {
        matches!(self, ToolKind::Rectangle | ToolKind::RoundedRectangle | ToolKind::Ellipse | ToolKind::FreeformShape)
    }
    pub fn is_select(self) -> bool {
        matches!(self, ToolKind::RectangleSelect | ToolKind::EllipseSelect | ToolKind::LassoSelect | ToolKind::MagicWand)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillStyle {
    Outline,
    Fill,
    OutlineFill,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GradientType {
    Linear,
    LinearReflected,
    Diamond,
    Radial,
    Conical,
}

impl GradientType {
    pub const ALL: [GradientType; 5] = [GradientType::Linear, GradientType::LinearReflected, GradientType::Diamond, GradientType::Radial, GradientType::Conical];
    pub fn name(self) -> &'static str {
        match self {
            GradientType::Linear => "Linear Gradient",
            GradientType::LinearReflected => "Linear Reflected Gradient",
            GradientType::Diamond => "Linear Diamond Gradient",
            GradientType::Radial => "Radial Gradient",
            GradientType::Conical => "Conical Gradient",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            GradientType::Linear => "tool-gradient-linear",
            GradientType::LinearReflected => "tool-gradient-linear-reflected",
            GradientType::Diamond => "tool-gradient-diamond",
            GradientType::Radial => "tool-gradient-radial",
            GradientType::Conical => "tool-gradient-conical",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SampleFrom {
    Layer,
    Image,
}

pub struct ToolSettings {
    pub brush_width: f32,
    /// Brush edge hardness in percent (paintbrush, eraser, clone stamp).
    pub hardness: f32,
    pub antialias: bool,
    pub alpha_blending: bool,
    pub fill_style: FillStyle,
    pub radius: f32,
    pub tolerance: f32,
    pub flood_global: bool,
    pub sel_mode: CombineMode,
    pub gradient: GradientType,
    pub gradient_transparency: bool,
    pub picker_size: u32,
    pub picker_from: SampleFrom,
    pub picker_switch_back: bool,
    pub wand_from: SampleFrom,
    pub arrow: bool,
}

impl Default for ToolSettings {
    fn default() -> Self {
        ToolSettings {
            brush_width: 2.0,
            hardness: 100.0,
            antialias: true,
            alpha_blending: true,
            fill_style: FillStyle::Outline,
            radius: 20.0,
            tolerance: 50.0,
            flood_global: false,
            sel_mode: CombineMode::Replace,
            gradient: GradientType::Linear,
            gradient_transparency: false,
            picker_size: 1,
            picker_from: SampleFrom::Layer,
            picker_switch_back: false,
            wand_from: SampleFrom::Layer,
            arrow: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Pointer {
    /// Image coordinates.
    pub pos: [f32; 2],
    /// Screen coordinates (points).
    pub screen: [f32; 2],
    pub secondary: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

pub struct Palette {
    /// Straight (non-premultiplied) RGBA.
    pub primary: [u8; 4],
    pub secondary: [u8; 4],
    pub recent: Vec<[u8; 4]>,
}

impl Default for Palette {
    fn default() -> Self {
        Palette { primary: [0, 0, 0, 255], secondary: [255, 255, 255, 255], recent: Vec::new() }
    }
}

impl Palette {
    pub fn touch(&mut self, c: [u8; 4]) {
        self.recent.retain(|x| *x != c);
        self.recent.insert(0, c);
        self.recent.truncate(10);
    }
}

// ---- stroke engine -------------------------------------------------------

enum StrokeKind {
    Paint([u8; 4]),
    Erase,
    Clone { src: Surface, dx: i32, dy: i32 },
    Recolor { from: [u8; 4], to: [u8; 4], tol: i64 },
}

struct StrokeState {
    base: Surface,
    cov: HashMap<usize, Box<[u8; TILE_PX]>>,
    kind: StrokeKind,
    last: [f32; 2],
    width: f32,
    aa: bool,
    pixel: bool,
    /// 0..1, 1 = hard edge
    hardness: f32,
}

impl StrokeState {
    fn segment(&mut self, doc: &mut Document, a: [f32; 2], b: [f32; 2]) {
        let st = &mut doc.state;
        let (w, h) = (st.width, st.height);
        let half = if self.pixel { 1.0 } else { self.width / 2.0 + 2.0 };
        let bbox = IRect::from_f(a[0].min(b[0]) - half, a[1].min(b[1]) - half, a[0].max(b[0]) + half, a[1].max(b[1]) + half).intersect(&IRect::new(0, 0, w as i32, h as i32));
        if bbox.is_empty() {
            return;
        }
        let mut m = tiny_skia::Mask::new(bbox.w as u32, bbox.h as u32).unwrap();
        if self.pixel {
            // Bresenham, 1px, aliased (Pinta pencil)
            let (mut x0, mut y0) = (a[0].floor() as i32, a[1].floor() as i32);
            let (x1, y1) = (b[0].floor() as i32, b[1].floor() as i32);
            let dx = (x1 - x0).abs();
            let dy = -(y1 - y0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = dx + dy;
            let data = m.data_mut();
            loop {
                if bbox.contains(x0, y0) {
                    data[((y0 - bbox.y) * bbox.w + (x0 - bbox.x)) as usize] = 255;
                }
                if x0 == x1 && y0 == y1 {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x0 += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y0 += sy;
                }
            }
        } else if self.hardness < 0.999 {
            // Soft brush: coverage from distance to the segment, full inside
            // radius*hardness, smoothly fading to zero at the radius. Max-merged
            // per stroke, so overlapping segments don't build up dark dots.
            let r = (self.width / 2.0).max(0.5);
            let inner = r * self.hardness;
            let span = (r - inner).max(1.0);
            let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = vx * vx + vy * vy;
            let data = m.data_mut();
            for y in 0..bbox.h {
                let py = (bbox.y + y) as f32 + 0.5;
                for x in 0..bbox.w {
                    let px = (bbox.x + x) as f32 + 0.5;
                    let t = if l2 > 1e-6 { (((px - a[0]) * vx + (py - a[1]) * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    let dx = px - (a[0] + vx * t);
                    let dy = py - (a[1] + vy * t);
                    let d = (dx * dx + dy * dy).sqrt();
                    if d >= r {
                        continue;
                    }
                    let k = ((r - d) / span).clamp(0.0, 1.0);
                    let k = k * k * (3.0 - 2.0 * k);
                    data[(y * bbox.w + x) as usize] = (k * 255.0).round() as u8;
                }
            }
        } else {
            let mut pb = PathBuilder::new();
            if (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01 {
                pb.push_circle(a[0], a[1], (self.width / 2.0).max(0.5));
                if let Some(p) = pb.finish() {
                    m.fill_path(&p, FillRule::Winding, self.aa, Transform::from_translate(-bbox.x as f32, -bbox.y as f32));
                }
            } else {
                pb.move_to(a[0], a[1]);
                pb.line_to(b[0], b[1]);
                if let Some(p) = pb.finish() {
                    let s = SkStroke { width: self.width.max(1.0), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                    if let Some(sp) = p.stroke(&s, 1.0) {
                        m.fill_path(&sp, FillRule::Winding, self.aa, Transform::from_translate(-bbox.x as f32, -bbox.y as f32));
                    }
                }
            }
        }
        // Merge coverage (max) into per-tile accumulation.
        let layer = &mut st.layers[st.current];
        let tiles_x = layer.surface.tiles_x;
        let mdata = m.data();
        for i in layer.surface.tiles_in(bbox) {
            let tr = layer.surface.tile_rect(i);
            let c = tr.intersect(&bbox);
            let acc = self.cov.entry(i).or_insert_with(|| vec![0u8; TILE_PX].into_boxed_slice().try_into().unwrap());
            for y in c.y..c.bottom() {
                for x in c.x..c.right() {
                    let v = mdata[((y - bbox.y) * bbox.w + (x - bbox.x)) as usize];
                    let d = &mut acc[(y - tr.y) as usize * TILE + (x - tr.x) as usize];
                    if v > *d {
                        *d = v;
                    }
                }
            }
        }
        // Recompute affected pixels from base + coverage.
        let base = &self.base;
        let cov = &self.cov;
        let sel = &st.selection;
        let kind = &self.kind;
        let alloc = !matches!(kind, StrokeKind::Erase);
        layer.surface.par_tiles_mut(bbox, alloc, |t, tr| {
            let i = ((tr.y as usize / TILE) as u32 * tiles_x + (tr.x as usize / TILE) as u32) as usize;
            let Some(acc) = cov.get(&i) else { return };
            let stile = sel_tile(sel, i);
            if matches!(stile, Some(MTile::Empty)) {
                return;
            }
            let btile = base.tiles[i].as_deref();
            let c = tr.intersect(&bbox);
            for y in c.y..c.bottom() {
                let ly = (y - tr.y) as usize;
                for x in c.x..c.right() {
                    let lx = (x - tr.x) as usize;
                    let mut cv = acc[ly * TILE + lx] as u32;
                    if cv == 0 {
                        continue;
                    }
                    if let Some(s) = stile {
                        cv = cv * s.get(lx, ly) as u32 / 255;
                    }
                    let di = (ly * TILE + lx) * 4;
                    let bpx = btile.map_or([0; 4], |b| [b.px[di], b.px[di + 1], b.px[di + 2], b.px[di + 3]]);
                    let mut out = bpx;
                    match kind {
                        StrokeKind::Paint(c) => over(&mut out, *c, cv as u8),
                        StrokeKind::Erase => {
                            let k = 255 - cv;
                            for ch in &mut out {
                                *ch = crate::surface::mul8(*ch as u32, k);
                            }
                        }
                        StrokeKind::Clone { src, dx, dy } => {
                            let s = src.get_pixel(x - dx, y - dy);
                            over(&mut out, s, cv as u8);
                        }
                        StrokeKind::Recolor { from, to, tol } => {
                            let u = unpremul(bpx);
                            let d = |k: usize| (u[k] as i64 - from[k] as i64).pow(2);
                            if u[3] > 0 && d(0) + d(1) + d(2) <= tol * tol * 3 {
                                let target = premul([to[0], to[1], to[2], u[3]]);
                                out = lerp_px(&bpx, target, cv as u8);
                            }
                        }
                    }
                    t.px[di..di + 4].copy_from_slice(&out);
                }
            }
        });
        self.last = b;
    }
}

// ---- shapes -----------------------------------------------------------

struct ShapeState {
    base: Surface,
    start: [f32; 2],
    end: [f32; 2],
    points: Vec<[f32; 2]>,
    outline: [u8; 4],
    fill: [u8; 4],
}

// ---- active gesture ------------------------------------------------------

enum Active {
    None,
    Stroke(StrokeState),
    Shape(ShapeState),
    SelectShape { start: [f32; 2], end: [f32; 2], base: Selection, mode: CombineMode },
    Lasso { points: Vec<[f32; 2]>, base: Selection, mode: CombineMode },
    MoveSelection { start: [f32; 2], base: Mask },
    MoveSelected(Box<MoveSelectedState>),
    Gradient { base: Surface, start: [f32; 2], end: [f32; 2], swap: bool },
    Pan { last: [f32; 2] },
    ZoomRect { start: [f32; 2], end: [f32; 2], out: bool },
    Picker { secondary: bool },
    LineDrag { idx: usize },
}

struct MoveSelectedState {
    hole: Surface,
    lifted: Pixmap,
    lifted_rect: IRect,
    mask: Mask,
    start: [f32; 2],
    offset: [f32; 2],
    angle: f32,
    /// Signed scale factors (negative = flipped).
    scale: [f32; 2],
    mode: MsMode,
    rot_start: f32,
}

#[derive(Clone, Copy)]
enum MsMode {
    Move,
    Rotate,
    /// Dragging a grab handle; `anchor` (opposite handle, image coords) stays put.
    Handle { hx: i32, hy: i32, anchor: [f32; 2], s0: [f32; 2] },
    /// Shift-drag inside: scale around the centre.
    Center { s0: [f32; 2], d0: [f32; 2] },
}

/// Grab handles: corners and edge midpoints, as (-1|0|1, -1|0|1).
const MS_HANDLES: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)];

fn rot(v: [f32; 2], a: f32) -> [f32; 2] {
    let (s, c) = a.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

/// Transformed selection box: centre, signed half extents, angle.
#[derive(Clone, Copy)]
pub struct MsBox {
    c: [f32; 2],
    half: [f32; 2],
    angle: f32,
}

impl MsBox {
    fn point(&self, hx: f32, hy: f32) -> [f32; 2] {
        let v = rot([hx * self.half[0], hy * self.half[1]], self.angle);
        [self.c[0] + v[0], self.c[1] + v[1]]
    }
    fn of_rect(r: IRect) -> MsBox {
        MsBox { c: [r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0], half: [r.w as f32 / 2.0, r.h as f32 / 2.0], angle: 0.0 }
    }
}

impl MoveSelectedState {
    fn bx(&self) -> MsBox {
        let r = self.lifted_rect;
        MsBox {
            c: [r.x as f32 + r.w as f32 / 2.0 + self.offset[0], r.y as f32 + r.h as f32 / 2.0 + self.offset[1]],
            half: [self.scale[0] * r.w as f32 / 2.0, self.scale[1] * r.h as f32 / 2.0],
            angle: self.angle,
        }
    }
    /// lifted pixmap local coords -> image coords
    fn transform(&self) -> Transform {
        let r = self.lifted_rect;
        let (hw, hh) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
        Transform::from_translate(r.x as f32 + self.offset[0] + hw, r.y as f32 + self.offset[1] + hh)
            .pre_concat(Transform::from_rotate(self.angle.to_degrees()))
            .pre_scale(self.scale[0], self.scale[1])
            .pre_translate(-hw, -hh)
    }
    fn identity_scale(&self) -> bool {
        self.scale == [1.0, 1.0]
    }
    /// Pick the drag mode for a new press.
    fn begin(&mut self, p: &Pointer, pick: f32) {
        let b = self.bx();
        self.start = [p.pos[0] - self.offset[0], p.pos[1] - self.offset[1]];
        self.rot_start = (p.pos[1] - b.c[1]).atan2(p.pos[0] - b.c[0]) - self.angle;
        let hit = MS_HANDLES.iter().copied().filter(|&(hx, hy)| dist(b.point(hx as f32, hy as f32), p.pos) <= pick).min_by(|a, c| {
            dist(b.point(a.0 as f32, a.1 as f32), p.pos).total_cmp(&dist(b.point(c.0 as f32, c.1 as f32), p.pos))
        });
        self.mode = if p.secondary {
            MsMode::Rotate
        } else if let Some((hx, hy)) = hit {
            MsMode::Handle { hx, hy, anchor: b.point(-hx as f32, -hy as f32), s0: self.scale }
        } else if p.shift {
            MsMode::Center { s0: self.scale, d0: rot([p.pos[0] - b.c[0], p.pos[1] - b.c[1]], -self.angle) }
        } else {
            MsMode::Move
        };
    }
}

fn clamp_scale(s: f32, size: f32) -> f32 {
    if s.abs() * size < 1.0 {
        s.signum() / size
    } else {
        s
    }
}

pub struct TextEdit {
    pub origin: [f32; 2],
    pub text: String,
    /// Caret as a char index into `text`.
    pub caret: usize,
    base: Surface,
    pub bounds: IRect,
    layout_lines: Vec<(f32, f32, f32)>,
    layout_carets: Vec<Vec<f32>>,
    line_height: f32,
}

pub struct LineEdit {
    sh: ShapeState,
    pub selected: Option<usize>,
    sig: (u32, bool, bool, bool),
}

struct WandLast {
    doc_id: u64,
    history_pos: usize,
    at: (i32, i32),
    base: Selection,
    mode: CombineMode,
    shift: bool,
    sig: (u32, bool, SampleFrom),
}

pub struct Tools {
    wand_last: Option<WandLast>,
    pub text: Option<TextEdit>,
    pub line: Option<LineEdit>,
    /// Hit radius for handles, in image pixels (set by the canvas each frame).
    pub pick_radius: f32,
    pub text_params: crate::text::TextParams,
    pub current: ToolKind,
    pub previous: ToolKind,
    pub settings: ToolSettings,
    active: Active,
    pub hover: Option<[f32; 2]>,
    clone_source: Option<(u64, [f32; 2])>,
    clone_offset: Option<[f32; 2]>,
    /// Pixels lifted by Move Selected stay floating between drags (like
    /// Pinta's selection layer) so moving them again never cuts holes in
    /// the content they passed over. Keyed by (doc id, layer id, layer
    /// tile table ptr, selection tile table ptr) to detect outside changes.
    floating: Option<((u64, u64, usize, usize), Box<MoveSelectedState>)>,
    /// Box + grab handles shown by Move Selected (refreshed in `tick`).
    ms_box: Option<MsBox>,
    ms_bounds_cache: Option<((u64, usize), IRect)>,
    pub status: String,
}

pub enum ViewRequest {
    None,
    ZoomAt { factor: f32, at: [f32; 2] },
    ZoomToRect(IRect),
    PanBy([f32; 2]),
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            wand_last: None,
            text: None,
            line: None,
            pick_radius: 6.0,
            text_params: crate::text::TextParams::default(),
            current: ToolKind::Paintbrush,
            previous: ToolKind::Paintbrush,
            settings: ToolSettings::default(),
            active: Active::None,
            hover: None,
            clone_source: None,
            clone_offset: None,
            floating: None,
            ms_box: None,
            ms_bounds_cache: None,
            status: String::new(),
        }
    }
}

fn combine_mode_for(p: &Pointer, default: CombineMode) -> CombineMode {
    match (p.ctrl, p.secondary, p.alt) {
        (true, true, _) => CombineMode::Xor,
        (true, false, _) => CombineMode::Union,
        (false, true, _) => CombineMode::Exclude,
        (false, false, true) => CombineMode::Intersect,
        _ => default,
    }
}

fn base_mask(sel: &Selection, w: u32, h: u32) -> Mask {
    sel.mask.clone().unwrap_or_else(|| Mask::empty(w, h))
}

fn apply_selection(doc: &mut Document, base: &Selection, new: Mask, mode: CombineMode) {
    let (w, h) = (doc.state.width, doc.state.height);
    let combined = if mode == CombineMode::Replace { new } else { base_mask(base, w, h).combine(&new, mode) };
    doc.state.selection = Selection { mask: Some(combined) };
}

fn constrain_square(start: [f32; 2], end: [f32; 2]) -> [f32; 2] {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let m = dx.abs().max(dy.abs());
    [start[0] + m * dx.signum(), start[1] + m * dy.signum()]
}

fn snap_angle(start: [f32; 2], end: [f32; 2]) -> [f32; 2] {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let len = (dx * dx + dy * dy).sqrt();
    let step = std::f32::consts::PI / 12.0;
    let a = (dy.atan2(dx) / step).round() * step;
    [start[0] + len * a.cos(), start[1] + len * a.sin()]
}

/// Composite colour of all visible layers at a pixel (straight alpha).
pub fn image_pixel(doc: &Document, x: i32, y: i32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for l in doc.state.layers.iter().filter(|l| !l.hidden) {
        let p = l.surface.get_pixel(x, y);
        let op = (l.opacity * 255.0).round() as u8;
        crate::blend::blend_row(&mut out, &p, op, l.blend);
    }
    out
}

impl Tools {
    pub fn is_busy(&self) -> bool {
        !matches!(self.active, Active::None) || self.line.is_some()
    }

    pub fn set_tool(&mut self, doc: Option<&mut Document>, t: ToolKind) {
        if let Some(doc) = doc {
            self.finish(doc);
        }
        if t != self.current {
            self.previous = self.current;
            self.current = t;
        }
    }

    /// Commit or cancel any in-progress gesture (e.g. when switching tools).
    pub fn finish(&mut self, doc: &mut Document) {
        self.commit_text(doc);
        self.commit_line(doc);
        if !matches!(self.active, Active::None) {
            self.active = Active::None;
            doc.revert_uncommitted();
        }
    }

    pub fn cancel(&mut self, doc: &mut Document) {
        self.active = Active::None;
        self.text = None;
        self.line = None;
        doc.revert_uncommitted();
    }

    pub fn press(&mut self, doc: &mut Document, pal: &mut Palette, p: Pointer) -> ViewRequest {
        let s = &self.settings;
        let (w, h) = (doc.state.width, doc.state.height);
        let primary = if p.secondary { pal.secondary } else { pal.primary };
        let secondary = if p.secondary { pal.primary } else { pal.secondary };
        match self.current {
            ToolKind::Paintbrush | ToolKind::Pencil | ToolKind::Eraser | ToolKind::CloneStamp | ToolKind::Recolor => {
                let kind = match self.current {
                    ToolKind::Eraser => StrokeKind::Erase,
                    ToolKind::CloneStamp => {
                        if p.ctrl {
                            self.clone_source = Some((doc.state.layer().id, p.pos));
                            self.clone_offset = None;
                            self.status = format!("Clone source set at {:.0}, {:.0}", p.pos[0], p.pos[1]);
                            return ViewRequest::None;
                        }
                        let Some((_, src)) = self.clone_source else {
                            self.status = "Ctrl-click to set the clone source first".into();
                            return ViewRequest::None;
                        };
                        let off = *self.clone_offset.get_or_insert([p.pos[0] - src[0], p.pos[1] - src[1]]);
                        StrokeKind::Clone { src: doc.state.layer().surface.clone(), dx: off[0].round() as i32, dy: off[1].round() as i32 }
                    }
                    ToolKind::Recolor => StrokeKind::Recolor { from: secondary, to: primary, tol: crate::flood::tolerance_value(s.tolerance) },
                    _ => StrokeKind::Paint(premul(primary)),
                };
                let pixel = self.current == ToolKind::Pencil;
                let mut st = StrokeState {
                    base: doc.state.layer().surface.clone(),
                    cov: HashMap::new(),
                    kind,
                    last: p.pos,
                    width: if pixel { 1.0 } else { s.brush_width },
                    aa: s.antialias && !pixel,
                    pixel,
                    hardness: if pixel { 1.0 } else { (s.hardness / 100.0).clamp(0.0, 1.0) },
                };
                st.segment(doc, p.pos, p.pos);
                self.active = Active::Stroke(st);
            }
            ToolKind::LineCurve => {
                let r = self.pick_radius;
                if let Some(le) = self.line.as_mut() {
                    let pts = &mut le.sh.points;
                    // grab an existing point?
                    if let Some(i) = (0..pts.len()).filter(|&i| dist(pts[i], p.pos) <= r).min_by(|&a, &b| dist(pts[a], p.pos).total_cmp(&dist(pts[b], p.pos))) {
                        if p.secondary {
                            if pts.len() > 2 {
                                pts.remove(i);
                                le.selected = None;
                                self.render_line(doc);
                            }
                            return ViewRequest::None;
                        }
                        le.selected = Some(i);
                        self.active = Active::LineDrag { idx: i };
                        return ViewRequest::None;
                    }
                    // click on the curve inserts a point there
                    if let Some(seg) = hit_curve(pts, p.pos, r.max(self.settings.brush_width / 2.0 + 1.0)) {
                        pts.insert(seg + 1, p.pos);
                        le.selected = Some(seg + 1);
                        self.active = Active::LineDrag { idx: seg + 1 };
                        self.render_line(doc);
                        return ViewRequest::None;
                    }
                    self.status = "Drag points to edit, click the line to add a point, Enter to finish".into();
                    return ViewRequest::None;
                }
                self.line = Some(LineEdit {
                    sh: ShapeState { base: doc.state.layer().surface.clone(), start: p.pos, end: p.pos, points: vec![p.pos, p.pos], outline: primary, fill: secondary },
                    selected: Some(1),
                    sig: self.line_sig(),
                });
                self.active = Active::LineDrag { idx: 1 };
            }
            ToolKind::Rectangle | ToolKind::RoundedRectangle | ToolKind::Ellipse | ToolKind::FreeformShape => {
                self.active = Active::Shape(ShapeState {
                    base: doc.state.layer().surface.clone(),
                    start: p.pos,
                    end: p.pos,
                    points: vec![p.pos],
                    outline: primary,
                    fill: if s.fill_style == FillStyle::Fill { primary } else { secondary },
                });
            }
            ToolKind::RectangleSelect | ToolKind::EllipseSelect => {
                let mode = combine_mode_for(&p, s.sel_mode);
                self.active = Active::SelectShape { start: p.pos, end: p.pos, base: doc.state.selection.clone(), mode };
            }
            ToolKind::LassoSelect => {
                let mode = combine_mode_for(&p, s.sel_mode);
                self.active = Active::Lasso { points: vec![p.pos], base: doc.state.selection.clone(), mode };
            }
            ToolKind::MagicWand => {
                let mode = combine_mode_for(&p, s.sel_mode);
                let (x, y) = (p.pos[0].floor() as i32, p.pos[1].floor() as i32);
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    return ViewRequest::None;
                }
                let src = match s.wand_from {
                    SampleFrom::Layer => doc.state.layer().surface.clone(),
                    SampleFrom::Image => flatten(doc),
                };
                let t0 = std::time::Instant::now();
                let m = fill_mask(&src, x, y, s.tolerance, s.flood_global || p.shift, &Selection::default());
                let base = doc.state.selection.clone();
                apply_selection(doc, &base, m, mode);
                doc.commit("Magic Wand Select");
                self.wand_last = Some(WandLast { doc_id: doc.id, history_pos: doc.history_pos, at: (x, y), base, mode, shift: p.shift, sig: (s.tolerance.to_bits(), s.flood_global, s.wand_from) });
                self.status = format!("Magic wand: {:.0} ms", t0.elapsed().as_secs_f32() * 1000.0);
            }
            ToolKind::PaintBucket => {
                let (x, y) = (p.pos[0].floor() as i32, p.pos[1].floor() as i32);
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    return ViewRequest::None;
                }
                if doc.state.selection.coverage(x, y) == 0 {
                    return ViewRequest::None;
                }
                let base = doc.state.layer().surface.clone();
                let m = fill_mask(&base, x, y, s.tolerance, s.flood_global || p.shift, &doc.state.selection);
                let col = premul(primary);
                let blend = s.alpha_blending;
                let r = m.bounds().unwrap_or_default();
                let sel = doc.state.selection.clone();
                let mut target = base.clone();
                map_pixels(&mut target, &base, r, &sel, |x, y, px| {
                    let c = m.get(x, y);
                    if c == 0 {
                        return px;
                    }
                    if blend {
                        let mut o = px;
                        over(&mut o, col, c);
                        o
                    } else {
                        lerp_px(&px, col, c)
                    }
                });
                doc.state.layer_mut().surface = target;
                doc.commit("Paint Bucket");
            }
            ToolKind::Gradient => {
                self.active = Active::Gradient { base: doc.state.layer().surface.clone(), start: p.pos, end: p.pos, swap: p.secondary };
            }
            ToolKind::ColorPicker => {
                self.active = Active::Picker { secondary: p.secondary };
                self.pick(doc, pal, p);
            }
            ToolKind::MoveSelection => {
                let base = doc.state.selection.mask.clone().unwrap_or_else(|| Mask::full(w, h));
                self.active = Active::MoveSelection { start: p.pos, base };
            }
            ToolKind::MoveSelected => {
                // continue moving the still-floating pixels if nothing else touched the layer
                if let Some((key, mut ms)) = self.floating.take() {
                    if key == floating_key(doc) {
                        ms.begin(&p, self.pick_radius * 1.3);
                        self.active = Active::MoveSelected(ms);
                        return ViewRequest::None;
                    }
                }
                let mask = doc.state.selection.mask.clone().unwrap_or_else(|| Mask::full(w, h));
                let Some(r) = mask.bounds() else { return ViewRequest::None };
                let base = doc.state.layer().surface.clone();
                let mut data = base.read_rect(r);
                // keep only selected pixels
                for y in 0..r.h {
                    for x in 0..r.w {
                        let c = mask.get(r.x + x, r.y + y) as u32;
                        if c < 255 {
                            let i = ((y * r.w + x) * 4) as usize;
                            for k in 0..4 {
                                data[i + k] = crate::surface::mul8(data[i + k] as u32, c);
                            }
                        }
                    }
                }
                let mut hole = base.clone();
                let msk = mask.clone();
                hole.par_tiles_mut(r, false, |t, tr| {
                    let c = tr.intersect(&r);
                    for y in c.y..c.bottom() {
                        for x in c.x..c.right() {
                            let k = 255 - msk.get(x, y) as u32;
                            if k == 255 {
                                continue;
                            }
                            let i = ((y - tr.y) as usize * TILE + (x - tr.x) as usize) * 4;
                            for ch in 0..4 {
                                t.px[i + ch] = crate::surface::mul8(t.px[i + ch] as u32, k);
                            }
                        }
                    }
                });
                let lifted = Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(r.w as u32, r.h as u32).unwrap()).unwrap();
                let mut ms = Box::new(MoveSelectedState {
                    hole,
                    lifted,
                    lifted_rect: r,
                    mask,
                    start: p.pos,
                    offset: [0.0, 0.0],
                    angle: 0.0,
                    scale: [1.0, 1.0],
                    mode: MsMode::Move,
                    rot_start: 0.0,
                });
                ms.begin(&p, self.pick_radius * 1.3);
                self.active = Active::MoveSelected(ms);
            }
            ToolKind::Pan => self.active = Active::Pan { last: p.screen },
            ToolKind::Zoom => self.active = Active::ZoomRect { start: p.pos, end: p.pos, out: p.secondary },
            ToolKind::Text => {
                if let Some(te) = &self.text {
                    if te.bounds.inflate(4).contains(p.pos[0] as i32, p.pos[1] as i32) {
                        // click inside the text being edited: move the caret
                        let caret = te.caret_at(p.pos);
                        self.text.as_mut().unwrap().caret = caret;
                        return ViewRequest::None;
                    }
                    self.commit_text(doc);
                }
                let base = doc.state.layer().surface.clone();
                self.text = Some(TextEdit {
                    origin: [p.pos[0].round(), p.pos[1].round()],
                    text: String::new(),
                    caret: 0,
                    base,
                    bounds: IRect::new(p.pos[0] as i32, p.pos[1] as i32, 1, self.text_params.size as i32),
                    layout_lines: vec![],
                    layout_carets: vec![vec![0.0]],
                    line_height: self.text_params.size * 1.2,
                });
                self.render_text(doc, pal);
            }
        }
        ViewRequest::None
    }

    pub fn drag(&mut self, doc: &mut Document, pal: &mut Palette, p: Pointer) -> ViewRequest {
        self.hover = Some(p.pos);
        let s = &self.settings;
        let current = self.current;
        let mut req = ViewRequest::None;
        let mut pick = false;
        match &mut self.active {
            Active::None => {}
            Active::Stroke(st) => {
                let a = st.last;
                st.segment(doc, a, p.pos);
            }
            Active::Shape(sh) => {
                sh.end = if p.shift {
                    if current == ToolKind::LineCurve { snap_angle(sh.start, p.pos) } else { constrain_square(sh.start, p.pos) }
                } else {
                    p.pos
                };
                if current == ToolKind::FreeformShape {
                    sh.points.push(p.pos);
                }
                render_shape(doc, current, sh, s);
            }
            Active::SelectShape { start, end, base, mode } => {
                *end = if p.shift { constrain_square(*start, p.pos) } else { p.pos };
                let (w, h) = (doc.state.width, doc.state.height);
                let r = rect_of(*start, *end);
                let m = if current == ToolKind::EllipseSelect {
                    match Rect::from_xywh(r.x as f32, r.y as f32, r.w.max(1) as f32, r.h.max(1) as f32).and_then(PathBuilder::from_oval) {
                        Some(path) => Mask::from_path(w, h, &path, true),
                        None => Mask::empty(w, h),
                    }
                } else {
                    Mask::from_rect(w, h, r)
                };
                let base = base.clone();
                apply_selection(doc, &base, m, *mode);
            }
            Active::Lasso { points, base, mode } => {
                points.push(p.pos);
                let (w, h) = (doc.state.width, doc.state.height);
                let mut pb = PathBuilder::new();
                pb.move_to(points[0][0], points[0][1]);
                for q in &points[1..] {
                    pb.line_to(q[0], q[1]);
                }
                pb.close();
                if let Some(path) = pb.finish() {
                    let m = Mask::from_path(w, h, &path, true);
                    let base = base.clone();
                    apply_selection(doc, &base, m, *mode);
                }
            }
            Active::MoveSelection { start, base } => {
                let dx = (p.pos[0] - start[0]).round() as i32;
                let dy = (p.pos[1] - start[1]).round() as i32;
                doc.state.selection = Selection { mask: Some(base.translated(dx, dy)) };
                self.status = format!("Offset {dx}, {dy}");
            }
            Active::MoveSelected(ms) => {
                let r = ms.lifted_rect;
                let (w0, h0) = (r.w as f32, r.h as f32);
                let base_c = [r.x as f32 + w0 / 2.0, r.y as f32 + h0 / 2.0];
                match ms.mode {
                    MsMode::Rotate => {
                        let c = ms.bx().c;
                        let mut a = (p.pos[1] - c[1]).atan2(p.pos[0] - c[0]) - ms.rot_start;
                        if p.shift {
                            let step = std::f32::consts::PI / 12.0;
                            a = (a / step).round() * step;
                        }
                        ms.angle = a;
                    }
                    MsMode::Move => {
                        ms.offset = [p.pos[0] - ms.start[0], p.pos[1] - ms.start[1]];
                        if ms.angle == 0.0 && ms.identity_scale() {
                            ms.offset = [ms.offset[0].round(), ms.offset[1].round()];
                        }
                    }
                    MsMode::Handle { hx, hy, anchor, s0 } => {
                        let d = rot([p.pos[0] - anchor[0], p.pos[1] - anchor[1]], -ms.angle);
                        let (fx, fy) = (hx as f32, hy as f32);
                        let mut sc = s0;
                        if p.alt {
                            // keep proportions: project onto the handle's original direction
                            let h = [fx * s0[0] * w0, fy * s0[1] * h0];
                            let k = (d[0] * h[0] + d[1] * h[1]) / (h[0] * h[0] + h[1] * h[1]).max(1e-6);
                            sc = [s0[0] * k, s0[1] * k];
                        } else {
                            if hx != 0 {
                                sc[0] = d[0] / (fx * w0);
                            }
                            if hy != 0 {
                                sc[1] = d[1] / (fy * h0);
                            }
                        }
                        ms.scale = [clamp_scale(sc[0], w0), clamp_scale(sc[1], h0)];
                        // the anchor stays fixed; the centre follows
                        let v = rot([fx * ms.scale[0] * w0 / 2.0, fy * ms.scale[1] * h0 / 2.0], ms.angle);
                        let c = [anchor[0] + v[0], anchor[1] + v[1]];
                        ms.offset = [c[0] - base_c[0], c[1] - base_c[1]];
                    }
                    MsMode::Center { s0, d0 } => {
                        // grow/shrink by how far the pointer moved away from the
                        // centre (pixels), so starting near the centre stays stable
                        let c = ms.bx().c;
                        let d = rot([p.pos[0] - c[0], p.pos[1] - c[1]], -ms.angle);
                        let half0 = [s0[0].abs() * w0 / 2.0, s0[1].abs() * h0 / 2.0];
                        let k = if p.alt {
                            let l = (half0[0] * half0[0] + half0[1] * half0[1]).sqrt().max(1.0);
                            let g = (d[0] * d[0] + d[1] * d[1]).sqrt() - (d0[0] * d0[0] + d0[1] * d0[1]).sqrt();
                            let kk = ((l + g) / l).max(0.0);
                            [kk, kk]
                        } else {
                            [((half0[0] + d[0].abs() - d0[0].abs()) / half0[0].max(0.5)).max(0.0), ((half0[1] + d[1].abs() - d0[1].abs()) / half0[1].max(0.5)).max(0.0)]
                        };
                        ms.scale = [clamp_scale(s0[0] * k[0], w0), clamp_scale(s0[1] * k[1], h0)];
                    }
                }
                render_move_selected(doc, ms);
                let b = ms.bx();
                self.status = format!(
                    "Offset {:.0}, {:.0}  Size {:.0} x {:.0} ({:.0}% x {:.0}%)  Angle {:.1}°",
                    ms.offset[0],
                    ms.offset[1],
                    (b.half[0] * 2.0).abs(),
                    (b.half[1] * 2.0).abs(),
                    ms.scale[0] * 100.0,
                    ms.scale[1] * 100.0,
                    ms.angle.to_degrees()
                );
            }
            Active::Gradient { base, start, end, swap } => {
                *end = if p.shift { snap_angle(*start, p.pos) } else { p.pos };
                let (c0, c1) = if *swap { (pal.secondary, pal.primary) } else { (pal.primary, pal.secondary) };
                render_gradient(doc, base, *start, *end, c0, c1, s);
            }
            Active::Pan { last } => {
                req = ViewRequest::PanBy([p.screen[0] - last[0], p.screen[1] - last[1]]);
                *last = p.screen;
            }
            Active::ZoomRect { end, .. } => *end = p.pos,
            Active::Picker { .. } => pick = true,
            Active::LineDrag { idx } => {
                let idx = *idx;
                if let Some(le) = self.line.as_mut() {
                    let mut q = p.pos;
                    if p.shift && idx > 0 {
                        q = snap_angle(le.sh.points[idx - 1], q);
                    }
                    if let Some(pt) = le.sh.points.get_mut(idx) {
                        *pt = q;
                    }
                }
                self.render_line(doc);
            }
        }
        if pick {
            self.pick(doc, pal, p);
        }
        req
    }

    pub fn release(&mut self, doc: &mut Document, pal: &mut Palette, p: Pointer) -> ViewRequest {
        let active = std::mem::replace(&mut self.active, Active::None);
        let mut req = ViewRequest::None;
        match active {
            Active::None => {}
            Active::Stroke(_) => {
                doc.commit(self.current.name());
                if let Some(c) = [pal.primary].first() {
                    pal.touch(*c);
                }
            }
            Active::Shape(sh) => {
                let moved = (sh.end[0] - sh.start[0]).abs() + (sh.end[1] - sh.start[1]).abs() > 0.5 || sh.points.len() > 2;
                if moved {
                    doc.commit(self.current.name());
                    pal.touch(sh.outline);
                } else {
                    doc.revert_uncommitted();
                }
            }
            Active::SelectShape { start, end, base, .. } => {
                let r = rect_of(start, end);
                if r.w < 1 || r.h < 1 {
                    // click without drag: deselect (like Pinta)
                    doc.state.selection = if base.is_active() { Selection::default() } else { base };
                    if doc.history[doc.history_pos].state.selection.is_active() {
                        doc.commit("Deselect All");
                    } else {
                        doc.revert_uncommitted();
                    }
                } else {
                    doc.commit(self.current.name());
                }
            }
            Active::Lasso { points, .. } => {
                if points.len() > 2 {
                    doc.commit("Lasso Select");
                } else {
                    doc.revert_uncommitted();
                }
            }
            Active::MoveSelection { start, .. } => {
                if (p.pos[0] - start[0]).abs() >= 0.5 || (p.pos[1] - start[1]).abs() >= 0.5 {
                    doc.commit("Move Selection");
                } else {
                    doc.revert_uncommitted();
                }
            }
            Active::MoveSelected(ms) => {
                if ms.offset != [0.0, 0.0] || ms.angle != 0.0 || !ms.identity_scale() {
                    if doc.history[doc.history_pos].state.layers.get(doc.state.current).is_none_or(|l| !Arc::ptr_eq(&l.surface.tiles, &doc.state.layer().surface.tiles)) {
                        doc.commit("Move Selected");
                    }
                    self.floating = Some((floating_key(doc), ms));
                } else {
                    doc.revert_uncommitted();
                }
            }
            Active::Gradient { start, end, .. } => {
                if (end[0] - start[0]).abs() + (end[1] - start[1]).abs() > 0.5 {
                    doc.commit("Gradient");
                } else {
                    doc.revert_uncommitted();
                }
            }
            Active::Pan { .. } => {}
            Active::ZoomRect { start, end, out } => {
                let r = rect_of(start, end);
                req = if out {
                    ViewRequest::ZoomAt { factor: 0.5, at: p.pos }
                } else if r.w > 4 && r.h > 4 {
                    ViewRequest::ZoomToRect(r)
                } else {
                    ViewRequest::ZoomAt { factor: 2.0, at: p.pos }
                };
            }
            Active::LineDrag { .. } => {
                // a click without dragging on an empty canvas makes no line
                if let Some(le) = &self.line {
                    let pts = &le.sh.points;
                    if pts.len() == 2 && dist(pts[0], pts[1]) < 0.5 {
                        self.line = None;
                        doc.revert_uncommitted();
                    } else {
                        self.status = "Drag points to edit, click the line to add a point, Enter to finish".into();
                    }
                }
            }
            Active::Picker { .. } => {
                if self.settings.picker_switch_back {
                    self.current = self.previous;
                }
            }
        }
        req
    }

    fn pick(&mut self, doc: &Document, pal: &mut Palette, p: Pointer) {
        let (x, y) = (p.pos[0].floor() as i32, p.pos[1].floor() as i32);
        if x < 0 || y < 0 || x >= doc.state.width as i32 || y >= doc.state.height as i32 {
            return;
        }
        let r = (self.settings.picker_size / 2) as i32;
        let mut acc = [0u32; 4];
        let mut n = 0;
        for yy in y - r..=y + r {
            for xx in x - r..=x + r {
                if xx < 0 || yy < 0 || xx >= doc.state.width as i32 || yy >= doc.state.height as i32 {
                    continue;
                }
                let c = match self.settings.picker_from {
                    SampleFrom::Layer => doc.state.layer().surface.get_pixel(xx, yy),
                    SampleFrom::Image => image_pixel(doc, xx, yy),
                };
                for k in 0..4 {
                    acc[k] += c[k] as u32;
                }
                n += 1;
            }
        }
        let avg = [(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, (acc[3] / n) as u8];
        let c = unpremul(avg);
        let secondary = matches!(self.active, Active::Picker { secondary: true });
        if secondary { pal.secondary = c } else { pal.primary = c }
    }

    /// Overlay drawn on top of the canvas (in screen space).
    pub fn overlay(&self, painter: &egui::Painter, to_screen: &dyn Fn([f32; 2]) -> egui::Pos2, zoom_pts: f32) {
        use egui::{Color32, Stroke};
        let dark = Stroke::new(1.0, Color32::from_black_alpha(200));
        let light = Stroke::new(1.0, Color32::from_white_alpha(200));
        match &self.active {
            Active::ZoomRect { start, end, out: false } => {
                let r = egui::Rect::from_two_pos(to_screen(*start), to_screen(*end));
                painter.rect_stroke(r, 0.0, dark, egui::StrokeKind::Middle);
            }
            Active::Gradient { start, end, .. } => {
                let a = to_screen(*start);
                let b = to_screen(*end);
                painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(160)));
                painter.line_segment([a, b], light);
                for q in [a, b] {
                    painter.circle(q, 5.0, Color32::from_white_alpha(220), dark);
                }
            }
            Active::Lasso { points, .. } if points.len() > 1 => {
                let pts: Vec<egui::Pos2> = points.iter().map(|p| to_screen(*p)).collect();
                painter.add(egui::Shape::line(pts, dark));
            }
            _ => {}
        }
        if let Some(b) = &self.ms_box {
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y)| to_screen(b.point(x, y)));
            for i in 0..4 {
                let seg = [corners[i], corners[(i + 1) % 4]];
                painter.line_segment(seg, Stroke::new(3.0, Color32::from_black_alpha(90)));
                painter.line_segment(seg, Stroke::new(1.0, Color32::from_white_alpha(220)));
            }
            let rotating = matches!(&self.active, Active::MoveSelected(ms) if matches!(ms.mode, MsMode::Rotate));
            for (hx, hy) in MS_HANDLES {
                let c = to_screen(b.point(hx as f32, hy as f32));
                let r = egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0));
                if rotating {
                    painter.circle_filled(c, 4.5, Color32::WHITE);
                    painter.circle_stroke(c, 4.5, Stroke::new(1.0, Color32::BLACK));
                } else {
                    painter.rect_filled(r, 1.0, Color32::WHITE);
                    painter.rect_stroke(r, 1.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Inside);
                }
            }
            if rotating {
                let c = to_screen(b.c);
                painter.circle_stroke(c, 4.0, Stroke::new(2.0, Color32::from_black_alpha(160)));
                painter.circle_stroke(c, 4.0, light);
            }
        }
        if let Some(le) = &self.line {
            for (i, q) in le.sh.points.iter().enumerate() {
                let c = to_screen(*q);
                let r = egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0));
                let fill = if le.selected == Some(i) { Color32::from_rgb(30, 90, 220) } else { Color32::WHITE };
                painter.rect_filled(r, 1.0, fill);
                painter.rect_stroke(r, 1.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Inside);
            }
        }
        if let Some(te) = &self.text {
            let b = te.bounds;
            let r = egui::Rect::from_two_pos(to_screen([b.x as f32 - 2.0, b.y as f32 - 2.0]), to_screen([(b.right() + 2) as f32, (b.bottom() + 2) as f32]));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 80, 80, 160)), egui::StrokeKind::Middle);
            let (line, col) = te.caret_line_col();
            let (x0, base, _) = te.layout_lines.get(line).copied().unwrap_or((0.0, te.line_height * 0.8, 0.0));
            let cx = x0 + te.layout_carets.get(line).and_then(|c| c.get(col)).copied().unwrap_or(0.0);
            let top = base - te.line_height * 0.8;
            let a = to_screen([te.origin[0] + cx, te.origin[1] + top]);
            let bpos = to_screen([te.origin[0] + cx, te.origin[1] + top + te.line_height]);
            painter.line_segment([a, bpos], Stroke::new(2.0, Color32::from_rgb(30, 90, 220)));
        }
        if let Some(h) = self.hover {
            let c = to_screen(h);
            if self.current.uses_brush_width() && !self.current.is_shape() && self.current != ToolKind::LineCurve {
                let r = (self.settings.brush_width * zoom_pts / 2.0).max(2.0);
                painter.circle_stroke(c, r, Stroke::new(2.0, Color32::from_black_alpha(140)));
                painter.circle_stroke(c, r, light);
            }
            if self.current == ToolKind::CloneStamp {
                if let Some((_, src)) = self.clone_source {
                    let off = self.clone_offset.unwrap_or([0.0, 0.0]);
                    let sp = if matches!(self.active, Active::Stroke(_)) { [h[0] - off[0], h[1] - off[1]] } else { src };
                    let q = to_screen(sp);
                    painter.line_segment([q - egui::vec2(6.0, 0.0), q + egui::vec2(6.0, 0.0)], dark);
                    painter.line_segment([q - egui::vec2(0.0, 6.0), q + egui::vec2(0.0, 6.0)], dark);
                }
            }
        }
    }

    /// Called when the clone stamp gesture ends so the next stroke re-aligns.
    pub fn reset_clone_alignment(&mut self) {
        if self.current != ToolKind::CloneStamp {
            self.clone_offset = None;
        }
    }
}

fn rect_of(a: [f32; 2], b: [f32; 2]) -> IRect {
    let x0 = a[0].min(b[0]).round() as i32;
    let y0 = a[1].min(b[1]).round() as i32;
    let x1 = a[0].max(b[0]).round() as i32;
    let y1 = a[1].max(b[1]).round() as i32;
    IRect::new(x0, y0, x1 - x0, y1 - y0)
}

pub fn flatten(doc: &Document) -> Surface {
    let st = &doc.state;
    let mut out = Surface::new(st.width, st.height);
    let layers = &st.layers;
    let n = out.tiles.len();
    let tiles: Vec<_> = (0..n)
        .map(|i| crate::composite::composite_tile(layers, i, 0, 0, st.width, st.height))
        .collect();
    out.tiles = Arc::new(tiles);
    out
}

fn render_shape(doc: &mut Document, kind: ToolKind, sh: &ShapeState, s: &ToolSettings) {
    let bw = s.brush_width.max(1.0);
    let mut pb = PathBuilder::new();
    let (a, b) = if kind == ToolKind::LineCurve && sh.points.len() >= 2 {
        // arrow direction follows the last segment
        let n = sh.points.len();
        (sh.points[n - 2], sh.points[n - 1])
    } else {
        (sh.start, sh.end)
    };
    let outline = s.fill_style != FillStyle::Fill || kind == ToolKind::LineCurve;
    let fill = s.fill_style != FillStyle::Outline && kind != ToolKind::LineCurve;
    // Inset outlines so they stay inside the dragged rectangle (as Pinta does).
    let inset = if outline { bw / 2.0 } else { 0.0 };
    let x0 = a[0].min(b[0]).round() + inset;
    let y0 = a[1].min(b[1]).round() + inset;
    let x1 = a[0].max(b[0]).round() - inset;
    let y1 = a[1].max(b[1]).round() - inset;
    let rw = (x1 - x0).max(0.5);
    let rh = (y1 - y0).max(0.5);
    match kind {
        ToolKind::Rectangle => {
            if let Some(r) = Rect::from_xywh(x0, y0, rw, rh) {
                pb.push_rect(r);
            }
        }
        ToolKind::Ellipse => {
            if let Some(r) = Rect::from_xywh(x0, y0, rw, rh) {
                pb.push_oval(r);
            }
        }
        ToolKind::RoundedRectangle => {
            let rad = (s.radius).min(rw / 2.0).min(rh / 2.0).max(0.0);
            rounded_rect(&mut pb, x0, y0, rw, rh, rad);
        }
        ToolKind::LineCurve => curve_path(&mut pb, &sh.points),
        ToolKind::FreeformShape => {
            pb.move_to(sh.points[0][0], sh.points[0][1]);
            for q in &sh.points[1..] {
                pb.line_to(q[0], q[1]);
            }
            pb.close();
        }
        _ => {}
    }
    let Some(path) = pb.finish() else { return };
    let mut arrow_path = None;
    if kind == ToolKind::LineCurve && s.arrow {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / len, dy / len);
        let al = (bw * 4.0).max(8.0);
        let aw = al * 0.6;
        let base = [b[0] - ux * al, b[1] - uy * al];
        let mut ab = PathBuilder::new();
        ab.move_to(b[0], b[1]);
        ab.line_to(base[0] - uy * aw, base[1] + ux * aw);
        ab.line_to(base[0] + uy * aw, base[1] - ux * aw);
        ab.close();
        arrow_path = ab.finish();
    }
    let bb = path.bounds();
    let pad = bw + 2.0 + if arrow_path.is_some() { bw * 4.0 + 8.0 } else { 0.0 };
    let r = IRect::from_f(bb.left() - pad, bb.top() - pad, bb.right() + pad, bb.bottom() + pad).intersect(&doc.state.layer().surface.bounds());
    if r.is_empty() {
        doc.state.layer_mut().surface = sh.base.clone();
        return;
    }
    let mut pm = Pixmap::new(r.w as u32, r.h as u32).unwrap();
    let tr = Transform::from_translate(-r.x as f32, -r.y as f32);
    if fill {
        let mut paint = Paint::default();
        paint.set_color(sk_color(sh.fill));
        paint.anti_alias = s.antialias;
        pm.fill_path(&path, &paint, FillRule::Winding, tr, None);
    }
    if outline {
        let mut paint = Paint::default();
        paint.set_color(sk_color(sh.outline));
        paint.anti_alias = s.antialias;
        let st = SkStroke { width: bw, line_cap: LineCap::Square, line_join: LineJoin::Miter, ..Default::default() };
        pm.stroke_path(&path, &paint, &st, tr, None);
        if let Some(ap) = &arrow_path {
            pm.fill_path(ap, &paint, FillRule::Winding, tr, None);
        }
    }
    let sel = doc.state.selection.clone();
    let mode = if s.alpha_blending { PasteMode::Over } else { PasteMode::Replace };
    let mut target = sh.base.clone();
    if mode == PasteMode::Replace {
        // Overwrite only where the shape has coverage.
        let mut data = sh.base.read_rect(r);
        for (d, sp) in data.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
            if sp[3] > 0 {
                d.copy_from_slice(sp);
            }
        }
        paste_rgba(&mut target, &sh.base, &data, r, &sel, PasteMode::Replace);
    } else {
        paste_pixmap(&mut target, &sh.base, &pm, (r.x, r.y), &sel, mode);
    }
    doc.state.layer_mut().surface = target;
}

fn rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    if r <= 0.0 {
        if let Some(rr) = Rect::from_xywh(x, y, w, h) {
            pb.push_rect(rr);
        }
        return;
    }
    let k = 0.552_284_8 * r;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
}

fn render_gradient(doc: &mut Document, base: &Surface, start: [f32; 2], end: [f32; 2], c0: [u8; 4], c1: [u8; 4], s: &ToolSettings) {
    let (w, h) = (doc.state.width, doc.state.height);
    let sel = doc.state.selection.clone();
    let r = sel.bounds(w, h);
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let len2 = (dx * dx + dy * dy).max(1e-6);
    let len = len2.sqrt();
    let gt = s.gradient;
    let transparency = s.gradient_transparency;
    let blend = s.alpha_blending;
    let base_angle = dy.atan2(dx);
    let t_at = move |x: f32, y: f32| -> f32 {
        let px = x - start[0];
        let py = y - start[1];
        match gt {
            GradientType::Linear => ((px * dx + py * dy) / len2).clamp(0.0, 1.0),
            GradientType::LinearReflected => ((px * dx + py * dy) / len2).abs().clamp(0.0, 1.0),
            GradientType::Diamond => {
                let u = (px * dx + py * dy) / len2;
                let v = (-px * dy + py * dx) / len2;
                (u.abs() + v.abs()).clamp(0.0, 1.0)
            }
            GradientType::Radial => ((px * px + py * py).sqrt() / len).clamp(0.0, 1.0),
            GradientType::Conical => {
                let a = py.atan2(px) - base_angle;
                let a = a.rem_euclid(std::f32::consts::TAU);
                let t = a / std::f32::consts::PI;
                if t > 1.0 { 2.0 - t } else { t }
            }
        }
    };
    let mut target = base.clone();
    map_pixels(&mut target, base, r, &sel, move |x, y, px| {
        let t = t_at(x as f32 + 0.5, y as f32 + 0.5);
        if transparency {
            let a0 = c0[3] as f32;
            let a1 = c1[3] as f32;
            let k = ((a0 + (a1 - a0) * t) / 255.0).clamp(0.0, 1.0);
            let k8 = (k * 255.0).round() as u32;
            [crate::surface::mul8(px[0] as u32, k8), crate::surface::mul8(px[1] as u32, k8), crate::surface::mul8(px[2] as u32, k8), crate::surface::mul8(px[3] as u32, k8)]
        } else {
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            let c = premul([lerp(c0[0], c1[0]), lerp(c0[1], c1[1]), lerp(c0[2], c1[2]), lerp(c0[3], c1[3])]);
            if blend {
                let mut o = px;
                over(&mut o, c, 255);
                o
            } else {
                c
            }
        }
    });
    doc.state.layer_mut().surface = target;
}

fn render_move_selected(doc: &mut Document, ms: &MoveSelectedState) {
    let (w, h) = (doc.state.width, doc.state.height);
    let r = ms.lifted_rect;
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    // transform: lifted pixmap local -> image
    let t = ms.transform();
    let integer = ms.angle == 0.0 && ms.identity_scale() && ms.offset[0].fract() == 0.0 && ms.offset[1].fract() == 0.0;
    let sel_none = Selection::default();
    let mut target = ms.hole.clone();
    if integer {
        let dst = IRect::new(r.x + ms.offset[0] as i32, r.y + ms.offset[1] as i32, r.w, r.h);
        paste_rgba(&mut target, &ms.hole, ms.lifted.data(), dst, &sel_none, PasteMode::Over);
        doc.state.selection = Selection { mask: Some(ms.mask.translated(ms.offset[0] as i32, ms.offset[1] as i32)) };
    } else {
        // bounding box of rotated rect
        let corners = [(0.0, 0.0), (r.w as f32, 0.0), (0.0, r.h as f32), (r.w as f32, r.h as f32)];
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for (x, y) in corners {
            let mut p = tiny_skia::Point::from_xy(x, y);
            t.map_points(std::slice::from_mut(&mut p));
            bb = [bb[0].min(p.x), bb[1].min(p.y), bb[2].max(p.x), bb[3].max(p.y)];
        }
        let dst = IRect::from_f(bb[0], bb[1], bb[2], bb[3]).inflate(1).intersect(&IRect::new(0, 0, w as i32, h as i32));
        if !dst.is_empty() {
            let mut pm = Pixmap::new(dst.w as u32, dst.h as u32).unwrap();
            let quality = if ms.identity_scale() { tiny_skia::FilterQuality::Bilinear } else { tiny_skia::FilterQuality::Bicubic };
            let paint = tiny_skia::PixmapPaint { quality, ..Default::default() };
            let tt = Transform::from_translate(-dst.x as f32, -dst.y as f32).pre_concat(t);
            pm.draw_pixmap(0, 0, ms.lifted.as_ref(), &paint, tt, None);
            paste_pixmap(&mut target, &ms.hole, &pm, (dst.x, dst.y), &sel_none, PasteMode::Over);
            // transform the selection mask the same way
            let mut mp = Pixmap::new(r.w as u32, r.h as u32).unwrap();
            for y in 0..r.h {
                for x in 0..r.w {
                    let a = ms.mask.get(r.x + x, r.y + y);
                    let i = ((y * r.w + x) * 4) as usize;
                    mp.data_mut()[i..i + 4].copy_from_slice(&[a, a, a, a]);
                }
            }
            let mut out = Pixmap::new(dst.w as u32, dst.h as u32).unwrap();
            out.draw_pixmap(0, 0, mp.as_ref(), &paint, tt, None);
            let od = out.data().to_vec();
            let m = Mask::from_fn(w, h, dst, move |x, y| od[(((y - dst.y) * dst.w + (x - dst.x)) * 4 + 3) as usize]);
            doc.state.selection = Selection { mask: Some(m) };
        }
        let _ = (cx, cy);
    }
    doc.state.layer_mut().surface = target;
}

impl TextEdit {
    fn caret_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for (i, ch) in self.text.chars().enumerate() {
            if i == self.caret {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn caret_at(&self, pos: [f32; 2]) -> usize {
        let ly = ((pos[1] - self.origin[1]) / self.line_height).floor().max(0.0) as usize;
        let ly = ly.min(self.layout_lines.len().saturating_sub(1));
        let x = pos[0] - self.origin[0] - self.layout_lines.get(ly).map_or(0.0, |l| l.0);
        let carets = self.layout_carets.get(ly).cloned().unwrap_or_else(|| vec![0.0]);
        let col = carets.iter().enumerate().min_by(|a, b| (a.1 - x).abs().partial_cmp(&(b.1 - x).abs()).unwrap()).map_or(0, |(i, _)| i);
        // char index of (ly, col)
        let mut idx = 0;
        for (n, line) in self.text.split('\n').enumerate() {
            if n == ly {
                return idx + col.min(line.chars().count());
            }
            idx += line.chars().count() + 1;
        }
        self.text.chars().count()
    }
}

pub enum TextKey {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Commit,
}

impl Tools {
    pub fn text_editing(&self) -> bool {
        self.text.is_some()
    }

    pub fn commit_text(&mut self, doc: &mut Document) {
        if let Some(te) = self.text.take() {
            if te.text.trim().is_empty() {
                doc.state.layer_mut().surface = te.base;
                doc.revert_uncommitted();
            } else {
                doc.commit("Text");
            }
        }
    }

    pub fn text_key(&mut self, doc: &mut Document, pal: &Palette, k: TextKey) {
        let Some(te) = self.text.as_mut() else { return };
        let n = te.text.chars().count();
        let byte = |t: &str, ci: usize| t.char_indices().nth(ci).map_or(t.len(), |(b, _)| b);
        match k {
            TextKey::Insert(s) => {
                let b = byte(&te.text, te.caret);
                te.text.insert_str(b, &s);
                te.caret += s.chars().count();
            }
            TextKey::Backspace => {
                if te.caret > 0 {
                    let b = byte(&te.text, te.caret - 1);
                    te.text.remove(b);
                    te.caret -= 1;
                }
            }
            TextKey::Delete => {
                if te.caret < n {
                    let b = byte(&te.text, te.caret);
                    te.text.remove(b);
                }
            }
            TextKey::Left => te.caret = te.caret.saturating_sub(1),
            TextKey::Right => te.caret = (te.caret + 1).min(n),
            TextKey::Home | TextKey::End | TextKey::Up | TextKey::Down => {
                let (line, col) = te.caret_line_col();
                let lines: Vec<usize> = te.text.split('\n').map(|l| l.chars().count()).collect();
                let start_of = |l: usize| lines[..l].iter().map(|c| c + 1).sum::<usize>();
                te.caret = match k {
                    TextKey::Home => start_of(line),
                    TextKey::End => start_of(line) + lines[line],
                    TextKey::Up if line > 0 => start_of(line - 1) + col.min(lines[line - 1]),
                    TextKey::Down if line + 1 < lines.len() => start_of(line + 1) + col.min(lines[line + 1]),
                    _ => te.caret,
                };
            }
            TextKey::Commit => {
                self.commit_text(doc);
                return;
            }
        }
        self.render_text(doc, pal);
    }

    pub fn rerender_text(&mut self, doc: &mut Document, pal: &Palette) {
        if self.text.is_some() {
            self.render_text(doc, pal);
        }
    }

    fn render_text(&mut self, doc: &mut Document, pal: &Palette) {
        let params = self.text_params.clone();
        let Some(te) = self.text.as_mut() else { return };
        let (layout, _) = crate::text::layout_and_path(&te.text, &params);
        te.layout_lines = layout.lines.clone();
        te.layout_carets = layout.carets.clone();
        te.line_height = layout.line_height;
        te.bounds = IRect::new(te.origin[0] as i32, te.origin[1] as i32, layout.width.ceil().max(1.0) as i32, layout.height.ceil() as i32);
        let sel = doc.state.selection.clone();
        let mut target = te.base.clone();
        if !te.text.is_empty() {
            if let Some((pm, off, _)) = crate::text::render(&te.text, &params, pal.primary, pal.secondary) {
                let at = (te.origin[0] as i32 + off.0, te.origin[1] as i32 + off.1);
                paste_pixmap(&mut target, &te.base, &pm, at, &sel, PasteMode::Over);
            }
        }
        doc.state.layer_mut().surface = target;
    }
}

fn floating_key(doc: &Document) -> (u64, u64, usize, usize) {
    let l = doc.state.layer();
    let m = doc.state.selection.mask.as_ref().map_or(0, |m| Arc::as_ptr(&m.tiles) as usize);
    (doc.id, l.id, Arc::as_ptr(&l.surface.tiles) as usize, m)
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Catmull-Rom control points for segment i (p[i] -> p[i+1]) as cubic Bezier.
fn seg_bezier(p: &[[f32; 2]], i: usize) -> ([f32; 2], [f32; 2], [f32; 2], [f32; 2]) {
    let p0 = if i == 0 { p[0] } else { p[i - 1] };
    let p1 = p[i];
    let p2 = p[i + 1];
    let p3 = if i + 2 < p.len() { p[i + 2] } else { p[i + 1] };
    let c1 = [p1[0] + (p2[0] - p0[0]) / 6.0, p1[1] + (p2[1] - p0[1]) / 6.0];
    let c2 = [p2[0] - (p3[0] - p1[0]) / 6.0, p2[1] - (p3[1] - p1[1]) / 6.0];
    (p1, c1, c2, p2)
}

/// Smooth curve through all points (straight line for two points).
fn curve_path(pb: &mut PathBuilder, p: &[[f32; 2]]) {
    if p.is_empty() {
        return;
    }
    pb.move_to(p[0][0], p[0][1]);
    for i in 0..p.len().saturating_sub(1) {
        let (_, c1, c2, e) = seg_bezier(p, i);
        pb.cubic_to(c1[0], c1[1], c2[0], c2[1], e[0], e[1]);
    }
}

/// Index of the segment whose curve passes within `r` of `q`.
fn hit_curve(p: &[[f32; 2]], q: [f32; 2], r: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for i in 0..p.len().saturating_sub(1) {
        let (a, b, c, d) = seg_bezier(p, i);
        let at = |t: f32| {
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            [w[0] * a[0] + w[1] * b[0] + w[2] * c[0] + w[3] * d[0], w[0] * a[1] + w[1] * b[1] + w[2] * c[1] + w[3] * d[1]]
        };
        let mut prev = at(0.0);
        for k in 1..=32 {
            let cur = at(k as f32 / 32.0);
            // distance from q to segment prev-cur
            let (vx, vy) = (cur[0] - prev[0], cur[1] - prev[1]);
            let l2 = (vx * vx + vy * vy).max(1e-6);
            let t = (((q[0] - prev[0]) * vx + (q[1] - prev[1]) * vy) / l2).clamp(0.0, 1.0);
            let d = dist(q, [prev[0] + vx * t, prev[1] + vy * t]);
            if d <= r && best.is_none_or(|b| d < b.1) {
                best = Some((i, d));
            }
            prev = cur;
        }
    }
    best.map(|b| b.0)
}

impl Tools {
    fn line_sig(&self) -> (u32, bool, bool, bool) {
        let s = &self.settings;
        (s.brush_width.to_bits(), s.antialias, s.arrow, s.alpha_blending)
    }

    fn render_line(&mut self, doc: &mut Document) {
        if let Some(le) = &self.line {
            render_shape(doc, ToolKind::LineCurve, &le.sh, &self.settings);
        }
    }

    /// Re-render the line being edited when its toolbar options change, and
    /// redo the last magic-wand selection live when its tolerance/mode changes.
    pub fn tick(&mut self, doc: &mut Document) {
        self.ms_box = None;
        if self.current == ToolKind::MoveSelected {
            self.ms_box = match &self.active {
                Active::MoveSelected(ms) => Some(ms.bx()),
                _ => match &self.floating {
                    Some((k, ms)) if *k == floating_key(doc) => Some(ms.bx()),
                    _ => {
                        let key = (doc.id, doc.state.selection.mask.as_ref().map_or(0, |m| Arc::as_ptr(&m.tiles) as usize));
                        let r = match self.ms_bounds_cache {
                            Some((k, r)) if k == key => r,
                            _ => {
                                let r = doc.state.selection.bounds(doc.state.width, doc.state.height);
                                self.ms_bounds_cache = Some((key, r));
                                r
                            }
                        };
                        (!r.is_empty()).then(|| MsBox::of_rect(r))
                    }
                },
            };
        }
        let s = &self.settings;
        let wsig = (s.tolerance.to_bits(), s.flood_global, s.wand_from);
        if let Some(w) = self.wand_last.as_mut() {
            let current = w.doc_id == doc.id && w.history_pos == doc.history_pos && doc.history.get(doc.history_pos).is_some_and(|h| h.label == "Magic Wand Select");
            if !current {
                self.wand_last = None;
            } else if w.sig != wsig {
                w.sig = wsig;
                let src = match s.wand_from {
                    SampleFrom::Layer => doc.state.layer().surface.clone(),
                    SampleFrom::Image => flatten(doc),
                };
                let m = fill_mask(&src, w.at.0, w.at.1, s.tolerance, s.flood_global || w.shift, &Selection::default());
                let base = w.base.clone();
                apply_selection(doc, &base, m, w.mode);
                // replace the wand's history step instead of adding new ones
                doc.history[doc.history_pos].state = doc.state.clone();
            }
        }
        let sig = self.line_sig();
        if let Some(le) = self.line.as_mut() {
            if le.sig != sig {
                le.sig = sig;
                self.render_line(doc);
            }
        }
    }

    pub fn line_editing(&self) -> bool {
        self.line.is_some()
    }

    /// Enter: write the line into the layer as one history step.
    pub fn commit_line(&mut self, doc: &mut Document) {
        if let Some(le) = self.line.take() {
            self.active = Active::None;
            self.status.clear();
            let n = le.sh.points.len();
            if n >= 2 && (0..n - 1).any(|i| dist(le.sh.points[i], le.sh.points[i + 1]) >= 0.5) {
                doc.commit("Line/Curve");
            } else {
                doc.revert_uncommitted();
            }
        }
    }

    /// Delete key: remove the selected point (keeps at least two).
    pub fn delete_line_point(&mut self, doc: &mut Document) {
        if let Some(le) = self.line.as_mut() {
            if let Some(i) = le.selected {
                if le.sh.points.len() > 2 {
                    le.sh.points.remove(i);
                    le.selected = None;
                    self.render_line(doc);
                }
            }
        }
    }
}
