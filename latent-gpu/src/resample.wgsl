// Resample by inverse mapping: trace each output pixel back through the
// homography into the source and sample it with the Catmull-Rom cubic, sized to
// the texel's source footprint so a locally-minifying corner is prefiltered, not
// aliased. A tap outside the source contributes black, so sampling past the
// border fades to black — matching the CPU `resample` + `sample_cubic` exactly.

struct Params {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    // Row-major 3x3 homography; src = (m0·x + m1·y + m2, m3·x + m4·y + m5) / w,
    // where w = m6·x + m7·y + m8. Affine is the w ≡ 1 case (m6 = m7 = 0, m8 = 1).
    m0: f32,
    m1: f32,
    m2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    m6: f32,
    m7: f32,
    m8: f32,
    // Three trailing pads round the 13-scalar (52-byte) struct up to a 16-byte
    // multiple (64 B) to satisfy std140 uniform struct alignment; do not remove.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

fn fetch(xi: i32, yi: i32) -> vec3<f32> {
    if xi < 0 || yi < 0 || xi >= i32(p.src_width) || yi >= i32(p.src_height) {
        return vec3<f32>(0.0);
    }
    let idx = (u32(yi) * p.src_width + u32(xi)) * 3u;
    return vec3<f32>(src[idx], src[idx + 1u], src[idx + 2u]);
}

// The Catmull-Rom cubic weight (Mitchell-Netravali B = 0, C = 1/2), on [-2, 2],
// `0` outside. Mirrors the CPU `catmull_rom` coefficient for coefficient.
fn catmull_rom(t: f32) -> f32 {
    let a = abs(t);
    if a < 1.0 {
        return 1.5 * a * a * a - 2.5 * a * a + 1.0;
    } else if a < 2.0 {
        return -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0;
    }
    return 0.0;
}

// Higher-order sample of `src` at the fractional coordinate `(x, y)` with the
// per-axis minification prefilter — a transcription of the CPU `sample_cubic`.
// `footprint` is the source-space extent (in pixels) one output texel covers
// along each axis (the local map Jacobian's column norms); `(1, 1)` is unit
// scale. An axis that minifies (footprint > 1) stretches the kernel's sampling
// step so the cubic low-passes the source region the texel covers; at unit scale
// or magnification (footprint ≤ 1) the step is 1 and the result is pure bicubic.
fn sample_cubic(x: f32, y: f32, footprint: vec2<f32>) -> vec3<f32> {
    let sx = max(footprint.x, 1.0);
    let sy = max(footprint.y, 1.0);
    let half_x = i32(ceil(2.0 * sx));
    let half_y = i32(ceil(2.0 * sy));
    let cx = i32(round(x));
    let cy = i32(round(y));
    let inv_sx = 1.0 / sx;
    let inv_sy = 1.0 / sy;

    // Separable weighted sum. The denominator is the full kernel weight (every
    // tap, in or out of bounds), and an out-of-bounds tap contributes black to
    // the numerator — so a sample past the border fades to black exactly as the
    // CPU sampler does, while the in-bounds region still normalizes correctly.
    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var dy = -half_y; dy <= half_y; dy = dy + 1) {
        let yi = cy + dy;
        let wy = catmull_rom((f32(yi) - y) * inv_sy);
        if wy == 0.0 {
            continue;
        }
        for (var dx = -half_x; dx <= half_x; dx = dx + 1) {
            let xi = cx + dx;
            let wx = catmull_rom((f32(xi) - x) * inv_sx);
            if wx == 0.0 {
                continue;
            }
            let wgt = wx * wy;
            wsum = wsum + wgt;
            acc = acc + wgt * fetch(xi, yi);
        }
    }
    if wsum == 0.0 {
        return vec3<f32>(0.0);
    }
    return acc / wsum;
}

// The source coordinate an output pixel `(ox, oy)` maps to, with the same w ≤ 0
// behind-the-plane sentinel `(-1, -1)` the CPU `Transform::map` returns.
fn map_point(ox: f32, oy: f32) -> vec2<f32> {
    let w = p.m6 * ox + p.m7 * oy + p.m8;
    if w <= 0.0 {
        return vec2<f32>(-1.0, -1.0);
    }
    let mx = (p.m0 * ox + p.m1 * oy + p.m2) / w;
    let my = (p.m3 * ox + p.m4 * oy + p.m5) / w;
    return vec2<f32>(mx, my);
}

// The source-space footprint of one output texel at `(ox, oy)`, from a central
// difference of half a pixel — a transcription of the CPU `map_footprint`. A
// behind-the-plane sample yields a unit footprint (the texel reads black anyway).
fn map_footprint(ox: f32, oy: f32) -> vec2<f32> {
    let r = map_point(ox + 0.5, oy);
    let l = map_point(ox - 0.5, oy);
    let d = map_point(ox, oy + 0.5);
    let u = map_point(ox, oy - 0.5);
    if r.x < 0.0 || l.x < 0.0 || d.x < 0.0 || u.x < 0.0 {
        return vec2<f32>(1.0, 1.0);
    }
    let dx = vec2<f32>(r.x - l.x, r.y - l.y);
    let dy = vec2<f32>(d.x - u.x, d.y - u.y);
    return vec2<f32>(length(dx), length(dy));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.out_width || gid.y >= p.out_height {
        return;
    }
    let ox = f32(gid.x);
    let oy = f32(gid.y);
    let o = (gid.y * p.out_width + gid.x) * 3u;

    let w = p.m6 * ox + p.m7 * oy + p.m8;
    if w <= 0.0 {
        // Behind the projection plane (extreme keystone): no valid source, so
        // read black — exactly as the CPU `Transform::map` sentinel does. Guards
        // against w == 0 feeding inf/NaN into the sampler (undefined in WGSL).
        dst[o] = 0.0;
        dst[o + 1u] = 0.0;
        dst[o + 2u] = 0.0;
        return;
    }
    // `floor`/`round` before `i32`: the cubic indexes around `round(s)`, matching
    // the CPU `x.round() as i32`; do not "simplify" the truncation to `i32(s)`.
    let s = map_point(ox, oy);
    let fp = map_footprint(ox, oy);
    let c = sample_cubic(s.x, s.y, fp);

    dst[o] = c.x;
    dst[o + 1u] = c.y;
    dst[o + 2u] = c.z;
}
