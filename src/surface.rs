//! Sparse, copy-on-write tiled RGBA8 (premultiplied) surfaces.
//!
//! A surface is a grid of 256x256 tiles. Missing tiles are fully transparent and
//! cost nothing. Tiles and the tile table are reference counted, so cloning a
//! surface is O(1) and edits only copy the tiles they touch. This is what makes
//! undo snapshots, previews and huge images cheap.

use rayon::prelude::*;
use std::sync::Arc;

pub const TILE: usize = 256;
pub const TILE_SHIFT: u32 = 8;
pub const TILE_PX: usize = TILE * TILE;
pub const TILE_BYTES: usize = TILE_PX * 4;

#[derive(Clone)]
pub struct Tile {
    pub px: Box<[u8; TILE_BYTES]>,
}

impl Tile {
    pub fn transparent() -> Self {
        Tile { px: vec![0u8; TILE_BYTES].into_boxed_slice().try_into().unwrap() }
    }
    pub fn filled(c: [u8; 4]) -> Self {
        let mut t = Self::transparent();
        for p in t.px.chunks_exact_mut(4) {
            p.copy_from_slice(&c);
        }
        t
    }
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * TILE + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: [u8; 4]) {
        let i = (y * TILE + x) * 4;
        self.px[i..i + 4].copy_from_slice(&c);
    }
    pub fn is_transparent(&self) -> bool {
        self.px.chunks_exact(4).all(|p| p[3] == 0)
    }
}

pub type TileRef = Option<Arc<Tile>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        IRect { x, y, w, h }
    }
    pub fn from_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        IRect::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 { IRect::default() } else { IRect::new(x0, y0, x1 - x0, y1 - y0) }
    }
    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        IRect::new(x0, y0, x1 - x0, y1 - y0)
    }
    pub fn inflate(&self, d: i32) -> IRect {
        IRect::new(self.x - d, self.y - d, self.w + 2 * d, self.h + 2 * d)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    /// Bounding rect of floating point coordinates, rounded outward.
    pub fn from_f(x0: f32, y0: f32, x1: f32, y1: f32) -> IRect {
        let ax = x0.min(x1).floor() as i32;
        let ay = y0.min(y1).floor() as i32;
        let bx = x0.max(x1).ceil() as i32;
        let by = y0.max(y1).ceil() as i32;
        IRect::new(ax, ay, bx - ax, by - ay)
    }
}

/// Generic tiled grid storage shared by colour surfaces and 8-bit masks.
#[derive(Clone)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub tiles: Arc<Vec<TileRef>>,
}

pub fn tiles_for(n: u32) -> u32 {
    n.div_ceil(TILE as u32).max(1)
}

impl Surface {
    pub fn new(width: u32, height: u32) -> Self {
        let tx = tiles_for(width);
        let ty = tiles_for(height);
        Surface { width, height, tiles_x: tx, tiles_y: ty, tiles: Arc::new(vec![None; (tx * ty) as usize]) }
    }

    pub fn filled(width: u32, height: u32, c: [u8; 4]) -> Self {
        let mut s = Self::new(width, height);
        if c[3] != 0 {
            let t = Arc::new(Tile::filled(c));
            let tiles = Arc::make_mut(&mut s.tiles);
            for slot in tiles.iter_mut() {
                *slot = Some(t.clone());
            }
            s.clear_outside();
        }
        s
    }

    pub fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.width as i32, self.height as i32)
    }

    #[inline]
    pub fn tile_index(&self, tx: u32, ty: u32) -> usize {
        (ty * self.tiles_x + tx) as usize
    }

    pub fn tile_rect(&self, idx: usize) -> IRect {
        let tx = idx as u32 % self.tiles_x;
        let ty = idx as u32 / self.tiles_x;
        IRect::new((tx as usize * TILE) as i32, (ty as usize * TILE) as i32, TILE as i32, TILE as i32)
    }

    pub fn tile(&self, tx: u32, ty: u32) -> Option<&Tile> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        self.tiles[self.tile_index(tx, ty)].as_deref()
    }

    /// Mutable access to a tile, allocating a transparent tile if needed.
    pub fn tile_mut(&mut self, tx: u32, ty: u32) -> &mut Tile {
        let idx = self.tile_index(tx, ty);
        let tiles = Arc::make_mut(&mut self.tiles);
        let slot = tiles[idx].get_or_insert_with(|| Arc::new(Tile::transparent()));
        Arc::make_mut(slot)
    }

    #[inline]
    pub fn get_pixel(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        match self.tile(x as u32 >> TILE_SHIFT, y as u32 >> TILE_SHIFT) {
            Some(t) => t.get(x as usize & (TILE - 1), y as usize & (TILE - 1)),
            None => [0; 4],
        }
    }

    pub fn set_pixel(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let t = self.tile_mut(x as u32 >> TILE_SHIFT, y as u32 >> TILE_SHIFT);
        t.set(x as usize & (TILE - 1), y as usize & (TILE - 1), c);
    }

    /// Indices of tiles overlapping `r` (clipped to the surface).
    pub fn tiles_in(&self, r: IRect) -> Vec<usize> {
        let r = r.intersect(&self.bounds());
        if r.is_empty() {
            return vec![];
        }
        let tx0 = r.x as u32 >> TILE_SHIFT;
        let ty0 = r.y as u32 >> TILE_SHIFT;
        let tx1 = (r.right() - 1) as u32 >> TILE_SHIFT;
        let ty1 = (r.bottom() - 1) as u32 >> TILE_SHIFT;
        let mut v = Vec::with_capacity(((tx1 - tx0 + 1) * (ty1 - ty0 + 1)) as usize);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                v.push(self.tile_index(tx, ty));
            }
        }
        v
    }

    /// Run `f` in parallel over every tile overlapping `r`, allocating them.
    /// `f` receives the tile and the tile's rect in image space.
    pub fn par_tiles_mut<F>(&mut self, r: IRect, alloc: bool, f: F)
    where
        F: Fn(&mut Tile, IRect) + Sync + Send,
    {
        let wanted = self.tiles_in(r);
        if wanted.is_empty() {
            return;
        }
        let tiles_x = self.tiles_x;
        let mut mask = vec![false; self.tiles.len()];
        for i in wanted {
            mask[i] = true;
        }
        let tiles = Arc::make_mut(&mut self.tiles);
        tiles.par_iter_mut().enumerate().filter(|(i, _)| mask[*i]).for_each(|(i, slot)| {
            if slot.is_none() {
                if !alloc {
                    return;
                }
                *slot = Some(Arc::new(Tile::transparent()));
            }
            let t = Arc::make_mut(slot.as_mut().unwrap());
            let tx = i as u32 % tiles_x;
            let ty = i as u32 / tiles_x;
            f(t, IRect::new((tx as usize * TILE) as i32, (ty as usize * TILE) as i32, TILE as i32, TILE as i32));
        });
    }

    /// Copy a rectangle out as a contiguous RGBA buffer (row stride = r.w*4).
    /// Pixels outside the surface come out transparent.
    pub fn read_rect(&self, r: IRect) -> Vec<u8> {
        let mut out = vec![0u8; (r.w.max(0) * r.h.max(0) * 4) as usize];
        if r.is_empty() {
            return out;
        }
        let stride = r.w as usize * 4;
        let clip = r.intersect(&self.bounds());
        if clip.is_empty() {
            return out;
        }
        out.par_chunks_mut(stride).enumerate().for_each(|(row, line)| {
            let y = r.y + row as i32;
            if y < clip.y || y >= clip.bottom() {
                return;
            }
            let ty = y as u32 >> TILE_SHIFT;
            let iy = y as usize & (TILE - 1);
            let mut x = clip.x;
            while x < clip.right() {
                let tx = x as u32 >> TILE_SHIFT;
                let ix = x as usize & (TILE - 1);
                let n = (TILE - ix).min((clip.right() - x) as usize);
                let dst = ((x - r.x) as usize) * 4;
                if let Some(t) = self.tile(tx, ty) {
                    let s = (iy * TILE + ix) * 4;
                    line[dst..dst + n * 4].copy_from_slice(&t.px[s..s + n * 4]);
                }
                x += n as i32;
            }
        });
        out
    }

    /// Write a contiguous RGBA buffer into the rectangle (clipped).
    pub fn write_rect(&mut self, r: IRect, data: &[u8]) {
        let stride = r.w as usize * 4;
        self.par_tiles_mut(r, true, |t, tr| {
            let c = tr.intersect(&r);
            for y in c.y..c.bottom() {
                let iy = (y - tr.y) as usize;
                let src = ((y - r.y) as usize) * stride + ((c.x - r.x) as usize) * 4;
                let dst = (iy * TILE + (c.x - tr.x) as usize) * 4;
                t.px[dst..dst + c.w as usize * 4].copy_from_slice(&data[src..src + c.w as usize * 4]);
            }
        });
        self.clear_outside();
    }

    /// Ensures pixels beyond width/height in edge tiles are transparent.
    pub fn clear_outside(&mut self) {
        let w = self.width as usize;
        let h = self.height as usize;
        let ex = w % TILE;
        let ey = h % TILE;
        if ex == 0 && ey == 0 {
            return;
        }
        let tx_last = self.tiles_x - 1;
        let ty_last = self.tiles_y - 1;
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let edge_x = ex != 0 && tx == tx_last;
                let edge_y = ey != 0 && ty == ty_last;
                if !edge_x && !edge_y {
                    continue;
                }
                let idx = self.tile_index(tx, ty);
                if self.tiles[idx].is_none() {
                    continue;
                }
                // Skip work if already clear.
                let t = self.tiles[idx].as_ref().unwrap();
                let lx = if edge_x { ex } else { TILE };
                let ly = if edge_y { ey } else { TILE };
                let dirty = (0..TILE).any(|y| {
                    let row = &t.px[y * TILE * 4..(y + 1) * TILE * 4];
                    if y >= ly { row.iter().any(|&b| b != 0) } else { row[lx * 4..].iter().any(|&b| b != 0) }
                });
                if !dirty {
                    continue;
                }
                let t = self.tile_mut(tx, ty);
                for y in 0..TILE {
                    let row = &mut t.px[y * TILE * 4..(y + 1) * TILE * 4];
                    if y >= ly {
                        row.fill(0);
                    } else {
                        row[lx * 4..].fill(0);
                    }
                }
            }
        }
    }

    /// Drop tiles that became fully transparent (keeps memory sparse).
    pub fn compact(&mut self) {
        let any = self.tiles.par_iter().any(|t| t.as_ref().is_some_and(|t| t.is_transparent()));
        if any {
            let tiles = Arc::make_mut(&mut self.tiles);
            tiles.par_iter_mut().for_each(|t| {
                if t.as_ref().is_some_and(|t| t.is_transparent()) {
                    *t = None;
                }
            });
        }
    }

    /// Full contiguous copy of the image (for saving / clipboard).
    pub fn to_rgba(&self) -> Vec<u8> {
        self.read_rect(self.bounds())
    }

    pub fn from_rgba(width: u32, height: u32, data: &[u8]) -> Self {
        let mut s = Surface::new(width, height);
        s.write_rect(IRect::new(0, 0, width as i32, height as i32), data);
        s.compact();
        s
    }

    /// Bounding box of non-transparent pixels, if any.
    pub fn content_bounds(&self) -> Option<IRect> {
        let mut acc: Option<IRect> = None;
        for (i, t) in self.tiles.iter().enumerate() {
            let Some(t) = t else { continue };
            let tr = self.tile_rect(i);
            let mut x0 = TILE;
            let mut y0 = TILE;
            let mut x1 = 0;
            let mut y1 = 0;
            for y in 0..TILE {
                for x in 0..TILE {
                    if t.px[(y * TILE + x) * 4 + 3] != 0 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                    }
                }
            }
            if x1 > x0 {
                let r = IRect::new(tr.x + x0 as i32, tr.y + y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32);
                acc = Some(acc.map_or(r, |a| a.union(&r)));
            }
        }
        acc
    }
}

#[inline]
pub fn premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    [mul8(c[0] as u32, a), mul8(c[1] as u32, a), mul8(c[2] as u32, a), c[3]]
}

#[inline]
pub fn unpremul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    if a == 0 {
        return [0; 4];
    }
    if a == 255 {
        return c;
    }
    let f = |v: u8| ((v as u32 * 255 + a / 2) / a).min(255) as u8;
    [f(c[0]), f(c[1]), f(c[2]), c[3]]
}

/// (a*b)/255 with rounding.
#[inline(always)]
pub fn mul8(a: u32, b: u32) -> u8 {
    let t = a * b + 128;
    ((t + (t >> 8)) >> 8) as u8
}
