//! Document model: layers, selection and snapshot history.
//!
//! Every history entry is a full `DocState`, but because surfaces and masks
//! are copy-on-write tile tables, a snapshot only owns the tiles that changed
//! since the previous one.

use crate::blend::BlendMode;
use crate::selection::Selection;
use crate::surface::Surface;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
pub struct Layer {
    pub id: u64,
    pub name: String,
    pub hidden: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub surface: Surface,
}

impl Layer {
    pub fn new(name: impl Into<String>, surface: Surface) -> Self {
        Layer { id: next_id(), name: name.into(), hidden: false, opacity: 1.0, blend: BlendMode::Normal, surface }
    }
}

#[derive(Clone)]
pub struct DocState {
    pub width: u32,
    pub height: u32,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    pub current: usize,
    pub selection: Selection,
}

impl DocState {
    pub fn layer(&self) -> &Layer {
        &self.layers[self.current]
    }
    pub fn layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.current]
    }
}

pub struct HistoryItem {
    pub label: String,
    pub state: DocState,
}

pub struct View {
    /// Screen pixels per image pixel.
    pub zoom: f32,
    /// Image coordinate shown at the viewport centre.
    pub center: [f32; 2],
    /// Set when the view should be fitted to the window next frame.
    pub fit_pending: bool,
}

pub struct Document {
    pub state: DocState,
    pub history: Vec<HistoryItem>,
    pub history_pos: usize,
    /// History position that matches the file on disk.
    pub clean_pos: Option<usize>,
    pub path: Option<PathBuf>,
    pub title: String,
    pub view: View,
    pub id: u64,
}

impl Document {
    pub fn new(title: impl Into<String>, width: u32, height: u32, background: Option<[u8; 4]>) -> Self {
        let surface = match background {
            Some(c) => Surface::filled(width, height, crate::surface::premul(c)),
            None => Surface::new(width, height),
        };
        let layer = Layer::new("Background", surface);
        Self::from_layers(title, width, height, vec![layer], "New Image")
    }

    pub fn from_layers(title: impl Into<String>, width: u32, height: u32, layers: Vec<Layer>, label: &str) -> Self {
        let current = layers.len() - 1;
        let state = DocState { width, height, layers, current, selection: Selection::default() };
        Document {
            history: vec![HistoryItem { label: label.into(), state: state.clone() }],
            state,
            history_pos: 0,
            clean_pos: Some(0),
            path: None,
            title: title.into(),
            view: View { zoom: 1.0, center: [width as f32 / 2.0, height as f32 / 2.0], fit_pending: true },
            id: next_id(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.clean_pos != Some(self.history_pos)
    }

    pub fn mark_clean(&mut self) {
        self.clean_pos = Some(self.history_pos);
    }

    /// Record the current state as a new history step.
    pub fn commit(&mut self, label: impl Into<String>) {
        self.history.truncate(self.history_pos + 1);
        if let Some(c) = self.clean_pos {
            if c > self.history_pos {
                self.clean_pos = None;
            }
        }
        self.history.push(HistoryItem { label: label.into(), state: self.state.clone() });
        self.history_pos += 1;
        // Keep memory bounded: very old steps are dropped.
        const MAX: usize = 200;
        if self.history.len() > MAX {
            let drop = self.history.len() - MAX;
            self.history.drain(0..drop);
            self.history_pos -= drop;
            self.clean_pos = self.clean_pos.and_then(|c| c.checked_sub(drop));
        }
    }

    /// Discard uncommitted changes (e.g. a cancelled tool preview).
    pub fn revert_uncommitted(&mut self) {
        self.state = self.history[self.history_pos].state.clone();
    }

    pub fn can_undo(&self) -> bool {
        self.history_pos > 0
    }
    pub fn can_redo(&self) -> bool {
        self.history_pos + 1 < self.history.len()
    }
    pub fn undo(&mut self) {
        if self.can_undo() {
            self.history_pos -= 1;
            self.state = self.history[self.history_pos].state.clone();
        }
    }
    pub fn redo(&mut self) {
        if self.can_redo() {
            self.history_pos += 1;
            self.state = self.history[self.history_pos].state.clone();
        }
    }
    pub fn goto_history(&mut self, pos: usize) {
        if pos < self.history.len() {
            self.history_pos = pos;
            self.state = self.history[pos].state.clone();
        }
    }

    pub fn display_name(&self) -> String {
        if self.is_dirty() { format!("{}*", self.title) } else { self.title.clone() }
    }
}
