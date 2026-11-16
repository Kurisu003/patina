//! Selection stored as a sparse tiled 8-bit coverage mask.
//! Empty and fully-selected tiles cost no memory, so "Select All" on a huge
//! image is instant.

use crate::surface::{IRect, TILE, TILE_PX, TILE_SHIFT, tiles_for};
use rayon::prelude::*;
use std::sync::Arc;
use tiny_skia::{FillRule, Path, Transform};

#[derive(Clone)]
pub enum MTile {
    Empty,
    Full,
    Partial(Arc<[u8; TILE_PX]>),
}

impl MTile {
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        match self {
            MTile::Empty => 0,
            MTile::Full => 255,
            MTile::Partial(p) => p[y * TILE + x],
        }
    }
    fn normalize(data: Box<[u8; TILE_PX]>) -> MTile {
        if data.iter().all(|&v| v == 0) {
            MTile::Empty
        } else if data.iter().all(|&v| v == 255) {
            MTile::Full
        } else {
            MTile::Partial(Arc::from(data))
        }
    }
    fn to_box(&self) -> Box<[u8; TILE_PX]> {
        match self {
            MTile::Empty => vec![0u8; TILE_PX].into_boxed_slice().try_into().unwrap(),
            MTile::Full => vec![255u8; TILE_PX].into_boxed_slice().try_into().unwrap(),
            MTile::Partial(p) => Box::new(**p),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CombineMode {
    #[default]
    Replace,
    Union,
    Exclude,
    Xor,
    Intersect,
}

impl CombineMode {
    pub const ALL: [CombineMode; 5] = [CombineMode::Replace, CombineMode::Union, CombineMode::Exclude, CombineMode::Xor, CombineMode::Intersect];
    pub fn name(self) -> &'static str {
        match self {
            CombineMode::Replace => "Replace",
            CombineMode::Union => "Union",
            CombineMode::Exclude => "Exclude",
            CombineMode::Xor => "Xor",
            CombineMode::Intersect => "Intersect",
        }
    }
}

#[derive(Clone)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub tiles: Arc<Vec<MTile>>,
}

impl Mask {
    pub fn empty(width: u32, height: u32) -> Self {
        let tx = tiles_for(width);
        let ty = tiles_for(height);
        Mask { width, height, tiles_x: tx, tiles_y: ty, tiles: Arc::new(vec![MTile::Empty; (tx * ty) as usize]) }
    }

    pub fn full(width: u32, height: u32) -> Self {
        Self::from_rect(width, height, IRect::new(0, 0, width as i32, height as i32))
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0;
        }
        let tx = x as u32 >> TILE_SHIFT;
        let ty = y as u32 >> TILE_SHIFT;
        self.tiles[(ty * self.tiles_x + tx) as usize].get(x as usize & (TILE - 1), y as usize & (TILE - 1))
    }

    pub fn tile_at(&self, tx: u32, ty: u32) -> &MTile {
        static EMPTY: MTile = MTile::Empty;
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return &EMPTY;
        }
        &self.tiles[(ty * self.tiles_x + tx) as usize]
    }

    fn tile_rect(&self, i: usize) -> IRect {
        let tx = i as u32 % self.tiles_x;
        let ty = i as u32 / self.tiles_x;
        IRect::new((tx as usize * TILE) as i32, (ty as usize * TILE) as i32, TILE as i32, TILE as i32)
    }

    fn image_rect(&self) -> IRect {
        IRect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Build a mask with a per-tile generator. `f` gets the tile rect (in image
    /// space, already clipped to the image) and returns the tile.
    fn build<F: Fn(IRect, IRect) -> MTile + Sync>(width: u32, height: u32, f: F) -> Mask {
        let mut m = Mask::empty(width, height);
        let img = m.image_rect();
        let tx = m.tiles_x;
        let tiles: Vec<MTile> = (0..m.tiles.len())
            .into_par_iter()
            .map(|i| {
                let tr = IRect::new(((i as u32 % tx) as usize * TILE) as i32, ((i as u32 / tx) as usize * TILE) as i32, TILE as i32, TILE as i32);
                let clip = tr.intersect(&img);
                f(tr, clip)
            })
            .collect();
        m.tiles = Arc::new(tiles);
        m
    }

    pub fn from_rect(width: u32, height: u32, r: IRect) -> Mask {
        let r = r.intersect(&IRect::new(0, 0, width as i32, height as i32));
        Self::build(width, height, |tr, clip| {
            let c = clip.intersect(&r);
            if c.is_empty() {
                return MTile::Empty;
            }
            if c == tr {
                return MTile::Full;
            }
            let mut d = vec![0u8; TILE_PX];
            for y in c.y..c.bottom() {
                let o = (y - tr.y) as usize * TILE;
                d[o + (c.x - tr.x) as usize..o + (c.right() - tr.x) as usize].fill(255);
            }
            // Edge tiles that are "full" within the image still count as full.
            if c == clip && clip.w as usize == TILE && clip.h as usize == TILE {
                return MTile::Full;
            }
            MTile::Partial(Arc::from(Box::<[u8; TILE_PX]>::try_from(d.into_boxed_slice()).unwrap()))
        })
    }

    /// Rasterise a vector path (anti-aliased) into a mask.
    pub fn from_path(width: u32, height: u32, path: &Path, antialias: bool) -> Mask {
        let b = path.bounds();
        let pr = IRect::from_f(b.left(), b.top(), b.right(), b.bottom()).inflate(1);
        Self::build(width, height, |tr, clip| {
            let c = clip.intersect(&pr);
            if c.is_empty() {
                return MTile::Empty;
            }
            let mut m = tiny_skia::Mask::new(TILE as u32, TILE as u32).unwrap();
            m.fill_path(path, FillRule::EvenOdd, antialias, Transform::from_translate(-tr.x as f32, -tr.y as f32));
            let mut d: Box<[u8; TILE_PX]> = m.data().to_vec().into_boxed_slice().try_into().unwrap();
            // clear pixels outside the image
            if clip.w < TILE as i32 || clip.h < TILE as i32 {
                for y in 0..TILE {
                    for x in 0..TILE {
                        if x as i32 >= clip.w || y as i32 >= clip.h {
                            d[y * TILE + x] = 0;
                        }
                    }
                }
                let full_inside = (0..clip.h as usize).all(|y| d[y * TILE..y * TILE + clip.w as usize].iter().all(|&v| v == 255));
                if full_inside {
                    return MTile::Full;
                }
            }
            MTile::normalize(d)
        })
    }

    /// Build from a predicate-like per-pixel function over the image (parallel).
    pub fn from_fn<F: Fn(i32, i32) -> u8 + Sync>(width: u32, height: u32, bounds: IRect, f: F) -> Mask {
        Self::build(width, height, |tr, clip| {
            let c = clip.intersect(&bounds);
            if c.is_empty() {
                return MTile::Empty;
            }
            let mut d: Box<[u8; TILE_PX]> = vec![0u8; TILE_PX].into_boxed_slice().try_into().unwrap();
            for y in c.y..c.bottom() {
                for x in c.x..c.right() {
                    d[(y - tr.y) as usize * TILE + (x - tr.x) as usize] = f(x, y);
                }
            }
            if clip.w < TILE as i32 || clip.h < TILE as i32 {
                let full = c == clip && (0..clip.h as usize).all(|y| d[y * TILE..y * TILE + clip.w as usize].iter().all(|&v| v == 255));
                if full {
                    return MTile::Full;
                }
            }
            MTile::normalize(d)
        })
    }

    pub fn combine(&self, other: &Mask, mode: CombineMode) -> Mask {
        if mode == CombineMode::Replace {
            return other.clone();
        }
        let mut out = self.clone();
        let tiles: Vec<MTile> = self
            .tiles
            .par_iter()
            .zip(other.tiles.par_iter())
            .map(|(a, b)| {
                use MTile::*;
                match (mode, a, b) {
                    (CombineMode::Union, Full, _) | (CombineMode::Union, _, Full) => return Full,
                    (CombineMode::Union, Empty, x) | (CombineMode::Union, x, Empty) => return x.clone(),
                    (CombineMode::Intersect, Empty, _) | (CombineMode::Intersect, _, Empty) => return Empty,
                    (CombineMode::Intersect, Full, x) | (CombineMode::Intersect, x, Full) => return x.clone(),
                    (CombineMode::Exclude, Empty, _) => return Empty,
                    (CombineMode::Exclude, x, Empty) => return x.clone(),
                    (CombineMode::Exclude, _, Full) => return Empty,
                    (CombineMode::Xor, Empty, x) | (CombineMode::Xor, x, Empty) => return x.clone(),
                    _ => {}
                }
                let mut d = a.to_box();
                let bb = b.to_box();
                for (p, q) in d.iter_mut().zip(bb.iter()) {
                    let (x, y) = (*p as i32, *q as i32);
                    *p = match mode {
                        CombineMode::Union => x.max(y),
                        CombineMode::Intersect => x.min(y),
                        CombineMode::Exclude => (x - y).max(0),
                        CombineMode::Xor => (x - y).abs(),
                        CombineMode::Replace => y,
                    } as u8;
                }
                MTile::normalize(d)
            })
            .collect();
        out.tiles = Arc::new(tiles);
        out.fix_edges();
        out
    }

    pub fn invert(&self) -> Mask {
        let mut out = self.clone();
        let img = self.image_rect();
        let tiles: Vec<MTile> = self
            .tiles
            .par_iter()
            .enumerate()
            .map(|(i, t)| {
                let tr = self.tile_rect(i);
                let clip = tr.intersect(&img);
                let edge = clip != tr;
                match t {
                    MTile::Full => MTile::Empty,
                    MTile::Empty if !edge => MTile::Full,
                    _ => {
                        let mut d = t.to_box();
                        for v in d.iter_mut() {
                            *v = 255 - *v;
                        }
                        MTile::normalize(d)
                    }
                }
            })
            .collect();
        out.tiles = Arc::new(tiles);
        out.fix_edges();
        out
    }

    /// Edge tiles that extend past the image must keep outside pixels at 0;
    /// treat a tile as Full if everything inside the image is selected.
    fn fix_edges(&mut self) {
        let img = self.image_rect();
        let n = self.tiles.len();
        for i in 0..n {
            let tr = self.tile_rect(i);
            let clip = tr.intersect(&img);
            if clip == tr {
                continue;
            }
            let t = self.tiles[i].clone();
            let mut d = t.to_box();
            let mut all_in = true;
            for y in 0..TILE {
                for x in 0..TILE {
                    if x as i32 >= clip.w || y as i32 >= clip.h {
                        d[y * TILE + x] = 0;
                    } else if d[y * TILE + x] != 255 {
                        all_in = false;
                    }
                }
            }
            let nt = if all_in { MTile::Full } else { MTile::normalize(d) };
            Arc::make_mut(&mut self.tiles)[i] = nt;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| matches!(t, MTile::Empty))
    }

    /// Bounding box of selected pixels.
    pub fn bounds(&self) -> Option<IRect> {
        let img = self.image_rect();
        let mut acc: Option<IRect> = None;
        for (i, t) in self.tiles.iter().enumerate() {
            let tr = self.tile_rect(i);
            let r = match t {
                MTile::Empty => continue,
                MTile::Full => tr.intersect(&img),
                MTile::Partial(p) => {
                    let (mut x0, mut y0, mut x1, mut y1) = (TILE, TILE, 0, 0);
                    for y in 0..TILE {
                        for x in 0..TILE {
                            if p[y * TILE + x] != 0 {
                                x0 = x0.min(x);
                                y0 = y0.min(y);
                                x1 = x1.max(x + 1);
                                y1 = y1.max(y + 1);
                            }
                        }
                    }
                    if x1 <= x0 {
                        continue;
                    }
                    IRect::new(tr.x + x0 as i32, tr.y + y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
                }
            };
            acc = Some(acc.map_or(r, |a| a.union(&r)));
        }
        acc
    }

    /// Translate the mask by an integer offset.
    pub fn translated(&self, dx: i32, dy: i32) -> Mask {
        if dx == 0 && dy == 0 {
            return self.clone();
        }
        let src = self.clone();
        let b = self.bounds().unwrap_or_default();
        let nb = IRect::new(b.x + dx, b.y + dy, b.w, b.h);
        Mask::from_fn(self.width, self.height, nb, move |x, y| src.get(x - dx, y - dy))
    }

    /// Marching-ants outline: axis-aligned unit edges between selected
    /// (>=50%) and unselected pixels, merged into runs. Returned as
    /// (x0, y0, x1, y1) in image pixel coordinates.
    pub fn outline(&self) -> Vec<[f32; 4]> {
        let n = self.tiles.len();
        let per: Vec<Vec<[f32; 4]>> = (0..n)
            .into_par_iter()
            .map(|i| {
                let tx = i as u32 % self.tiles_x;
                let ty = i as u32 / self.tiles_x;
                let tr = self.tile_rect(i);
                let me = &self.tiles[i];
                let left = if tx > 0 { Some(self.tile_at(tx - 1, ty)) } else { None };
                let up = if ty > 0 { Some(self.tile_at(tx, ty - 1)) } else { None };
                let right_edge = tx == self.tiles_x - 1;
                let bottom_edge = ty == self.tiles_y - 1;
                let uniform = |t: &MTile| match t {
                    MTile::Empty => Some(false),
                    MTile::Full => Some(true),
                    _ => None,
                };
                let mut segs = Vec::new();
                let on = |t: &MTile, x: usize, y: usize| t.get(x, y) >= 128;
                // Interior edges only matter for partial tiles.
                let interior = uniform(me).is_none();
                // Horizontal edges: boundary above row y (between y-1 and y).
                for y in 0..=TILE {
                    let mut run: Option<usize> = None;
                    for x in 0..=TILE {
                        let edge = if x == TILE {
                            false
                        } else if y == 0 {
                            let a = up.map(|u| on(u, x, TILE - 1)).unwrap_or(false);
                            a != on(me, x, 0)
                        } else if y == TILE {
                            // only at the image bottom (next tile handles otherwise)
                            bottom_edge && on(me, x, TILE - 1)
                        } else {
                            interior && on(me, x, y - 1) != on(me, x, y)
                        };
                        match (edge, run) {
                            (true, None) => run = Some(x),
                            (false, Some(s)) => {
                                segs.push([(tr.x + s as i32) as f32, (tr.y + y as i32) as f32, (tr.x + x as i32) as f32, (tr.y + y as i32) as f32]);
                                run = None;
                            }
                            _ => {}
                        }
                    }
                    if y > 0 && y < TILE && !interior {
                        continue;
                    }
                }
                // Vertical edges: boundary left of column x.
                for x in 0..=TILE {
                    if x > 0 && x < TILE && !interior {
                        continue;
                    }
                    let mut run: Option<usize> = None;
                    for y in 0..=TILE {
                        let edge = if y == TILE {
                            false
                        } else if x == 0 {
                            let a = left.map(|l| on(l, TILE - 1, y)).unwrap_or(false);
                            a != on(me, 0, y)
                        } else if x == TILE {
                            right_edge && on(me, TILE - 1, y)
                        } else {
                            on(me, x - 1, y) != on(me, x, y)
                        };
                        match (edge, run) {
                            (true, None) => run = Some(y),
                            (false, Some(s)) => {
                                segs.push([(tr.x + x as i32) as f32, (tr.y + s as i32) as f32, (tr.x + x as i32) as f32, (tr.y + y as i32) as f32]);
                                run = None;
                            }
                            _ => {}
                        }
                    }
                }
                segs
            })
            .collect();
        per.into_iter().flatten().collect()
    }
}

/// The document selection. `mask == None` means nothing is selected, which
/// Pinta treats as "operate on the whole image".
#[derive(Clone, Default)]
pub struct Selection {
    pub mask: Option<Mask>,
}

impl Selection {
    pub fn is_active(&self) -> bool {
        self.mask.is_some()
    }
    /// Coverage at a pixel (255 when nothing is selected).
    #[inline]
    pub fn coverage(&self, x: i32, y: i32) -> u8 {
        match &self.mask {
            None => 255,
            Some(m) => m.get(x, y),
        }
    }
    pub fn bounds(&self, w: u32, h: u32) -> IRect {
        match &self.mask {
            None => IRect::new(0, 0, w as i32, h as i32),
            Some(m) => m.bounds().unwrap_or_default(),
        }
    }
}
