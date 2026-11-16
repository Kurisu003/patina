struct View {
    // image coordinate at the viewport's top-left
    origin: vec2<f32>,
    // physical pixels per image pixel
    scale: f32,
    time: f32,
    viewport: vec2<f32>,
    flags: u32,      // bit0: nearest sampling, bit1: pixel grid, bit2: srgb target
    checker: f32,    // checker square size in physical pixels
    image: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var samp_nearest: sampler;
@group(0) @binding(3) var samp_linear: sampler;

struct TileIn {
    @location(0) rect: vec4<f32>,   // image-space x, y, w, h
    @location(1) uv: vec4<f32>,     // u0, v0, u1, v1
    @location(2) slot: u32,
};

struct TileOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) slot: u32,
    @location(2) img: vec2<f32>,
};

fn to_clip(img: vec2<f32>) -> vec4<f32> {
    let px = (img - view.origin) * view.scale;
    let ndc = px / view.viewport * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_tile(@builtin(vertex_index) vi: u32, t: TileIn) -> TileOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    var o: TileOut;
    let img = t.rect.xy + corner * t.rect.zw;
    o.pos = to_clip(img);
    o.uv = mix(t.uv.xy, t.uv.zw, corner);
    o.slot = t.slot;
    o.img = img;
    return o;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_tile(i: TileOut) -> @location(0) vec4<f32> {
    var c: vec4<f32>;
    if ((view.flags & 1u) != 0u) {
        c = textureSample(tiles, samp_nearest, i.uv, i.slot);
    } else {
        c = textureSample(tiles, samp_linear, i.uv, i.slot);
    }
    // checkerboard behind transparency, fixed in screen space like Pinta
    let p = floor(i.pos.xy / view.checker);
    let odd = (i32(p.x) + i32(p.y)) & 1;
    let bg = select(vec3<f32>(1.0), vec3<f32>(0.8), odd == 1);
    var rgb = c.rgb + bg * (1.0 - c.a);
    if ((view.flags & 2u) != 0u) {
        // pixel grid: darken pixels on the boundary of image pixels
        let f = fract(i.img);
        let w = 1.0 / view.scale;
        if (f.x < w || f.y < w) {
            rgb = mix(rgb, vec3<f32>(0.5), 0.6);
        }
    }
    if ((view.flags & 4u) != 0u) {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}

// ---- marching ants -------------------------------------------------------

struct AntIn {
    @location(0) seg: vec4<f32>, // x0 y0 x1 y1 in image space
};

struct AntOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_ant(@builtin(vertex_index) vi: u32, a: AntIn) -> AntOut {
    let p0 = round((a.seg.xy - view.origin) * view.scale);
    let p1 = round((a.seg.zw - view.origin) * view.scale);
    let lo = min(p0, p1) - vec2<f32>(0.5);
    let hi = max(p0, p1) + vec2<f32>(0.5);
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = mix(lo, hi, corner);
    let ndc = px / view.viewport * 2.0 - 1.0;
    var o: AntOut;
    o.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    return o;
}

@fragment
fn fs_ant(i: AntOut) -> @location(0) vec4<f32> {
    let k = floor((i.pos.x + i.pos.y + view.time * 8.0) / 4.0);
    let on = (i32(k) & 1) == 0;
    let v = select(1.0, 0.0, on);
    return vec4<f32>(v, v, v, 1.0);
}
