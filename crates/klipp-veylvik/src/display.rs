//! Klipp's viewer for Veylvik's: the Veylvik page's viewer shows its
//! picture as Klipp's viewer shows a frame, through the same function:
//! into the project's working space, its display transform (BT.709's
//! encode, BT.2020's basis change and gamut compression, or ACES's RRT and
//! Rec.709 ODT), the monitor's transfer, then the viewer's LUT. So a clip
//! looks the same on the Veylvik page as on Edit.
//!
//! The picture comes from the viewer, at the viewer's size; the encode runs
//! on Veylvik's preview worker when the frame, the exposure or the display
//! changes, never on a pan or a zoom.
//!
//! On Klipp's own device the encode is Klipp's GPU display encode, the one
//! its viewer plays through: the picture never comes to the CPU as floats,
//! only its display bytes do. Elsewhere, and for a BT.1886 monitor (which
//! Klipp's GPU encode has no curve for), the CPU encode does the same work.

use std::sync::{Arc, Mutex};

use klipp_color::lut::LutData;
use klipp_core::{SdrTransfer, ToneMapOp, WorkingColorspace};
use klipp_effects::gpu::GpuEffectsPipeline;
use veylvik_editor::host::HostDisplay;
use veylvik_preview::screen::HostEncode;

/// The project's view, as Klipp's viewer has it now.
pub struct KlippDisplay {
    working: WorkingColorspace,
    transfer: SdrTransfer,
    lut: Option<Arc<LutData>>,
    identity: u64,
    /// Whether `encode`'s device is Klipp's shared one, so Klipp's GPU
    /// encode can take the picture where it is.
    on_shared_device: bool,
    gpu: Mutex<GpuState>,
}

enum GpuState {
    Untried,
    Ready(Box<Gpu>),
    /// Made and failed, or lost: the CPU encode from here on.
    Off,
}

/// Klipp's display encode, and the step before it.
struct Gpu {
    effects: GpuEffectsPipeline,
    prepare: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    straight: Option<wgpu::Buffer>,
    /// The ACES output transform's or BT.2020's tables, made once.
    tables: Vec<f32>,
    finish: wgpu::ComputePipeline,
    finish_layout: wgpu::BindGroupLayout,
    finish_params: wgpu::Buffer,
    /// The viewer's LUT as `LutData` holds it, or one zero when none.
    lut: wgpu::Buffer,
    lut_size: u32,
    bytes: Option<wgpu::Buffer>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FinishParams {
    width: u32,
    height: u32,
    n: u32,
    lut_on: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PrepareParams {
    m: [[f32; 4]; 3],
    count: u32,
    _pad: [u32; 3],
}

impl KlippDisplay {
    /// The viewer of a project in `working`, on a monitor of `transfer`,
    /// with `lut` (the project's viewing LUT and the monitor's calibration,
    /// as one) when one is set.
    pub fn new(working: WorkingColorspace, transfer: SdrTransfer, lut: Option<Arc<LutData>>) -> Self {
        // Named by what makes its pictures: the same in any session, so a
        // picture Veylvik keeps on disk is never shown under another view.
        let mut identity = crate::mix(0x6b6c_6970_7076_6965 ^ working as u64);
        identity = crate::mix(identity ^ transfer as u64);
        if let Some(lut) = &lut {
            identity = crate::mix(identity ^ lut.size as u64);
            for chunk in lut.data.chunks(64) {
                let mut word = 0u64;
                for v in chunk {
                    word = word.rotate_left(7) ^ u64::from(v.to_bits());
                }
                identity = crate::mix(identity ^ word);
            }
        }
        Self { working, transfer, lut, identity: identity.max(1), on_shared_device: false, gpu: Mutex::new(GpuState::Untried) }
    }

    /// The same display, told that the device `encode` is given is
    /// Klipp's shared one (`GpuContext::shared`), so the encode runs there
    /// on the GPU. Its pictures are the same either way.
    pub fn on_shared_device(mut self) -> Self {
        self.on_shared_device = true;
        self
    }

    /// Whether the last encode ran on the GPU.
    pub fn encodes_on_gpu(&self) -> bool {
        matches!(*self.gpu.lock().unwrap_or_else(|e| e.into_inner()), GpuState::Ready(_))
    }

    /// The display-encoded bytes of `picture` (RGBA, scene-linear Rec.709,
    /// premultiplied), as Klipp's viewer shows them, premultiplied again.
    pub fn encode_pixels(&self, picture: &[f32]) -> Vec<u8> {
        let straight = self.straight_working(picture);
        // Klipp's viewer's own encode, tone map and all, as it runs on a frame.
        let bytes = klipp_color::display::display_transform_ws_dithered_to(&straight, ToneMapOp::None, self.working, self.transfer);
        self.finish(bytes)
    }

    /// `picture` as Klipp's frames are: straight colour in the working
    /// space. `display_in.wgsl` is the twin.
    fn straight_working(&self, picture: &[f32]) -> Vec<f32> {
        let to_working = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, self.working);
        picture
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = p[3];
                let c = if a > 1e-6 { [p[0] / a, p[1] / a, p[2] / a] } else { [0.0; 3] };
                let w = to_working.map_or(c, |m| std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]));
                [w[0], w[1], w[2], a]
            })
            .collect()
    }

    /// Display bytes through the viewer's LUT, premultiplied as Veylvik's
    /// viewer takes them; opaque pixels as they are.
    fn finish(&self, mut bytes: Vec<u8>) -> Vec<u8> {
        if let Some(lut) = &self.lut {
            klipp_color::lut::apply_3d_lut(&mut bytes, lut, 1.0);
        }
        for p in bytes.as_chunks_mut::<4>().0 {
            if p[3] < 255 {
                let a = u16::from(p[3]);
                for c in &mut p[..3] {
                    *c = ((u16::from(*c) * a + 127) / 255) as u8;
                }
            }
        }
        bytes
    }

    /// `picture` encoded by Klipp's GPU encode, through the LUT and
    /// premultiplied there too, `None` when it cannot run here and the CPU
    /// encode is to.
    fn encode_on_gpu(&self, device: &wgpu::Device, queue: &wgpu::Queue, picture: &wgpu::Buffer, width: u32, height: u32) -> Option<wgpu::Texture> {
        if !self.on_shared_device || self.transfer != SdrTransfer::Srgb {
            return None;
        }
        let mut state = self.gpu.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, GpuState::Untried) {
            *state = match Gpu::new(device, self.working, self.lut.as_deref()) {
                Some(g) => GpuState::Ready(Box::new(g)),
                None => {
                    tracing::warn!("the Veylvik page's view: Klipp's GPU encode is unavailable, encoding on the CPU");
                    GpuState::Off
                }
            };
        }
        let GpuState::Ready(gpu) = &mut *state else { return None };
        let texture = gpu.encode(device, queue, picture, width, height, self.working);
        if texture.is_none() {
            tracing::warn!("the Veylvik page's view: Klipp's GPU encode failed, encoding on the CPU");
            *state = GpuState::Off;
        }
        texture
    }
}

impl Gpu {
    fn new(device: &wgpu::Device, working: WorkingColorspace, lut: Option<&LutData>) -> Option<Self> {
        let effects = GpuEffectsPipeline::new()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("veylvik page view in"), source: wgpu::ShaderSource::Wgsl(include_str!("display_in.wgsl").into()) });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("veylvik page view in"),
            entries: &[
                storage(0, true),
                storage(1, false),
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("veylvik page view in"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let prepare = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("veylvik page view in"), layout: Some(&pipeline_layout), module: &module, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let params = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view in"), size: std::mem::size_of::<PrepareParams>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let tables = match working {
            WorkingColorspace::Rec709Linear => Vec::new(),
            WorkingColorspace::AcesCG => klipp_color::aces::aces_rec709_gpu_data(1024),
            WorkingColorspace::Bt2020Linear => klipp_color::gamut::bt2020_to_bt709_gpu_data(),
        };
        let finish_module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("veylvik page view out"), source: wgpu::ShaderSource::Wgsl(include_str!("display_out.wgsl").into()) });
        let finish_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("veylvik page view out"),
            entries: &[
                storage(0, true),
                storage(1, true),
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba8Unorm, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let finish_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("veylvik page view out"), bind_group_layouts: &[Some(&finish_layout)], immediate_size: 0 });
        let finish = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("veylvik page view out"), layout: Some(&finish_pipeline_layout), module: &finish_module, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let finish_params = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view out"), size: std::mem::size_of::<FinishParams>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let table: &[f32] = lut.filter(|l| l.size >= 2).map_or(&[0.0], |l| &l.data);
        let lut_buffer = wgpu::util::DeviceExt::create_buffer_init(device, &wgpu::util::BufferInitDescriptor { label: Some("veylvik page view LUT"), contents: bytemuck::cast_slice(table), usage: wgpu::BufferUsages::STORAGE });
        let lut_size = lut.filter(|l| l.size >= 2).map_or(0, |l| l.size as u32);
        Some(Self { effects, prepare, layout, params, straight: None, tables, finish, finish_layout, finish_params, lut: lut_buffer, lut_size, bytes: None })
    }

    fn encode(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, picture: &wgpu::Buffer, width: u32, height: u32, working: WorkingColorspace) -> Option<wgpu::Texture> {
        let count = width.checked_mul(height)?;
        let size = u64::from(count) * 16;
        if count == 0 || picture.size() < size {
            return None;
        }
        if self.straight.as_ref().is_none_or(|b| b.size() < size) {
            self.straight = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view, straight"), size, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }));
        }
        let straight = self.straight.as_ref()?;
        let m = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, working).unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&PrepareParams { m: m.map(|r| [r[0], r[1], r[2], 0.0]), count, _pad: [0; 3] }));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("veylvik page view in"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: picture, offset: 0, size: std::num::NonZeroU64::new(size) }) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: straight, offset: 0, size: std::num::NonZeroU64::new(size) }) },
                wgpu::BindGroupEntry { binding: 2, resource: self.params.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik page view in") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("veylvik page view in"), timestamp_writes: None });
            pass.set_pipeline(&self.prepare);
            pass.set_bind_group(0, &group, &[]);
            let groups = count.div_ceil(256);
            let x = groups.min(65_535);
            pass.dispatch_workgroups(x, groups.div_ceil(x), 1);
        }
        queue.submit([encoder.finish()]);
        if !self.effects.copy_linear_into_chain(straight, width, height) {
            return None;
        }
        let byte_size = u64::from(count) * 4;
        let bytes_buffer = if working == WorkingColorspace::Rec709Linear {
            // Encoded where it stays: the bytes never leave the card (a
            // 4K frame's read back and sent again cost ~10 ms each frame).
            self.effects.encode_chain_to_display(true, width, height, None, false, None, None, false)?;
            self.effects.display_u8_buffer().filter(|b| b.size() >= byte_size)?
        } else {
            // These encodes hand the bytes back; sent to the card again.
            let bytes = match working {
                WorkingColorspace::AcesCG => self.effects.encode_chain_to_aces_rec709_u8(true, width, height, &self.tables),
                _ => self.effects.encode_chain_to_bt2020_display_u8(true, width, height, &self.tables),
            }?;
            if bytes.len() as u64 != byte_size {
                return None;
            }
            if self.bytes.as_ref().is_none_or(|b| b.size() < byte_size) {
                self.bytes = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view, display bytes"), size: byte_size, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
            }
            let buffer = self.bytes.as_ref()?;
            queue.write_buffer(buffer, 0, &bytes);
            buffer
        };
        // The LUT and the alpha on the card, into the texture the viewer
        // samples.
        queue.write_buffer(&self.finish_params, 0, bytemuck::bytes_of(&FinishParams { width, height, n: self.lut_size, lut_on: u32::from(self.lut_size >= 2) }));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("veylvik page, Klipp's view"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("veylvik page view out"),
            layout: &self.finish_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: bytes_buffer, offset: 0, size: std::num::NonZeroU64::new(byte_size) }) },
                wgpu::BindGroupEntry { binding: 1, resource: self.lut.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 3, resource: self.finish_params.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik page view out") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("veylvik page view out"), timestamp_writes: None });
            pass.set_pipeline(&self.finish);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
        }
        queue.submit([encoder.finish()]);
        Some(texture)
    }
}

impl HostEncode for KlippDisplay {
    fn identity(&self) -> u64 {
        self.identity
    }

    fn encode(&self, device: &wgpu::Device, queue: &wgpu::Queue, picture: &wgpu::Buffer, width: u32, height: u32) -> Result<wgpu::Texture, String> {
        if let Some(texture) = self.encode_on_gpu(device, queue, picture, width, height) {
            return Ok(texture);
        }
        let bytes = self.encode_pixels(&read_back(device, queue, picture, width, height)?);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("veylvik page, Klipp's view"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The bytes are display-encoded already: no decode on sampling.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([]);
        Ok(texture)
    }
}

impl HostDisplay for KlippDisplay {
    fn name(&self) -> String {
        let space = match self.working {
            WorkingColorspace::Rec709Linear => "Rec.709",
            WorkingColorspace::Bt2020Linear => "BT.2020",
            WorkingColorspace::AcesCG => "ACEScg, Rec.709 ODT",
        };
        let monitor = match self.transfer {
            SdrTransfer::Srgb => "sRGB",
            SdrTransfer::Bt1886 => "BT.1886",
        };
        let lut = if self.lut.is_some() { ", viewer LUT" } else { "" };
        format!("Klipp: {space}, {monitor}{lut}")
    }
}

/// `width × height` RGBA floats of `buffer`, read back from the card.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer, width: u32, height: u32) -> Result<Vec<f32>, String> {
    let size = u64::from(width) * u64::from(height) * 16;
    if buffer.size() < size {
        return Err(format!("the viewer's picture is {} bytes, {width}x{height} needs {size}", buffer.size()));
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view readback"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik page view readback") });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    klipp_effects::gpu::poll_until_mapped(device, &rx).map_err(|e| format!("the viewer's picture could not be read: {e}"))?.map_err(|e| format!("the viewer's picture could not be read: {e}"))?;
    let pixels: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's view of a picture is Klipp's viewer's, byte for byte: the
    /// same colours through `display_transform_ws_dithered_to` in each
    /// working space and on each monitor, and through a viewer LUT.
    #[test]
    fn the_pages_view_is_klipps_viewer() {
        let colours: Vec<[f32; 4]> = vec![[0.18, 0.18, 0.18, 1.0], [0.9, 0.05, 0.02, 1.0], [0.97, 0.55, 0.03, 1.0], [0.01, 0.4, 0.8, 1.0], [2.5, 1.2, 0.3, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let picture: Vec<f32> = colours.iter().flatten().copied().collect();
        let mut warm = LutData::identity(17);
        for c in warm.data.as_chunks_mut::<3>().0 {
            c[0] = c[0].powf(0.9);
        }
        let warm = Arc::new(warm);
        for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear, WorkingColorspace::AcesCG] {
            for transfer in [SdrTransfer::Srgb, SdrTransfer::Bt1886] {
                for lut in [None, Some(Arc::clone(&warm))] {
                    let display = KlippDisplay::new(working, transfer, lut.clone());
                    let m = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, working);
                    let in_working: Vec<f32> = colours.iter().flat_map(|c| {
                        let w = m.map_or([c[0], c[1], c[2]], |m| std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]));
                        [w[0], w[1], w[2], 1.0]
                    }).collect();
                    let mut want = klipp_color::display::display_transform_ws_dithered_to(&in_working, ToneMapOp::None, working, transfer);
                    if let Some(lut) = &lut {
                        klipp_color::lut::apply_3d_lut(&mut want, lut, 1.0);
                    }
                    assert_eq!(display.encode_pixels(&picture), want, "{working:?} {transfer:?} lut={}", lut.is_some());
                }
            }
        }
    }

    /// A display is named by what makes its pictures, the same in any
    /// session; another space, monitor or LUT is another display.
    #[test]
    fn a_displays_identity_is_its_settings() {
        let a = || KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Srgb, None).identity();
        assert_eq!(a(), a());
        assert_ne!(a(), KlippDisplay::new(WorkingColorspace::Rec709Linear, SdrTransfer::Srgb, None).identity());
        assert_ne!(a(), KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Bt1886, None).identity());
        let lut = |gain: f32| {
            let mut l = LutData::identity(5);
            l.data.iter_mut().for_each(|v| *v *= gain);
            Some(Arc::new(l))
        };
        let with = |gain| KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Srgb, lut(gain)).identity();
        assert_ne!(a(), with(1.0));
        assert_eq!(with(0.9), with(0.9));
        assert_ne!(with(0.9), with(0.8));
        assert_ne!(a(), 0);
    }

    /// Premultiplied in and out: a half-covered pixel is its colour at half.
    #[test]
    fn coverage_is_kept() {
        let display = KlippDisplay::new(WorkingColorspace::Rec709Linear, SdrTransfer::Srgb, None);
        let full = display.encode_pixels(&[0.5, 0.5, 0.5, 1.0]);
        let half = display.encode_pixels(&[0.25, 0.25, 0.25, 0.5]);
        assert_eq!(half[3], 128);
        assert!((i32::from(half[0]) - i32::from(full[0]) / 2).abs() <= 1, "{half:?} against {full:?}");
    }
}
