# Patina

A fast image editor written in Rust: a rewrite of [Pinta](https://github.com/PintaProject/Pinta)
that stays smooth on very large images (hundreds of megapixels) by drawing the canvas on
the GPU and doing all heavy work in parallel on tiles.

## Download

Linux builds are on the [Releases page](https://github.com/Kurisu003/patina/releases).
Download `patina-linux-x86_64.tar.gz`, unpack it and run `./patina/patina [images...]`.

To add it to your app menu:

```sh
tar xzf patina-linux-x86_64.tar.gz
install -Dm755 patina/patina ~/.local/bin/patina
install -Dm644 patina/patina.desktop ~/.local/share/applications/patina.desktop
```

The newest development build of every commit is also available under the
[Actions tab](https://github.com/Kurisu003/patina/actions) (open a run, then "Artifacts").

## Why it is fast

- **Tiled, copy-on-write images.** Every layer is made of 256 x 256 tiles that are shared
  until they change. Undo history, previews and edits only copy the tiles they touch,
  so a 20000 x 20000 image costs nothing extra to duplicate or undo.
- **GPU canvas.** The view is drawn with wgpu (Vulkan, Metal or DX12) from a mip pyramid
  of tiles; only visible tiles at the right zoom level are composited and uploaded.
- **Parallel everything.** Effects, fills, flood fills, selections, transforms and
  resampling run per tile on all CPU cores.
- **Interactive previews.** Moving, scaling and rotating a selection renders at the
  resolution shown on screen while dragging and at full quality on release.

Some timings on a 400 MP (20000 x 20000) image: brush stroke segment 0.2 ms, magic wand
130 ms, invert colors 130 ms, rotate 90° 83 ms, Gaussian blur 0.8 s. Run
`patina --selftest [size]` to measure on your machine.

## Features

- Everything from Pinta's toolbox: selections (rectangle, ellipse, lasso, magic wand),
  move selection / move selected, paintbrush, pencil, eraser, paint bucket, gradient,
  color picker, text, line/curve, rectangle, rounded rectangle, ellipse, freeform shape,
  clone stamp, recolor, zoom and pan.
- All of Pinta's adjustments and effects (blurs, distortions, noise, artistic, photo,
  render, stylize, object), with live preview.
- Layers with blend modes, opacity and properties; full undo history panel.
- Open and save PNG, JPEG, BMP, GIF, TIFF, WebP, TGA, ICO and OpenRaster (`.ora`, keeps layers).
- Extras beyond Pinta:
  - Editable line/curve: drag points, click the line to add points, Enter to finish.
  - Brush hardness for the paintbrush, eraser and clone stamp.
  - Live magic wand: change the tolerance after clicking and the selection updates.
  - Move Selected with grab handles: drag handles or Shift+drag to resize,
    hold Alt to keep proportions, right drag to rotate (Shift snaps to 15°).
  - Image > Center Object (horizontally, vertically or both).
  - Paste or drop an image and choose: into this image, a new layer or a new tab,
    and whether to expand the canvas, keep it, or fit to its width or height.
  - Outline Object can use custom outline and gradient colors instead of the palette.
  - Edit > Preferences: default canvas size and background, skin, and which modifier key
    (Shift / Ctrl / Alt) does what (keep proportions, snap angles, square shapes, global fill, ...).
  - Floating windows and the main window remember their positions and size, or turn on
    "Windows attached" to pin Tools, Colors, Layers and History to the canvas corners.
  - Optional **Classic** skin (View > Skin or Edit > Preferences) in Paint.NET light or dark, floating Tools / History /
    Layers / Colors windows (F5-F8) and a color wheel, for people used to Paint.NET.

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| Ctrl+N / Ctrl+O / Ctrl+S | New / Open / Save |
| Ctrl+Z / Ctrl+Y | Undo / Redo |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / Copy / Paste |
| Ctrl+Shift+V | Paste into new layer |
| Ctrl+A / Ctrl+Shift+A / Ctrl+I | Select all / Deselect / Invert selection |
| Ctrl+Shift+I | Invert colors |
| Ctrl+R / Ctrl+Shift+R | Resize image / Resize canvas |
| Ctrl+Shift+X | Crop to selection |
| Ctrl+B / Ctrl+0 | Best fit / Actual size |
| Ctrl+Plus / Ctrl+Minus | Zoom in / out |
| [ and ] | Smaller / larger brush |
| X | Swap primary and secondary color |
| Space + drag | Pan |
| F5 - F8 | Toggle Classic skin windows |

Single letters select tools as in Pinta (S selections, M move, B brush, E eraser, ...).

## Building from source

Requires a recent stable Rust toolchain.

```sh
git clone https://github.com/Kurisu003/patina
cd patina
cargo build --release
./target/release/patina
```

On Debian/Ubuntu you may need `libxkbcommon-dev libwayland-dev libx11-dev`.
Dropping files onto the window needs X11/XWayland (winit limitation); Patina prefers
XWayland when available. Set `PINTA_WAYLAND=1` to force native Wayland.

`patina --selftest [size]` runs headless correctness checks and timings.
`PINTA_SCRIPT=file patina` drives the UI from a script (used for automated UI tests,
see `src/script.rs`).

## License

Copyright (c) 2026 Daniel ([Kurisu003](https://github.com/Kurisu003)).

Patina is licensed under [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/):
you may use, change and share it freely as long as you **credit the author**, **do not use it
commercially**, and **share your changes under the same license**. See `LICENSE`.

Patina is a port of Pinta (MIT, see `LICENSE-PINTA-MIT.txt`), which contains code derived
from Paint.NET 3.36 (MIT, see `LICENSE-PDN.txt`). These notices must be kept. Icons come
from Pinta, Paint.NET and the Silk and Fugue icon sets; see `assets/icons/CREDITS.md`.
