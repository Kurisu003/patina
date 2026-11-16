//! CPU-side composite cache organised as a mip pyramid of 256px tiles.
//!
//! Level 0 is the flattened image; each higher level halves resolution. Only
//! tiles that are visible are ever computed, and only tiles whose source
//! layer tiles changed (detected by pointer identity of the copy-on-write
//! tiles) are recomputed. Changes propagate up the pyramid lazily.

use crate::blend::{BlendMode, blend_row};
use crate::document::{DocState, Layer};
use crate::surface::{IRect, TILE, Tile, TileRef, tiles_for};
use rayon::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static VERSION: AtomicU64 = AtomicU64::new(1);
fn next_version() -> u64 {
    VERSION.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
pub struct Slot {
    pub valid: bool,
    pub tile: TileRef,
    pub version: u64,
}

pub struct Level {
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub slots: Vec<Slot>,
}

pub struct Pyramid {
    pub width: u32,
    pub height: u32,
    pub levels: Vec<Level>,
    /// Snapshot of the layers we last composited, used for change detection
    /// (holding it also keeps tile pointers alive so identity is reliable).
    last: Option<Vec<Layer>>,
}

fn layer_sig(l: &Layer) -> (u64, bool, u32, BlendMode) {
    (l.id, l.hidden, (l.opacity * 1000.0) as u32, l.blend)
}

impl Pyramid {
    pub fn new(width: u32, height: u32) -> Self {
        let mut levels = Vec::new();
        let mut l = 0;
        loop {
            let w = (width >> l).max(1);
            let h = (height >> l).max(1);
            let tx = tiles_for(w);
            let ty = tiles_for(h);
            levels.push(Level { tiles_x: tx, tiles_y: ty, slots: vec![Slot { valid: false, tile: None, version: 0 }; (tx * ty) as usize] });
            if tx == 1 && ty == 1 {
                break;
            }
            l += 1;
        }
        Pyramid { width, height, levels, last: None }
    }

    pub fn invalidate_all(&mut self) {
        for lv in &mut self.levels {
            for s in &mut lv.slots {
                s.valid = false;
            }
        }
    }

    fn invalidate_base(&mut self, tx: u32, ty: u32) {
        for (l, lv) in self.levels.iter_mut().enumerate() {
            let x = tx >> l;
            let y = ty >> l;
            if x < lv.tiles_x && y < lv.tiles_y {
                lv.slots[(y * lv.tiles_x + x) as usize].valid = false;
            }
        }
    }

    /// Compare against the last composited layers and invalidate what changed.
    pub fn sync(&mut self, state: &DocState) {
        if state.width != self.width || state.height != self.height {
            *self = Pyramid::new(state.width, state.height);
        }
        let layers = &state.layers;
        let structural = match &self.last {
            None => true,
            Some(last) => last.len() != layers.len() || last.iter().zip(layers).any(|(a, b)| layer_sig(a) != layer_sig(b)),
        };
        if structural {
            self.invalidate_all();
        } else {
            let last = self.last.take().unwrap();
            let tiles_x = self.levels[0].tiles_x;
            for (a, b) in last.iter().zip(layers) {
                if Arc::ptr_eq(&a.surface.tiles, &b.surface.tiles) {
                    continue;
                }
                for (i, (ta, tb)) in a.surface.tiles.iter().zip(b.surface.tiles.iter()).enumerate() {
                    let same = match (ta, tb) {
                        (None, None) => true,
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        _ => false,
                    };
                    if !same {
                        self.invalidate_base(i as u32 % tiles_x, i as u32 / tiles_x);
                    }
                }
            }
        }
        self.last = Some(layers.clone());
    }

    /// Make sure the given tiles at `level` are valid; computes everything
    /// needed below them in parallel, bottom-up.
    pub fn ensure(&mut self, level: usize, want: &[(u32, u32)]) {
        let Some(layers) = self.last.clone() else { return };
        // Collect needed sets per level, top-down.
        let mut needed: Vec<Vec<(u32, u32)>> = vec![Vec::new(); level + 1];
        needed[level] = want.iter().copied().filter(|&(x, y)| !self.levels[level].slots[(y * self.levels[level].tiles_x + x) as usize].valid).collect();
        for l in (1..=level).rev() {
            let mut kids = Vec::new();
            let lv = &self.levels[l - 1];
            for &(x, y) in &needed[l] {
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let cx = x * 2 + dx;
                    let cy = y * 2 + dy;
                    if cx < lv.tiles_x && cy < lv.tiles_y && !lv.slots[(cy * lv.tiles_x + cx) as usize].valid {
                        kids.push((cx, cy));
                    }
                }
            }
            kids.sort_unstable();
            kids.dedup();
            needed[l - 1] = kids;
        }
        // Level 0: composite layers.
        if !needed[0].is_empty() {
            let w = self.width;
            let h = self.height;
            let tiles_x = self.levels[0].tiles_x;
            let results: Vec<((u32, u32), TileRef)> = needed[0]
                .par_iter()
                .map(|&(x, y)| ((x, y), composite_tile(&layers, (y * tiles_x + x) as usize, x, y, w, h)))
                .collect();
            let lv = &mut self.levels[0];
            for ((x, y), t) in results {
                let s = &mut lv.slots[(y * lv.tiles_x + x) as usize];
                s.tile = t;
                s.valid = true;
                s.version = next_version();
            }
        }
        // Higher levels: 2x2 box downsample.
        for l in 1..=level {
            if needed[l].is_empty() {
                continue;
            }
            let (below, above) = self.levels.split_at_mut(l);
            let child = &below[l - 1];
            let results: Vec<((u32, u32), TileRef)> = needed[l]
                .par_iter()
                .map(|&(x, y)| {
                    let get = |cx: u32, cy: u32| -> Option<&Tile> {
                        if cx < child.tiles_x && cy < child.tiles_y { child.slots[(cy * child.tiles_x + cx) as usize].tile.as_deref() } else { None }
                    };
                    let q = [get(x * 2, y * 2), get(x * 2 + 1, y * 2), get(x * 2, y * 2 + 1), get(x * 2 + 1, y * 2 + 1)];
                    ((x, y), downsample(q))
                })
                .collect();
            let lv = &mut above[0];
            for ((x, y), t) in results {
                let s = &mut lv.slots[(y * lv.tiles_x + x) as usize];
                s.tile = t;
                s.valid = true;
                s.version = next_version();
            }
        }
    }

    pub fn slot(&self, level: usize, x: u32, y: u32) -> &Slot {
        let lv = &self.levels[level];
        &lv.slots[(y * lv.tiles_x + x) as usize]
    }

    /// Visible tile coordinates at `level` for an image-space rectangle.
    pub fn visible(&self, level: usize, r: IRect) -> Vec<(u32, u32)> {
        let lv = &self.levels[level];
        let span = (TILE as i32) << level;
        let x0 = (r.x.max(0) / span) as u32;
        let y0 = (r.y.max(0) / span) as u32;
        let x1 = ((r.right().max(0) + span - 1) / span) as u32;
        let y1 = ((r.bottom().max(0) + span - 1) / span) as u32;
        let mut v = Vec::new();
        for y in y0..y1.min(lv.tiles_y) {
            for x in x0..x1.min(lv.tiles_x) {
                v.push((x, y));
            }
        }
        v
    }
}

/// Composite all visible layers for one base tile.
pub fn composite_tile(layers: &[Layer], idx: usize, _tx: u32, _ty: u32, _w: u32, _h: u32) -> TileRef {
    let vis: Vec<&Layer> = layers.iter().filter(|l| !l.hidden && l.opacity > 0.0).collect();
    let contributing: Vec<(&Layer, &Arc<Tile>)> = vis.iter().filter_map(|l| l.surface.tiles.get(idx).and_then(|t| t.as_ref()).map(|t| (*l, t))).collect();
    if contributing.is_empty() {
        return None;
    }
    // Share the layer tile directly when it is the only contributor.
    if contributing.len() == 1 {
        let (l, t) = contributing[0];
        if l.opacity >= 1.0 && matches!(l.blend, BlendMode::Normal | BlendMode::Xor | BlendMode::Screen | BlendMode::Lighten | BlendMode::Difference) {
            return Some(t.clone());
        }
    }
    let mut out = Tile::transparent();
    for (l, t) in contributing {
        let op = (l.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        blend_row(&mut out.px[..], &t.px[..], op, l.blend);
    }
    Some(Arc::new(out))
}

fn downsample(q: [Option<&Tile>; 4]) -> TileRef {
    if q.iter().all(|t| t.is_none()) {
        return None;
    }
    let mut out = Tile::transparent();
    let half = TILE / 2;
    for (qi, t) in q.iter().enumerate() {
        let Some(t) = t else { continue };
        let ox = (qi % 2) * half;
        let oy = (qi / 2) * half;
        for y in 0..half {
            let r0 = (2 * y) * TILE * 4;
            let r1 = (2 * y + 1) * TILE * 4;
            let d = ((oy + y) * TILE + ox) * 4;
            for x in 0..half {
                let s = 2 * x * 4;
                for c in 0..4 {
                    let sum = t.px[r0 + s + c] as u32 + t.px[r0 + s + 4 + c] as u32 + t.px[r1 + s + c] as u32 + t.px[r1 + s + 4 + c] as u32;
                    out.px[d + x * 4 + c] = ((sum + 2) / 4) as u8;
                }
            }
        }
    }
    Some(Arc::new(out))
}
