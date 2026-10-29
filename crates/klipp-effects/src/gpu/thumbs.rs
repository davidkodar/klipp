//! Node thumbnails taken inside a GPU chain.
//!
//! The node editor shows each visible node's output as a small sRGB
//! picture. Taken from CPU pixels, every thumbnailed node had to leave
//! the GPU chain and have its whole frame read back — at 4K that doubled
//! the cost of a grade. Here each thumbnail is a downsample recorded into
//! the chain right after its node's dispatch, into one small buffer, and
//! all of them come back in a single readback of a few hundred KB.
//!
//! The picture is the CPU thumbnail's: nearest-neighbour at the same
//! sample positions, clamped, sRGB-encoded and truncated to 8 bits.

use super::{GpuEffectsPipeline, WORKGROUP_SIZE};

/// Thumbnails one run can take; more visible nodes than this fall back
/// to CPU thumbnails.
pub const MAX_CHAIN_THUMBS: usize = 64;

/// [`SHADER_CHAIN_THUMB`] after the pixel-storage hook, as it is
/// compiled: the chain buffer it reads is a frame, so its read goes
/// through `unpack` like every other. Built once, so the pipeline and
/// `shader_sources`' checks read the same text.
pub(super) fn shader_chain_thumb() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}{}", super::shader_pixel_storage_source(), SHADER_CHAIN_THUMB))
}

const SHADER_CHAIN_THUMB: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/output/chain_thumb_body.wgsl"));

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ThumbParams {
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    offset: u32,
    _pad: [u32; 3],
}

/// The thumbnails of one graph run, on the GPU until read.
pub struct ChainThumbs {
    pipeline: wgpu::ComputePipeline,
    /// One uniform buffer per thumbnail: a run records them all before
    /// any is submitted, so they cannot share one.
    params: Vec<wgpu::Buffer>,
    out: wgpu::Buffer,
    /// Mappable copy of `out` for the readback. Its own: the pipeline's
    /// staging buffer is frame-sized, and a small frame's is smaller
    /// than a few thumbnails.
    staging: wgpu::Buffer,
    dst_w: u32,
    dst_h: u32,
    taken: usize,
}

impl ChainThumbs {
    /// Thumbnails of `dst_w` × `dst_h`, on `gpu`'s device.
    pub fn new(gpu: &GpuEffectsPipeline, dst_w: u32, dst_h: u32) -> Self {
        let device = &gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chain_thumb"),
            source: wgpu::ShaderSource::Wgsl(shader_chain_thumb().into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chain_thumb"),
            bind_group_layouts: &[&gpu.pipelines.bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("chain_thumb"),
            layout: Some(&layout),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });
        let size = (MAX_CHAIN_THUMBS as u64) * (dst_w as u64) * (dst_h as u64) * 4;
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chain_thumbs"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chain_thumbs_staging"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, params: Vec::new(), out, staging, dst_w, dst_h, taken: 0 }
    }

    /// Start a run: the next thumbnail taken is number 0.
    pub fn begin(&mut self) {
        self.taken = 0;
    }

    fn bytes_each(&self) -> usize {
        (self.dst_w as usize) * (self.dst_h as usize) * 4
    }
}

impl GpuEffectsPipeline {
    /// Take a thumbnail of the chain buffer `slot_a` names (`buf_a` when
    /// true), ordered after every dispatch recorded so far. Its number,
    /// for [`Self::read_chain_thumbs`]; `None` when it cannot be taken.
    pub fn take_chain_thumb(&mut self, thumbs: &mut ChainThumbs, slot_a: bool, width: u32, height: u32) -> Option<usize> {
        let i = thumbs.taken;
        if self.is_device_lost() || i >= MAX_CHAIN_THUMBS || width == 0 || height == 0 {
            return None;
        }
        let src = if slot_a { self.buf_a.as_ref()? } else { self.buf_b.as_ref()? };
        if src.size() < (width as u64) * (height as u64) * 16 {
            return None;
        }
        while thumbs.params.len() <= i {
            thumbs.params.push(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("chain_thumb_params"),
                size: std::mem::size_of::<ThumbParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let params = ThumbParams {
            src_w: width,
            src_h: height,
            dst_w: thumbs.dst_w,
            dst_h: thumbs.dst_h,
            offset: (i * thumbs.bytes_each() / 4) as u32,
            _pad: [0; 3],
        };
        self.queue.write_buffer(&thumbs.params[i], 0, bytemuck::bytes_of(&params));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_thumb"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: thumbs.out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: thumbs.params[i].as_entire_binding() },
            ],
        });
        let workgroups = (thumbs.dst_w * thumbs.dst_h).div_ceil(WORKGROUP_SIZE);
        match self.chain_encoder.as_mut() {
            Some(enc) => Self::record_compute_dispatch(enc, &thumbs.pipeline, &bind_group, workgroups),
            // No chain open (a seed about to start one): its own submit,
            // queued behind the seed's.
            None => {
                let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("chain_thumb") });
                Self::record_compute_dispatch(&mut enc, &thumbs.pipeline, &bind_group, workgroups);
                self.queue.submit(std::iter::once(enc.finish()));
                self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        thumbs.taken += 1;
        Some(i)
    }

    /// Every thumbnail taken since [`ChainThumbs::begin`], in order, as
    /// sRGB RGBA8. `None` while a chain is still open: its dispatches, and
    /// the thumbnails recorded among them, have not been submitted.
    pub fn read_chain_thumbs(&self, thumbs: &ChainThumbs) -> Option<Vec<Vec<u8>>> {
        if thumbs.taken == 0 {
            return Some(Vec::new());
        }
        if self.chain_encoder.is_some() || self.is_device_lost() {
            return None;
        }
        let each = thumbs.bytes_each();
        let size = (each * thumbs.taken) as u64;
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("chain_thumbs_read") });
        let mut readback = self.readbacks.begin(size);
        enc.copy_buffer_to_buffer(&thumbs.out, 0, &thumbs.staging, 0, size);
        self.queue.submit(std::iter::once(enc.finish()));
        self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let slice = thumbs.staging.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let read = match super::poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let thumbs = data.chunks_exact(each).map(|c| c.to_vec()).collect();
                drop(data);
                Some(thumbs)
            }
            _ => None,
        };
        thumbs.staging.unmap();
        readback.succeeded = read.is_some();
        read
    }
}
