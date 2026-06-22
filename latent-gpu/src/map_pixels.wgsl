// Per-pixel point operations, mirroring the CPU `PointOp` interpretation so the
// two backends agree within floating-point tolerance.
//
// Pixels are a tightly-packed array of f32 (three per pixel, RGB). One
// invocation handles one pixel. Workgroups are dispatched in a 2D grid (the x
// extent alone can exceed the per-dimension limit on large images), so the
// linear pixel index is reconstructed from `row_stride`.

struct Params {
    op: u32,         // 0 = Gain, 1 = Tone, 2 = Saturation
    n_pixels: u32,
    row_stride: u32, // invocations per grid row = (workgroups_x * 64)
    amount: f32,     // saturation amount
    gamma: f32,      // tone perceptual gamma
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
};

@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> lut: array<f32>;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);
const LUT_LAST: u32 = 255u;

// Apply the tone curve to one channel: encode to the perceptual domain, look up
// (linearly interpolated) in the uploaded LUT, then decode back to linear —
// matching `ToneCurve::apply_linear` + `eval` on the CPU.
fn tone_channel(x: f32, gamma: f32) -> f32 {
    let e = pow(clamp(x, 0.0, 1.0), 1.0 / gamma);
    let pos = clamp(e, 0.0, 1.0) * f32(LUT_LAST);
    let i = u32(floor(pos));
    var v: f32;
    if i >= LUT_LAST {
        v = lut[LUT_LAST];
    } else {
        let frac = pos - floor(pos);
        v = lut[i] * (1.0 - frac) + lut[i + 1u] * frac;
    }
    return pow(clamp(v, 0.0, 1.0), gamma);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * params.row_stride + gid.x;
    if i >= params.n_pixels {
        return;
    }
    let base = i * 3u;
    var px = vec3<f32>(data[base], data[base + 1u], data[base + 2u]);

    switch params.op {
        case 0u: {
            px = px * vec3<f32>(params.gain_r, params.gain_g, params.gain_b);
        }
        case 1u: {
            px = vec3<f32>(
                tone_channel(px.x, params.gamma),
                tone_channel(px.y, params.gamma),
                tone_channel(px.z, params.gamma),
            );
        }
        case 2u: {
            let y = dot(px, LUMA);
            // Clamp to ≥0 so over-saturation never emits negative light (matches CPU).
            px = max(vec3<f32>(y) + params.amount * (px - vec3<f32>(y)), vec3<f32>(0.0));
        }
        default: {}
    }

    data[base] = px.x;
    data[base + 1u] = px.y;
    data[base + 2u] = px.z;
}
