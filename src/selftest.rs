//! `patina --selftest [size]`: headless correctness checks and timings of
//! the core operations on a very large image.

use crate::composite::Pyramid;
use crate::document::Document;
use crate::effects::{self, Progress, RenderCtx};
use crate::surface::{IRect, Surface, TILE, premul};
use crate::tools::{Palette, Pointer, ToolKind, Tools};
use std::time::Instant;

fn t<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let t0 = Instant::now();
    let r = f();
    println!("{:<44} {:>9.1} ms", label, t0.elapsed().as_secs_f64() * 1000.0);
    r
}

fn ptr(x: f32, y: f32) -> Pointer {
    Pointer { pos: [x, y], screen: [x, y], secondary: false, shift: false, ctrl: false, alt: false }
}

/// `--bench-scale [size]`: time Move Selected resize/rotate drag frames.
pub fn bench_scale(size: u32) {
    let (w, h) = (size * 2, size * 2);
    let mut doc = Document::new("bench", w, h, Some([255, 255, 255, 255]));
    let b = doc.state.layers[0].surface.bounds();
    doc.state.layers[0].surface.par_tiles_mut(b, true, |tile, tr| {
        for y in 0..TILE {
            for x in 0..TILE {
                let gx = (tr.x + x as i32) as u32;
                let gy = (tr.y + y as i32) as u32;
                tile.set(x, y, premul([(gx / 7 % 256) as u8, (gy / 5 % 256) as u8, ((gx ^ gy) % 256) as u8, 255]));
            }
        }
    });
    let r = IRect::new((size / 2) as i32, (size / 2) as i32, size as i32, size as i32);
    doc.state.selection = crate::selection::Selection { mask: Some(crate::selection::Mask::from_rect(w, h, r)) };
    doc.commit("sel");
    let mut tools = Tools::default();
    tools.current = ToolKind::MoveSelected;
    tools.pick_radius = 4.0;
    tools.view_zoom = std::env::var("BENCH_ZOOM").ok().and_then(|z| z.parse().ok()).unwrap_or(1.0);
    let mut pal = Palette::default();
    println!("move-selected bench: {w} x {h} image, {size} x {size} selection");
    let corner = [r.right() as f32, r.bottom() as f32];
    t("press on corner handle (lift)", || tools.press(&mut doc, &mut pal, ptr(corner[0], corner[1])));
    let mut times = Vec::new();
    for i in 1..=10 {
        let d = i as f32 * size as f32 * 0.03;
        let t0 = Instant::now();
        tools.drag(&mut doc, &mut pal, ptr(corner[0] + d, corner[1] + d * 0.7));
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{:<44} {:>9.1} ms (avg of 10, max {:.1})", "scale drag frame", times.iter().sum::<f64>() / 10.0, times.iter().cloned().fold(0.0, f64::max));
    t("release", || tools.release(&mut doc, &mut pal, ptr(corner[0] + size as f32 * 0.3, corner[1] + size as f32 * 0.21)));
    // rotate the scaled selection
    let c = [r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0];
    let mut p = ptr(c[0] + 10.0, c[1]);
    p.secondary = true;
    tools.press(&mut doc, &mut pal, p);
    let mut times = Vec::new();
    for i in 1..=10 {
        let a = i as f32 * 0.05;
        let mut p = ptr(c[0] + 10.0 * a.cos(), c[1] + 10.0 * a.sin());
        p.secondary = true;
        let t0 = Instant::now();
        tools.drag(&mut doc, &mut pal, p);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{:<44} {:>9.1} ms (avg of 10, max {:.1})", "rotate drag frame (scaled)", times.iter().sum::<f64>() / 10.0, times.iter().cloned().fold(0.0, f64::max));
    let mut sync = Pyramid::new(w, h);
    t("pyramid sync after", || sync.sync(&doc.state));
}

pub fn run(size: u32) {
    let (w, h) = (size, size);
    println!("self-test on a {w} x {h} image ({:.0} MP)", (w as f64 * h as f64) / 1e6);
    let mut doc = t("create document (white, shared tiles)", || Document::new("big", w, h, Some([255, 255, 255, 255])));

    // Real content everywhere so nothing is shared: a colourful gradient.
    t("fill with gradient (unique tiles)", || {
        let s = &mut doc.state.layers[0].surface;
        let b = s.bounds();
        s.par_tiles_mut(b, true, |tile, tr| {
            for y in 0..TILE {
                for x in 0..TILE {
                    let gx = (tr.x + x as i32) as u32;
                    let gy = (tr.y + y as i32) as u32;
                    let c = premul([(gx / 37 % 256) as u8, (gy / 53 % 256) as u8, ((gx ^ gy) % 256) as u8, 255]);
                    tile.set(x, y, c);
                }
            }
        });
        s.clear_outside();
    });
    doc.commit("fill");
    let mem = doc.state.layers[0].surface.tiles.iter().filter(|t| t.is_some()).count() * TILE * TILE * 4;
    println!("  layer memory: {:.2} GB", mem as f64 / 1e9);

    // Viewport rendering: zoomed-out fit (top pyramid levels) and 100% at a 1920x1080 window.
    let mut pyr = Pyramid::new(w, h);
    pyr.sync(&doc.state);
    let fit_zoom = 1800.0 / w as f32;
    let level = ((1.0 / fit_zoom).log2().floor() as usize).min(pyr.levels.len() - 1);
    let all = IRect::new(0, 0, w as i32, h as i32);
    let want = pyr.visible(level, all);
    t(&format!("first full view, pyramid level {level} ({} tiles)", want.len()), || pyr.ensure(level, &want));
    t("same view again (cached)", || {
        pyr.sync(&doc.state);
        pyr.ensure(level, &want)
    });
    let view = IRect::new(w as i32 / 2, h as i32 / 2, 1920, 1080);
    let want0 = pyr.visible(0, view);
    t("100% view of 1920x1080 window", || pyr.ensure(0, &want0));

    // Paint a long brush stroke and measure per-frame incremental cost.
    let mut tools = Tools::default();
    let mut pal = Palette::default();
    tools.settings.brush_width = 40.0;
    tools.set_tool(None, ToolKind::Paintbrush);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let mut seg_ms = Vec::new();
    tools.press(&mut doc, &mut pal, ptr(cx, cy));
    for i in 1..=500 {
        let a = i as f32 * 0.05;
        let p = ptr(cx + a.cos() * i as f32 * 4.0, cy + a.sin() * i as f32 * 4.0);
        let t0 = Instant::now();
        tools.drag(&mut doc, &mut pal, p);
        pyr.sync(&doc.state);
        pyr.ensure(0, &want0);
        seg_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    tools.release(&mut doc, &mut pal, ptr(cx, cy));
    seg_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("{:<44} {:>9.2} ms median, {:.2} ms p99", "brush segment + recomposite (40px)", seg_ms[250], seg_ms[495]);
    println!("  history steps: {}, tiles changed by stroke: {}", doc.history.len(), changed_tiles(&doc.history[doc.history_pos - 1].state.layers[0].surface, &doc.state.layers[0].surface));

    // Undo / redo are snapshot swaps.
    t("undo", || doc.undo());
    t("redo", || doc.redo());

    // Selections.
    tools.set_tool(None, ToolKind::RectangleSelect);
    t("select all", || crate::ops::select_all(&mut doc));
    t("selection outline (marching ants)", || doc.state.selection.mask.as_ref().unwrap().outline().len());
    tools.set_tool(None, ToolKind::EllipseSelect);
    t("ellipse select 15000x15000 (AA mask)", || {
        tools.press(&mut doc, &mut pal, ptr(1000.0, 1000.0));
        tools.drag(&mut doc, &mut pal, ptr(16000.0f32.min(w as f32 - 1.0), 16000.0f32.min(h as f32 - 1.0)));
        tools.release(&mut doc, &mut pal, ptr(16000.0f32.min(w as f32 - 1.0), 16000.0f32.min(h as f32 - 1.0)));
    });
    let n = t("ellipse outline", || doc.state.selection.mask.as_ref().unwrap().outline().len());
    println!("  outline segments: {n}");
    crate::ops::deselect(&mut doc);

    // Magic wand on a big uniform region (contiguous flood).
    let mut flat = Document::new("flat", w, h, Some([255, 255, 255, 255]));
    tools.set_tool(None, ToolKind::MagicWand);
    tools.settings.tolerance = 0.0;
    t("magic wand contiguous on uniform image", || tools.press(&mut flat, &mut pal, ptr(10.0, 10.0)));
    tools.settings.flood_global = true;
    t("magic wand global on gradient image", || tools.press(&mut doc, &mut pal, ptr(10.0, 10.0)));
    tools.settings.flood_global = false;
    crate::ops::deselect(&mut doc);

    // Effects over the whole image.
    let ctx = RenderCtx { width: w, height: h, sel_bounds: IRect::new(0, 0, w as i32, h as i32), primary: [0, 0, 0, 255], secondary: [255; 4] };
    let reg = effects::registry();
    for name in ["Invert Colors", "Black and White", "Sepia"] {
        let mut e = reg.iter().find(|e| e.name() == name).unwrap().box_clone();
        let src = doc.state.layer().surface.clone();
        let sel = doc.state.selection.clone();
        let out = t(&format!("effect: {name} (whole image)"), || effects::apply(e.as_mut(), &src, &sel, &ctx, &Progress::new()));
        assert!(out.is_some());
    }
    {
        let mut e = reg.iter().find(|e| e.name() == "Gaussian Blur").unwrap().box_clone();
        let src = doc.state.layer().surface.clone();
        let sel = doc.state.selection.clone();
        t("effect: Gaussian Blur r=2 (whole image)", || effects::apply(e.as_mut(), &src, &sel, &ctx, &Progress::new()));
    }

    // Image operations.
    t("rotate 90° (all layers)", || crate::ops::rotate_cw(&mut doc));
    t("flip horizontal", || crate::ops::flip_image_h(&mut doc));
    t("resize to 50% (supersampling)", || crate::ops::resize_image(&mut doc, w / 2, h / 2, crate::ops::Resample::Supersampling));
    doc.undo();
    t("add layer + duplicate", || {
        crate::ops::add_layer(&mut doc);
        crate::ops::duplicate_layer(&mut doc);
    });
    t("flatten 3 layers", || crate::ops::flatten_image(&mut doc));

    // correctness checks
    check_blend();
    check_history();
    check_flood();
    println!("self-test passed");
}

fn changed_tiles(a: &Surface, b: &Surface) -> usize {
    a.tiles
        .iter()
        .zip(b.tiles.iter())
        .filter(|(x, y)| match (x, y) {
            (Some(x), Some(y)) => !std::sync::Arc::ptr_eq(x, y),
            (None, None) => false,
            _ => true,
        })
        .count()
}

fn check_blend() {
    use crate::blend::{BlendMode, blend_row};
    let mut d = [100u8, 100, 100, 255];
    blend_row(&mut d, &[255, 0, 0, 255], 255, BlendMode::Normal);
    assert_eq!(d, [255, 0, 0, 255]);
    let mut d = [200u8, 200, 200, 255];
    blend_row(&mut d, &[128, 128, 128, 255], 255, BlendMode::Multiply);
    assert!((d[0] as i32 - 100).abs() <= 1, "multiply {d:?}");
    let mut d = [0u8, 0, 0, 0];
    blend_row(&mut d, &[128, 0, 0, 128], 255, BlendMode::Screen);
    assert_eq!(d, [128, 0, 0, 128]);
    println!("blend checks ok");
}

fn check_history() {
    let mut doc = Document::new("h", 600, 400, Some([255, 255, 255, 255]));
    let before = doc.state.layers[0].surface.get_pixel(10, 10);
    doc.state.layers[0].surface.set_pixel(10, 10, [0, 0, 0, 255]);
    doc.commit("dot");
    doc.undo();
    assert_eq!(doc.state.layers[0].surface.get_pixel(10, 10), before);
    doc.redo();
    assert_eq!(doc.state.layers[0].surface.get_pixel(10, 10), [0, 0, 0, 255]);
    // ORA round trip
    let dir = std::env::temp_dir().join("patina-selftest.ora");
    crate::ops::add_layer(&mut doc);
    doc.state.layers[1].surface.set_pixel(5, 5, [10, 20, 30, 255]);
    doc.state.layers[1].opacity = 0.5;
    doc.state.layers[1].blend = crate::blend::BlendMode::Overlay;
    crate::io::save(&doc, &dir, 90).unwrap();
    let back = crate::io::load(&dir).unwrap();
    assert_eq!(back.state.layers.len(), 2);
    assert_eq!(back.state.layers[1].surface.get_pixel(5, 5), [10, 20, 30, 255]);
    assert_eq!(back.state.layers[1].blend, crate::blend::BlendMode::Overlay);
    assert!((back.state.layers[1].opacity - 0.5).abs() < 0.01);
    let _ = std::fs::remove_file(dir);
    println!("history + ORA checks ok");
}

fn check_flood() {
    use crate::selection::Selection;
    for (w, h, seed) in [(300u32, 200u32, 1u32), (517, 333, 7), (64, 64, 3), (1000, 70, 11)] {
        let mut s = Surface::new(w, h);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let v = crate::effects::util::hash01(x / 3, y / 2, seed) < 0.55;
                s.set_pixel(x, y, if v { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
            }
        }
        let (sx, sy) = (w as i32 / 2, h as i32 / 2);
        let m = crate::flood::fill_mask(&s, sx, sy, 0.0, false, &Selection::default());
        // naive BFS
        let target = s.get_pixel(sx, sy);
        let mut seen = vec![false; (w * h) as usize];
        let mut q = vec![(sx, sy)];
        seen[(sy as u32 * w + sx as u32) as usize] = true;
        while let Some((x, y)) = q.pop() {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let i = (ny as u32 * w + nx as u32) as usize;
                if !seen[i] && s.get_pixel(nx, ny) == target {
                    seen[i] = true;
                    q.push((nx, ny));
                }
            }
        }
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let a = m.get(x, y) == 255;
                let b = seen[(y as u32 * w + x as u32) as usize];
                assert_eq!(a, b, "flood mismatch at {x},{y} in {w}x{h}");
            }
        }
    }
    println!("flood fill checks ok");
}
