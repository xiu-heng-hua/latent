// Resample by inverse mapping: trace each output pixel back through the affine
// transform into the source and bilinearly sample it. A neighbour outside the
// source contributes black, so sampling past the border fades to black —
// matching the CPU `resample` + `sample_bilinear`.

struct Params {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    // Row-major 2x3 affine: src = (m0·x + m1·y + m2, m3·x + m4·y + m5).
    m0: f32,
    m1: f32,
    m2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    _pad0: f32,
    _pad1: f32,
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

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.out_width || gid.y >= p.out_height {
        return;
    }
    let ox = f32(gid.x);
    let oy = f32(gid.y);
    let sx = p.m0 * ox + p.m1 * oy + p.m2;
    let sy = p.m3 * ox + p.m4 * oy + p.m5;

    let x0 = floor(sx);
    let y0 = floor(sy);
    let fx = sx - x0;
    let fy = sy - y0;
    let xi = i32(x0);
    let yi = i32(y0);

    let top = mix(fetch(xi, yi), fetch(xi + 1, yi), fx);
    let bot = mix(fetch(xi, yi + 1), fetch(xi + 1, yi + 1), fx);
    let c = mix(top, bot, fy);

    let o = (gid.y * p.out_width + gid.x) * 3u;
    dst[o] = c.x;
    dst[o + 1u] = c.y;
    dst[o + 2u] = c.z;
}
