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
    gamma: f32,      // unused (tone now uses the L* transfer); kept for layout parity
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
};

@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> lut: array<f32>;

const LUT_LAST: u32 = 255u;

// CIE Lab companding break point δ = 6/29 — must match latent_image::color.
const LAB_DELTA: f32 = 0.20689656;

// The Lab forward companding f(t): cube-root with a linear segment below δ³ so
// the slope stays finite at black. Mirrors latent_image::color::lab_f.
fn lab_f(t: f32) -> f32 {
    let d3 = LAB_DELTA * LAB_DELTA * LAB_DELTA;
    if t > d3 {
        return pow(t, 1.0 / 3.0);
    }
    return t / (3.0 * LAB_DELTA * LAB_DELTA) + 4.0 / 29.0;
}

// Inverse of lab_f. Mirrors latent_image::color::lab_f_inv.
fn lab_f_inv(ft: f32) -> f32 {
    if ft > LAB_DELTA {
        return ft * ft * ft;
    }
    return 3.0 * LAB_DELTA * LAB_DELTA * (ft - 4.0 / 29.0);
}

// Encode a linear channel value into the perceptual L* tone domain (L*/100),
// matching latent_image::tone::encode. Decode is the inverse.
fn tone_encode(x: f32) -> f32 {
    return (116.0 * lab_f(max(x, 0.0)) - 16.0) / 100.0;
}
fn tone_decode(t: f32) -> f32 {
    return lab_f_inv((max(t, 0.0) * 100.0 + 16.0) / 116.0);
}

// Apply the tone curve to one channel: encode to the perceptual L* domain, look
// up (linearly interpolated) in the uploaded LUT, then decode back to linear —
// matching `ToneCurve::apply_linear` + `eval` on the CPU.
fn tone_channel(x: f32) -> f32 {
    let e = tone_encode(x);
    var v: f32;
    if e > 1.0 {
        // Headroom (>1.0) passes through with unit slope: eval(1) + (e - 1) —
        // shape, don't crush. Mirrors ToneCurve::eval's >1 branch.
        v = lut[LUT_LAST] + (e - 1.0);
    } else {
        let pos = clamp(e, 0.0, 1.0) * f32(LUT_LAST);
        let i = u32(floor(pos));
        if i >= LUT_LAST {
            v = lut[LUT_LAST];
        } else {
            let frac = pos - floor(pos);
            v = lut[i] * (1.0 - frac) + lut[i + 1u] * frac;
        }
    }
    return tone_decode(v);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * params.row_stride + gid.x;
    if i >= params.n_pixels {
        return;
    }
    let base = i * 3u;
    var px = vec3<f32>(data[base], data[base + 1u], data[base + 2u]);

    // Only Gain (0) and Tone (1) run on the GPU; Saturation and the CPU-only ops
    // are dispatched to the CPU backend before reaching this shader.
    switch params.op {
        case 0u: {
            px = px * vec3<f32>(params.gain_r, params.gain_g, params.gain_b);
        }
        case 1u: {
            px = vec3<f32>(
                tone_channel(px.x),
                tone_channel(px.y),
                tone_channel(px.z),
            );
        }
        default: {}
    }

    data[base] = px.x;
    data[base + 1u] = px.y;
    data[base + 2u] = px.z;
}
