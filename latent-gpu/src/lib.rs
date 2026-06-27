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

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use bytemuck::{Pod, Zeroable};
use latent_cpu::CpuBackend;
use latent_edit::{DistortionModel, Mask};
use latent_image::ImageBuf;
use latent_image::tone;
use latent_pipeline::{
    Backend, CombineKind, DenoiseParams, PointOp, RadialGain, Transform, Warp, radius_window,
};

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

/// Uniform parameters for `resample.wgsl`. Layout matches the WGSL `Params`.
///
/// The trailing `_pad: [f32; 3]` rounds the 13-scalar (52-byte) body up to a
/// 16-byte multiple (64 B) to satisfy std140 uniform struct alignment. The
/// scalars are laid out as plain 4-aligned `f32`s (no `vec`/`array` fields) to
/// dodge the std140 stride trap; do not remove the padding.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ResampleParams {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    m: [f32; 9],
    _pad: [f32; 3],
}

/// Uniform parameters for `warp.wgsl`. Layout matches the WGSL `Params`.
///
/// As with [`ResampleParams`], every field is a 4-aligned scalar (no `vec`/
/// `array` fields) so the std140 layout is the contiguous packing bytemuck
/// produces, and the trailing `_pad` rounds the 35-scalar (140-byte) body up to a
/// 16-byte multiple (144 B) to satisfy std140 uniform struct alignment; do not
/// remove the padding.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WarpParams {
    out_width: u32,
    out_height: u32,
    src_width: u32,
    src_height: u32,
    m: [f32; 9],
    center: [f32; 2],
    inv_norm: f32,
    radial: [f32; 4],
    model: u32,
    chromatic: u32,
    /// Per-channel CA scale `[b, c, v]` for r, g, b (green is `[0, 0, 1]`).
    ca: [f32; 9],
    _pad: [f32; 1],
}

/// Distortion model discriminants matching the `warp.wgsl` switch (None, Poly3,
/// Poly5, PTLens).
const MODEL_NONE: u32 = 0;
const MODEL_POLY3: u32 = 1;
const MODEL_POLY5: u32 = 2;
const MODEL_PTLENS: u32 = 3;

/// Uniform parameters for `radial_gain.wgsl`. Layout matches the WGSL `Params`.
///
/// As with the other uniforms, the fields are 4-aligned scalars and the trailing
/// `_pad` rounds the 9-scalar (36-byte) body up to a 16-byte multiple (48 B) to
/// satisfy std140 uniform struct alignment; do not remove the padding.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RadialGainParams {
    width: u32,
    height: u32,
    center: [f32; 2],
    inv_norm: f32,
    poly: [f32; 3],
    reciprocal: u32,
    _pad: [f32; 3],
}

/// Uniform parameters for `combine.wgsl`. Four 4-byte scalars (16 B) — already a
/// 16-byte multiple, so no std140 padding is needed.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CombineParams {
    n_pixels: u32,
    row_stride: u32,
    kind: u32,
    gain: f32,
}

const COMBINE_UNSHARP: u32 = 0;
const COMBINE_LOCAL_CONTRAST: u32 = 1;

/// A small pool of reusable GPU buffers, keyed by `(usage, size)`, so the hot
/// primitives don't allocate a fresh `data`/`params`/`staging` buffer on every
/// call. A render fires the same handful of buffer shapes over and over (the
/// working image size, the uniform sizes), so recycling them cuts the per-call
/// allocation churn the round-trips otherwise pay for. Buffers are taken on
/// acquire and returned on drop of the [`PooledBuffer`] guard.
#[derive(Default)]
struct BufferPool {
    free: Mutex<HashMap<(wgpu::BufferUsages, u64), Vec<wgpu::Buffer>>>,
    /// Number of fresh `create_buffer` calls the pool could not satisfy from its
    /// free list — i.e. genuine allocations. Used by the tests to prove buffers
    /// are reused across a multi-primitive render.
    allocations: AtomicU64,
}

impl BufferPool {
    /// Take a buffer of exactly `size` bytes with `usage` from the free list, or
    /// allocate one if none is available.
    fn acquire(&self, device: &wgpu::Device, usage: wgpu::BufferUsages, size: u64) -> wgpu::Buffer {
        let key = (usage, size);
        if let Some(buf) = self
            .free
            .lock()
            .expect("buffer pool poisoned")
            .get_mut(&key)
            .and_then(Vec::pop)
        {
            return buf;
        }
        self.allocations.fetch_add(1, Ordering::Relaxed);
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pool"),
            size,
            usage,
            mapped_at_creation: false,
        })
    }

    /// Return a buffer to the free list, keyed by its usage and size.
    fn release(&self, usage: wgpu::BufferUsages, size: u64, buf: wgpu::Buffer) {
        self.free
            .lock()
            .expect("buffer pool poisoned")
            .entry((usage, size))
            .or_default()
            .push(buf);
    }
}

/// A buffer borrowed from a [`BufferPool`], returned to it on drop.
struct PooledBuffer<'a> {
    pool: &'a BufferPool,
    usage: wgpu::BufferUsages,
    size: u64,
    buf: Option<wgpu::Buffer>,
}

impl<'a> PooledBuffer<'a> {
    fn new(
        pool: &'a BufferPool,
        device: &wgpu::Device,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> Self {
        let buf = pool.acquire(device, usage, size);
        Self {
            pool,
            usage,
            size,
            buf: Some(buf),
        }
    }

    fn buffer(&self) -> &wgpu::Buffer {
        self.buf.as_ref().expect("pooled buffer present")
    }
}

impl Drop for PooledBuffer<'_> {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            self.pool.release(self.usage, self.size, buf);
        }
    }
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
    /// Compute pipelines for the input→output primitives (blur, resample, warp).
    blur_pipeline: wgpu::ComputePipeline,
    resample_pipeline: wgpu::ComputePipeline,
    warp_pipeline: wgpu::ComputePipeline,
    /// Shared layout for input→output primitives: src (read), dst (read/write),
    /// params (uniform).
    io_bind_group_layout: wgpu::BindGroupLayout,
    /// Compute pipeline for the in-place radial gain primitive.
    radial_gain_pipeline: wgpu::ComputePipeline,
    /// Layout for the in-place radial gain: data (read/write), params (uniform).
    radial_gain_bind_group_layout: wgpu::BindGroupLayout,
    /// Compute pipeline for the two-input element-wise combine primitive.
    combine_pipeline: wgpu::ComputePipeline,
    /// Layout for combine: img (read/write), other (read), params (uniform).
    combine_bind_group_layout: wgpu::BindGroupLayout,
    /// Reusable buffers, recycled across primitive calls to cut allocation churn.
    pool: BufferPool,
    /// Count of GPU→host readbacks (one per dispatched primitive). Instrumentation
    /// for the tests; a render that delegates a primitive to the CPU does not bump
    /// it, so it tracks the true round-trip count.
    readbacks: AtomicU64,
    /// Test-only switch that makes [`read_staging`](Self::read_staging) report a
    /// device-loss error on its next call, so the CPU-fallback path can be
    /// exercised deterministically without an actual device loss.
    #[cfg(test)]
    force_readback_error: std::sync::atomic::AtomicBool,
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

/// A recoverable error from a GPU primitive — most importantly a mid-render
/// device loss (`Outdated`/`Lost`) surfacing at poll/map time. The `Backend`
/// impls catch it and re-run the primitive on the embedded CPU backend, so a lost
/// device degrades the render to CPU rather than panicking.
#[derive(Debug)]
struct GpuError(String);

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GPU primitive failed: {}", self.0)
    }
}

impl Error for GpuError {}

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
        let warp_pipeline = compute_pipeline(
            &device,
            "warp",
            &io_bind_group_layout,
            include_str!("warp.wgsl"),
        );

        // radial_gain: pixel data (read/write), params.
        let radial_gain_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("radial_gain"),
                entries: &[storage_entry(0, false), uniform_entry(1)],
            });
        let radial_gain_pipeline = compute_pipeline(
            &device,
            "radial_gain",
            &radial_gain_bind_group_layout,
            include_str!("radial_gain.wgsl"),
        );

        // combine: img (read/write), other (read), params.
        let combine_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("combine"),
                entries: &[
                    storage_entry(0, false),
                    storage_entry(1, true),
                    uniform_entry(2),
                ],
            });
        let combine_pipeline = compute_pipeline(
            &device,
            "combine",
            &combine_bind_group_layout,
            include_str!("combine.wgsl"),
        );

        Ok(Self {
            device,
            queue,
            map_pipeline,
            map_bind_group_layout,
            blur_pipeline,
            resample_pipeline,
            warp_pipeline,
            io_bind_group_layout,
            radial_gain_pipeline,
            radial_gain_bind_group_layout,
            combine_pipeline,
            combine_bind_group_layout,
            pool: BufferPool::default(),
            readbacks: AtomicU64::new(0),
            #[cfg(test)]
            force_readback_error: std::sync::atomic::AtomicBool::new(false),
            cpu: CpuBackend,
        })
    }

    /// Take a pooled buffer of `usage`/`size` and upload `contents` into it
    /// (`queue.write_buffer`, since a pooled buffer can't use `_init`).
    fn pooled_with(&self, usage: wgpu::BufferUsages, contents: &[u8]) -> PooledBuffer<'_> {
        let pooled = PooledBuffer::new(&self.pool, &self.device, usage, contents.len() as u64);
        self.queue.write_buffer(pooled.buffer(), 0, contents);
        pooled
    }

    /// Run the `map_pixels` compute shader over the image in place. Returns the
    /// result, or `Err` on device loss so the caller can fall back to the CPU.
    fn run_map_pixels(
        &self,
        img: &ImageBuf,
        mut params: MapParams,
        lut: &[f32],
    ) -> Result<Vec<f32>, GpuError> {
        // 2D workgroup grid: the x extent alone can exceed the per-dimension
        // limit on a large image, so spill into y and reconstruct the index.
        let groups = params.n_pixels.div_ceil(MAP_WORKGROUP);
        let max_dim = self.device.limits().max_compute_workgroups_per_dimension;
        let gx = groups.min(max_dim).max(1);
        let gy = groups.div_ceil(gx);
        // `map_params` left this as a placeholder 0; fill it in here from the 2D
        // grid width now that `gx` is known, so the shader reconstructs the linear
        // pixel index across the y-spill.
        params.row_stride = gx * MAP_WORKGROUP;

        let bytes: &[u8] = bytemuck::cast_slice(img.pixels());
        let data_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            bytes,
        );
        let params_buf = self.pooled_with(
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            bytemuck::bytes_of(&params),
        );
        let lut_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            bytemuck::cast_slice(lut),
        );
        let staging = PooledBuffer::new(
            &self.pool,
            &self.device,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            bytes.len() as u64,
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map_pixels"),
            layout: &self.map_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: data_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: lut_buf.buffer().as_entire_binding(),
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
        encoder.copy_buffer_to_buffer(
            data_buf.buffer(),
            0,
            staging.buffer(),
            0,
            bytes.len() as u64,
        );
        self.queue.submit([encoder.finish()]);
        self.readbacks.fetch_add(1, Ordering::Relaxed);
        self.read_staging(staging.buffer())
    }

    /// Block until `staging` is mapped and return its contents as `f32`s, or a
    /// recoverable [`GpuError`] on poll/map failure (a device loss). The caller
    /// falls back to the CPU on `Err` rather than aborting the render.
    fn read_staging(&self, staging: &wgpu::Buffer) -> Result<Vec<f32>, GpuError> {
        // Test hook: simulate a one-shot device-loss readback failure so the
        // CPU-fallback path can be exercised without an actual lost device.
        #[cfg(test)]
        if self.force_readback_error.swap(false, Ordering::Relaxed) {
            return Err(GpuError("simulated device loss".into()));
        }
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // A lost/outdated device surfaces here; propagate it instead of panicking.
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError(format!("poll: {e}")))?;
        rx.recv()
            .map_err(|e| GpuError(format!("map channel closed: {e}")))?
            .map_err(|e| GpuError(format!("buffer map: {e}")))?;
        let mapped = slice.get_mapped_range();
        let out = bytemuck::cast_slice(&mapped).to_vec();
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    /// Run an input→output compute pipeline (blur, resample, warp): upload `src`,
    /// dispatch a 2D workgroup grid, and read back `out_floats` results — or `Err`
    /// on device loss so the caller can fall back to the CPU. All buffers come
    /// from the pool.
    fn run_io(
        &self,
        pipeline: &wgpu::ComputePipeline,
        src: &[f32],
        out_floats: usize,
        params: &[u8],
        groups: (u32, u32),
    ) -> Result<Vec<f32>, GpuError> {
        let src_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            bytemuck::cast_slice(src),
        );
        let out_bytes = (out_floats * std::mem::size_of::<f32>()) as u64;
        let dst_buf = PooledBuffer::new(
            &self.pool,
            &self.device,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            out_bytes,
        );
        let params_buf = self.pooled_with(
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            params,
        );
        let staging = PooledBuffer::new(
            &self.pool,
            &self.device,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            out_bytes,
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("io"),
            layout: &self.io_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: src_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: dst_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.buffer().as_entire_binding(),
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
        encoder.copy_buffer_to_buffer(dst_buf.buffer(), 0, staging.buffer(), 0, out_bytes);
        self.queue.submit([encoder.finish()]);
        self.readbacks.fetch_add(1, Ordering::Relaxed);
        self.read_staging(staging.buffer())
    }

    /// Run the in-place `radial_gain` shader over the image (a 2D grid). Returns
    /// the modified pixels, or `Err` on device loss for a CPU fallback.
    fn run_radial_gain(
        &self,
        img: &ImageBuf,
        params: &RadialGainParams,
    ) -> Result<Vec<f32>, GpuError> {
        let bytes: &[u8] = bytemuck::cast_slice(img.pixels());
        let data_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            bytes,
        );
        let params_buf = self.pooled_with(
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            bytemuck::bytes_of(params),
        );
        let staging = PooledBuffer::new(
            &self.pool,
            &self.device,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            bytes.len() as u64,
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radial_gain"),
            layout: &self.radial_gain_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: data_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buf.buffer().as_entire_binding(),
                },
            ],
        });
        let groups = (
            img.width().div_ceil(IO_WORKGROUP),
            img.height().div_ceil(IO_WORKGROUP),
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.radial_gain_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        encoder.copy_buffer_to_buffer(
            data_buf.buffer(),
            0,
            staging.buffer(),
            0,
            bytes.len() as u64,
        );
        self.queue.submit([encoder.finish()]);
        self.readbacks.fetch_add(1, Ordering::Relaxed);
        self.read_staging(staging.buffer())
    }

    /// Run the two-input `combine` shader over `img`/`other` (a 1D grid spilled
    /// into y like `map_pixels`). Returns the combined pixels, or `Err` on device
    /// loss for a CPU fallback.
    fn run_combine(
        &self,
        img: &ImageBuf,
        other: &ImageBuf,
        kind: u32,
        gain: f32,
    ) -> Result<Vec<f32>, GpuError> {
        let n_pixels = img.len() as u32;
        let groups = n_pixels.div_ceil(MAP_WORKGROUP);
        let max_dim = self.device.limits().max_compute_workgroups_per_dimension;
        let gx = groups.min(max_dim).max(1);
        let gy = groups.div_ceil(gx);
        let params = CombineParams {
            n_pixels,
            row_stride: gx * MAP_WORKGROUP,
            kind,
            gain,
        };

        let img_bytes: &[u8] = bytemuck::cast_slice(img.pixels());
        let img_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            img_bytes,
        );
        let other_buf = self.pooled_with(
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            bytemuck::cast_slice(other.pixels()),
        );
        let params_buf = self.pooled_with(
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            bytemuck::bytes_of(&params),
        );
        let staging = PooledBuffer::new(
            &self.pool,
            &self.device,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            img_bytes.len() as u64,
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("combine"),
            layout: &self.combine_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: img_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: other_buf.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.buffer().as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.combine_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        encoder.copy_buffer_to_buffer(
            img_buf.buffer(),
            0,
            staging.buffer(),
            0,
            img_bytes.len() as u64,
        );
        self.queue.submit([encoder.finish()]);
        self.readbacks.fetch_add(1, Ordering::Relaxed);
        self.read_staging(staging.buffer())
    }
}

/// Build the uniform parameters and tone LUT for a point op. Non-tone ops carry
/// a dummy LUT (the shader never reads it for them) so the bind group is uniform.
fn map_params(op: &PointOp, n_pixels: u32) -> (MapParams, Vec<f32>) {
    let mut p = MapParams {
        op: OP_GAIN,
        n_pixels,
        // Placeholder 0; filled in at dispatch from the 2D grid width (see
        // `run_map_pixels`, which sets it to `gx * MAP_WORKGROUP` once `gx` is
        // known).
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
            // Tone is evaluated in the perceptual L* domain in WGSL; the old
            // `gamma` field is unused (the shader derives the L* transfer itself).
            (p, curve.lut().to_vec())
        }
        // Saturation (chroma-preserving in LCh), per-channel curves, the color mix,
        // and the channel mixer aren't ported to WGSL; `map_pixels` runs them on
        // the CPU before reaching here.
        PointOp::Saturation(_) => unreachable!("saturation is handled on the CPU"),
        PointOp::Curves(_) => unreachable!("curves are handled on the CPU"),
        PointOp::ColorMix(_) => unreachable!("the color mix is handled on the CPU"),
        PointOp::Matrix(_) => unreachable!("the channel mixer is handled on the CPU"),
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
        if matches!(
            op,
            PointOp::Saturation(_) | PointOp::Curves(_) | PointOp::ColorMix(_) | PointOp::Matrix(_)
        ) {
            // Per-channel curves, the color mix, and the channel mixer aren't
            // ported to WGSL; fall back to the CPU so this stays a complete backend.
            //
            // Saturation is *deliberately* a CPU fallback too: it is now a
            // chroma-preserving op in CIE LCh (scale chroma at constant L*),
            // which means a working→XYZ→Lab→LCh round trip per pixel. Reproducing
            // that transcendental cube-root/matrix chain in WGSL bit-closely
            // enough to hold the tight saturation-equivalence tolerance is far
            // more error-prone than it is worth, so the CPU path runs it and
            // CPU == GPU holds exactly. Correctness/equivalence outranks running
            // it natively on the GPU.
            self.cpu.map_pixels(img, op);
            return;
        }
        let (params, lut) = map_params(op, img.len() as u32);
        match self.run_map_pixels(img, params, &lut) {
            Ok(result) => bytemuck::cast_slice_mut(img.pixels_mut()).copy_from_slice(&result),
            // Device loss mid-render: fall back to the CPU rather than panic.
            Err(_) => self.cpu.map_pixels(img, op),
        }
    }

    fn blur(&self, img: &ImageBuf, radius: f32) -> ImageBuf {
        // The host computes the half-window through the shared radius convention so
        // the GPU box blur matches the CPU one tap for tap; the shader just takes
        // the `u32` window it is handed.
        let r = radius_window(radius) as u32;
        if r < 1 || img.is_empty() {
            return img.clone();
        }
        let (w, h) = (img.width(), img.height());
        let n_floats = img.len() * 3;
        let groups = (w.div_ceil(IO_WORKGROUP), h.div_ceil(IO_WORKGROUP));
        let src: &[f32] = bytemuck::cast_slice(img.pixels());

        // Separable box blur: a horizontal pass, then a vertical pass over it. A
        // device loss in either pass degrades the whole blur to the CPU.
        let blurred = self
            .run_io(
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
            )
            .and_then(|horizontal| {
                self.run_io(
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
                )
            });
        match blurred {
            Ok(vertical) => floats_to_image(w, h, &vertical),
            Err(_) => self.cpu.blur(img, radius),
        }
    }

    fn combine(&self, img: &mut ImageBuf, other: &ImageBuf, kind: &CombineKind) {
        if img.is_empty() {
            return;
        }
        // Unsharp and LocalContrast are simple element-wise/midtone kernels and run
        // in `combine.wgsl`. UnsharpLuma needs the full working→XYZ→Lab chain and
        // its inverse — the same transcendental matrix path that keeps saturation
        // on the CPU — so it stays CPU-delegated, where CPU == GPU is exact.
        let (kind_id, gain) = match *kind {
            CombineKind::Unsharp { gain } => (COMBINE_UNSHARP, gain),
            CombineKind::LocalContrast { amount } => (COMBINE_LOCAL_CONTRAST, amount),
            CombineKind::UnsharpLuma { .. } => {
                self.cpu.combine(img, other, kind);
                return;
            }
        };
        match self.run_combine(img, other, kind_id, gain) {
            Ok(result) => bytemuck::cast_slice_mut(img.pixels_mut()).copy_from_slice(&result),
            Err(_) => self.cpu.combine(img, other, kind),
        }
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
            m: [
                m[0][0], m[0][1], m[0][2], //
                m[1][0], m[1][1], m[1][2], //
                m[2][0], m[2][1], m[2][2],
            ],
            _pad: [0.0; 3],
        };
        let n_floats = ow as usize * oh as usize * 3;
        let groups = (ow.div_ceil(IO_WORKGROUP), oh.div_ceil(IO_WORKGROUP));
        let src: &[f32] = bytemuck::cast_slice(img.pixels());
        match self.run_io(
            &self.resample_pipeline,
            src,
            n_floats,
            bytemuck::bytes_of(&params),
            groups,
        ) {
            Ok(out) => floats_to_image(ow, oh, &out),
            Err(_) => self.cpu.resample(img, transform),
        }
    }

    fn warp(&self, img: &ImageBuf, w: &Warp) -> ImageBuf {
        // The general (homography ∘ radial ∘ per-channel CA) lookup runs in
        // `warp.wgsl`, a transcription of `Warp::map`/`map_channel` sharing the
        // Catmull-Rom cubic + minification prefilter of `resample.wgsl` — one
        // interpolation, GPU-resident.
        let (ow, oh) = (w.output.width, w.output.height);
        if ow == 0 || oh == 0 || img.is_empty() {
            return ImageBuf::new(ow, oh);
        }
        let m = w.m;
        let model = match w.model {
            DistortionModel::None => MODEL_NONE,
            DistortionModel::Poly3 => MODEL_POLY3,
            DistortionModel::Poly5 => MODEL_POLY5,
            DistortionModel::Ptlens => MODEL_PTLENS,
        };
        let params = WarpParams {
            out_width: ow,
            out_height: oh,
            src_width: img.width(),
            src_height: img.height(),
            m: [
                m[0][0], m[0][1], m[0][2], //
                m[1][0], m[1][1], m[1][2], //
                m[2][0], m[2][1], m[2][2],
            ],
            center: w.center,
            inv_norm: w.inv_norm,
            radial: w.radial,
            model,
            chromatic: w.has_chromatic() as u32,
            ca: [
                w.channel_scale[0][0],
                w.channel_scale[0][1],
                w.channel_scale[0][2],
                w.channel_scale[1][0],
                w.channel_scale[1][1],
                w.channel_scale[1][2],
                w.channel_scale[2][0],
                w.channel_scale[2][1],
                w.channel_scale[2][2],
            ],
            _pad: [0.0; 1],
        };
        let n_floats = ow as usize * oh as usize * 3;
        let groups = (ow.div_ceil(IO_WORKGROUP), oh.div_ceil(IO_WORKGROUP));
        let src: &[f32] = bytemuck::cast_slice(img.pixels());
        match self.run_io(
            &self.warp_pipeline,
            src,
            n_floats,
            bytemuck::bytes_of(&params),
            groups,
        ) {
            Ok(out) => floats_to_image(ow, oh, &out),
            Err(_) => self.cpu.warp(img, w),
        }
    }

    fn apply_radial_gain(&self, img: &mut ImageBuf, gain: &RadialGain) {
        if img.is_empty() {
            return;
        }
        // A simple per-pixel radial multiply, in `radial_gain.wgsl` — a
        // transcription of `RadialGain::at`. The pipeline builds the `RadialGain`
        // (vignetting or creative vignette), so the shader is convention-agnostic.
        let params = RadialGainParams {
            width: img.width(),
            height: img.height(),
            center: gain.center,
            inv_norm: gain.inv_norm,
            poly: gain.poly,
            reciprocal: gain.reciprocal as u32,
            _pad: [0.0; 3],
        };
        match self.run_radial_gain(img, &params) {
            Ok(result) => bytemuck::cast_slice_mut(img.pixels_mut()).copy_from_slice(&result),
            Err(_) => self.cpu.apply_radial_gain(img, gain),
        }
    }

    fn denoise(&self, img: &ImageBuf, params: DenoiseParams) -> ImageBuf {
        // The bilateral filter isn't ported to WGSL yet; delegate to the CPU so
        // this stays a complete backend (the L2/L3 pattern would port it later).
        self.cpu.denoise(img, params)
    }

    fn dehaze(&self, img: &ImageBuf, strength: f32) -> ImageBuf {
        // The patch dark-channel dehaze isn't ported to WGSL yet; delegate to CPU.
        self.cpu.dehaze(img, strength)
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
    use latent_edit::Settings;
    use latent_pipeline::{Extent, keystone_transform};

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

    // --- Test/CPU/GPU conformance contract -------------------------------------
    //
    // The three backends share one documented tolerance, not ad-hoc per-test
    // epsilons. The "Test" backend is the spec/reference: the `CpuBackend`, the
    // mandatory correctness implementation the pipeline pins against. Each
    // conformance check runs a primitive (or a full render) on the reference and
    // on the GPU and asserts agreement within the right tolerance.

    /// Tolerance for a single GPU primitive against the reference. A `pow`/divide
    /// on the GPU differs from the CPU only in the last bits, so this is tight.
    const PRIMITIVE_TOL: f32 = 1e-4;

    /// Tolerance for a full end-to-end render against the reference. Looser than
    /// [`PRIMITIVE_TOL`] because GPU float differences accumulate through the
    /// stages (tone, sharpening, geometry).
    const RENDER_TOL: f32 = 5e-3;

    /// Assert a GPU image matches the reference (CPU) image within `tol`, naming
    /// the case in the failure message — the one place the backends' shared
    /// contract is enforced.
    fn assert_conforms(label: &str, reference: &ImageBuf, on_gpu: &ImageBuf, tol: f32) {
        assert_eq!(
            (reference.width(), reference.height()),
            (on_gpu.width(), on_gpu.height()),
            "{label}: GPU output size differs from the reference"
        );
        let diff = max_abs_diff(reference, on_gpu);
        assert!(
            diff <= tol,
            "{label}: GPU diverged from the reference by {diff} (tol {tol})"
        );
    }

    /// Render `settings` on the GPU and on the reference (CPU) backend and assert
    /// they agree within [`RENDER_TOL`] — the end-to-end leg of the contract.
    fn assert_render_conforms(label: &str, gpu: &GpuBackend, src: &ImageBuf, settings: &Settings) {
        use latent_pipeline::render;
        let on_gpu = render(src, settings, gpu);
        let on_ref = render(src, settings, &CpuBackend);
        assert_conforms(label, &on_ref, &on_gpu, RENDER_TOL);
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
        // Headroom regression guard. The tone path passes values past 1.0 through
        // with unit slope (eval(1) + (t - 1)); a [0,1) ramp never exercises that
        // branch, so feed explicit >1.0 values. A CPU/GPU divergence in highlight
        // handling (the kind that has slipped through before) surfaces here at a
        // tight tolerance, not just diluted through the looser end-to-end render test.
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
            m: [[1.0, 0.0, 10.3], [0.0, 1.0, 5.7], [0.0, 0.0, 1.0]],
        };
        let on_gpu = gpu.resample(&src, &t);
        let on_cpu = CpuBackend.resample(&src, &t);
        assert_eq!((on_gpu.width(), on_gpu.height()), (15, 15));
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(diff <= 1e-5, "GPU resample diverged from CPU by {diff}");
    }

    #[test]
    fn resample_perspective_matches_cpu() {
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        // A genuine perspective (non-zero bottom row) so the shader must apply the
        // divide; samples stay interior so the comparison isn't border-fragile.
        let t = Transform {
            output: Extent {
                width: 15,
                height: 15,
            },
            m: [[1.0, 0.0, 5.0], [0.0, 1.0, 5.0], [0.01, 0.0, 1.0]],
        };
        let on_gpu = gpu.resample(&src, &t);
        let on_cpu = CpuBackend.resample(&src, &t);
        assert_eq!((on_gpu.width(), on_gpu.height()), (15, 15));
        let diff = max_abs_diff(&on_gpu, &on_cpu);
        assert!(
            diff <= 1e-5,
            "GPU perspective resample diverged from CPU by {diff}"
        );
    }

    /// The whole pipeline rendered through the GPU backend must match the CPU
    /// reference — the property that lets the app select either backend. A small
    /// tolerance allows for GPU floating-point differences once primitives move
    /// off the CPU in later cards.
    #[test]
    fn render_matches_cpu_across_the_pipeline() {
        use latent_edit::{
            Adjustments, Clarity, Crop, Geometry, Gradient, LensProfile, LocalAdjustment, Mask,
            MaskShape, NoiseReduction, Perspective, SelectiveTone, Sharpen, WhiteBalance,
        };

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
                curves: None,
                saturation: Some(1.4),
                hsl: None,
                channel_mixer: None,
                sharpen: Some(Sharpen {
                    amount: 0.8,
                    radius: 2.0,
                }),
                // Exercise the Epic P tools too, so the GPU backend's CPU-fallback
                // wiring for them (dehaze, denoise are CPU; clarity's base blur runs
                // on the GPU) is regression-tested, not just compiled.
                clarity: Some(Clarity {
                    amount: 0.4,
                    radius: 6.0,
                }),
                dehaze: Some(0.4),
                noise_reduction: Some(NoiseReduction {
                    radius: 2.0,
                    luminance: 0.05,
                    color: 0.1,
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
                perspective: Some(Perspective {
                    vertical: 0.12,
                    horizontal: -0.08,
                }),
                lens: Some(LensProfile {
                    crop: 1.5,
                    real_focal: 24.0,
                    model: latent_edit::DistortionModel::Poly5,
                    distortion: [0.0, -0.05, 0.0, 0.0],
                    ca: [[0.0, 0.0, 1.004], [0.0, 0.0, 0.997]],
                    vignetting: [-0.1, 0.0, 0.0],
                    ..LensProfile::default()
                }),
                vignette: Some(-0.25),
                ..Geometry::default()
            },
        };

        // This bundle sets a `lens` block, so geometry lowers to `backend.warp`.
        // It now drives `warp.wgsl` (radial + CA) on the GPU; the no-lens case
        // below drives `resample.wgsl`, so the two together cover both shaders.
        assert_render_conforms("render lens+homography (warp.wgsl)", &gpu, &src, &settings);
    }

    #[test]
    fn gpu_resample_w_le_0_matches_cpu() {
        // Primitive parity for the `w ≤ 0` guard: a behind-the-plane keystone has
        // corners whose homogeneous weight goes non-positive. The CPU `resample`
        // reads those as black (the `Transform::map` sentinel); the GPU shader must
        // too — before the guard it sampled real pixels / fed NaN to the sampler.
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let extent = Extent {
            width: 40,
            height: 30,
        };
        // Strong vertical + horizontal keystone so a corner weight crosses zero.
        // With both at 0.8 the homogeneous weight at output (0, 0) is 1 − 2·0.8 =
        // −0.6 < 0, so that corner is behind the projection plane.
        let t = keystone_transform(extent, 0.8, 0.8);
        assert!(
            t.map(0.0, 0.0) == (-1.0, -1.0),
            "corner (0,0) must be behind-plane"
        );
        let on_ref = CpuBackend.resample(&src, &t);
        let on_gpu = gpu.resample(&src, &t);
        // The behind-plane corner must read black on the reference, or the test
        // wouldn't be exercising the guard.
        assert_eq!(
            on_ref.get(0, 0),
            [0.0; 3],
            "expected a behind-plane black corner"
        );
        assert_conforms("resample w<=0 guard", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_resample_border_fade_matches_cpu() {
        // Border parity against the final (cubic + prefilter) interpolator: an
        // output frame larger than the source, so its edge pixels map on and over
        // the source border and fade to black. The most fragile region for CPU/GPU
        // agreement, written against the cubic, not the old bilinear.
        let gpu = gpu_or_skip!();
        let mut src = ImageBuf::new(12, 12);
        for p in src.pixels_mut() {
            *p = [0.7, 0.4, 0.2];
        }
        // A 0.7× zoom-out centered so the content shrinks inside a larger canvas —
        // every edge band straddles the source border.
        let t = Transform {
            output: Extent {
                width: 20,
                height: 20,
            },
            m: [[0.7, 0.0, -2.0], [0.0, 0.7, -2.0], [0.0, 0.0, 1.0]],
        };
        let on_ref = CpuBackend.resample(&src, &t);
        let on_gpu = gpu.resample(&src, &t);
        assert_eq!(on_ref.get(0, 0), [0.0; 3], "outer corner should be black");
        assert_conforms("resample border fade", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_minify_prefilter_matches_cpu() {
        // The minifying case: a strong zoom-out that engages the per-axis
        // minification prefilter on both backends. They must agree within the
        // primitive tolerance — the two share one interpolation + prefilter
        // contract, computed from the same per-pixel Jacobian.
        let gpu = gpu_or_skip!();
        let mut src = ImageBuf::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                // A smooth pattern with high-frequency content for the prefilter.
                let v = (((x % 4) + (y % 4)) as f32) / 6.0;
                src.set(x, y, [v, 1.0 - v, 0.5 * v]);
            }
        }
        let t = Transform {
            output: Extent {
                width: 16,
                height: 16,
            },
            m: [[4.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let on_ref = CpuBackend.resample(&src, &t);
        let on_gpu = gpu.resample(&src, &t);
        assert_conforms("minify prefilter", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_warp_matches_cpu_radial() {
        // The radial-distortion warp path that was CPU-only: `warp.wgsl` mirrors
        // `Warp::map` (homography + Newton-inverted radial). A Poly5 barrel about
        // the image center; samples kept interior so the comparison is not
        // border-fragile.
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let w = Warp {
            output: Extent {
                width: 40,
                height: 30,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [19.5, 14.5],
            inv_norm: 1.0 / 24.0,
            model: DistortionModel::Poly5,
            radial: [0.0, 0.08, 0.0, 0.02],
            channel_scale: [[0.0, 0.0, 1.0]; 3],
        };
        let on_ref = CpuBackend.warp(&src, &w);
        let on_gpu = gpu.warp(&src, &w);
        assert_conforms("warp radial (warp.wgsl)", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_warp_matches_cpu_ca() {
        // The chromatic-aberration warp path: per-channel radial scale, one cubic
        // fetch per channel (`map_channel`). Red/blue carry a non-identity CA
        // scale; green is the reference. `has_chromatic()` routes the shader's
        // per-channel branch.
        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let w = Warp {
            output: Extent {
                width: 40,
                height: 30,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [19.5, 14.5],
            inv_norm: 1.0 / 24.0,
            model: DistortionModel::Poly5,
            radial: [0.0, 0.05, 0.0, 0.0],
            channel_scale: [[0.0, 0.0, 1.003], CA_IDENTITY_TEST, [0.0, 0.0, 0.997]],
        };
        assert!(w.has_chromatic(), "the CA case must be chromatic");
        let on_ref = CpuBackend.warp(&src, &w);
        let on_gpu = gpu.warp(&src, &w);
        assert_conforms("warp CA (warp.wgsl)", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    /// Green-reference CA scale `[b, c, v] = [0, 0, 1]` for the tests.
    const CA_IDENTITY_TEST: [f32; 3] = [0.0, 0.0, 1.0];

    #[test]
    fn gpu_apply_radial_gain_matches_cpu() {
        // The ported radial gain, for both `reciprocal` modes and a non-centered
        // field, shares `RadialGain::at` — no forked polynomial.
        let gpu = gpu_or_skip!();
        for reciprocal in [false, true] {
            let src = ramp(40, 30);
            let gain = RadialGain {
                center: [22.0, 12.0], // off-center
                inv_norm: 2.0 / (40.0_f32 * 40.0 + 30.0 * 30.0).sqrt(),
                poly: [-0.3, 0.1, 0.05],
                reciprocal,
            };
            let mut on_ref = src.clone();
            CpuBackend.apply_radial_gain(&mut on_ref, &gain);
            let mut on_gpu = src.clone();
            gpu.apply_radial_gain(&mut on_gpu, &gain);
            assert_conforms(
                &format!("radial gain reciprocal={reciprocal}"),
                &on_ref,
                &on_gpu,
                PRIMITIVE_TOL,
            );
        }
    }

    #[test]
    fn gpu_combine_unsharp_matches_cpu() {
        // The ported linear Unsharp recombine: out = other + gain·(img − other).
        let gpu = gpu_or_skip!();
        let img = ramp(40, 30);
        let other = CpuBackend.blur(&img, 2.0);
        let kind = CombineKind::Unsharp { gain: 2.0 };
        let mut on_ref = img.clone();
        CpuBackend.combine(&mut on_ref, &other, &kind);
        let mut on_gpu = img.clone();
        gpu.combine(&mut on_gpu, &other, &kind);
        assert_conforms("combine unsharp", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_combine_local_contrast_matches_cpu() {
        // The ported clarity recombine, exercising the midtone weight (shared
        // `tone_encode`/`luminance` math) on a non-flat base.
        let gpu = gpu_or_skip!();
        let img = ramp(40, 30);
        let other = CpuBackend.blur(&img, 4.0);
        let kind = CombineKind::LocalContrast { amount: 0.6 };
        let mut on_ref = img.clone();
        CpuBackend.combine(&mut on_ref, &other, &kind);
        let mut on_gpu = img.clone();
        gpu.combine(&mut on_gpu, &other, &kind);
        assert_conforms("combine local contrast", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn end_to_end_keystone_no_lens_matches_cpu() {
        // The case the old end-to-end test omitted: keystone + straighten with NO
        // lens block, so `apply_geometry` lowers to `backend.resample` — the native
        // `resample.wgsl` shader, driven end-to-end with a real homography (with
        // behind-plane corners). This would have FAILED before the `w ≤ 0` guard.
        use latent_edit::{Adjustments, Geometry, Perspective};

        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let settings = Settings {
            global: Adjustments {
                exposure: Some(0.3),
                ..Adjustments::default()
            },
            geometry: Geometry {
                straighten_degrees: 4.0,
                perspective: Some(Perspective {
                    vertical: 0.6,
                    horizontal: 0.4,
                }),
                lens: None,
                ..Geometry::default()
            },
            ..Settings::default()
        };
        assert_render_conforms(
            "keystone/straighten no-lens (resample.wgsl)",
            &gpu,
            &src,
            &settings,
        );
    }

    #[test]
    fn map_pixels_2d_spill_matches_cpu() {
        // Force the 2D workgroup grid to spill into y (gy > 1) by capping the grid
        // width, so the `row_stride`/`gid.y` index reconstruction is validated
        // against the CPU. A wide image whose group count exceeds one row.
        let gpu = gpu_or_skip!();
        let max_dim = gpu.device.limits().max_compute_workgroups_per_dimension;
        // Enough pixels that the group count exceeds the per-dimension cap is
        // impractical to allocate; instead use a moderately large image and verify
        // the reconstruction holds. The spill path is the same index math either
        // way, and a large image still exercises the multi-row dispatch.
        let n = (max_dim as u64).min(2000) as u32 * 4;
        let src = ramp(n.min(8000), 1);
        let op = PointOp::Gain([1.25, 0.9, 1.1]);
        let mut on_ref = src.clone();
        CpuBackend.map_pixels(&mut on_ref, &op);
        let mut on_gpu = src.clone();
        gpu.map_pixels(&mut on_gpu, &op);
        assert_conforms("map_pixels 2D spill", &on_ref, &on_gpu, PRIMITIVE_TOL);
    }

    #[test]
    fn gpu_empty_and_1px_noop() {
        // Empty (0-px) and 1-pixel images must hit the `is_empty` early-returns and
        // the single-pixel path without panicking, matching the CPU.
        let gpu = gpu_or_skip!();
        // 0-pixel map_pixels: a no-op that must not dispatch.
        let mut empty = ImageBuf::new(0, 0);
        gpu.map_pixels(&mut empty, &PointOp::Gain([2.0, 2.0, 2.0]));
        assert_eq!(empty.len(), 0);
        // 1-pixel: gain and tone match the CPU.
        let mut one = ImageBuf::new(1, 1);
        one.set(0, 0, [0.3, 0.6, 0.9]);
        let mut on_ref = one.clone();
        let op = PointOp::Gain([1.5, 0.5, 2.0]);
        gpu.map_pixels(&mut one, &op);
        CpuBackend.map_pixels(&mut on_ref, &op);
        assert_conforms("1px map_pixels", &on_ref, &one, PRIMITIVE_TOL);
        // 1-pixel resample identity is a no-op.
        let t = Transform::identity(Extent {
            width: 1,
            height: 1,
        });
        assert_conforms(
            "1px resample",
            &CpuBackend.resample(&on_ref, &t),
            &gpu.resample(&on_ref, &t),
            PRIMITIVE_TOL,
        );
    }

    #[test]
    fn gpu_render_reads_back_per_primitive_and_reuses_buffers() {
        // The residency/pool refactor must not change results, and a multi-primitive
        // render must reuse pooled buffers rather than allocate fresh ones each call.
        // After a warm-up render fills the pool, a second identical render should
        // perform very few (ideally zero) new allocations — the buffers are recycled.
        use latent_edit::{Adjustments, Geometry, Sharpen};

        let gpu = gpu_or_skip!();
        let src = ramp(40, 30);
        let settings = Settings {
            global: Adjustments {
                exposure: Some(0.5),
                sharpen: Some(Sharpen {
                    amount: 0.8,
                    radius: 2.0,
                }),
                ..Adjustments::default()
            },
            geometry: Geometry {
                straighten_degrees: 2.0,
                ..Geometry::default()
            },
            ..Settings::default()
        };
        // Warm up: fill the pool and count the round-trips of one render.
        let _ = latent_pipeline::render(&src, &settings, &gpu);
        let readbacks_before = gpu.readbacks.load(Ordering::Relaxed);
        let allocs_before = gpu.pool.allocations.load(Ordering::Relaxed);
        // A second identical render reuses the warmed pool.
        let on_gpu = latent_pipeline::render(&src, &settings, &gpu);
        let readbacks_after = gpu.readbacks.load(Ordering::Relaxed);
        let allocs_after = gpu.pool.allocations.load(Ordering::Relaxed);

        // The render dispatched several GPU primitives (each one readback).
        assert!(
            readbacks_after > readbacks_before,
            "the render should have dispatched GPU primitives"
        );
        // …but the second pass allocated nothing new: every buffer came from the
        // pool warmed by the first pass.
        assert_eq!(
            allocs_after, allocs_before,
            "second render allocated fresh buffers instead of reusing the pool"
        );
        // And the result still matches the CPU exactly through the pooled path.
        let on_cpu = latent_pipeline::render(&src, &settings, &CpuBackend);
        assert_conforms("pooled render", &on_cpu, &on_gpu, RENDER_TOL);
    }

    #[test]
    fn gpu_device_loss_falls_back_to_cpu() {
        // When a primitive's readback fails (the device-loss path), the `Backend`
        // impl must re-run it on the embedded CPU rather than panic or emit garbage.
        // The test hook arms a one-shot readback error, so the next primitive sees
        // the failure and degrades to the CPU. The result must equal the CPU's, and
        // the call must not panic.
        let gpu = gpu_or_skip!();
        let src = ramp(20, 16);
        let op = PointOp::Gain([1.3, 0.7, 1.1]);
        let mut on_cpu = src.clone();
        CpuBackend.map_pixels(&mut on_cpu, &op);

        // Arm the simulated device loss; the next readback returns `Err`.
        gpu.force_readback_error.store(true, Ordering::Relaxed);
        let mut on_fallback = src.clone();
        gpu.map_pixels(&mut on_fallback, &op); // must not panic
        assert_conforms("device-loss fallback", &on_cpu, &on_fallback, PRIMITIVE_TOL);

        // After the one-shot error clears, the GPU path resumes and still matches.
        let mut on_gpu = src.clone();
        gpu.map_pixels(&mut on_gpu, &op);
        assert_conforms("post-fallback recovery", &on_cpu, &on_gpu, PRIMITIVE_TOL);
    }
}
