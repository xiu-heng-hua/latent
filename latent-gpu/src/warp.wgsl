// Resample through a general OUTPUT → SOURCE warp — a homography composed with a
// radial distortion and an optional per-channel chromatic-aberration scale — in a
// single interpolation, mirroring the CPU `Warp::map`/`map_channel`. The same
// Catmull-Rom cubic + minification prefilter as `resample.wgsl` is used, so the
// two GPU shaders and the CPU reference share one interpolation contract.

struct Params {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    // Row-major 3x3 homography (output → rectilinear source).
    m0: f32,
    m1: f32,
    m2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    m6: f32,
    m7: f32,
    m8: f32,
    // Radial term: center, reciprocal normalization, the four focal-frame
    // coefficients, and the distortion model selector (0 None, 1 Poly3, 2 Poly5,
    // 3 PTLens) laid out exactly as the `Warp` fields.
    center_x: f32,
    center_y: f32,
    inv_norm: f32,
    radial0: f32,
    radial1: f32,
    radial2: f32,
    radial3: f32,
    model: u32,
    chromatic: u32, // 1 = sample each channel at its own CA radius
    // Per-channel CA scale [b, c, v] for r, g, b (green is the identity [0,0,1]).
    ca_r0: f32,
    ca_r1: f32,
    ca_r2: f32,
    ca_g0: f32,
    ca_g1: f32,
    ca_g2: f32,
    ca_b0: f32,
    ca_b1: f32,
    ca_b2: f32,
    // One trailing pad rounds the 35-scalar (140-byte) struct up to a 16-byte
    // multiple (144 B) to satisfy std140 uniform struct alignment; do not remove.
    _pad0: f32,
};

const MODEL_NONE: u32 = 0u;
const MODEL_POLY3: u32 = 1u;
const MODEL_POLY5: u32 = 2u;
const MODEL_PTLENS: u32 = 3u;

// Newton iterations for the even-model radius inversion — must match the CPU
// `NEWTON_STEPS`.
const NEWTON_STEPS: i32 = 3;

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

// Catmull-Rom cubic (Mitchell B = 0, C = 1/2) — identical to the CPU kernel.
fn catmull_rom(t: f32) -> f32 {
    let a = abs(t);
    if a < 1.0 {
        return 1.5 * a * a * a - 2.5 * a * a + 1.0;
    } else if a < 2.0 {
        return -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0;
    }
    return 0.0;
}

// Higher-order sample with the per-axis minification prefilter — identical to the
// CPU `sample_cubic` and to `resample.wgsl`.
fn sample_cubic(x: f32, y: f32, footprint: vec2<f32>) -> vec3<f32> {
    let sx = max(footprint.x, 1.0);
    let sy = max(footprint.y, 1.0);
    let half_x = i32(ceil(2.0 * sx));
    let half_y = i32(ceil(2.0 * sy));
    let cx = i32(round(x));
    let cy = i32(round(y));
    let inv_sx = 1.0 / sx;
    let inv_sy = 1.0 / sy;

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

// The ratio r_src / r_out for a corrected-output radius — a transcription of the
// CPU `Warp::undistort_ratio` (Newton inversion for the even models, the direct
// PTLENS multiply).
fn undistort_ratio(r_out: f32) -> f32 {
    if r_out == 0.0 {
        return 1.0;
    }
    switch p.model {
        case 1u: { // Poly3: r_out = r_src + k1·r_src³
            let k1 = p.radial1;
            var ru = r_out;
            for (var i = 0; i < NEWTON_STEPS; i = i + 1) {
                let f = ru + k1 * ru * ru * ru - r_out;
                ru = ru - f / (1.0 + 3.0 * k1 * ru * ru);
            }
            return ru / r_out;
        }
        case 2u: { // Poly5: r_out = r_src(1 + k1·r_src² + k2·r_src⁴)
            let k1 = p.radial1;
            let k2 = p.radial3;
            var ru = r_out;
            for (var i = 0; i < NEWTON_STEPS; i = i + 1) {
                let ru2 = ru * ru;
                let f = ru * (1.0 + k1 * ru2 + k2 * ru2 * ru2) - r_out;
                ru = ru - f / (1.0 + 3.0 * k1 * ru2 + 5.0 * k2 * ru2 * ru2);
            }
            return ru / r_out;
        }
        case 3u: { // PTLENS: s = 1 + c·r + b·r² + a·r³ (Horner) at the output radius
            let c = p.radial0;
            let b = p.radial1;
            let a = p.radial2;
            return 1.0 + r_out * (c + r_out * (b + r_out * a));
        }
        default: {
            return 1.0;
        }
    }
}

// The geometric (CA-free) source coordinate of an output pixel — `Warp::map`.
// The behind-the-plane sentinel is `(-1, -1)`, matching the CPU map.
fn map_point(ox: f32, oy: f32) -> vec2<f32> {
    let w = p.m6 * ox + p.m7 * oy + p.m8;
    if w <= 0.0 {
        return vec2<f32>(-1.0, -1.0);
    }
    let ix = (p.m0 * ox + p.m1 * oy + p.m2) / w;
    let iy = (p.m3 * ox + p.m4 * oy + p.m5) / w;
    if p.model == MODEL_NONE {
        return vec2<f32>(ix, iy);
    }
    let dx = ix - p.center_x;
    let dy = iy - p.center_y;
    let r_out = sqrt(dx * dx + dy * dy) * p.inv_norm;
    let s = undistort_ratio(r_out);
    return vec2<f32>(p.center_x + dx * s, p.center_y + dy * s);
}

// The source coordinate channel `c` (0 = r, 1 = g, 2 = b) samples from —
// `Warp::map_channel`: the shared geometry, then the per-channel CA scale of the
// offset from `center`.
fn map_channel(ox: f32, oy: f32, scale: vec3<f32>) -> vec2<f32> {
    let base = map_point(ox, oy);
    if scale.x == 0.0 && scale.y == 0.0 && scale.z == 1.0 {
        return base;
    }
    let dx = base.x - p.center_x;
    let dy = base.y - p.center_y;
    let r = sqrt(dx * dx + dy * dy) * p.inv_norm;
    let s = scale.z + r * (scale.y + r * scale.x);
    return vec2<f32>(p.center_x + dx * s, p.center_y + dy * s);
}

// The source-space footprint of one output texel, from the shared (CA-free) map —
// `map_footprint` over `Warp::map`, so all channels prefilter the same region.
fn warp_footprint(ox: f32, oy: f32) -> vec2<f32> {
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

    let fp = warp_footprint(ox, oy);
    var c: vec3<f32>;
    if p.chromatic == 1u {
        // One cubic fetch per channel, each at its own CA radius (the shared
        // footprint is reused so all three prefilter the same source region).
        let sr = map_channel(ox, oy, vec3<f32>(p.ca_r0, p.ca_r1, p.ca_r2));
        let sg = map_channel(ox, oy, vec3<f32>(p.ca_g0, p.ca_g1, p.ca_g2));
        let sb = map_channel(ox, oy, vec3<f32>(p.ca_b0, p.ca_b1, p.ca_b2));
        c = vec3<f32>(
            sample_cubic(sr.x, sr.y, fp).x,
            sample_cubic(sg.x, sg.y, fp).y,
            sample_cubic(sb.x, sb.y, fp).z,
        );
    } else {
        let s = map_point(ox, oy);
        c = sample_cubic(s.x, s.y, fp);
    }

    dst[o] = c.x;
    dst[o + 1u] = c.y;
    dst[o + 2u] = c.z;
}
