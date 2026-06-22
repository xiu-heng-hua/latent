// One separable box-blur pass (horizontal or vertical), mirroring the CPU
// `blur_axis`: each output pixel is the mean over a `2*radius+1` window along one
// axis, with the border clamped (edge pixels replicated). The caller runs it
// twice — horizontal then vertical — for a full box blur.

struct Params {
    width: u32,
    height: u32,
    radius: u32,
    vertical: u32, // 0 = average along x (rows), 1 = along y (columns)
};

@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.width || gid.y >= p.height {
        return;
    }
    let r = i32(p.radius);
    let n = f32(2 * r + 1);
    var sum = vec3<f32>(0.0);
    for (var d = -r; d <= r; d = d + 1) {
        var sx = i32(gid.x);
        var sy = i32(gid.y);
        if p.vertical == 1u {
            sy = clamp(sy + d, 0, i32(p.height) - 1);
        } else {
            sx = clamp(sx + d, 0, i32(p.width) - 1);
        }
        let idx = (u32(sy) * p.width + u32(sx)) * 3u;
        sum = sum + vec3<f32>(src[idx], src[idx + 1u], src[idx + 2u]);
    }
    let avg = sum / n;
    let o = (gid.y * p.width + gid.x) * 3u;
    dst[o] = avg.x;
    dst[o + 1u] = avg.y;
    dst[o + 2u] = avg.z;
}
