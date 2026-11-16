//! Text tool rendering: system fonts via fontdb, glyph outlines via ab_glyph,
//! filled/stroked with tiny-skia (so outline styles and antialiasing match
//! the shape tools).

use ab_glyph::{Font, FontVec, OutlineCurve};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tiny_skia::{FillRule, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub struct FontCatalog {
    pub db: resvg::usvg::fontdb::Database,
    pub families: Vec<String>,
    cache: std::sync::Mutex<HashMap<(String, bool, bool), Option<Arc<FontVec>>>>,
}

pub fn catalog() -> &'static FontCatalog {
    static C: OnceLock<FontCatalog> = OnceLock::new();
    C.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        let mut families: Vec<String> = db.faces().filter_map(|f| f.families.first().map(|(n, _)| n.clone())).collect();
        families.sort_by_key(|a| a.to_lowercase());
        families.dedup();
        FontCatalog { db, families, cache: Default::default() }
    })
}

impl FontCatalog {
    pub fn font(&self, family: &str, bold: bool, italic: bool) -> Option<Arc<FontVec>> {
        let key = (family.to_string(), bold, italic);
        if let Some(f) = self.cache.lock().unwrap().get(&key) {
            return f.clone();
        }
        use resvg::usvg::fontdb::{Family, Query, Style, Weight};
        let fams = [Family::Name(family), Family::SansSerif];
        let q = Query { families: &fams, weight: if bold { Weight::BOLD } else { Weight::NORMAL }, style: if italic { Style::Italic } else { Style::Normal }, ..Default::default() };
        let f = self.db.query(&q).and_then(|id| self.db.with_face_data(id, |data, index| FontVec::try_from_vec_and_index(data.to_vec(), index).ok()).flatten()).map(Arc::new);
        self.cache.lock().unwrap().insert(key, f.clone());
        f
    }

    /// Whether the found face really is italic (otherwise we slant it).
    pub fn has_italic(&self, family: &str) -> bool {
        use resvg::usvg::fontdb::Style;
        self.db.faces().any(|f| f.families.iter().any(|(n, _)| n == family) && f.style != Style::Normal)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextStyle {
    Normal,
    NormalOutline,
    Outline,
    FillBackground,
}

impl TextStyle {
    pub const ALL: [TextStyle; 4] = [TextStyle::Normal, TextStyle::NormalOutline, TextStyle::Outline, TextStyle::FillBackground];
    pub fn name(self) -> &'static str {
        match self {
            TextStyle::Normal => "Normal",
            TextStyle::NormalOutline => "Normal and Outline",
            TextStyle::Outline => "Outline",
            TextStyle::FillBackground => "Fill Background",
        }
    }
}

#[derive(Clone)]
pub struct TextParams {
    pub family: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub align: Align,
    pub style: TextStyle,
    pub outline_width: f32,
    pub antialias: bool,
}

impl Default for TextParams {
    fn default() -> Self {
        let cat = catalog();
        let family = ["Noto Sans", "DejaVu Sans", "Liberation Sans", "Cantarell", "Arial"].iter().find(|f| cat.families.iter().any(|x| x == *f)).map(|s| s.to_string()).or_else(|| cat.families.first().cloned()).unwrap_or_else(|| "sans-serif".into());
        TextParams { family, size: 24.0, bold: false, italic: false, underline: false, align: Align::Left, style: TextStyle::Normal, outline_width: 2.0, antialias: true }
    }
}

pub struct Layout {
    /// One entry per line: (x offset of line start, baseline y, width)
    pub lines: Vec<(f32, f32, f32)>,
    /// Caret x positions for each char boundary of each line.
    pub carets: Vec<Vec<f32>>,
    pub width: f32,
    pub height: f32,
    pub line_height: f32,
    pub ascent: f32,
}

/// Lay out `text` and optionally render it. Coordinates are relative to the
/// text origin (top-left of the first line box).
pub fn layout_and_path(text: &str, p: &TextParams) -> (Layout, Option<tiny_skia::Path>) {
    let cat = catalog();
    let font = cat.font(&p.family, p.bold, p.italic);
    let Some(font) = font else {
        return (Layout { lines: vec![(0.0, p.size, 0.0)], carets: vec![vec![0.0]], width: 0.0, height: p.size * 1.2, line_height: p.size * 1.2, ascent: p.size }, None);
    };
    let upem = font.units_per_em().unwrap_or(1000.0);
    let k = p.size / upem;
    let ascent = font.ascent_unscaled() * k;
    let descent = font.descent_unscaled() * k;
    let gap = font.line_gap_unscaled() * k;
    let line_height = (ascent - descent + gap).max(p.size);
    let slant = if p.italic && !cat.has_italic(&p.family) { 0.2 } else { 0.0 };
    let mut pb = PathBuilder::new();
    let mut lines = Vec::new();
    let mut carets = Vec::new();
    let mut widths = Vec::new();
    // measure
    for line in text.split('\n') {
        let mut x = 0.0;
        let mut cs = vec![0.0];
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(pid) = prev {
                x += font.kern_unscaled(pid, id) * k;
            }
            x += font.h_advance_unscaled(id) * k;
            cs.push(x);
            prev = Some(id);
        }
        widths.push(x);
        carets.push(cs);
    }
    let width = widths.iter().copied().fold(0.0, f32::max);
    for (li, line) in text.split('\n').enumerate() {
        let lw = widths[li];
        let x0 = match p.align {
            Align::Left => 0.0,
            Align::Center => (width - lw) / 2.0,
            Align::Right => width - lw,
        };
        let baseline = li as f32 * line_height + ascent;
        lines.push((x0, baseline, lw));
        let mut prev = None;
        for (ci, ch) in line.chars().enumerate() {
            let id = font.glyph_id(ch);
            let _ = prev.map(|pid| font.kern_unscaled(pid, id));
            let gx = x0 + carets[li][ci];
            if let Some(o) = font.outline(id) {
                let tf = |pt: ab_glyph::Point| (gx + pt.x * k + pt.y * k * slant, baseline - pt.y * k);
                let mut last: Option<(f32, f32)> = None;
                for c in &o.curves {
                    let (start, end) = match c {
                        OutlineCurve::Line(a, b) => (tf(*a), tf(*b)),
                        OutlineCurve::Quad(a, _, b) => (tf(*a), tf(*b)),
                        OutlineCurve::Cubic(a, _, _, b) => (tf(*a), tf(*b)),
                    };
                    if last.is_none_or(|l| (l.0 - start.0).abs() > 1e-3 || (l.1 - start.1).abs() > 1e-3) {
                        if last.is_some() {
                            pb.close();
                        }
                        pb.move_to(start.0, start.1);
                    }
                    match c {
                        OutlineCurve::Line(_, _) => pb.line_to(end.0, end.1),
                        OutlineCurve::Quad(_, c1, _) => {
                            let q = tf(*c1);
                            pb.quad_to(q.0, q.1, end.0, end.1)
                        }
                        OutlineCurve::Cubic(_, c1, c2, _) => {
                            let q1 = tf(*c1);
                            let q2 = tf(*c2);
                            pb.cubic_to(q1.0, q1.1, q2.0, q2.1, end.0, end.1)
                        }
                    }
                    last = Some(end);
                }
                if last.is_some() {
                    pb.close();
                }
            }
            prev = Some(id);
        }
        if p.underline && lw > 0.0 {
            let th = (p.size / 14.0).max(1.0);
            if let Some(r) = tiny_skia::Rect::from_xywh(x0, baseline + th * 1.5, lw, th) {
                pb.push_rect(r);
            }
        }
    }
    let n = lines.len() as f32;
    let layout = Layout { lines, carets, width, height: n * line_height, line_height, ascent };
    (layout, pb.finish())
}

/// Render text into a pixmap placed at `origin` (image coords). Returns the
/// pixmap and its top-left in image space.
pub fn render(text: &str, p: &TextParams, primary: [u8; 4], secondary: [u8; 4]) -> Option<(Pixmap, (i32, i32), Layout)> {
    let (layout, path) = layout_and_path(text, p);
    let pad = (p.outline_width + 4.0).ceil() as i32 + (p.size * 0.3) as i32;
    let w = layout.width.ceil() as i32 + 2 * pad;
    let h = layout.height.ceil() as i32 + 2 * pad;
    let mut pm = Pixmap::new(w.max(1) as u32, h.max(1) as u32)?;
    let tr = Transform::from_translate(pad as f32, pad as f32);
    let col = |c: [u8; 4]| {
        let mut paint = Paint::default();
        paint.set_color(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
        paint.anti_alias = p.antialias;
        paint
    };
    if p.style == TextStyle::FillBackground {
        if let Some(r) = tiny_skia::Rect::from_xywh(pad as f32 - 2.0, pad as f32 - 2.0, layout.width + 4.0, layout.height + 4.0) {
            pm.fill_rect(r, &col(secondary), Transform::identity(), None);
        }
    }
    if let Some(path) = &path {
        let stroke = Stroke { width: p.outline_width.max(0.5), line_join: LineJoin::Round, ..Default::default() };
        match p.style {
            TextStyle::Normal => pm.fill_path(path, &col(primary), FillRule::Winding, tr, None),
            TextStyle::FillBackground => pm.fill_path(path, &col(primary), FillRule::Winding, tr, None),
            TextStyle::Outline => pm.stroke_path(path, &col(primary), &stroke, tr, None),
            TextStyle::NormalOutline => {
                pm.stroke_path(path, &col(secondary), &stroke, tr, None);
                pm.fill_path(path, &col(primary), FillRule::Winding, tr, None);
            }
        }
    }
    Some((pm, (-pad, -pad), layout))
}
