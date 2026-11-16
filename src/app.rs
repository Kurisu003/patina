//! Main window: menus, tool box, tool bar, canvas, layers/history panels,
//! palette and dialogs. Layout follows Pinta.

use crate::composite::Pyramid;
use crate::document::Document;
use crate::effects::{self, Category, Effect, Progress, RenderCtx};
use crate::gpu::{CanvasCallback, CanvasFrame, FrameTile, GpuResources, ViewUniform};
use crate::ops;
use crate::selection::CombineMode;
use crate::surface::{IRect, Surface, TILE};
use crate::tools::{FillStyle, GradientType, Palette, Pointer, SampleFrom, ToolKind, Tools, ViewRequest};
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

const ICON_PX: u32 = 32;

struct AntsCache {
    key: usize,
    version: u64,
    segs: Arc<Vec<[f32; 4]>>,
}

struct EffectJob {
    effect: Box<dyn Effect>,
    doc_id: u64,
    layer: usize,
    base: Surface,
    progress: Arc<Progress>,
    rx: Receiver<(u64, Option<Surface>)>,
    tx: std::sync::mpsc::Sender<(u64, Option<Surface>)>,
    generation: u64,
    rendered: Option<u64>,
    dialog: bool,
    apply_when_done: bool,
    started: Instant,
}

enum Dialog {
    None,
    Preferences { w: u32, h: u32, bg: u8 },
    /// Clipboard image: paste into the current image or open as a new one.
    PasteChoice { w: u32, h: u32, data: Arc<Vec<u8>>, fit: ops::PasteFit },
    /// Files dropped onto the window while an image is open.
    DropChoice { paths: Vec<PathBuf>, fit: ops::PasteFit },
    NewImage { w: u32, h: u32, bg: u8 },
    ResizeImage { pct: bool, percent: f32, w: u32, h: u32, keep: bool, mode: ops::Resample },
    CanvasSize { pct: bool, percent: f32, w: u32, h: u32, keep: bool, anchor: u8 },
    LayerProps { name: String, hidden: bool, opacity: f32, blend: crate::blend::BlendMode, orig: (String, bool, f32, crate::blend::BlendMode) },
    Jpeg { path: PathBuf, quality: u8 },
    ConfirmClose { doc: usize, quit: bool },
    About,
}

pub struct App {
    docs: Vec<Document>,
    active: usize,
    pyramids: HashMap<u64, Pyramid>,
    ants: HashMap<u64, AntsCache>,
    ants_counter: u64,
    thumbs: HashMap<u64, (usize, egui::TextureHandle)>,
    tools: Tools,
    palette: Palette,
    icons: HashMap<String, egui::TextureHandle>,
    effects: Vec<Box<dyn Effect>>,
    job: Option<EffectJob>,
    last_effect: Option<usize>,
    dialog: Dialog,
    show_grid: bool,
    prefs: Prefs,
    skin: Skin,
    skin_applied: Option<Skin>,
    win_tools: bool,
    win_history: bool,
    win_layers: bool,
    win_colors: bool,
    color_target_primary: bool,
    error: Option<String>,
    start: Instant,
    drag: Option<bool>,
    middle_pan: Option<Pos2>,
    canvas_rect: Rect,
    clipboard: Option<arboard::Clipboard>,
    jpeg_quality: u8,
    gpu_slots: u32,
    frame_ms: f32,
    cursor: Option<[f32; 2]>,
    quit_confirmed: bool,
    palette_edit: Option<bool>,
    script: Option<crate::script::Script>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Skin {
    /// Pinta's docked layout.
    Pinta,
    /// Paint.NET-style layout: light theme, floating tool windows.
    Classic,
}

impl Skin {
    fn path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/patina/skin"))
    }
    fn load() -> Skin {
        let old = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/pinta-rs/skin"));
        let text = Self::path().and_then(|p| std::fs::read_to_string(p).ok()).or_else(|| old.and_then(|p| std::fs::read_to_string(p).ok()));
        match text.as_deref().map(str::trim) {
            Some("classic") => Skin::Classic,
            _ => Skin::Pinta,
        }
    }
    fn save(self) {
        if let Some(p) = Self::path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, if self == Skin::Classic { "classic" } else { "pinta" });
        }
    }
}

/// User preferences, stored in ~/.config/patina/prefs as key=value lines.
#[derive(Clone, Copy, Debug)]
struct Prefs {
    /// Default canvas for the startup image and New Image.
    w: u32,
    h: u32,
    /// 0 white, 1 secondary color, 2 transparent
    bg: u8,
}

impl Prefs {
    fn path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/patina/prefs"))
    }
    fn load() -> Prefs {
        let mut p = Prefs { w: 800, h: 600, bg: 0 };
        let text = Self::path().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "canvas_width" => p.w = v.parse().unwrap_or(p.w).clamp(1, 100_000),
                "canvas_height" => p.h = v.parse().unwrap_or(p.h).clamp(1, 100_000),
                "canvas_background" => p.bg = v.parse().unwrap_or(p.bg).min(2),
                _ => {}
            }
        }
        p
    }
    fn save(self) {
        if let Some(f) = Self::path() {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            let _ = std::fs::write(f, format!("canvas_width={}\ncanvas_height={}\ncanvas_background={}\n", self.w, self.h, self.bg));
        }
    }
}

fn classic_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    let accent = Color32::from_rgb(51, 153, 255);
    v.panel_fill = Color32::from_rgb(234, 239, 247);
    v.window_fill = Color32::from_rgb(246, 248, 252);
    v.extreme_bg_color = Color32::from_rgb(207, 215, 230);
    v.faint_bg_color = Color32::from_rgb(240, 243, 249);
    v.selection.bg_fill = accent;
    v.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = accent;
    v.window_stroke = egui::Stroke::new(1.0, Color32::from_rgb(160, 175, 200));
    v.window_corner_radius = egui::CornerRadius::same(4);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(225, 231, 241);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(204, 228, 252);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(170, 210, 250);
    v
}

fn default_palette() -> Vec<[u8; 4]> {
    let mut v = Vec::new();
    for i in 0..8u32 {
        let g = (255 * (7 - i) / 7) as u8;
        v.push([g, g, g, 255]);
    }
    for (s, val) in [(100.0, 100.0), (100.0, 60.0), (45.0, 100.0)] {
        for k in 0..12 {
            let (r, g, b) = crate::effects::util::hsv_to_rgb(k as f64 * 30.0, s, val);
            v.push([r, g, b, 255]);
        }
    }
    v
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let mut gpu_slots = 0;
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let res = GpuResources::new(&rs.device, rs.target_format);
            res.clear_slot0(&rs.queue);
            gpu_slots = res.slot_count();
            rs.renderer.write().callback_resources.insert(res);
            log::info!("GPU: {:?}, target {:?}", rs.adapter.get_info().name, rs.target_format);
        }
        let mut app = App {
            docs: Vec::new(),
            active: 0,
            pyramids: HashMap::new(),
            ants: HashMap::new(),
            ants_counter: 0,
            thumbs: HashMap::new(),
            tools: Tools::default(),
            palette: Palette::default(),
            icons: load_icons(&cc.egui_ctx),
            effects: effects::registry(),
            job: None,
            last_effect: None,
            dialog: Dialog::None,
            show_grid: false,
            prefs: Prefs::load(),
            skin: Skin::load(),
            skin_applied: None,
            win_tools: true,
            win_history: true,
            win_layers: true,
            win_colors: true,
            color_target_primary: true,
            error: None,
            start: Instant::now(),
            drag: None,
            middle_pan: None,
            canvas_rect: Rect::NOTHING,
            clipboard: arboard::Clipboard::new().ok(),
            jpeg_quality: 85,
            gpu_slots,
            frame_ms: 0.0,
            cursor: None,
            quit_confirmed: false,
            palette_edit: None,
            script: std::env::var("PINTA_SCRIPT").ok().and_then(|p| crate::script::Script::load(&p)),
        };
        for f in files {
            app.open_path(f);
        }
        if app.docs.is_empty() {
            let p = app.prefs;
            let bg = app.bg_color(p.bg);
            app.docs.push(Document::new("Unsaved Image 1", p.w, p.h, bg));
        }
        app
    }

    fn doc(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.active)
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn open_path(&mut self, p: PathBuf) {
        let t0 = Instant::now();
        match crate::io::load(&p) {
            Ok(d) => {
                log::info!("opened {} ({}x{}) in {:.0} ms", p.display(), d.state.width, d.state.height, t0.elapsed().as_secs_f32() * 1000.0);
                self.docs.push(d);
                self.active = self.docs.len() - 1;
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new().add_filter("Images", crate::io::OPEN_EXTENSIONS).add_filter("All files", &["*"]).pick_files() {
            for f in files {
                self.open_path(f);
            }
        }
    }

    fn save(&mut self, save_as: bool) {
        let Some(doc) = self.docs.get(self.active) else { return };
        let path = match (&doc.path, save_as) {
            (Some(p), false) if crate::io::SAVE_EXTENSIONS.contains(&p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default().as_str()) => Some(p.clone()),
            _ => {
                let name = doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| doc.title.clone());
                let name = if name.contains('.') { name } else { format!("{name}.png") };
                rfd::FileDialog::new()
                    .set_file_name(name)
                    .add_filter("PNG", &["png"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .add_filter("OpenRaster", &["ora"])
                    .add_filter("WebP", &["webp"])
                    .add_filter("BMP", &["bmp"])
                    .add_filter("TIFF", &["tif", "tiff"])
                    .add_filter("TGA", &["tga"])
                    .add_filter("ICO", &["ico"])
                    .add_filter("QOI", &["qoi"])
                    .save_file()
            }
        };
        let Some(path) = path else { return };
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if ext == "jpg" || ext == "jpeg" {
            self.dialog = Dialog::Jpeg { path, quality: self.jpeg_quality };
            return;
        }
        self.write_file(path, 90);
    }

    fn write_file(&mut self, mut path: PathBuf, quality: u8) {
        if path.extension().is_none() {
            path.set_extension("png");
        }
        let Some(doc) = self.docs.get_mut(self.active) else { return };
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if doc.state.layers.len() > 1 && ext != "ora" {
            log::info!("flattening {} layers for {}", doc.state.layers.len(), ext);
        }
        match crate::io::save(doc, &path, quality) {
            Ok(()) => {
                doc.title = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                doc.path = Some(path);
                doc.mark_clean();
            }
            Err(e) => self.error = Some(format!("Could not save: {e}")),
        }
    }

    fn close_doc(&mut self, i: usize, force: bool) {
        if i >= self.docs.len() {
            return;
        }
        if !force && self.docs[i].is_dirty() {
            self.dialog = Dialog::ConfirmClose { doc: i, quit: false };
            return;
        }
        let d = self.docs.remove(i);
        self.pyramids.remove(&d.id);
        self.ants.remove(&d.id);
        if self.job.as_ref().is_some_and(|j| j.doc_id == d.id) {
            self.job = None;
        }
        self.active = self.active.min(self.docs.len().saturating_sub(1));
    }

    // ---- effects ------------------------------------------------------

    fn start_effect(&mut self, idx: usize) {
        if self.busy() {
            return;
        }
        let Some(doc) = self.docs.get_mut(self.active) else { return };
        self.tools.finish(doc);
        let effect = self.effects[idx].box_clone();
        let dialog = effect.has_dialog();
        let (tx, rx) = channel();
        let job = EffectJob {
            effect,
            doc_id: doc.id,
            layer: doc.state.current,
            base: doc.state.layer().surface.clone(),
            progress: Progress::new(),
            rx,
            tx,
            generation: 0,
            rendered: None,
            dialog,
            apply_when_done: !dialog,
            started: Instant::now(),
        };
        self.last_effect = Some(idx);
        self.job = Some(job);
        self.render_effect();
    }

    fn render_effect(&mut self) {
        let Some(job) = self.job.as_mut() else { return };
        let Some(doc) = self.docs.iter().find(|d| d.id == job.doc_id) else { return };
        job.progress.cancel.store(true, Ordering::Relaxed);
        job.progress = Progress::new();
        job.generation += 1;
        job.started = Instant::now();
        let generation = job.generation;
        let mut effect = job.effect.box_clone();
        let base = job.base.clone();
        let sel = doc.state.selection.clone();
        let ctx = RenderCtx {
            width: doc.state.width,
            height: doc.state.height,
            sel_bounds: sel.bounds(doc.state.width, doc.state.height),
            primary: self.palette.primary,
            secondary: self.palette.secondary,
        };
        let progress = job.progress.clone();
        let tx = job.tx.clone();
        std::thread::spawn(move || {
            let out = effects::apply(effect.as_mut(), &base, &sel, &ctx, &progress);
            let _ = tx.send((generation, out));
        });
    }

    fn poll_effect(&mut self, ctx: &egui::Context) {
        let Some(job) = self.job.as_mut() else { return };
        let mut got = None;
        while let Ok((g, s)) = job.rx.try_recv() {
            if g == job.generation {
                got = Some(s);
            }
        }
        if let Some(Some(surface)) = got {
            log::info!("{} rendered in {:.0} ms", job.effect.name(), job.started.elapsed().as_secs_f32() * 1000.0);
            job.rendered = Some(job.generation);
            if let Some(doc) = self.docs.iter_mut().find(|d| d.id == job.doc_id) {
                if job.layer < doc.state.layers.len() {
                    doc.state.layers[job.layer].surface = surface;
                }
            }
        }
        let job = self.job.as_ref().unwrap();
        if job.apply_when_done && job.rendered == Some(job.generation) {
            let name = job.effect.name();
            let id = job.doc_id;
            if let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) {
                doc.commit(name);
            }
            self.job = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn cancel_effect(&mut self) {
        if let Some(job) = self.job.take() {
            job.progress.cancel.store(true, Ordering::Relaxed);
            if let Some(doc) = self.docs.iter_mut().find(|d| d.id == job.doc_id) {
                doc.revert_uncommitted();
            }
        }
    }

    // ---- clipboard ------------------------------------------------------

    fn copy(&mut self, merged: bool, cut: bool) {
        let Some(doc) = self.docs.get_mut(self.active) else { return };
        let Some((r, mut data)) = ops::copy_selection(doc, merged) else { return };
        for p in data.chunks_exact_mut(4) {
            let c = crate::surface::unpremul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
        if let Some(cb) = self.clipboard.as_mut() {
            let img = arboard::ImageData { width: r.w as usize, height: r.h as usize, bytes: data.into() };
            if let Err(e) = cb.set_image(img) {
                self.error = Some(format!("Clipboard: {e}"));
            }
        }
        if cut {
            ops::erase_selection(doc);
            if let Some(h) = doc.history.last_mut() {
                h.label = "Cut".into();
            }
        }
    }

    fn paste(&mut self, target: u8) {
        let Some(cb) = self.clipboard.as_mut() else { return };
        let img = match cb.get_image() {
            Ok(i) => i,
            Err(_) => {
                self.error = Some("The clipboard does not contain an image.".into());
                return;
            }
        };
        let (w, h) = (img.width as u32, img.height as u32);
        let mut data = img.bytes.into_owned();
        for p in data.chunks_exact_mut(4) {
            let c = crate::surface::premul([p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&c);
        }
        if target == 0 && !self.docs.is_empty() {
            let fit = self.default_fit(w, h);
            self.dialog = Dialog::PasteChoice { w, h, data: Arc::new(data), fit };
            return;
        }
        let fit = self.default_fit(w, h);
        self.paste_data(w, h, &data, target, "Paste Into New Image", fit);
    }

    /// Expand when the pasted image is bigger than the canvas, else keep.
    fn default_fit(&self, w: u32, h: u32) -> ops::PasteFit {
        match self.docs.get(self.active) {
            Some(d) if w > d.state.width || h > d.state.height => ops::PasteFit::Expand,
            _ => ops::PasteFit::Keep,
        }
    }

    /// Place premultiplied pixels: 0 = current layer, 1 = new layer, 2 = new image.
    fn paste_data(&mut self, w: u32, h: u32, data: &[u8], target: u8, new_label: &str, fit: ops::PasteFit) {
        if target == 2 || self.docs.is_empty() {
            let s = Surface::from_rgba(w, h, &data);
            let n = self.docs.len() + 1;
            let mut d = Document::from_layers(format!("Unsaved Image {n}"), w, h, vec![crate::document::Layer::new("Background", s)], new_label);
            d.clean_pos = None;
            self.docs.push(d);
            self.active = self.docs.len() - 1;
            return;
        }
        // paste at the top-left of the visible area, like Pinta
        let at = self.visible_origin(&self.docs[self.active]);
        let doc = &mut self.docs[self.active];
        self.tools.finish(doc);
        ops::paste(doc, w, h, data, at, target == 1, fit);
        self.tools.set_tool(None, ToolKind::MoveSelected);
    }

    fn bg_color(&self, bg: u8) -> Option<[u8; 4]> {
        match bg {
            0 => Some([255, 255, 255, 255]),
            1 => Some(self.palette.secondary),
            _ => None,
        }
    }

    fn visible_origin(&self, doc: &Document) -> (i32, i32) {
        let ppp = 1.0;
        let z = doc.view.zoom / ppp;
        let r = self.canvas_rect;
        if !r.is_positive() {
            return (0, 0);
        }
        let x = doc.view.center[0] - r.width() / 2.0 / z;
        let y = doc.view.center[1] - r.height() / 2.0 / z;
        (x.max(0.0) as i32, y.max(0.0) as i32)
    }

    // ---- view -------------------------------------------------------------

    fn zoom_steps() -> &'static [f32] {
        &[0.01, 0.02, 0.03, 0.05, 0.08, 0.12, 0.16, 0.25, 0.33, 0.5, 0.66, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 24.0, 32.0, 40.0, 48.0, 64.0, 80.0, 100.0, 120.0, 160.0]
    }

    fn zoom_in(z: f32) -> f32 {
        Self::zoom_steps().iter().copied().find(|&s| s > z * 1.001).unwrap_or(z)
    }
    fn zoom_out(z: f32) -> f32 {
        Self::zoom_steps().iter().rev().copied().find(|&s| s < z * 0.999).unwrap_or(z)
    }

    fn fit(doc: &mut Document, r: Rect, ppp: f32) {
        let avail = r.size() * ppp - Vec2::splat(40.0);
        let z = (avail.x / doc.state.width as f32).min(avail.y / doc.state.height as f32);
        doc.view.zoom = z.min(1.0).max(0.001);
        doc.view.center = [doc.state.width as f32 / 2.0, doc.state.height as f32 / 2.0];
    }

    fn clamp_center(doc: &mut Document) {
        let (w, h) = (doc.state.width as f32, doc.state.height as f32);
        doc.view.center[0] = doc.view.center[0].clamp(0.0, w);
        doc.view.center[1] = doc.view.center[1].clamp(0.0, h);
    }
}

fn load_icons(ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
    mod embedded {
        include!(concat!(env!("OUT_DIR"), "/icons.rs"));
    }
    let mut out = HashMap::new();
    for &(stem, data) in embedded::ICONS {
        let Ok(tree) = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default()) else { continue };
        let Some(mut pm) = resvg::tiny_skia::Pixmap::new(ICON_PX, ICON_PX) else { continue };
        let s = tree.size();
        let sc = ICON_PX as f32 / s.width().max(s.height());
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(sc, sc), &mut pm.as_mut());
        // symbolic icons: keep alpha, paint white, tint at draw time
        let mut rgba = Vec::with_capacity((ICON_PX * ICON_PX * 4) as usize);
        for px in pm.data().chunks_exact(4) {
            rgba.extend_from_slice(&[255, 255, 255, px[3]]);
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([ICON_PX as usize, ICON_PX as usize], &rgba);
        let name = stem.trim_end_matches("-symbolic").to_string();
        out.insert(name.clone(), ctx.load_texture(name, img, egui::TextureOptions::LINEAR));
    }
    out
}

fn c32(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

impl App {
    fn icon(&self, name: &str) -> Option<egui::Image<'static>> {
        self.icons.get(name).map(|t| egui::Image::new((t.id(), Vec2::splat(18.0))))
    }

    fn icon_small(&self, ui: &mut egui::Ui, name: &str, tip: &str) -> egui::Response {
        let tint = ui.visuals().text_color();
        match self.icons.get(name) {
            Some(t) => ui.add(egui::Button::image(egui::Image::new((t.id(), Vec2::splat(12.0))).tint(tint)).small()),
            None => ui.small_button(tip),
        }
        .on_hover_text(tip)
    }

    fn icon_button(&self, ui: &mut egui::Ui, name: &str, tip: &str, selected: bool) -> egui::Response {
        let tint = ui.visuals().text_color();
        let r = match self.icon(name) {
            Some(img) => ui.add(egui::Button::image(img.tint(tint)).selected(selected)),
            None => ui.add(egui::Button::new(tip.chars().take(2).collect::<String>()).selected(selected)),
        };
        r.on_hover_text(tip)
    }

    // ---- shortcuts ----------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        // exact modifier match (egui's consume_shortcut lets Ctrl+N fire on Ctrl+Shift+N)
        let sc = |m: Modifiers, k: Key| {
            ctx.input_mut(|i| {
                let pos = i.events.iter().position(|e| matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == k && modifiers.matches_exact(m)));
                match pos {
                    Some(p) => {
                        i.events.remove(p);
                        true
                    }
                    None => false,
                }
            })
        };
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let ca = Modifiers::COMMAND | Modifiers::ALT;
        if sc(Modifiers::COMMAND, Key::N) {
            self.dialog = Dialog::NewImage { w: self.prefs.w, h: self.prefs.h, bg: self.prefs.bg };
        }
        if sc(Modifiers::COMMAND, Key::O) {
            self.open_dialog();
        }
        if sc(cs, Key::S) {
            self.save(true);
        } else if sc(Modifiers::COMMAND, Key::S) {
            self.save(false);
        }
        if sc(Modifiers::COMMAND, Key::W) {
            let a = self.active;
            self.close_doc(a, false);
        }
        if typing || self.busy() {
            return;
        }
        // Line/Curve editing: Enter confirms, Delete removes the selected point
        if self.tools.line_editing() {
            let tools = &mut self.tools;
            if let Some(d) = self.docs.get_mut(self.active) {
                if sc(Modifiers::NONE, Key::Enter) {
                    tools.commit_line(d);
                }
                if sc(Modifiers::NONE, Key::Delete) || sc(Modifiers::NONE, Key::Backspace) {
                    tools.delete_line_point(d);
                }
            }
        }
        // egui-winit turns Ctrl+X/C/V into Cut/Copy/Paste events instead of
        // key presses, so handle those (and swallow them) here.
        let clip: Vec<u8> = ctx.input_mut(|i| {
            let mut v = Vec::new();
            i.events.retain(|e| match e {
                egui::Event::Copy => {
                    v.push(0);
                    false
                }
                egui::Event::Cut => {
                    v.push(1);
                    false
                }
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => {
                    v.push(2);
                    false
                }
                _ => true,
            });
            v
        });
        for c in clip {
            match c {
                0 => self.copy(false, false),
                1 => self.copy(false, true),
                _ => self.paste(0),
            }
        }
        if sc(cs, Key::Z) || sc(Modifiers::COMMAND, Key::Y) {
            if let Some(d) = self.doc() {
                d.redo();
            }
        } else if sc(Modifiers::COMMAND, Key::Z) {
            let tools = &mut self.tools;
            if let Some(d) = self.docs.get_mut(self.active) {
                if tools.is_busy() { tools.cancel(d) } else { d.undo() }
            }
        }
        if sc(cs, Key::V) {
            self.paste(1);
        } else if sc(ca, Key::V) {
            self.paste(2);
        } else if sc(Modifiers::COMMAND, Key::V) {
            self.paste(0);
        }
        if sc(cs, Key::C) {
            self.copy(true, false);
        } else if sc(Modifiers::COMMAND, Key::C) {
            self.copy(false, false);
        }
        if sc(Modifiers::COMMAND, Key::X) {
            self.copy(false, true);
        }
        if sc(cs, Key::A) {
            if let Some(d) = self.doc() {
                ops::deselect(d);
            }
        } else if sc(Modifiers::COMMAND, Key::A) {
            if let Some(d) = self.doc() {
                ops::select_all(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::I) {
            if let Some(d) = self.doc() {
                ops::invert_selection(d);
            }
        }
        if sc(cs, Key::I) {
            if let Some(i) = self.effects.iter().position(|e| e.name() == "Invert Colors") {
                self.start_effect(i);
            }
        }
        if sc(Modifiers::NONE, Key::Delete) {
            if let Some(d) = self.doc() {
                ops::erase_selection(d);
            }
        }
        if sc(Modifiers::NONE, Key::Backspace) {
            let c = self.palette.primary;
            if let Some(d) = self.doc() {
                ops::fill_selection(d, c);
            }
        }
        if sc(cs, Key::X) {
            if let Some(d) = self.doc() {
                ops::crop_to_selection(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::R) {
            self.open_resize();
        }
        if sc(cs, Key::R) {
            self.open_canvas_size();
        }
        if sc(cs, Key::F) {
            if let Some(d) = self.doc() {
                ops::flatten_image(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::H) {
            if let Some(d) = self.doc() {
                ops::rotate_cw(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::G) {
            if let Some(d) = self.doc() {
                ops::rotate_ccw(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::J) {
            if let Some(d) = self.doc() {
                ops::rotate_180(d);
            }
        }
        if sc(cs, Key::N) {
            if let Some(d) = self.doc() {
                ops::add_layer(d);
            }
        }
        if sc(cs, Key::D) {
            if let Some(d) = self.doc() {
                ops::duplicate_layer(d);
            }
        }
        if sc(Modifiers::COMMAND, Key::M) {
            if let Some(d) = self.doc() {
                ops::merge_down(d);
            }
        }
        if sc(cs, Key::Delete) {
            if let Some(d) = self.doc() {
                ops::delete_layer(d);
            }
        }
        if sc(Modifiers::NONE, Key::F4) {
            self.open_layer_props();
        }
        if sc(Modifiers::COMMAND, Key::F) {
            if let Some(i) = self.last_effect {
                self.start_effect(i);
            }
        }
        if sc(Modifiers::COMMAND, Key::Plus) || sc(Modifiers::COMMAND, Key::Equals) {
            if let Some(d) = self.doc() {
                d.view.zoom = Self::zoom_in(d.view.zoom);
            }
        }
        if sc(Modifiers::COMMAND, Key::Minus) {
            if let Some(d) = self.doc() {
                d.view.zoom = Self::zoom_out(d.view.zoom);
            }
        }
        if sc(Modifiers::COMMAND, Key::B) {
            if let Some(d) = self.doc() {
                d.view.fit_pending = true;
            }
        }
        if sc(Modifiers::COMMAND, Key::Num0) {
            if let Some(d) = self.doc() {
                d.view.zoom = 1.0;
            }
        }
        if sc(Modifiers::NONE, Key::Escape) {
            let tools = &mut self.tools;
            if let Some(d) = self.docs.get_mut(self.active) {
                if tools.is_busy() {
                    tools.cancel(d);
                }
            }
        }
        if sc(Modifiers::NONE, Key::X) {
            std::mem::swap(&mut self.palette.primary, &mut self.palette.secondary);
        }
        // Tool shortcuts cycle through tools sharing a key, like Pinta.
        let keys = [Key::M, Key::Z, Key::H, Key::S, Key::B, Key::P, Key::E, Key::F, Key::G, Key::K, Key::T, Key::O, Key::L, Key::R];
        for k in keys {
            if sc(Modifiers::NONE, k) {
                let ch = k.name().chars().next().unwrap();
                let group: Vec<ToolKind> = ToolKind::ALL.iter().copied().filter(|t| t.shortcut() == Some(ch)).collect();
                if group.is_empty() {
                    continue;
                }
                let next = match group.iter().position(|t| *t == self.tools.current) {
                    Some(i) => group[(i + 1) % group.len()],
                    None => group[0],
                };
                let tools = &mut self.tools;
                tools.set_tool(self.docs.get_mut(self.active), next);
            }
        }
        // Brush width: [ and ]
        if ctx.input(|i| i.key_pressed(Key::OpenBracket)) {
            self.tools.settings.brush_width = (self.tools.settings.brush_width - 1.0).max(1.0);
        }
        if ctx.input(|i| i.key_pressed(Key::CloseBracket)) {
            self.tools.settings.brush_width = (self.tools.settings.brush_width + 1.0).min(1000.0);
        }
    }

    /// Route keyboard input to the text tool while it is editing. Returns
    /// true when text editing consumed the keyboard.
    fn text_input(&mut self, ctx: &egui::Context) -> bool {
        use crate::tools::TextKey;
        if !self.tools.text_editing() || ctx.egui_wants_keyboard_input() {
            return false;
        }
        let events = ctx.input(|i| i.events.clone());
        let mut keys = Vec::new();
        for e in events {
            match e {
                egui::Event::Text(t) | egui::Event::Paste(t) => keys.push(TextKey::Insert(t)),
                egui::Event::Key { key, pressed: true, modifiers, .. } => match key {
                    Key::Backspace => keys.push(TextKey::Backspace),
                    Key::Delete => keys.push(TextKey::Delete),
                    Key::Enter if modifiers.command => keys.push(TextKey::Commit),
                    Key::V if modifiers.command_only() => {
                        if let Some(t) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                            keys.push(TextKey::Insert(t.replace("\r\n", "\n")));
                        }
                    }
                    Key::Enter => keys.push(TextKey::Insert("\n".into())),
                    Key::ArrowLeft => keys.push(TextKey::Left),
                    Key::ArrowRight => keys.push(TextKey::Right),
                    Key::ArrowUp => keys.push(TextKey::Up),
                    Key::ArrowDown => keys.push(TextKey::Down),
                    Key::Home => keys.push(TextKey::Home),
                    Key::End => keys.push(TextKey::End),
                    Key::Escape => keys.push(TextKey::Commit),
                    _ => {}
                },
                _ => {}
            }
        }
        // swallow everything so tool letters and Delete/Backspace shortcuts don't fire
        ctx.input_mut(|i| {
            i.events.retain(|e| !matches!(e, egui::Event::Text(_) | egui::Event::Key { .. } | egui::Event::Paste(_) | egui::Event::PasteImage(_) | egui::Event::Copy | egui::Event::Cut));
        });
        let tools = &mut self.tools;
        let pal = &self.palette;
        if let Some(d) = self.docs.get_mut(self.active) {
            for k in keys {
                tools.text_key(d, pal, k);
            }
        }
        true
    }

    fn open_resize(&mut self) {
        if let Some(d) = self.docs.get(self.active) {
            self.dialog = Dialog::ResizeImage { pct: true, percent: 100.0, w: d.state.width, h: d.state.height, keep: true, mode: ops::Resample::Supersampling };
        }
    }
    fn open_canvas_size(&mut self) {
        if let Some(d) = self.docs.get(self.active) {
            self.dialog = Dialog::CanvasSize { pct: false, percent: 100.0, w: d.state.width, h: d.state.height, keep: false, anchor: 4 };
        }
    }
    fn open_layer_props(&mut self) {
        if let Some(d) = self.docs.get(self.active) {
            let l = d.state.layer();
            self.dialog = Dialog::LayerProps { name: l.name.clone(), hidden: l.hidden, opacity: l.opacity, blend: l.blend, orig: (l.name.clone(), l.hidden, l.opacity, l.blend) };
        }
    }

    // ---- menus --------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New…").shortcut_text("Ctrl+N")).clicked() {
                    self.dialog = Dialog::NewImage { w: self.prefs.w, h: self.prefs.h, bg: self.prefs.bg };
                }
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    self.open_dialog();
                }
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.save(false);
                }
                if ui.add(egui::Button::new("Save As…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    self.save(true);
                }
                ui.separator();
                if ui.add(egui::Button::new("Close").shortcut_text("Ctrl+W")).clicked() {
                    let a = self.active;
                    self.close_doc(a, false);
                }
                if ui.add(egui::Button::new("Quit").shortcut_text("Ctrl+Q")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let (can_undo, can_redo) = self.docs.get(self.active).map_or((false, false), |d| (d.can_undo(), d.can_redo()));
                if ui.add_enabled(can_undo, egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                    if let Some(d) = self.doc() {
                        d.undo();
                    }
                }
                if ui.add_enabled(can_redo, egui::Button::new("Redo").shortcut_text("Ctrl+Y")).clicked() {
                    if let Some(d) = self.doc() {
                        d.redo();
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
                    self.copy(false, true);
                }
                if ui.add(egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
                    self.copy(false, false);
                }
                if ui.add(egui::Button::new("Copy Merged").shortcut_text("Ctrl+Shift+C")).clicked() {
                    self.copy(true, false);
                }
                if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                    self.paste(0);
                }
                if ui.add(egui::Button::new("Paste Into New Layer").shortcut_text("Ctrl+Shift+V")).clicked() {
                    self.paste(1);
                }
                if ui.add(egui::Button::new("Paste Into New Image").shortcut_text("Ctrl+Alt+V")).clicked() {
                    self.paste(2);
                }
                ui.separator();
                if ui.add(egui::Button::new("Select All").shortcut_text("Ctrl+A")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::select_all(d);
                    }
                }
                if ui.add(egui::Button::new("Deselect All").shortcut_text("Ctrl+Shift+A")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::deselect(d);
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new("Erase Selection").shortcut_text("Delete")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::erase_selection(d);
                    }
                }
                if ui.add(egui::Button::new("Fill Selection").shortcut_text("Backspace")).clicked() {
                    let c = self.palette.primary;
                    if let Some(d) = self.doc() {
                        ops::fill_selection(d, c);
                    }
                }
                if ui.add(egui::Button::new("Invert Selection").shortcut_text("Ctrl+I")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::invert_selection(d);
                    }
                }
                ui.separator();
                if ui.button("Preferences...").clicked() {
                    let p = self.prefs;
                    self.dialog = Dialog::Preferences { w: p.w, h: p.h, bg: p.bg };
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add(egui::Button::new("Zoom In").shortcut_text("Ctrl++")).clicked() {
                    if let Some(d) = self.doc() {
                        d.view.zoom = Self::zoom_in(d.view.zoom);
                    }
                }
                if ui.add(egui::Button::new("Zoom Out").shortcut_text("Ctrl+-")).clicked() {
                    if let Some(d) = self.doc() {
                        d.view.zoom = Self::zoom_out(d.view.zoom);
                    }
                }
                if ui.add(egui::Button::new("Best Fit").shortcut_text("Ctrl+B")).clicked() {
                    if let Some(d) = self.doc() {
                        d.view.fit_pending = true;
                    }
                }
                if ui.add(egui::Button::new("Actual Size").shortcut_text("Ctrl+0")).clicked() {
                    if let Some(d) = self.doc() {
                        d.view.zoom = 1.0;
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.show_grid, "Pixel Grid");
                ui.separator();
                ui.label("Skin");
                ui.radio_value(&mut self.skin, Skin::Pinta, "Pinta");
                ui.radio_value(&mut self.skin, Skin::Classic, "Classic (Paint.NET-style)");
            });
            ui.menu_button("Image", |ui| {
                ui.menu_button("Center Object", |ui| {
                    for (label, hz, vt) in [("Center Both", true, true), ("Center Horizontally", true, false), ("Center Vertically", false, true)] {
                        if ui.button(label).on_hover_text("Centers the selected pixels (or the whole layer) on the canvas").clicked() {
                            if let Some(d) = self.doc() {
                                ops::center_object(d, hz, vt);
                            }
                        }
                    }
                });
                ui.separator();
                if ui.add(egui::Button::new("Crop to Selection").shortcut_text("Ctrl+Shift+X")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::crop_to_selection(d);
                    }
                }
                if ui.button("Auto Crop").clicked() {
                    if let Some(d) = self.doc() {
                        ops::auto_crop(d);
                    }
                }
                if ui.add(egui::Button::new("Resize Image…").shortcut_text("Ctrl+R")).clicked() {
                    self.open_resize();
                }
                if ui.add(egui::Button::new("Resize Canvas…").shortcut_text("Ctrl+Shift+R")).clicked() {
                    self.open_canvas_size();
                }
                ui.separator();
                if ui.button("Flip Horizontal").clicked() {
                    if let Some(d) = self.doc() {
                        ops::flip_image_h(d);
                    }
                }
                if ui.button("Flip Vertical").clicked() {
                    if let Some(d) = self.doc() {
                        ops::flip_image_v(d);
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new("Rotate 90° Clockwise").shortcut_text("Ctrl+H")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::rotate_cw(d);
                    }
                }
                if ui.add(egui::Button::new("Rotate 90° Counter-Clockwise").shortcut_text("Ctrl+G")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::rotate_ccw(d);
                    }
                }
                if ui.add(egui::Button::new("Rotate 180°").shortcut_text("Ctrl+J")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::rotate_180(d);
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new("Flatten").shortcut_text("Ctrl+Shift+F")).clicked() {
                    if let Some(d) = self.doc() {
                        ops::flatten_image(d);
                    }
                }
            });
            ui.menu_button("Layers", |ui| self.layer_menu(ui));
            ui.menu_button("Adjustments", |ui| {
                let items: Vec<(usize, &'static str, &'static str)> = self.effects.iter().enumerate().filter(|(_, e)| e.category() == Category::Adjustment).map(|(i, e)| (i, e.name(), e.icon())).collect();
                for (i, name, icon) in items {
                    let label = if self.effects[i].has_dialog() { format!("{name}…") } else { name.to_string() };
                    let tint = ui.visuals().text_color();
                    let btn = match self.icon(icon.trim_end_matches("-symbolic")) {
                        Some(img) => egui::Button::image_and_text(img.tint(tint), label),
                        None => egui::Button::new(label),
                    };
                    let btn = if name == "Invert Colors" { btn.shortcut_text("Ctrl+Shift+I") } else { btn };
                    if ui.add_enabled(!self.busy(), btn).clicked() {
                        self.start_effect(i);
                    }
                }
            });
            ui.menu_button("Effects", |ui| {
                if let Some(i) = self.last_effect {
                    if ui.add(egui::Button::new(format!("Repeat {}", self.effects[i].name())).shortcut_text("Ctrl+F")).clicked() {
                        self.start_effect(i);
                    }
                    ui.separator();
                }
                for cat in Category::EFFECT_MENUS {
                    let items: Vec<(usize, &'static str, &'static str)> = self.effects.iter().enumerate().filter(|(_, e)| e.category() == cat).map(|(i, e)| (i, e.name(), e.icon())).collect();
                    if items.is_empty() {
                        continue;
                    }
                    ui.menu_button(cat.name(), |ui| {
                        for (i, name, icon) in items {
                            let label = if self.effects[i].has_dialog() { format!("{name}…") } else { name.to_string() };
                            let tint = ui.visuals().text_color();
                            let btn = match self.icon(icon.trim_end_matches("-symbolic")) {
                                Some(img) => egui::Button::image_and_text(img.tint(tint), label),
                                None => egui::Button::new(label),
                            };
                            if ui.add_enabled(!self.busy(), btn).clicked() {
                                self.start_effect(i);
                            }
                        }
                    });
                }
            });
            if self.skin == Skin::Classic {
                ui.menu_button("Window", |ui| {
                    ui.checkbox(&mut self.win_tools, "Tools  (F5)");
                    ui.checkbox(&mut self.win_history, "History  (F6)");
                    ui.checkbox(&mut self.win_layers, "Layers  (F7)");
                    ui.checkbox(&mut self.win_colors, "Colors  (F8)");
                    if ui.button("Reset Window Positions").clicked() {
                        ui.ctx().memory_mut(|m| m.reset_areas());
                    }
                });
            }
            ui.menu_button("Help", |ui| {
                if ui.button("About").clicked() {
                    self.dialog = Dialog::About;
                }
            });
        });
    }

    fn layer_menu(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if ui.add(egui::Button::new("Add New Layer").shortcut_text("Ctrl+Shift+N")).clicked() {
            ops::add_layer(d);
        }
        if ui.add_enabled(d.state.layers.len() > 1, egui::Button::new("Delete Layer").shortcut_text("Ctrl+Shift+Del")).clicked() {
            ops::delete_layer(d);
        }
        if ui.add(egui::Button::new("Duplicate Layer").shortcut_text("Ctrl+Shift+D")).clicked() {
            ops::duplicate_layer(d);
        }
        if ui.add_enabled(d.state.current > 0, egui::Button::new("Merge Layer Down").shortcut_text("Ctrl+M")).clicked() {
            ops::merge_down(d);
        }
        if ui.button("Import from File…").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Images", crate::io::OPEN_EXTENSIONS).pick_file() {
                match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| crate::io::decode_image_bytes(&b)) {
                    Ok((w, h, mut data)) => {
                        for px in data.chunks_exact_mut(4) {
                            let c = crate::surface::premul([px[0], px[1], px[2], px[3]]);
                            px.copy_from_slice(&c);
                        }
                        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        ops::import_layer(d, &name, w, h, &data, (0, 0));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        ui.separator();
        if ui.button("Flip Horizontal").clicked() {
            ops::flip_layer(d, true);
        }
        if ui.button("Flip Vertical").clicked() {
            ops::flip_layer(d, false);
        }
        ui.separator();
        if ui.add_enabled(d.state.current + 1 < d.state.layers.len(), egui::Button::new("Move Layer Up")).clicked() {
            ops::move_layer(d, true);
        }
        if ui.add_enabled(d.state.current > 0, egui::Button::new("Move Layer Down")).clicked() {
            ops::move_layer(d, false);
        }
        ui.separator();
        if ui.add(egui::Button::new("Layer Properties…").shortcut_text("F4")).clicked() {
            self.open_layer_props();
        }
    }

    // ---- tool box & tool bar -------------------------------------------------

    fn toolbox(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("tools").spacing([2.0, 2.0]).show(ui, |ui| {
            for (i, t) in ToolKind::ALL.iter().enumerate() {
                let tip = match t.shortcut() {
                    Some(k) => format!("{} ({k})", t.name()),
                    None => t.name().to_string(),
                };
                if self.icon_button(ui, t.icon(), &tip, self.tools.current == *t).clicked() {
                    let tools = &mut self.tools;
                    tools.set_tool(self.docs.get_mut(self.active), *t);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let t = self.tools.current;
            let tint = ui.visuals().text_color();
            if let Some(img) = self.icon(t.icon()) {
                ui.add(img.tint(tint));
            }
            ui.strong(t.name());
            ui.separator();
            let s = &mut self.tools.settings;
            if t.uses_brush_width() {
                ui.label("Brush width:");
                ui.add(egui::DragValue::new(&mut s.brush_width).range(1.0..=1000.0).speed(0.25));
            }
            if matches!(t, ToolKind::Paintbrush | ToolKind::Eraser | ToolKind::CloneStamp) {
                ui.label("Hardness:");
                ui.add(egui::Slider::new(&mut s.hardness, 0.0..=100.0).suffix("%").integer());
            }
            if matches!(t, ToolKind::Paintbrush | ToolKind::Eraser | ToolKind::LineCurve | ToolKind::Rectangle | ToolKind::RoundedRectangle | ToolKind::Ellipse | ToolKind::FreeformShape | ToolKind::CloneStamp) {
                ui.checkbox(&mut s.antialias, "Antialiasing");
            }
            if matches!(t, ToolKind::Paintbrush | ToolKind::PaintBucket | ToolKind::Gradient | ToolKind::LineCurve | ToolKind::Rectangle | ToolKind::RoundedRectangle | ToolKind::Ellipse | ToolKind::FreeformShape) {
                egui::ComboBox::from_id_salt("blend").selected_text(if s.alpha_blending { "Normal Blending" } else { "Overwrite" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.alpha_blending, true, "Normal Blending");
                    ui.selectable_value(&mut s.alpha_blending, false, "Overwrite");
                });
            }
            if t.is_shape() {
                ui.label("Fill style:");
                egui::ComboBox::from_id_salt("fill").selected_text(match s.fill_style {
                    FillStyle::Outline => "Outline Shape",
                    FillStyle::Fill => "Fill Shape",
                    FillStyle::OutlineFill => "Fill and Outline Shape",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.fill_style, FillStyle::Outline, "Outline Shape");
                    ui.selectable_value(&mut s.fill_style, FillStyle::Fill, "Fill Shape");
                    ui.selectable_value(&mut s.fill_style, FillStyle::OutlineFill, "Fill and Outline Shape");
                });
            }
            if t == ToolKind::RoundedRectangle {
                ui.label("Radius:");
                ui.add(egui::DragValue::new(&mut s.radius).range(0.0..=1000.0));
            }
            if t == ToolKind::LineCurve {
                ui.checkbox(&mut s.arrow, "Arrow");
            }
            if t.is_select() {
                ui.label("Selection mode:");
                egui::ComboBox::from_id_salt("selmode").selected_text(s.sel_mode.name()).show_ui(ui, |ui| {
                    for m in CombineMode::ALL {
                        ui.selectable_value(&mut s.sel_mode, m, m.name());
                    }
                });
            }
            if matches!(t, ToolKind::MagicWand | ToolKind::PaintBucket) {
                ui.label("Flood mode:");
                egui::ComboBox::from_id_salt("flood").selected_text(if s.flood_global { "Global" } else { "Contiguous" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.flood_global, false, "Contiguous");
                    ui.selectable_value(&mut s.flood_global, true, "Global");
                });
            }
            if matches!(t, ToolKind::MagicWand | ToolKind::PaintBucket | ToolKind::Recolor) {
                ui.label("Tolerance:");
                ui.add(egui::Slider::new(&mut s.tolerance, 0.0..=100.0).suffix("%"));
            }
            if t == ToolKind::MagicWand {
                ui.label("Sampling:");
                egui::ComboBox::from_id_salt("wandfrom").selected_text(if s.wand_from == SampleFrom::Layer { "Layer" } else { "Image" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.wand_from, SampleFrom::Layer, "Layer");
                    ui.selectable_value(&mut s.wand_from, SampleFrom::Image, "Image");
                });
            }
            if t == ToolKind::Gradient {
                for g in GradientType::ALL {
                    let sel = s.gradient == g;
                    let r = match self.icons.get(g.icon()) {
                        Some(tex) => ui.add(egui::Button::image(egui::Image::new((tex.id(), Vec2::splat(18.0))).tint(tint)).selected(sel)),
                        None => ui.add(egui::Button::new(g.name()).selected(sel)),
                    };
                    if r.on_hover_text(g.name()).clicked() {
                        s.gradient = g;
                    }
                }
                ui.separator();
                ui.selectable_value(&mut s.gradient_transparency, false, "Color Mode");
                ui.selectable_value(&mut s.gradient_transparency, true, "Transparency Mode");
            }
            if t == ToolKind::ColorPicker {
                ui.label("Sampling:");
                egui::ComboBox::from_id_salt("pk").selected_text(format!("{0} x {0}", s.picker_size)).show_ui(ui, |ui| {
                    for n in [1, 3, 5, 7, 9] {
                        ui.selectable_value(&mut s.picker_size, n, format!("{n} x {n} region"));
                    }
                });
                egui::ComboBox::from_id_salt("pkfrom").selected_text(if s.picker_from == SampleFrom::Layer { "Layer" } else { "Image" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.picker_from, SampleFrom::Layer, "Layer");
                    ui.selectable_value(&mut s.picker_from, SampleFrom::Image, "Image");
                });
                ui.checkbox(&mut s.picker_switch_back, "Switch to previous tool after");
            }
            if t == ToolKind::CloneStamp {
                ui.label("Ctrl+click to set the source");
            }
            if t == ToolKind::Text {
                let tp = &mut self.tools.text_params;
                let mut ch = false;
                let cat = crate::text::catalog();
                egui::ComboBox::from_id_salt("font").width(160.0).selected_text(&tp.family).height(400.0).show_ui(ui, |ui| {
                    for f in &cat.families {
                        ch |= ui.selectable_value(&mut tp.family, f.clone(), f).changed();
                    }
                });
                ch |= ui.add(egui::DragValue::new(&mut tp.size).range(1.0..=2000.0).speed(0.5).suffix(" px")).changed();
                ch |= ui.toggle_value(&mut tp.bold, egui::RichText::new("B").strong()).on_hover_text("Bold").changed();
                ch |= ui.toggle_value(&mut tp.italic, egui::RichText::new("I").italics()).on_hover_text("Italic").changed();
                ch |= ui.toggle_value(&mut tp.underline, egui::RichText::new("U").underline()).on_hover_text("Underline").changed();
                ui.separator();
                for (a, n) in [(crate::text::Align::Left, "Left"), (crate::text::Align::Center, "Center"), (crate::text::Align::Right, "Right")] {
                    ch |= ui.selectable_value(&mut tp.align, a, n).changed();
                }
                ui.separator();
                egui::ComboBox::from_id_salt("tstyle").selected_text(tp.style.name()).show_ui(ui, |ui| {
                    for st in crate::text::TextStyle::ALL {
                        ch |= ui.selectable_value(&mut tp.style, st, st.name()).changed();
                    }
                });
                if matches!(tp.style, crate::text::TextStyle::Outline | crate::text::TextStyle::NormalOutline) {
                    ui.label("Outline width:");
                    ch |= ui.add(egui::DragValue::new(&mut tp.outline_width).range(0.5..=100.0).speed(0.1)).changed();
                }
                ch |= ui.checkbox(&mut tp.antialias, "Antialiasing").changed();
                if ch {
                    let pal = &self.palette;
                    if let Some(d) = self.docs.get_mut(self.active) {
                        self.tools.rerender_text(d, pal);
                    }
                }
            }
            if t == ToolKind::MoveSelected {
                ui.label("Drag moves · handles or Shift+drag resize (Alt keeps proportions) · right drag rotates (Shift snaps)");
            }
        });
    }

    // ---- palette & status ------------------------------------------------------

    fn palette_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // primary / secondary swatches
            let (rect, _) = ui.allocate_exact_size(Vec2::new(44.0, 36.0), Sense::hover());
            let sec = Rect::from_min_size(rect.min + Vec2::new(16.0, 12.0), Vec2::splat(24.0));
            let pri = Rect::from_min_size(rect.min + Vec2::new(2.0, 2.0), Vec2::splat(24.0));
            let p = ui.painter();
            checker(p, sec);
            p.rect_filled(sec, 2.0, c32(self.palette.secondary));
            p.rect_stroke(sec, 2.0, (1.0, Color32::GRAY), egui::StrokeKind::Inside);
            checker(p, pri);
            p.rect_filled(pri, 2.0, c32(self.palette.primary));
            p.rect_stroke(pri, 2.0, (1.0, Color32::GRAY), egui::StrokeKind::Inside);
            let rp = ui.interact(pri, ui.id().with("pri"), Sense::click());
            let rs = ui.interact(sec.intersect(Rect::from_min_max(Pos2::new(pri.max.x, sec.min.y), sec.max)).union(Rect::from_min_max(Pos2::new(sec.min.x, pri.max.y), sec.max)), ui.id().with("sec"), Sense::click());
            if rp.on_hover_text("Primary color").clicked() {
                self.palette_edit = Some(true);
            }
            if rs.on_hover_text("Secondary color").clicked() {
                self.palette_edit = Some(false);
            }
            ui.vertical(|ui| {
                if self.icon_small(ui, "edit-swap-vert", "Swap colors (X)").clicked() {
                    std::mem::swap(&mut self.palette.primary, &mut self.palette.secondary);
                }
                if ui.small_button("Reset").on_hover_text("Reset colors to black and white").clicked() {
                    self.palette.primary = [0, 0, 0, 255];
                    self.palette.secondary = [255, 255, 255, 255];
                }
            });
            ui.separator();
            let colors = default_palette();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(1.0);
                for row in colors.chunks(colors.len().div_ceil(2)) {
                    ui.horizontal(|ui| {
                        for c in row {
                            let (r, resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
                            ui.painter().rect_filled(r, 1.0, c32(*c));
                            if resp.clicked() {
                                self.palette.primary = *c;
                            }
                            if resp.secondary_clicked() {
                                self.palette.secondary = *c;
                            }
                        }
                    });
                }
            });
            if !self.palette.recent.is_empty() {
                ui.separator();
                ui.label("Recent:");
                let recent = self.palette.recent.clone();
                for c in recent {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
                    checker(ui.painter(), r);
                    ui.painter().rect_filled(r, 1.0, c32(c));
                    if resp.clicked() {
                        self.palette.primary = c;
                    }
                    if resp.secondary_clicked() {
                        self.palette.secondary = c;
                    }
                }
            }
        });
        if let Some(primary) = self.palette_edit {
            let mut open = true;
            let mut col = c32(if primary { self.palette.primary } else { self.palette.secondary });
            egui::Window::new(if primary { "Primary Color" } else { "Secondary Color" }).open(&mut open).collapsible(false).resizable(false).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                egui::color_picker::color_picker_color32(ui, &mut col, egui::color_picker::Alpha::OnlyBlend);
                let mut hex = format!("{:02X}{:02X}{:02X}{:02X}", col.r(), col.g(), col.b(), col.a());
                ui.horizontal(|ui| {
                    ui.label("Hex:");
                    if ui.text_edit_singleline(&mut hex).changed() {
                        if let Ok(v) = u32::from_str_radix(hex.trim_start_matches('#'), 16) {
                            let v = if hex.trim_start_matches('#').len() <= 6 { (v << 8) | 0xFF } else { v };
                            col = Color32::from_rgba_unmultiplied((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8);
                        }
                    }
                });
            });
            let c = col.to_srgba_unmultiplied();
            if primary {
                self.palette.primary = c;
            } else {
                self.palette.secondary = c;
            }
            if !open {
                self.palette_edit = None;
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(d) = self.docs.get(self.active) {
                if let Some(c) = self.cursor {
                    ui.label(format!("{}, {}", c[0].floor() as i32, c[1].floor() as i32));
                    ui.separator();
                }
                let b = d.state.selection.mask.as_ref().and_then(|m| m.bounds());
                if let Some(b) = b {
                    ui.label(format!("Selection {} x {}", b.w, b.h));
                    ui.separator();
                }
                ui.label(format!("{} x {}", d.state.width, d.state.height));
                ui.separator();
                if !self.tools.status.is_empty() {
                    ui.label(&self.tools.status);
                    ui.separator();
                }
                if let Some(job) = &self.job {
                    ui.add(egui::ProgressBar::new(job.progress.fraction()).desired_width(160.0).text(job.effect.name()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut z = self.docs[self.active].view.zoom * 100.0;
                    let resp = ui.add(egui::DragValue::new(&mut z).range(0.1..=16000.0).suffix("%").speed(1.0).max_decimals(1));
                    if resp.changed() {
                        self.docs[self.active].view.zoom = z / 100.0;
                    }
                    ui.label("Zoom:");
                    ui.separator();
                    ui.weak(format!("{:.1} ms  GPU cache {} tiles", self.frame_ms, self.gpu_slots));
                });
            }
        });
    }

    // ---- side panels -------------------------------------------------------------

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let mut action: Option<(usize, u8)> = None;
        egui::ScrollArea::vertical().id_salt("layers").max_height(ui.available_height() * 0.5).show(ui, |ui| {
            for i in (0..d.state.layers.len()).rev() {
                let l = &d.state.layers[i];
                let key = Arc::as_ptr(&l.surface.tiles) as usize;
                let tex = match self.thumbs.get(&l.id) {
                    Some((k, t)) if *k == key => t.clone(),
                    _ => {
                        let (tw, th, data) = ops::layer_thumbnail(&l.surface, 40);
                        let img = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &data);
                        let t = ui.ctx().load_texture(format!("thumb{}", l.id), img, egui::TextureOptions::NEAREST);
                        self.thumbs.insert(l.id, (key, t.clone()));
                        t
                    }
                };
                let selected = i == d.state.current;
                let frame = egui::Frame::new().inner_margin(2.0).fill(if selected { ui.visuals().selection.bg_fill } else { Color32::TRANSPARENT }).corner_radius(3.0);
                let r = frame
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let mut vis = !l.hidden;
                            if ui.checkbox(&mut vis, "").on_hover_text("Visible").changed() {
                                action = Some((i, 1));
                            }
                            let sz = tex.size_vec2();
                            let s = 40.0 / sz.x.max(sz.y);
                            let (rr, _) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::hover());
                            let ir = Rect::from_center_size(rr.center(), sz * s);
                            checker(ui.painter(), ir);
                            ui.painter().image(tex.id(), ir, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                            ui.label(&l.name);
                            ui.allocate_space(Vec2::new(ui.available_width(), 1.0));
                        })
                    })
                    .response;
                let r = ui.interact(r.rect, ui.id().with(("layer", l.id)), Sense::click());
                if r.clicked() {
                    action = Some((i, 0));
                }
                if r.double_clicked() {
                    action = Some((i, 2));
                }
            }
        });
        ui.horizontal(|ui| {
            let d = &mut self.docs[self.active];
            let tint = ui.visuals().text_color();
            let mk = |ui: &mut egui::Ui, icons: &HashMap<String, egui::TextureHandle>, n: &str, tip: &str| match icons.get(n) {
                Some(t) => ui.add(egui::Button::image(egui::Image::new((t.id(), Vec2::splat(18.0))).tint(tint))).on_hover_text(tip),
                None => ui.button(tip).on_hover_text(tip),
            };
            if mk(ui, &self.icons, "layers-add-layer", "Add New Layer").clicked() {
                ops::add_layer(d);
            }
            if mk(ui, &self.icons, "layers-remove-layer", "Delete Layer").clicked() {
                ops::delete_layer(d);
            }
            if mk(ui, &self.icons, "layers-duplicate-layer", "Duplicate Layer").clicked() {
                ops::duplicate_layer(d);
            }
            if ui.button("Up").on_hover_text("Move Layer Up").clicked() {
                ops::move_layer(d, true);
            }
            if ui.button("Down").on_hover_text("Move Layer Down").clicked() {
                ops::move_layer(d, false);
            }
            if mk(ui, &self.icons, "layers-merge-down", "Merge Layer Down").clicked() {
                ops::merge_down(d);
            }
            if ui.button("Properties").on_hover_text("Layer Properties (F4)").clicked() {
                action = Some((d.state.current, 2));
            }
        });
        let d = &mut self.docs[self.active];
        match action {
            Some((i, 0)) => {
                if !self.tools.is_busy() {
                    d.state.current = i;
                    // selecting a layer is not a history step in Pinta; keep the
                    // history snapshot in sync so undo doesn't jump layers.
                    d.history[d.history_pos].state.current = i;
                }
            }
            Some((i, 1)) => {
                let l = &mut d.state.layers[i];
                l.hidden = !l.hidden;
                let label = if l.hidden { "Hide Layer" } else { "Show Layer" };
                d.commit(label);
            }
            Some((i, 2)) => {
                d.state.current = i;
                d.history[d.history_pos].state.current = i;
                self.open_layer_props();
            }
            _ => {}
        }
    }

    fn history_panel(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let mut goto = None;
        egui::ScrollArea::vertical().id_salt("history").stick_to_bottom(true).show(ui, |ui| {
            for (i, h) in d.history.iter().enumerate() {
                let undone = i > d.history_pos;
                let text = if undone { egui::RichText::new(&h.label).weak() } else { egui::RichText::new(&h.label) };
                if ui.selectable_label(i == d.history_pos, text).clicked() {
                    goto = Some(i);
                }
            }
        });
        if let Some(i) = goto {
            if !self.tools.is_busy() {
                d.goto_history(i);
            }
        }
    }

    // ---- canvas ---------------------------------------------------------------------

    fn canvas(&mut self, ui: &mut egui::Ui) {
        // tabs
        if self.docs.len() > 1 {
            let fill = ui.visuals().panel_fill;
            egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut close = None;
                for (i, d) in self.docs.iter().enumerate() {
                    if ui.selectable_label(i == self.active, d.display_name()).clicked() {
                        self.active = i;
                    }
                    if ui.small_button("×").clicked() {
                        close = Some(i);
                    }
                    ui.separator();
                }
                if let Some(i) = close {
                    self.close_doc(i, false);
                }
            });
            });
            ui.add_space(-ui.spacing().item_spacing.y);
        }
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        // the canvas must never hold keyboard focus, or tool shortcuts and
        // text-tool typing would be swallowed as "egui wants keyboard input"
        if resp.has_focus() {
            resp.surrender_focus();
        }
        self.canvas_rect = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let ppp = ui.ctx().pixels_per_point();
        let Some(doc) = self.docs.get_mut(self.active) else {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Open or create an image", egui::FontId::proportional(18.0), ui.visuals().weak_text_color());
            return;
        };
        if doc.view.fit_pending {
            Self::fit(doc, rect, ppp);
            doc.view.fit_pending = false;
        }
        // ---- input ----
        let (scroll, zoom_delta, mods, hover, pressed_mid, mid_down) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers, i.pointer.hover_pos(), i.pointer.button_pressed(egui::PointerButton::Middle), i.pointer.middle_down()));
        let space = ui.input(|i| i.key_down(Key::Space)) && !ui.ctx().egui_wants_keyboard_input();
        let over_canvas = hover.is_some_and(|h| rect.contains(h)) && resp.contains_pointer();
        let zp = |doc: &Document| doc.view.zoom / ppp;
        if over_canvas {
            if zoom_delta != 1.0 || (mods.command && scroll.y != 0.0) {
                let f = if zoom_delta != 1.0 { zoom_delta } else { (scroll.y / 200.0).exp() };
                let h = hover.unwrap();
                let before = [doc.view.center[0] + (h.x - rect.center().x) / zp(doc), doc.view.center[1] + (h.y - rect.center().y) / zp(doc)];
                doc.view.zoom = (doc.view.zoom * f).clamp(0.002, 160.0);
                let after = [doc.view.center[0] + (h.x - rect.center().x) / zp(doc), doc.view.center[1] + (h.y - rect.center().y) / zp(doc)];
                doc.view.center[0] += before[0] - after[0];
                doc.view.center[1] += before[1] - after[1];
            } else if scroll != Vec2::ZERO {
                let (dx, dy) = if mods.shift && scroll.x == 0.0 { (scroll.y, 0.0) } else { (scroll.x, scroll.y) };
                doc.view.center[0] -= dx / zp(doc);
                doc.view.center[1] -= dy / zp(doc);
            }
        }
        if pressed_mid && over_canvas {
            self.middle_pan = hover;
        }
        if !mid_down {
            self.middle_pan = None;
        }
        if let (Some(last), Some(h)) = (self.middle_pan, hover) {
            doc.view.center[0] -= (h.x - last.x) / zp(doc);
            doc.view.center[1] -= (h.y - last.y) / zp(doc);
            self.middle_pan = Some(h);
        }
        Self::clamp_center(doc);
        let center = doc.view.center;
        let z = zp(doc);
        let rc = rect.center();
        let to_img = move |p: Pos2| [center[0] + (p.x - rc.x) / z, center[1] + (p.y - rc.y) / z];
        self.cursor = hover.filter(|h| rect.contains(*h)).map(to_img);
        self.tools.pick_radius = 7.0 / z;
        self.tools.view_zoom = z;
        self.tools.tick(doc);
        let modal = !matches!(self.dialog, Dialog::None) || self.palette_edit.is_some();
        if self.job.is_none() && !modal {
            let pointer = |p: Pos2, secondary: bool| Pointer { pos: to_img(p), screen: [p.x, p.y], secondary, shift: mods.shift, ctrl: mods.command, alt: mods.alt };
            let mut reqs = Vec::new();
            if space && resp.dragged_by(egui::PointerButton::Primary) {
                let d = resp.drag_delta();
                doc.view.center[0] -= d.x / z;
                doc.view.center[1] -= d.y / z;
            } else {
                if self.drag.is_none() {
                    let sec = resp.drag_started_by(egui::PointerButton::Secondary) || (resp.secondary_clicked() && !resp.dragged());
                    let pri = resp.drag_started_by(egui::PointerButton::Primary) || resp.clicked();
                    if pri || sec {
                        let p = resp.interact_pointer_pos().unwrap_or(rc);
                        let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                        reqs.push(self.tools.press(doc, &mut self.palette, pointer(press, sec)));
                        self.drag = Some(sec);
                        if resp.clicked() || resp.secondary_clicked() {
                            // click without drag: finish immediately
                            reqs.push(self.tools.release(doc, &mut self.palette, pointer(p, sec)));
                            self.drag = None;
                        }
                    }
                }
                if let Some(sec) = self.drag {
                    if let Some(p) = resp.interact_pointer_pos().or(hover) {
                        if resp.dragged() {
                            reqs.push(self.tools.drag(doc, &mut self.palette, pointer(p, sec)));
                        }
                        if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
                            reqs.push(self.tools.release(doc, &mut self.palette, pointer(p, sec)));
                            self.drag = None;
                        }
                    }
                }
            }
            self.tools.hover = self.cursor;
            for r in reqs {
                match r {
                    ViewRequest::None => {}
                    ViewRequest::PanBy(d) => {
                        doc.view.center[0] -= d[0] / z;
                        doc.view.center[1] -= d[1] / z;
                    }
                    ViewRequest::ZoomAt { factor, at } => {
                        doc.view.zoom = if factor > 1.0 { Self::zoom_in(doc.view.zoom) } else { Self::zoom_out(doc.view.zoom) };
                        doc.view.center = at;
                    }
                    ViewRequest::ZoomToRect(r) => {
                        let zz = ((rect.width() * ppp) / r.w as f32).min((rect.height() * ppp) / r.h as f32);
                        doc.view.zoom = zz.clamp(0.002, 160.0);
                        doc.view.center = [r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0];
                    }
                }
            }
        }
        Self::clamp_center(doc);
        // ---- render ----
        let center = doc.view.center;
        let z = doc.view.zoom / ppp;
        let to_screen = move |p: [f32; 2]| Pos2::new(rc.x + (p[0] - center[0]) * z, rc.y + (p[1] - center[1]) * z);
        let to_img = move |p: Pos2| [center[0] + (p.x - rc.x) / z, center[1] + (p.y - rc.y) / z];
        let (w, h) = (doc.state.width, doc.state.height);
        let img_rect = Rect::from_min_max(to_screen([0.0, 0.0]), to_screen([w as f32, h as f32]));
        painter.rect_filled(img_rect.translate(Vec2::new(2.0, 2.0)).expand(1.0), 0.0, Color32::from_black_alpha(90));
        let tl = to_img(rect.min);
        let br = to_img(rect.max);
        let vis = IRect::from_f(tl[0], tl[1], br[0], br[1]).intersect(&IRect::new(0, 0, w as i32, h as i32));
        self.tools.view_rect = Some(vis);
        let pyr = self.pyramids.entry(doc.id).or_insert_with(|| Pyramid::new(w, h));
        pyr.sync(&doc.state);
        let zoom = doc.view.zoom;
        let level = if zoom >= 1.0 { 0 } else { ((1.0 / zoom).log2().floor() as usize).min(pyr.levels.len() - 1) };
        let want = pyr.visible(level, vis);
        pyr.ensure(level, &want);
        let span = (TILE << level) as f32;
        let mut tiles = Vec::with_capacity(want.len());
        for &(tx, ty) in &want {
            let s = pyr.slot(level, tx, ty);
            let x0 = tx as f32 * span;
            let y0 = ty as f32 * span;
            let rw = (w as f32 - x0).min(span);
            let rh = (h as f32 - y0).min(span);
            tiles.push(FrameTile { key: (doc.id, level as u8, tx, ty), version: s.version, data: s.tile.clone(), rect: [x0, y0, rw, rh], uv: [0.0, 0.0, rw / span, rh / span] });
        }
        // marching ants
        let ants = match &doc.state.selection.mask {
            Some(m) => {
                let key = Arc::as_ptr(&m.tiles) as usize;
                let cache = self.ants.get(&doc.id);
                if cache.is_none_or(|c| c.key != key) {
                    self.ants_counter += 1;
                    let segs = Arc::new(m.outline());
                    self.ants.insert(doc.id, AntsCache { key, version: self.ants_counter, segs });
                }
                let c = &self.ants[&doc.id];
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
                Some((c.version, c.segs.clone()))
            }
            None => None,
        };
        let srgb = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("srgb_target"))).unwrap_or(false);
        let origin = to_img(rect.min);
        let flags = (if zoom >= 1.0 { 1 } else { 0 }) | (if self.show_grid && zoom >= 4.0 { 2 } else { 0 }) | (if srgb { 4 } else { 0 });
        let view = ViewUniform {
            origin,
            scale: zoom,
            time: self.start.elapsed().as_secs_f32(),
            viewport: [rect.width() * ppp, rect.height() * ppp],
            flags,
            checker: 8.0 * ppp,
            image: [w as f32, h as f32],
            _pad: [0.0; 2],
        };
        let frame = CanvasFrame { view, tiles, ants };
        painter.add(egui_wgpu::Callback::new_paint_callback(rect, CanvasCallback { frame: Arc::new(frame) }));
        // overlays
        let overlay_painter = ui.painter_at(rect);
        self.tools.overlay(&overlay_painter, &to_screen, z);
        if over_canvas && !self.busy() {
            let icon = match self.tools.current {
                ToolKind::Pan => Some(egui::CursorIcon::Grab),
                ToolKind::Zoom => Some(egui::CursorIcon::ZoomIn),
                ToolKind::MoveSelected | ToolKind::MoveSelection => Some(egui::CursorIcon::Move),
                ToolKind::Text => Some(egui::CursorIcon::Text),
                _ if space => Some(egui::CursorIcon::Grab),
                _ => Some(egui::CursorIcon::Crosshair),
            };
            if let Some(i) = icon {
                ui.ctx().set_cursor_icon(i);
            }
        }
    }

    // ---- dialogs ----------------------------------------------------------------------

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(e) = self.error.clone() {
            egui::Window::new("Error").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                ui.label(e);
                if ui.button("OK").clicked() {
                    self.error = None;
                }
            });
        }
        // effect dialog
        let mut effect_action = 0; // 1 ok, 2 cancel, 3 rerender
        if let Some(job) = self.job.as_mut() {
            if job.dialog {
                egui::Window::new(job.effect.name()).collapsible(false).resizable(false).anchor(egui::Align2::RIGHT_TOP, Vec2::new(-260.0, 80.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    if job.effect.ui(ui) {
                        effect_action = 3;
                    }
                    ui.add_space(6.0);
                    let rendering = job.rendered != Some(job.generation);
                    ui.add(egui::ProgressBar::new(if rendering { job.progress.fraction() } else { 1.0 }).desired_height(6.0));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            effect_action = 2;
                        }
                        if ui.button(if job.apply_when_done { "Applying…" } else { "OK" }).clicked() {
                            effect_action = 1;
                        }
                    });
                });
            }
        }
        match effect_action {
            1 => {
                if let Some(j) = self.job.as_mut() {
                    j.apply_when_done = true;
                }
            }
            2 => self.cancel_effect(),
            3 => self.render_effect(),
            _ => {}
        }

        let mut close = false;
        let mut act: Option<Box<dyn FnOnce(&mut App)>> = None;
        match &mut self.dialog {
            Dialog::None => {}
            Dialog::Preferences { w, h, bg } => {
                egui::Window::new("Preferences").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.strong("Default canvas");
                    ui.weak("Used for the image Patina starts with and as the New Image size.");
                    ui.add_space(4.0);
                    egui::Grid::new("prefs").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Width:");
                        ui.add(egui::DragValue::new(w).range(1..=100_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Height:");
                        ui.add(egui::DragValue::new(h).range(1..=100_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Background:");
                        ui.horizontal(|ui| {
                            ui.selectable_value(bg, 0, "White");
                            ui.selectable_value(bg, 1, "Secondary Color");
                            ui.selectable_value(bg, 2, "Transparent");
                        });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        for (n, a, b) in [("800x600", 800, 600), ("1920x1080", 1920, 1080), ("2560x1440", 2560, 1440), ("3840x2160", 3840, 2160)] {
                            if ui.small_button(n).clicked() {
                                *w = a;
                                *h = b;
                            }
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.add(egui::Button::new("Save").fill(ui.visuals().selection.bg_fill)).clicked() {
                            let p = Prefs { w: *w, h: *h, bg: *bg };
                            act = Some(Box::new(move |app: &mut App| {
                                app.prefs = p;
                                p.save();
                            }));
                            close = true;
                        }
                    });
                });
            }
            Dialog::NewImage { w, h, bg } => {
                egui::Window::new("New Image").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    egui::Grid::new("ni").num_columns(2).show(ui, |ui| {
                        ui.label("Width:");
                        ui.add(egui::DragValue::new(w).range(1..=100_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Height:");
                        ui.add(egui::DragValue::new(h).range(1..=100_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Background:");
                        ui.horizontal(|ui| {
                            ui.selectable_value(bg, 0, "White");
                            ui.selectable_value(bg, 1, "Secondary Color");
                            ui.selectable_value(bg, 2, "Transparent");
                        });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        for (n, a, b) in [("640x480", 640, 480), ("1920x1080", 1920, 1080), ("3840x2160", 3840, 2160), ("16K", 16384, 16384)] {
                            if ui.small_button(n).clicked() {
                                *w = a;
                                *h = b;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("OK").clicked() {
                            let (w, h, bg) = (*w, *h, *bg);
                            act = Some(Box::new(move |app: &mut App| {
                                let c = app.bg_color(bg);
                                let n = app.docs.len() + 1;
                                let d = Document::new(format!("Unsaved Image {n}"), w, h, c);
                                app.docs.push(d);
                                app.active = app.docs.len() - 1;
                            }));
                            close = true;
                        }
                    });
                });
            }
            Dialog::ResizeImage { pct, percent, w, h, keep, mode } => {
                let (ow, oh) = self.docs.get(self.active).map_or((1, 1), |d| (d.state.width, d.state.height));
                egui::Window::new("Resize Image").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.radio_value(pct, true, "By percentage:");
                    ui.add_enabled(*pct, egui::DragValue::new(percent).range(1.0..=1000.0).suffix("%"));
                    ui.radio_value(pct, false, "By absolute size:");
                    ui.add_enabled_ui(!*pct, |ui| {
                        ui.checkbox(keep, "Maintain aspect ratio");
                        egui::Grid::new("rs").show(ui, |ui| {
                            ui.label("Width:");
                            if ui.add(egui::DragValue::new(w).range(1..=100_000)).changed() && *keep {
                                *h = ((*w as f64 * oh as f64 / ow as f64).round() as u32).max(1);
                            }
                            ui.end_row();
                            ui.label("Height:");
                            if ui.add(egui::DragValue::new(h).range(1..=100_000)).changed() && *keep {
                                *w = ((*h as f64 * ow as f64 / oh as f64).round() as u32).max(1);
                            }
                            ui.end_row();
                        });
                    });
                    egui::ComboBox::from_label("Resampling").selected_text(format!("{mode:?}")).show_ui(ui, |ui| {
                        ui.selectable_value(mode, ops::Resample::Supersampling, "Supersampling");
                        ui.selectable_value(mode, ops::Resample::Bilinear, "Bilinear");
                        ui.selectable_value(mode, ops::Resample::NearestNeighbor, "NearestNeighbor");
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("OK").clicked() {
                            let (nw, nh) = if *pct { (((ow as f32 * *percent / 100.0).round() as u32).max(1), ((oh as f32 * *percent / 100.0).round() as u32).max(1)) } else { (*w, *h) };
                            let m = *mode;
                            act = Some(Box::new(move |app: &mut App| {
                                if let Some(d) = app.doc() {
                                    ops::resize_image(d, nw, nh, m);
                                }
                            }));
                            close = true;
                        }
                    });
                });
            }
            Dialog::CanvasSize { pct, percent, w, h, keep, anchor } => {
                let (ow, oh) = self.docs.get(self.active).map_or((1, 1), |d| (d.state.width, d.state.height));
                let icons = &self.icons;
                egui::Window::new("Resize Canvas").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    let (nw, nh) = if *pct { (((ow as f32 * *percent / 100.0).round() as u32).max(1), ((oh as f32 * *percent / 100.0).round() as u32).max(1)) } else { (*w, *h) };
                    ui.horizontal(|ui| {
                        // left column: size
                        ui.vertical(|ui| {
                            ui.strong("Size");
                            ui.horizontal(|ui| {
                                ui.radio_value(pct, true, "By percentage");
                                ui.add_enabled(*pct, egui::DragValue::new(percent).range(1.0..=1000.0).suffix(" %").speed(0.5));
                            });
                            ui.radio_value(pct, false, "By absolute size");
                            ui.add_enabled_ui(!*pct, |ui| {
                                ui.indent("abs", |ui| {
                                    egui::Grid::new("cs").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                        ui.label("Width");
                                        if ui.add(egui::DragValue::new(w).range(1..=100_000).suffix(" px")).changed() && *keep {
                                            *h = ((*w as f64 * oh as f64 / ow as f64).round() as u32).max(1);
                                        }
                                        ui.end_row();
                                        ui.label("Height");
                                        if ui.add(egui::DragValue::new(h).range(1..=100_000).suffix(" px")).changed() && *keep {
                                            *w = ((*h as f64 * ow as f64 / oh as f64).round() as u32).max(1);
                                        }
                                        ui.end_row();
                                    });
                                    ui.checkbox(keep, "Maintain aspect ratio");
                                });
                            });
                            ui.add_space(4.0);
                            ui.weak(format!("Current: {ow} x {oh} px"));
                            ui.weak(format!("New: {nw} x {nh} px"));
                        });
                        ui.separator();
                        // right column: anchor grid + preview
                        ui.vertical(|ui| {
                            ui.strong("Anchor");
                            let tint = ui.visuals().text_color();
                            egui::Grid::new("anchor").spacing([3.0, 3.0]).show(ui, |ui| {
                                for i in 0..9u8 {
                                    let name = anchor_icon(i, *anchor);
                                    let btn = match name.and_then(|n| icons.get(n)) {
                                        Some(t) => egui::Button::image(egui::Image::new((t.id(), Vec2::splat(20.0))).tint(tint)),
                                        None => egui::Button::new(""),
                                    };
                                    if ui.add(btn.selected(*anchor == i).min_size(Vec2::splat(38.0))).clicked() {
                                        *anchor = i;
                                    }
                                    if i % 3 == 2 {
                                        ui.end_row();
                                    }
                                }
                            });
                            // preview: old image placed inside the new canvas
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(120.0, 80.0), Sense::hover());
                            let mw = nw.max(ow) as f32;
                            let mh = nh.max(oh) as f32;
                            let sc = (rect.width() / mw).min(rect.height() / mh);
                            let c0 = rect.center() - Vec2::new(mw * sc, mh * sc) / 2.0;
                            let canvas = Rect::from_min_size(c0 + Vec2::new((mw - nw as f32) / 2.0 * sc, (mh - nh as f32) / 2.0 * sc), Vec2::new(nw as f32 * sc, nh as f32 * sc));
                            let (ax, ay) = ((*anchor % 3) as f32, (*anchor / 3) as f32);
                            let img_min = canvas.min + Vec2::new((nw as f32 - ow as f32) * sc * ax / 2.0, (nh as f32 - oh as f32) * sc * ay / 2.0);
                            let img = Rect::from_min_size(img_min, Vec2::new(ow as f32 * sc, oh as f32 * sc));
                            let p = ui.painter();
                            p.rect_filled(canvas, 2.0, ui.visuals().extreme_bg_color);
                            p.rect_stroke(canvas, 2.0, (1.0, ui.visuals().weak_text_color()), egui::StrokeKind::Inside);
                            p.rect_filled(img.intersect(canvas), 0.0, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
                            p.rect_stroke(img, 0.0, (1.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                        });
                    });
                    ui.separator();
                    let row = Vec2::new(ui.available_width(), ui.spacing().interact_size.y);
                    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("  Resize  ").fill(ui.visuals().selection.bg_fill)).clicked() {
                            let a = *anchor;
                            act = Some(Box::new(move |app: &mut App| {
                                if let Some(d) = app.doc() {
                                    ops::resize_canvas(d, nw, nh, a);
                                }
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::LayerProps { name, hidden, opacity, blend, orig } => {
                let mut changed = false;
                let mut ok = false;
                let mut cancel = false;
                egui::Window::new("Layer Properties").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    egui::Grid::new("lp").num_columns(2).show(ui, |ui| {
                        ui.label("Name:");
                        changed |= ui.text_edit_singleline(name).changed();
                        ui.end_row();
                        ui.label("Visible:");
                        let mut vis = !*hidden;
                        if ui.checkbox(&mut vis, "").changed() {
                            *hidden = !vis;
                            changed = true;
                        }
                        ui.end_row();
                        ui.label("Blend mode:");
                        egui::ComboBox::from_id_salt("bm").selected_text(blend.name()).show_ui(ui, |ui| {
                            for m in crate::blend::BlendMode::ALL {
                                changed |= ui.selectable_value(blend, m, m.name()).changed();
                            }
                        });
                        ui.end_row();
                        ui.label("Opacity:");
                        let mut o = (*opacity * 100.0).round();
                        if ui.add(egui::Slider::new(&mut o, 0.0..=100.0)).changed() {
                            *opacity = o / 100.0;
                            changed = true;
                        }
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        cancel = ui.button("Cancel").clicked();
                        ok = ui.button("OK").clicked();
                    });
                });
                let (n, hd, op, bl) = (name.clone(), *hidden, *opacity, *blend);
                let o = orig.clone();
                if let Some(d) = self.docs.get_mut(self.active) {
                    if changed || ok {
                        let l = d.state.layer_mut();
                        l.name = n.clone();
                        l.hidden = hd;
                        l.opacity = op;
                        l.blend = bl;
                    }
                    if ok {
                        if (n.clone(), hd, op, bl) != o {
                            d.commit("Layer Properties");
                        }
                        close = true;
                    }
                    if cancel {
                        d.revert_uncommitted();
                        close = true;
                    }
                }
            }
            Dialog::PasteChoice { w, h, data, fit } => {
                let (w, h, data) = (*w, *h, data.clone());
                let canvas = self.docs.get(self.active).map(|d| (d.state.width, d.state.height));
                egui::Window::new("Paste Image").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.label(format!("Paste the {w} x {h} clipboard image where?"));
                    ui.add_space(6.0);
                    if let Some(c) = canvas {
                        fit_choice(ui, fit, (w, h), c);
                    }
                    let fitv = *fit;
                    let mut choice = None;
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Paste into current layer").fill(ui.visuals().selection.bg_fill)).on_hover_text("Paste onto the current layer (Enter)").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            choice = Some(0u8);
                        }
                        if ui.button("Paste into new layer").clicked() {
                            choice = Some(1);
                        }
                        if ui.button("Paste into new tab").clicked() {
                            choice = Some(2);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                    if let Some(t) = choice {
                        act = Some(Box::new(move |app: &mut App| app.paste_data(w, h, &data, t, "Paste Into New Image", fitv)));
                        close = true;
                    }
                });
            }
            Dialog::DropChoice { paths, fit } => {
                let paths = paths.clone();
                let canvas = self.docs.get(self.active).map(|d| (d.state.width, d.state.height));
                let first = image::image_dimensions(&paths[0]).ok();
                let what = if paths.len() == 1 { paths[0].file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default() } else { format!("{} files", paths.len()) };
                egui::Window::new("Dropped Image").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.label(format!("What should happen with {what}?"));
                    ui.add_space(6.0);
                    if let (Some(c), Some(sz)) = (canvas, first) {
                        fit_choice(ui, fit, sz, c);
                    }
                    let fitv = *fit;
                    let mut choice = None;
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Open in new tab").fill(ui.visuals().selection.bg_fill)).on_hover_text("Enter").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            choice = Some(2u8);
                        }
                        if ui.button("Paste into current layer").clicked() {
                            choice = Some(0);
                        }
                        if ui.button("Paste into new layer").clicked() {
                            choice = Some(1);
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                    if let Some(t) = choice {
                        act = Some(Box::new(move |app: &mut App| {
                            for p in paths {
                                if t == 2 {
                                    app.open_path(p);
                                    continue;
                                }
                                match crate::io::load(&p) {
                                    Ok(d) => {
                                        let (w, h) = (d.state.width, d.state.height);
                                        let data = crate::tools::flatten(&d).read_rect(IRect::new(0, 0, w as i32, h as i32));
                                        app.paste_data(w, h, &data, t, "Open Image", fitv);
                                    }
                                    Err(e) => app.error = Some(e),
                                }
                            }
                        }));
                        close = true;
                    }
                });
            }
            Dialog::Jpeg { path, quality } => {
                egui::Window::new("JPEG Quality").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.add(egui::Slider::new(quality, 1..=100).text("Quality"));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("OK").clicked() {
                            let (p, q) = (path.clone(), *quality);
                            act = Some(Box::new(move |app: &mut App| {
                                app.jpeg_quality = q;
                                app.write_file(p, q);
                            }));
                            close = true;
                        }
                    });
                });
            }
            Dialog::ConfirmClose { doc, quit } => {
                let name = self.docs.get(*doc).map(|d| d.title.clone()).unwrap_or_default();
                let (doc, quit) = (*doc, *quit);
                egui::Window::new("Save changes?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.label(format!("Save changes to \"{name}\" before closing?"));
                    ui.horizontal(|ui| {
                        if ui.button("Close without saving").clicked() {
                            act = Some(Box::new(move |app: &mut App| {
                                app.close_doc(doc, true);
                                if quit {
                                    app.quit_next();
                                }
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Save").clicked() {
                            act = Some(Box::new(move |app: &mut App| {
                                app.active = doc;
                                app.save(false);
                                if app.docs.get(doc).is_some_and(|d| !d.is_dirty()) {
                                    app.close_doc(doc, true);
                                    if quit {
                                        app.quit_next();
                                    }
                                }
                            }));
                            close = true;
                        }
                    });
                });
            }
            Dialog::About => {
                egui::Window::new("About Patina").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
                    ui.heading("Patina");
                    ui.label("By Daniel (Kurisu003). Licensed under CC BY-NC-SA 4.0.");
                    ui.hyperlink("https://github.com/Kurisu003/patina");
                    ui.label("A Rust port of Pinta with a tiled, GPU-rendered canvas.");
                    ui.label("Based on Pinta (MIT License), which is based on Paint.NET 3.36 (MIT License).");
                    ui.label("Icons: Paint.NET (MIT), Silk and Fugue icon sets (CC BY 3.0), Pinta contributors (MIT).");
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            }
        }
        if close {
            self.dialog = Dialog::None;
        }
        if let Some(f) = act {
            f(self);
        }
    }

    fn quit_next(&mut self) {
        if let Some(i) = self.docs.iter().position(|d| d.is_dirty()) {
            self.dialog = Dialog::ConfirmClose { doc: i, quit: true };
        } else {
            self.quit_confirmed = true;
        }
    }
}

/// Size options for pasting into the current image or a new layer.
fn fit_choice(ui: &mut egui::Ui, fit: &mut ops::PasteFit, (w, h): (u32, u32), (cw, ch): (u32, u32)) {
    use ops::PasteFit::*;
    // only ask when the picture does not fit; otherwise paste it as is
    if w <= cw && h <= ch {
        *fit = Keep;
        return;
    }
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(format!("The {w} x {h} picture is larger than this image ({cw} x {ch}). It should:"));
        let ew = cw.max(w);
        let eh = ch.max(h);
        let cut = |fits: bool| if fits { "" } else { ", overhang cut off" };
        ui.radio_value(fit, Expand, format!("Expand the canvas to fit  ({ew} x {eh})"));
        ui.radio_value(fit, Keep, format!("Keep the canvas size  ({cw} x {ch}, overhang cut off)"));
        let wh = ((h as f64 * cw as f64 / w.max(1) as f64).round() as u32).max(1);
        ui.radio_value(fit, Width, format!("Scale to the canvas width  (picture {cw} x {wh}{})", cut(wh <= ch)));
        let hw = ((w as f64 * ch as f64 / h.max(1) as f64).round() as u32).max(1);
        ui.radio_value(fit, Height, format!("Scale to the canvas height  (picture {hw} x {ch}{})", cut(hw <= cw)));
    });
    ui.add_space(6.0);
}

/// Pinta-style anchor grid icon: arrows point away from the anchor.
fn anchor_icon(cell: u8, anchor: u8) -> Option<&'static str> {
    let dx = (cell % 3) as i32 - (anchor % 3) as i32;
    let dy = (cell / 3) as i32 - (anchor / 3) as i32;
    if dx.abs() > 1 || dy.abs() > 1 {
        return None;
    }
    Some(match (dx, dy) {
        (0, 0) => "image-resize-canvas-base",
        (0, -1) => "image-resize-canvas-up",
        (0, 1) => "image-resize-canvas-down",
        (-1, 0) => "image-resize-canvas-left",
        (1, 0) => "image-resize-canvas-right",
        (-1, -1) => "image-resize-canvas-nw",
        (1, -1) => "image-resize-canvas-ne",
        (-1, 1) => "image-resize-canvas-sw",
        _ => "image-resize-canvas-se",
    })
}

impl App {
    /// Paint.NET-style layout: menu + image strip, toolbar rows, full-size
    /// canvas, floating Tools / History / Layers / Colors windows.
    fn classic_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if matches!(self.dialog, Dialog::None) && !ctx.egui_wants_keyboard_input() {
            let mut toggle = |k: Key, b: &mut bool| {
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k)) {
                    *b = !*b;
                }
            };
            toggle(Key::F5, &mut self.win_tools);
            toggle(Key::F6, &mut self.win_history);
            toggle(Key::F7, &mut self.win_layers);
            toggle(Key::F8, &mut self.win_colors);
        }
        egui::Panel::top("c_menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                self.menu_bar(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.image_strip(ui));
            });
        });
        egui::Panel::top("c_toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let b = |ui: &mut egui::Ui, t: &str, tip: &str| ui.add(egui::Button::new(t).small()).on_hover_text(tip).clicked();
                if b(ui, "New", "New image (Ctrl+N)") {
                    self.dialog = Dialog::NewImage { w: self.prefs.w, h: self.prefs.h, bg: self.prefs.bg };
                }
                if b(ui, "Open", "Open (Ctrl+O)") {
                    self.open_dialog();
                }
                if b(ui, "Save", "Save (Ctrl+S)") {
                    self.save(false);
                }
                ui.separator();
                if b(ui, "Cut", "Cut (Ctrl+X)") {
                    self.copy(false, true);
                }
                if b(ui, "Copy", "Copy (Ctrl+C)") {
                    self.copy(false, false);
                }
                if b(ui, "Paste", "Paste (Ctrl+V)") {
                    self.paste(0);
                }
                ui.separator();
                let tools = &mut self.tools;
                if let Some(d) = self.docs.get_mut(self.active) {
                    if ui.add_enabled(d.can_undo(), egui::Button::new("Undo").small()).on_hover_text("Undo (Ctrl+Z)").clicked() {
                        if tools.is_busy() { tools.cancel(d) } else { d.undo() }
                    }
                    if ui.add_enabled(d.can_redo(), egui::Button::new("Redo").small()).on_hover_text("Redo (Ctrl+Y)").clicked() {
                        d.redo();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Deselect").small()).on_hover_text("Deselect All (Ctrl+Shift+A)").clicked() {
                        ops::deselect(d);
                    }
                    if ui.add(egui::Button::new("Crop").small()).on_hover_text("Crop to Selection (Ctrl+Shift+X)").clicked() {
                        ops::crop_to_selection(d);
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.show_grid, "Pixel grid");
            });
        });
        egui::Panel::top("c_options").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("c_status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| self.canvas(ui));

        let area = self.canvas_rect;
        if !area.is_positive() {
            return;
        }
        let m = 10.0;
        let mut open = self.win_tools;
        egui::Window::new("Tools").id(egui::Id::new("c_tools")).open(&mut open).auto_sized().collapsible(false).default_pos(area.left_top() + Vec2::new(m, m)).show(ctx, |ui| self.toolbox(ui));
        self.win_tools = open;
        let mut open = self.win_history;
        egui::Window::new("History").id(egui::Id::new("c_history")).open(&mut open).collapsible(false).default_size([200.0, 230.0]).default_pos(area.right_top() + Vec2::new(-210.0 - m, m)).show(ctx, |ui| self.history_panel(ui));
        self.win_history = open;
        let mut open = self.win_layers;
        egui::Window::new("Layers").id(egui::Id::new("c_layers")).open(&mut open).collapsible(false).default_size([230.0, 260.0]).default_pos(area.right_bottom() + Vec2::new(-240.0 - m, -300.0 - m)).show(ctx, |ui| self.layers_panel(ui));
        self.win_layers = open;
        let mut open = self.win_colors;
        egui::Window::new("Colors").id(egui::Id::new("c_colors")).open(&mut open).auto_sized().collapsible(false).default_pos(area.left_bottom() + Vec2::new(m, -265.0 - m)).show(ctx, |ui| self.colors_window(ui));
        self.win_colors = open;
    }

    /// Thumbnails of open images, newest right, click to switch.
    fn image_strip(&mut self, ui: &mut egui::Ui) {
        let mut switch = None;
        let mut close = None;
        for i in (0..self.docs.len()).rev() {
            let d = &self.docs[i];
            let l = &d.state.layers[0];
            let key = Arc::as_ptr(&l.surface.tiles) as usize;
            let tex = match self.thumbs.get(&l.id) {
                Some((k, t)) if *k == key => t.clone(),
                _ => {
                    let (tw, th, data) = ops::layer_thumbnail(&l.surface, 40);
                    let img = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &data);
                    let t = ui.ctx().load_texture(format!("thumb{}", l.id), img, egui::TextureOptions::LINEAR);
                    self.thumbs.insert(l.id, (key, t.clone()));
                    t
                }
            };
            let sz = tex.size_vec2();
            let sc = 28.0 / sz.x.max(sz.y);
            let r = ui.add(egui::Button::image(egui::Image::new((tex.id(), sz * sc))).selected(i == self.active)).on_hover_text(d.display_name());
            if r.clicked() {
                switch = Some(i);
            }
            if r.middle_clicked() {
                close = Some(i);
            }
        }
        if let Some(i) = switch {
            self.active = i;
        }
        if let Some(i) = close {
            self.close_doc(i, false);
        }
    }

    fn colors_window(&mut self, ui: &mut egui::Ui) {
        use crate::effects::util::{hsv_to_rgb, rgb_to_hsv};
        ui.set_max_width(320.0);
        let target = if self.color_target_primary { self.palette.primary } else { self.palette.secondary };
        let (h, s, v) = rgb_to_hsv(target[0], target[1], target[2]);
        let mut new = target;
        ui.horizontal(|ui| {
            // HSV wheel
            let size = 150.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
            let c = rect.center();
            let rad = size / 2.0 - 3.0;
            let mut mesh = egui::Mesh::default();
            let vc = |hh: f64, ss: f64| {
                let (r, g, b) = hsv_to_rgb(hh, ss, 100.0);
                Color32::from_rgb(r, g, b)
            };
            mesh.colored_vertex(c, vc(0.0, 0.0));
            let n = 90;
            for i in 0..=n {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                mesh.colored_vertex(c + Vec2::new(a.cos(), -a.sin()) * rad, vc(a.to_degrees() as f64, 100.0));
                if i > 0 {
                    mesh.add_triangle(0, i as u32, i as u32 + 1);
                }
            }
            ui.painter().add(egui::Shape::mesh(mesh));
            ui.painter().circle_stroke(c, rad, (1.0, ui.visuals().weak_text_color()));
            let ha = (h as f32).to_radians();
            let mp = c + Vec2::new(ha.cos(), -ha.sin()) * rad * (s as f32 / 100.0);
            ui.painter().circle_stroke(mp, 5.0, (2.0, Color32::BLACK));
            ui.painter().circle_stroke(mp, 4.0, (1.0, Color32::WHITE));
            if let Some(p) = resp.interact_pointer_pos() {
                if resp.dragged() || resp.clicked() {
                    let d = p - c;
                    let hue = (-d.y).atan2(d.x).to_degrees().rem_euclid(360.0) as f64;
                    let sat = ((d.length() / rad).min(1.0) * 100.0) as f64;
                    let vv = if v < 1.0 { 100.0 } else { v };
                    let (r, g, b) = hsv_to_rgb(hue, sat, vv);
                    new = [r, g, b, target[3]];
                }
            }
            ui.vertical(|ui| {
                // primary/secondary swatches pick which colour the wheel edits
                ui.horizontal(|ui| {
                    for (primary, col, label) in [(true, self.palette.primary, "Primary"), (false, self.palette.secondary, "Secondary")] {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(34.0, 34.0), Sense::click());
                        checker(ui.painter(), r);
                        ui.painter().rect_filled(r, 2.0, c32(col));
                        let sel = self.color_target_primary == primary;
                        ui.painter().rect_stroke(r, 2.0, (if sel { 3.0 } else { 1.0 }, if sel { ui.visuals().selection.bg_fill } else { Color32::GRAY }), egui::StrokeKind::Inside);
                        if resp.on_hover_text(label).clicked() {
                            self.color_target_primary = primary;
                        }
                    }
                    if self.icon_small(ui, "edit-swap-vert", "Swap colors (X)").clicked() {
                        std::mem::swap(&mut self.palette.primary, &mut self.palette.secondary);
                    }
                });
                ui.label(if self.color_target_primary { "Editing primary" } else { "Editing secondary" });
                let mut vv = v;
                ui.horizontal(|ui| {
                    ui.label("Value");
                    if ui.add(egui::Slider::new(&mut vv, 0.0..=100.0).show_value(false)).changed() {
                        let (r, g, b) = hsv_to_rgb(h, s, vv);
                        new = [r, g, b, new[3]];
                    }
                });
                let mut a = new[3];
                ui.horizontal(|ui| {
                    ui.label("Alpha");
                    if ui.add(egui::Slider::new(&mut a, 0..=255).show_value(false)).changed() {
                        new[3] = a;
                    }
                });
                let mut hex = format!("{:02X}{:02X}{:02X}", new[0], new[1], new[2]);
                ui.horizontal(|ui| {
                    ui.label("Hex");
                    if ui.add(egui::TextEdit::singleline(&mut hex).desired_width(64.0)).changed() {
                        if let Ok(x) = u32::from_str_radix(hex.trim_start_matches('#'), 16) {
                            if hex.trim_start_matches('#').len() == 6 {
                                new = [(x >> 16) as u8, (x >> 8) as u8, x as u8, new[3]];
                            }
                        }
                    }
                });
            });
        });
        if new != target {
            if self.color_target_primary { self.palette.primary = new } else { self.palette.secondary = new }
        }
        ui.separator();
        // palette grid: left click primary, right click secondary
        let colors = default_palette();
        ui.spacing_mut().item_spacing = Vec2::splat(1.0);
        for row in colors.chunks(22) {
            ui.horizontal(|ui| {
                for c in row {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::click());
                    ui.painter().rect_filled(r, 1.0, c32(*c));
                    if resp.clicked() {
                        self.palette.primary = *c;
                    }
                    if resp.secondary_clicked() {
                        self.palette.secondary = *c;
                    }
                }
            });
        }
    }
}

fn checker(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, 0.0, Color32::WHITE);
    let s = 5.0;
    let mut y = r.min.y;
    let mut row = 0;
    while y < r.max.y {
        let mut x = r.min.x + if row % 2 == 1 { s } else { 0.0 };
        while x < r.max.x {
            p.rect_filled(Rect::from_min_max(Pos2::new(x, y), Pos2::new((x + s).min(r.max.x), (y + s).min(r.max.y))), 0.0, Color32::from_gray(204));
            x += 2.0 * s;
        }
        y += s;
        row += 1;
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(s) = self.script.as_mut() {
            s.step(ctx, raw);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        // handle window close with unsaved docs
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            if self.docs.iter().any(|d| d.is_dirty()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit_next();
            }
        }
        if self.quit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Q))) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // drag and drop files
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            if self.docs.is_empty() {
                for p in dropped {
                    self.open_path(p);
                }
            } else {
                // only the first file's size picks the default fit
                let fit = image::image_dimensions(&dropped[0]).map_or(ops::PasteFit::Expand, |(w, h)| self.default_fit(w, h));
                self.dialog = Dialog::DropChoice { paths: dropped, fit };
            }
        }
        // text widgets (layer name, hex colour...) still need Ctrl+X/C/V
        if ctx.egui_wants_keyboard_input() {
            let paste_text = self.clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
            ctx.input_mut(|i| {
                for e in i.events.iter_mut() {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                        if modifiers.command_only() {
                            match key {
                                Key::C => *e = egui::Event::Copy,
                                Key::X => *e = egui::Event::Cut,
                                Key::V => *e = egui::Event::Paste(paste_text.clone()),
                                _ => {}
                            }
                        }
                    }
                }
            });
        }
        self.poll_effect(&ctx);
        if matches!(self.dialog, Dialog::None) {
            if !self.text_input(&ctx) {
                self.shortcuts(&ctx);
            }
        }
        let title = self.docs.get(self.active).map(|d| format!("{} - Patina", d.display_name())).unwrap_or_else(|| "Patina".into());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        if self.skin_applied != Some(self.skin) {
            ctx.set_visuals(if self.skin == Skin::Classic { classic_visuals() } else { egui::Visuals::dark() });
            self.skin.save();
            self.skin_applied = Some(self.skin);
        }
        if self.skin == Skin::Classic {
            self.classic_ui(ui, &ctx);
            self.dialogs(&ctx);
            self.frame_ms = self.frame_ms * 0.9 + t0.elapsed().as_secs_f32() * 1000.0 * 0.1;
            return;
        }
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("palette").show(ui, |ui| self.palette_bar(ui));
        egui::Panel::left("toolbox").resizable(false).exact_size(96.0).show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbox(ui)
        });
        egui::Panel::right("layers").default_size(230.0).show(ui, |ui| {
            ui.heading("Layers");
            self.layers_panel(ui);
            ui.separator();
            ui.heading("History");
            self.history_panel(ui)
        });
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| self.canvas(ui));
        self.dialogs(&ctx);
        self.frame_ms = self.frame_ms * 0.9 + t0.elapsed().as_secs_f32() * 1000.0 * 0.1;
    }
}
