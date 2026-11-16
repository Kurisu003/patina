//! Flood fill (contiguous) and global colour match, producing masks.
//!
//! Tolerance follows Pinta: tol = (t^2 * 256), a pixel matches when the
//! squared RGBA distance (straight alpha) <= tol^2 * 4.
//!
//! Implementation: a match bitset is computed in parallel per tile row, then
//! the contiguous fill runs span-by-span on 64-bit words, so filling a huge
//! uniform area costs a few word operations per 64 pixels.

use crate::selection::{MTile, Mask, Selection};
use crate::surface::{IRect, Surface, TILE, TILE_PX, unpremul};
use rayon::prelude::*;
use std::sync::Arc;

pub fn tolerance_value(percent: f32) -> i64 {
    let t = (percent / 100.0).clamp(0.0, 1.0);
    (t * t * 256.0) as i64
}

#[inline]
fn within(a: [u8; 4], b: [u8; 4], tol: i64) -> bool {
    let d = |i: usize| (a[i] as i64 - b[i] as i64).pow(2);
    d(0) + d(1) + d(2) + d(3) <= tol * tol * 4
}

/// Row-major bitset covering the tile grid (stride = tiles_x * 4 words).
struct Bits {
    stride: usize,
    words: Vec<u64>,
}

impl Bits {
    #[inline]
    fn get(&self, x: i32, y: i32) -> bool {
        let w = self.words[y as usize * self.stride + (x as usize >> 6)];
        w >> (x & 63) & 1 != 0
    }
}

fn match_bits(src: &Surface, seed: [u8; 4], tol: i64, limit: &Selection) -> Bits {
    let stride = src.tiles_x as usize * (TILE / 64);
    let rows = src.tiles_y as usize * TILE;
    let mut words = vec![0u64; stride * rows];
    let (w, h) = (src.width as usize, src.height as usize);
    let empty_match = within([0; 4], seed, tol);
    words.par_chunks_mut(stride * TILE).enumerate().for_each(|(ty, chunk)| {
        for tx in 0..src.tiles_x as usize {
            let idx = ty * src.tiles_x as usize + tx;
            let tile = src.tiles[idx].as_deref();
            let sel = limit.mask.as_ref().map(|m| &m.tiles[idx]);
            if matches!(sel, Some(MTile::Empty)) {
                continue;
            }
            for y in 0..TILE {
                let gy = ty * TILE + y;
                if gy >= h {
                    break;
                }
                for k in 0..TILE / 64 {
                    let mut word = 0u64;
                    for b in 0..64 {
                        let lx = k * 64 + b;
                        let gx = tx * TILE + lx;
                        if gx >= w {
                            break;
                        }
                        if let Some(s) = sel {
                            if s.get(lx, y) == 0 {
                                continue;
                            }
                        }
                        let m = match tile {
                            None => empty_match,
                            Some(t) => within(unpremul(t.get(lx, y)), seed, tol),
                        };
                        if m {
                            word |= 1 << b;
                        }
                    }
                    chunk[y * stride + tx * 4 + k] = word;
                }
            }
        }
    });
    Bits { stride, words }
}

fn bits_to_mask(bits: &Bits, w: u32, h: u32) -> Mask {
    let mut m = Mask::empty(w, h);
    let tiles_x = m.tiles_x as usize;
    let tiles: Vec<MTile> = (0..m.tiles.len())
        .into_par_iter()
        .map(|i| {
            let tx = i % tiles_x;
            let ty = i / tiles_x;
            let tile_rows = (ty * TILE..(ty * TILE + TILE).min(h as usize)).collect::<Vec<_>>();
            let full_w = ((w as usize).saturating_sub(tx * TILE)).min(TILE);
            let all_zero = tile_rows.iter().all(|&y| (0..4).all(|k| bits.words[y * bits.stride + tx * 4 + k] == 0));
            if all_zero {
                return MTile::Empty;
            }
            let row_full = |y: usize| (0..full_w).all(|x| bits.words[y * bits.stride + tx * 4 + x / 64] >> (x % 64) & 1 != 0);
            if tile_rows.iter().all(|&y| row_full(y)) {
                return MTile::Full;
            }
            let mut d: Box<[u8; TILE_PX]> = vec![0u8; TILE_PX].into_boxed_slice().try_into().unwrap();
            for &y in &tile_rows {
                for x in 0..full_w {
                    if bits.words[y * bits.stride + tx * 4 + x / 64] >> (x % 64) & 1 != 0 {
                        d[(y - ty * TILE) * TILE + x] = 255;
                    }
                }
            }
            MTile::Partial(Arc::from(d))
        })
        .collect();
    m.tiles = Arc::new(tiles);
    m
}

/// Returns a mask of matching pixels. `limit` restricts filling to the
/// selection when the selection is active.
pub fn fill_mask(src: &Surface, x: i32, y: i32, tolerance_pct: f32, global: bool, limit: &Selection) -> Mask {
    let (w, h) = (src.width, src.height);
    let tol = tolerance_value(tolerance_pct);
    let seed = unpremul(src.get_pixel(x, y));
    let ok = match_bits(src, seed, tol, limit);
    if global {
        return bits_to_mask(&ok, w, h);
    }
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || !ok.get(x, y) {
        return Mask::empty(w, h);
    }
    let mut done = Bits { stride: ok.stride, words: vec![0u64; ok.words.len()] };
    let stride = ok.stride;
    // fillable = ok & !done
    let avail = |done: &Bits, px: i32, py: i32| {
        let i = py as usize * stride + (px as usize >> 6);
        (ok.words[i] & !done.words[i]) >> (px & 63) & 1 != 0
    };
    let wi = w as i32;
    let hi = h as i32;
    let mut stack = vec![(x, y)];
    while let Some((sx, sy)) = stack.pop() {
        if !avail(&done, sx, sy) {
            continue;
        }
        // extend left / right using whole words where possible
        let row = sy as usize * stride;
        let mut l = sx;
        while l > 0 {
            let nx = l - 1;
            if nx & 63 == 63 && nx >= 63 {
                let wv = ok.words[row + (nx as usize >> 6)] & !done.words[row + (nx as usize >> 6)];
                if wv == u64::MAX {
                    l -= 64;
                    continue;
                }
            }
            if avail(&done, nx, sy) { l = nx } else { break }
        }
        let mut r = sx;
        while r + 1 < wi {
            let nx = r + 1;
            if nx & 63 == 0 && nx + 63 < wi {
                let wv = ok.words[row + (nx as usize >> 6)] & !done.words[row + (nx as usize >> 6)];
                if wv == u64::MAX {
                    r += 64;
                    continue;
                }
            }
            if avail(&done, nx, sy) { r = nx } else { break }
        }
        // mark [l, r] done
        let (a, b) = (l as usize, r as usize);
        let (wa, wb) = (a >> 6, b >> 6);
        for wi_ in wa..=wb {
            let lo = if wi_ == wa { a & 63 } else { 0 };
            let hi_ = if wi_ == wb { b & 63 } else { 63 };
            let mask = if hi_ - lo == 63 { u64::MAX } else { ((1u64 << (hi_ - lo + 1)) - 1) << lo };
            done.words[row + wi_] |= mask;
        }
        // seed runs in the rows above and below
        for ny in [sy - 1, sy + 1] {
            if ny < 0 || ny >= hi {
                continue;
            }
            let nrow = ny as usize * stride;
            let mut px = l;
            while px <= r {
                let wi_ = px as usize >> 6;
                let bits = ok.words[nrow + wi_] & !done.words[nrow + wi_];
                let shifted = bits >> (px & 63);
                if shifted == 0 {
                    px = ((wi_ as i32) + 1) << 6;
                    continue;
                }
                let skip = shifted.trailing_zeros() as i32;
                let start = px + skip;
                if start > r {
                    break;
                }
                stack.push((start, ny));
                // skip past this run
                let run = (bits >> (start & 63)).trailing_ones() as i32;
                px = start + run.max(1);
            }
        }
    }
    let _ = IRect::default();
    bits_to_mask(&done, w, h)
}
