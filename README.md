# Pinta-rs

A Rust port of [Pinta](https://github.com/PintaProject/Pinta), rebuilt for very large images.

- Images are sparse, copy-on-write 256 px tiles. Undo snapshots, previews and edits only copy the tiles they touch.
- The canvas is drawn on the GPU (wgpu: Vulkan, Metal, DX12) from a tile mip pyramid, so only visible tiles at the right level are composited and uploaded.
- Effects, fills, transforms and selections run per tile in parallel (rayon).

Run `cargo run --release -- [files]`. `pinta-rs --selftest [size]` runs headless checks and timings.

## License

MIT. Pinta-rs is a port of Pinta (MIT, see `LICENSE-PINTA-MIT.txt`), which contains code
derived from Paint.NET 3.36 (MIT, see `LICENSE-PDN.txt`). Icons are from Pinta, see
`assets/icons/CREDITS.md`.
