//! GPU rendering backend (wgpu).
//!
//! A second [`Backend`] that runs primitives on the GPU where it can and falls
//! back to the CPU for the rest. The CPU backend stays the complete, mandatory
//! implementation and the correctness reference; this one is an optional
//! accelerator. Nothing in the pipeline or data model knows it exists — it is
//! selected only at the application's composition root.
//!
//! Acquiring a device can fail (no Vulkan adapter, headless server without a
//! software rasterizer); [`GpuBackend::new`] returns an error in that case so
//! the caller can stay on the CPU. No type here assumes a GPU is present.

use std::error::Error;
use std::fmt;

use bytemuck::{Pod, Zeroable};
use latent_cpu::CpuBackend;
use latent_edit::Mask;
use latent_image::ImageBuf;
use latent_image::tone;
use latent_pipeline::{Backend, CombineKind, PointOp, Transform};
use wgpu::util::DeviceExt;

/// Uniform parameters for the `map_pixels` compute shader. Layout matches the
/// `Params` struct in `map_pixels.wgsl` (all 4-byte scalars, 32 bytes total).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MapParams {
    op: u32,
    n_pixels: u32,
    row_stride: u32,
    amount: f32,
    gamma: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
}

const OP_GAIN: u32 = 0;
const OP_TONE: u32 = 1;
const OP_SATURATION: u32 = 2;

/// Threads per workgroup for `map_pixels` (matches `@workgroup_size` in WGSL).
const MAP_WORKGROUP: u32 = 64;

/// Workgroup side for the 2D input→output primitives (matches WGSL).
const IO_WORKGROUP: u32 = 8;

/// Uniform parameters for one `box_blur.wgsl` pass.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: u32,
    vertical: u32,
}

/// Uniform parameters for `resample.wgsl`. Layout matches the WGSL `Params`
/// (16 scalar fields padded to a 16-byte multiple).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ResampleParams {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    m: [f32; 6],
    _pad: [f32; 2],
}

/// A rendering backend backed by a wgpu device.
///
/// Holds the device/queue plus an embedded [`CpuBackend`]. Each primitive either
/// runs on the GPU or delegates to the CPU one, so a partially-ported backend is
/// always a complete backend.
pub struct GpuBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Compute pipeline for the `map_pixels` primitive.
    map_pipeline: wgpu::ComputePipeline,
    map_bind_group_layout: wgpu::BindGroupLayout,
    /// Compute pipelines for the input→output primitives (blur, resample).
    blur_pipeline: wgpu::ComputePipeline,
    resample_pipeline: wgpu::ComputePipeline,
    /// Shared layout for input→output primitives: src (read), dst (read/write),
    /// params (uniform).
    io_bind_group_layout: wgpu::BindGroupLayout,
    cpu: CpuBackend,
}

/// No usable GPU device could be acquired, so the caller should use the CPU
/// backend instead.
#[derive(Debug)]
pub struct GpuUnavailable(String);

impl fmt::Display for GpuUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no usable GPU device: {}", self.0)
    }
}

impl Error for GpuUnavailable {}

impl GpuBackend {
    /// Try to acquire a GPU device and build a backend. Returns
    /// [`GpuUnavailable`] if no adapter or device is available, so the caller
    /// can fall back to the CPU backend.
    pub fn new() -> Result<Self, GpuUnavailable> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|e| GpuUnavailable(e.to_string()))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("latent-gpu"),
            required_features: wgpu::Features::empty(),
            // Request the adapter's own limits so large images aren't capped by
            // the conservative downlevel defaults.
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| GpuUnavailable(e.to_string()))?;

        // map_pixels: pixel data (read/write), params, tone LUT (read only).
        let map_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("map_pixels"),
                entries: &[
                    storage_entry(0, false),
                    uniform_entry(1),
                    storage_entry(2, true),
                ],
            });
        let map_pipeline = compute_pipeline(
            &device,
            "map_pixels",
            &map_bind_group_layout,
            include_str!("map_pixels.wgsl"),
        );

        // blur and resample: src (read), dst (read/write), params.
        let io_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("io"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    uniform_entry(2),
                ],
            });
        let blur_pipeline = compute_pipeline(
            &device,
            "box_blur",
            &io_bind_group_layout,
            include_str!("box_blur.wgsl"),
        );
        let resample_pipeline = compute_pipeline(
            &device,
            "resample",
            &io_bind_group_layout,
            include_str!("resample.wgsl"),
        );

        Ok(Self {
            device,
            queue,
            map_pipeline,
            map_bind_group_layout,
            blur_pipeline,
            resample_pipeline,
            io_bind_group_layout,
            cpu: CpuBackend,
        })
    }

    /// Run the `map_pixels` compute shader over the image in place.
    fn run_map_pixels(&self, img: &mut ImageBuf, mut params: MapParams, lut: &[f32]) {
        // 2D workgroup grid: the x extent alone can exceed the per-dimension
        // limit on a large image, so spill into y and reconstruct the index.
        let groups = params.n_pixels.div_ceil(MAP_WORKGROUP);
        let max_dim = self.device.limits().max_compute_workgroups_per_dimension;
        let gx = groups.min(max_dim).max(1);
        let gy = groups.div_ceil(gx);
        params.row_stride = gx * MAP_WORKGROUP;

        let bytes: &[u8] = bytemuck::cast_slice(img.pixels());
        let data_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("map_pixels.data"),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("map_pixels.params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let lut_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("map_pixels.lut"),
                contents: bytemuck::cast_slice(lut),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("map_pixels.staging"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map_pixels"),
            layout: &self.map_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: data_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: lut_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.map_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        encoder.copy_buffer_to_buffer(&data_buf, 0, &staging, 0, bytes.len() as u64);
        self.queue.submit([encoder.finish()]);

        // Block until the GPU finishes, then copy the result back in place.
        let result = self.read_staging(&staging);
        bytemuck::cast_slice_mut(img.pixels_mut()).copy_from_slice(&result);
    }

    /// Block until `staging` is mapped and return its contents as `f32`s.
    fn read_staging(&self, staging: &wgpu::Buffer) -> Vec<f32> {
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
        rx.recv().expect("map channel closed").expect("buffer map");
        let mapped = slice.get_mapped_range();
        let out = bytemuck::cast_slice(&mapped).to_vec();
        drop(mapped);
        staging.unmap();
        out
    }

    /// Run an input→output compute pipeline (blur, resample): upload `src`,
    /// dispatch a 2D workgroup grid, and read back `out_floats` results.
    fn run_io(
        &self,
        pipeline: &wgpu::ComputePipeline,
        src: &[f32],
        out_floats: usize,
        params: &[u8],
        groups: (u32, u32),
    ) -> Vec<f32> {
        let src_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("io.src"),
                contents: bytemuck::cast_slice(src),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let out_bytes = (out_floats * std::mem::size_of::<f32>()) as u64;
        let dst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io.dst"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("io.params"),
                contents: params,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io.staging"),
            size: out_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("io"),
            layout: &self.io_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: src_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: dst_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        encoder.copy_buffer_to_buffer(&dst_buf, 0, &staging, 0, out_bytes);
        self.queue.submit([encoder.finish()]);
        self.read_staging(&staging)
    }
}

/// Build the uniform parameters and tone LUT for a point op. Non-tone ops carry
/// a dummy LUT (the shader never reads it for them) so the bind group is uniform.
fn map_params(op: &PointOp, n_pixels: u32) -> (MapParams, Vec<f32>) {
    let mut p = MapParams {
        op: OP_GAIN,
        n_pixels,
        row_stride: 0,
        amount: 0.0,
        gamma: 0.0,
        gain_r: 1.0,
        gain_g: 1.0,
        gain_b: 1.0,
    };
    match op {
        PointOp::Gain(g) => {
            p.op = OP_GAIN;
            [p.gain_r, p.gain_g, p.gain_b] = *g;
            (p, vec![0.0; tone::LUT_SIZE])
        }
        PointOp::Tone(curve) => {
            p.op = OP_TONE;
            p.gamma = tone::GAMMA;
            (p, curve.lut().to_vec())
        }
        PointOp::Saturation(amount) => {
            p.op = OP_SATURATION;
            p.amount = *amount;
            (p, vec![0.0; tone::LUT_SIZE])
        }
    }
}

/// Build a compute pipeline from a single bind group layout and WGSL source.
fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    bind_group_layout: &wgpu::BindGroupLayout,
    wgsl: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(bind_group_layout)],
                immediate_size: 0,
            }),
        ),
        module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        }),
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

/// Build an `ImageBuf` of the given size from a flat row-major RGB `f32` slice.
fn floats_to_image(width: u32, height: u32, data: &[f32]) -> ImageBuf {
    let mut img = ImageBuf::new(width, height);
    bytemuck::cast_slice_mut(img.pixels_mut()).copy_from_slice(data);
    img
}

/// A compute-visible storage-buffer bind group layout entry.
fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A compute-visible uniform-buffer bind group layout entry.
fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Backend for GpuBackend {
    fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp) {
        if img.is_empty() {
            return;
        }
        let (params, lut) = map_params(op, img.len() as u32);
        self.run_map_pixels(img, params, &lut);
    }

    fn blur(&self, img: &ImageBuf, radius: f32) -> ImageBuf {
        let r = radius.round().max(0.0) as u32;
        if r == 0 || img.is_empty() {
            return img.clone();
        }
        let (w, h) = (img.width(), img.height());
        let n_floats = img.len() * 3;
        let groups = (w.div_ceil(IO_WORKGROUP), h.div_ceil(IO_WORKGROUP));
        let src: &[f32] = bytemuck::cast_slice(img.pixels());

        // Separable box blur: a horizontal pass, then a vertical pass over it.
        let horizontal = self.run_io(
            &self.blur_pipeline,
            src,
            n_floats,
            bytemuck::bytes_of(&BlurParams {
                width: w,
                height: h,
                radius: r,
                vertical: 0,
            }),
            groups,
        );
        let vertical = self.run_io(
            &self.blur_pipeline,
            &horizontal,
            n_floats,
            bytemuck::bytes_of(&BlurParams {
                width: w,
                height: h,
                radius: r,
                vertical: 1,
            }),
            groups,
        );
        floats_to_image(w, h, &vertical)
    }

    fn combine(&self, img: &mut ImageBuf, other: &ImageBuf, kind: &CombineKind) {
        self.cpu.combine(img, other, kind);
    }

    fn resample(&self, img: &ImageBuf, transform: &Transform) -> ImageBuf {
        let (ow, oh) = (transform.output.width, transform.output.height);
        if ow == 0 || oh == 0 || img.is_empty() {
            return ImageBuf::new(ow, oh);
        }
        let m = transform.m;
        let params = ResampleParams {
            out_width: ow,
            out_height: oh,
            src_width: img.width(),
            src_height: img.height(),
            m: [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2]],
            _pad: [0.0; 2],
        };
        let n_floats = ow as usize * oh as usize * 3;
        let groups = (ow.div_ceil(IO_WORKGROUP), oh.div_ceil(IO_WORKGROUP));
        let src: &[f32] = bytemuck::cast_slice(img.pixels());
        let out = self.run_io(
            &self.resample_pipeline,
            src,
            n_floats,
            bytemuck::bytes_of(&params),
            groups,
        );
        floats_to_image(ow, oh, &out)
    }

    fn eval_mask(&self, mask: &Mask, source: &ImageBuf) -> Vec<f32> {
        self.cpu.eval_mask(mask, source)
    }

    fn blend(&self, base: &mut ImageBuf, top: &ImageBuf, weights: &[f32], opacity: f32) {
        self.cpu.blend(base, top, weights, opacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_pipeline::Extent;

    /// Acquire a GPU backend, or skip the test (returning early) when no device
    /// is available, so the suite still passes on machines without one. Where an
    /// adapter exists (e.g. a container with a software Vulkan driver) the test
    /// actually runs.
    macro_rules! gpu_or_skip {
        () => {
            match GpuBackend::new() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("skipping GPU test: {e}");
                    return;
                }
            }
        };
    }

    fn ramp(width: u32, height: u32) -> ImageBuf {
        let mut img = ImageBuf::new(width, height);
        let n = (width * height).max(1) as f32;
        for (i, px) in img.pixels_mut().iter_mut().enumerate() {
            let t = i as f32 / n;
            *px = [t, 0.5 * t, 1.0 - t];
        }
        img
    }

    /// Largest absolute per-channel difference between two same-size images.
    fn max_abs_diff(a: &ImageBuf, b: &ImageBuf) -> f32 {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        a.pixels()
            .iter()
            .zip(b.pixels())
            .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
            .fold(0.0, f32::max)
    }

    /// Run a point op on the GPU and on the CPU over the same image, asserting
    /// the largest channel difference stays within `tol`.
    fn assert_map_matches(gpu: &GpuBackend, op: PointOp, tol: f32) {
        let src = ramp(40, 30);
        let mut on_gpu = src.clone();
        gpu.map_pixels(&mut on_gpu, &op);
        let mut on_cpu = src.clone();
        CpuBackend.map_pixels(&mut on_cpu, &op);
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(
            diff <= tol,
            "op {op:?} diverged from CPU by {diff} (tol {tol})"
        );
    }

    #[test]
    fn device_acquires_or_skips() {
        let _gpu = gpu_or_skip!();
    }

    #[test]
    fn map_pixels_gain_matches_cpu() {
        let gpu = gpu_or_skip!();
        // A plain per-channel multiply: should match essentially exactly.
        assert_map_matches(&gpu, PointOp::Gain([1.5, 1.0, 0.5]), 1e-6);
    }

    #[test]
    fn map_pixels_tone_matches_cpu() {
        let gpu = gpu_or_skip!();
        // The tone path uploads the LUT and reproduces the encode/decode gamma
        // round-trip; pow on the GPU differs from the CPU only in the last bits.
        assert_map_matches(&gpu, PointOp::Tone(tone::contrast(0.6)), 1e-3);
    }

    #[test]
    fn map_pixels_tone_above_one_matches_cpu() {
        let gpu = gpu_or_skip!();
        // Headroom regression guard. The tone path extrapolates past 1.0 using the
        // LUT's end slope; a [0,1) ramp never exercises that branch, so feed
        // explicit >1.0 values. A CPU/GPU divergence in highlight handling (the
        // kind that has slipped through before) surfaces here at a tight tolerance,
        // not just diluted through the looser end-to-end render test.
        let mut src = ImageBuf::new(8, 1);
        for (i, px) in src.pixels_mut().iter_mut().enumerate() {
            let v = 1.0 + i as f32 * 0.5; // 1.0, 1.5, …, 4.5 — all above white
            *px = [v, v + 0.5, v + 1.0];
        }
        let op = PointOp::Tone(tone::contrast(0.6));
        let mut on_gpu = src.clone();
        gpu.map_pixels(&mut on_gpu, &op);
        let mut on_cpu = src.clone();
        CpuBackend.map_pixels(&mut on_cpu, &op);
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(diff <= 1e-3, "tone >1.0 diverged from CPU by {diff}");
    }

    #[test]
    fn map_pixels_saturation_matches_cpu() {
        let gpu = gpu_or_skip!();
        assert_map_matches(&gpu, PointOp::Saturation(1.6), 1e-5);
    }

    #[test]
    fn blur_matches_cpu() {
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let on_gpu = gpu.blur(&src, 3.0);
        let on_cpu = CpuBackend.blur(&src, 3.0);
        assert_eq!((on_gpu.width(), on_gpu.height()), (40, 30));
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(diff <= 1e-5, "GPU blur diverged from CPU by {diff}");
    }

    #[test]
    fn resample_identity_matches_cpu() {
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let t = Transform::identity(Extent {
            width: 40,
            height: 30,
        });
        let diff = max_abs_diff(&gpu.resample(&src, &t), &CpuBackend.resample(&src, &t));
        assert!(diff <= 1e-6, "GPU resample (identity) diverged by {diff}");
    }

    #[test]
    fn resample_subpixel_shift_matches_cpu() {
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        // Output 15x15, each pixel sampling a sub-pixel point kept well inside
        // the source — pure interior bilinear, no border discontinuity to make
        // the GPU/CPU comparison fragile.
        let t = Transform {
            output: Extent {
                width: 15,
                height: 15,
            },
            m: [[1.0, 0.0, 10.3], [0.0, 1.0, 5.7]],
        };
        let on_gpu = gpu.resample(&src, &t);
        let on_cpu = CpuBackend.resample(&src, &t);
        assert_eq!((on_gpu.width(), on_gpu.height()), (15, 15));
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(diff <= 1e-5, "GPU resample diverged from CPU by {diff}");
    }

    /// The whole pipeline rendered through the GPU backend must match the CPU
    /// reference — the property that lets the app select either backend. A small
    /// tolerance allows for GPU floating-point differences once primitives move
    /// off the CPU in later cards.
    #[test]
    fn render_matches_cpu_across_the_pipeline() {
        use latent_edit::{
            Adjustments, Crop, Geometry, Gradient, LocalAdjustment, Mask, MaskShape, SelectiveTone,
            Settings, Sharpen, WhiteBalance,
        };
        use latent_pipeline::render;

        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let settings = Settings {
            global: Adjustments {
                white_balance: Some(WhiteBalance {
                    temp: 0.1,
                    tint: -0.05,
                }),
                exposure: Some(0.5),
                tone: Some(SelectiveTone {
                    contrast: 0.3,
                    highlights: -0.2,
                    shadows: 0.2,
                    blacks: 0.1,
                }),
                saturation: Some(1.4),
                sharpen: Some(Sharpen {
                    amount: 0.8,
                    radius: 2.0,
                }),
            },
            locals: vec![LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Gradient(Gradient {
                        x0: 0.0,
                        y0: 0.5,
                        x1: 1.0,
                        y1: 0.5,
                    })],
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments {
                    exposure: Some(-1.0),
                    ..Adjustments::default()
                },
                opacity: 0.75,
            }],
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.1,
                    y: 0.1,
                    width: 0.8,
                    height: 0.8,
                }),
                straighten_degrees: 3.0,
            },
        };

        let on_gpu = render(&src, &settings, &gpu);
        let on_cpu = render(&src, &settings, &CpuBackend);
        assert_eq!(
            (on_gpu.width(), on_gpu.height()),
            (on_cpu.width(), on_cpu.height())
        );
        // map_pixels runs on the GPU now (the rest still delegates to the CPU);
        // the tolerance covers GPU float differences propagated through tone and
        // sharpening.
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(diff <= 5e-3, "GPU render diverged from CPU by {diff}");
    }
}
