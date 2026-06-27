// Per-pixel radial gain multiply, in place — a transcription of the CPU
// `RadialGain::at`: a gain that varies with the normalized distance from a center,
// `1 + poly0·r² + poly1·r⁴ + poly2·r⁶`, optionally reciprocated (lens-vignetting
// correction divides by the measured falloff; the creative vignette multiplies).

struct Params {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    inv_norm: f32,
    poly0: f32,
    poly1: f32,
    poly2: f32,
    reciprocal: u32, // 1 = divide by the polynomial instead of multiplying
    // Three trailing pads round the 9-scalar (36-byte) struct up to a 16-byte
    // multiple (48 B) to satisfy std140 uniform struct alignment; do not remove.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@group(0) @binding(1) var<uniform> p: Params;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.width || gid.y >= p.height {
        return;
    }
    let dx = f32(gid.x) - p.center_x;
    let dy = f32(gid.y) - p.center_y;
    let r = sqrt(dx * dx + dy * dy) * p.inv_norm;
    let r2 = r * r;
    let poly = 1.0 + p.poly0 * r2 + p.poly1 * r2 * r2 + p.poly2 * r2 * r2 * r2;
    var g = poly;
    if p.reciprocal == 1u {
        g = 1.0 / poly;
    }
    let o = (gid.y * p.width + gid.x) * 3u;
    data[o] = data[o] * g;
    data[o + 1u] = data[o + 1u] * g;
    data[o + 2u] = data[o + 2u] * g;
}
