// Two-input element-wise combine, in place — a transcription of `CpuBackend`'s
// `combine` for the kinds whose math is portable to WGSL bit-closely:
//   Unsharp:       out = o + gain·(px − o)            (linear, per channel)
//   LocalContrast: out = px + amount·m·(px − o)       (midtone-weighted clarity)
// where `m = midtone_weight(luminance(o))` evaluated exactly as the CPU does.
//
// The L*-perceptual `UnsharpLuma` kind is *not* handled here: it needs the full
// working→XYZ→Lab chain (and its inverse), the same transcendental matrix path
// that keeps chroma-preserving saturation on the CPU; reproducing it in WGSL
// bit-closely enough to hold the equivalence tolerance is not worth it. The GPU
// backend dispatches `UnsharpLuma` to the CPU instead, where CPU == GPU is exact.

struct Params {
    n_pixels: u32,
    row_stride: u32, // invocations per grid row = (workgroups_x * 64)
    kind: u32,       // 0 = Unsharp, 1 = LocalContrast
    gain: f32,       // Unsharp gain, or LocalContrast amount
};

@group(0) @binding(0) var<storage, read_write> img: array<f32>;
@group(0) @binding(1) var<storage, read> other: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

// CIE Lab companding break point δ = 6/29 — must match latent_image::color.
const LAB_DELTA: f32 = 0.20689656;

// Colorimetric relative luminance weights — must match latent_image::color::
// LUMA_WEIGHTS (the Y row of the working RGB→XYZ matrix).
const LUMA_R: f32 = 0.28807107;
const LUMA_G: f32 = 0.71184325;
const LUMA_B: f32 = 8.56539e-5;

fn luminance(c: vec3<f32>) -> f32 {
    return LUMA_R * c.x + LUMA_G * c.y + LUMA_B * c.z;
}

// Lab forward companding f(t) — mirrors latent_image::color::lab_f.
fn lab_f(t: f32) -> f32 {
    let d3 = LAB_DELTA * LAB_DELTA * LAB_DELTA;
    if t > d3 {
        return pow(t, 1.0 / 3.0);
    }
    return t / (3.0 * LAB_DELTA * LAB_DELTA) + 4.0 / 29.0;
}

// Encode a linear value into the perceptual L* tone domain (L*/100), matching
// latent_image::tone::encode — the domain `midtone_weight` evaluates its window in.
fn tone_encode(x: f32) -> f32 {
    return (116.0 * lab_f(max(x, 0.0)) - 16.0) / 100.0;
}

// Midtone window of a base luminance — a transcription of the pipeline
// `midtone_weight`: 1 − (2b − 1)² where b is the L*-encoded clamped luminance.
fn midtone_weight(base_luma: f32) -> f32 {
    let b = tone_encode(clamp(base_luma, 0.0, 1.0));
    return 1.0 - (2.0 * b - 1.0) * (2.0 * b - 1.0);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.y * p.row_stride + gid.x;
    if i >= p.n_pixels {
        return;
    }
    let base = i * 3u;
    let px = vec3<f32>(img[base], img[base + 1u], img[base + 2u]);
    let o = vec3<f32>(other[base], other[base + 1u], other[base + 2u]);

    var out = px;
    switch p.kind {
        case 0u: { // Unsharp: o + gain·(px − o)
            out = o + p.gain * (px - o);
        }
        case 1u: { // LocalContrast: px + amount·m·(px − o)
            let k = p.gain * midtone_weight(luminance(o));
            out = px + k * (px - o);
        }
        default: {}
    }

    img[base] = out.x;
    img[base + 1u] = out.y;
    img[base + 2u] = out.z;
}
