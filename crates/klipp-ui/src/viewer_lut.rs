//! The viewer (monitor) LUT: a 3D LUT applied to the picture on screen
//! and nowhere else (#465).
//!
//! The graded frame is left as the engine made it — the frame buffer,
//! the scopes, Auto White Balance, `frame_at` and every export read it
//! untouched. At draw time the viewer's texture (the GPU stack's output
//! while playing, the uploaded frame while parked) is run through the
//! LUT into a texture of its own, and the viewer shows that one.
//!
//! The lookup is on display-encoded values, as a monitor LUT is built:
//! both sources are sampled through sRGB views, which decode, so the
//! shader encodes again before the lookup. The result is display-encoded
//! too and is written to an `Rgba8Unorm` target that egui samples
//! through an sRGB view, as it samples the GPU stack's output (#455).
//! A LUT's colour-space tags are for LUT nodes inside the grade; here the
//! LUT maps the display signal to the display signal and they are not
//! applied.

use std::path::{Path, PathBuf};

use klipp_color::lut::LutData;

/// Where the output texture is written in, and sampled from by egui.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const SAMPLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const SHADER: &str = include_str!("viewer_lut.wgsl");

/// The LUT on the GPU, and the pass that applies it.
pub struct ViewerLutPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    lut_view: wgpu::TextureView,
    target: Option<Target>,
    /// Where an uploaded (parked) frame's bytes go before the lookup.
    source: Option<Target>,
}

struct Target {
    texture: wgpu::Texture,
    size: (u32, u32),
}

impl ViewerLutPass {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, lut: &LutData) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewer_lut"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let entry = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewer_lut"),
            entries: &[entry(0, wgpu::TextureViewDimension::D2), entry(1, wgpu::TextureViewDimension::D3)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewer_lut"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewer_lut"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: "vs", compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: TARGET_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let n = lut.size as u32;
        let texels: Vec<f32> = lut.data.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
        let lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer_lut_table"),
            size: wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Red fastest, then green, then blue: the `.cube` order, and the
        // texture's x, y, z.
        queue.write_texture(
            wgpu::ImageCopyTexture { texture: &lut_texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&texels),
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(n * 16), rows_per_image: Some(n) },
            wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
        );
        let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { pipeline, layout, lut_view, target: None, source: None }
    }

    /// Run `source` (a view that decodes to linear light: an sRGB view of
    /// display-encoded bytes) through the LUT into the output texture, of
    /// `size`. Returns true when the output texture was (re)made, so a
    /// view of it has to be registered with egui again.
    pub fn apply(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, source: &wgpu::TextureView, size: (u32, u32)) -> bool {
        let remade = self.target.as_ref().is_none_or(|t| t.size != size);
        if remade {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("viewer_lut_output"),
                size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[SAMPLE_FORMAT],
            });
            self.target = Some(Target { texture, size });
        }
        let Some(target) = &self.target else { return remade };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewer_lut"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.lut_view) },
            ],
        });
        let out = target.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("viewer_lut") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer_lut"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        remade
    }

    /// [`Self::apply`] on a frame of display-encoded RGBA bytes: the
    /// parked frame, which the viewer shows from an uploaded texture.
    pub fn apply_bytes(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rgba: &[u8], size: (u32, u32)) -> bool {
        if rgba.len() != (size.0 * size.1 * 4) as usize || size.0 == 0 || size.1 == 0 {
            return false;
        }
        if self.source.as_ref().is_none_or(|t| t.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("viewer_lut_source"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: SAMPLE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.source = Some(Target { texture, size });
        }
        let Some(source) = &self.source else { return false };
        queue.write_texture(
            wgpu::ImageCopyTexture { texture: &source.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            rgba,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(size.0 * 4), rows_per_image: Some(size.1) },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        let view = source.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.apply(device, queue, &view, size)
    }

    /// A view of the output for egui: through sRGB, so egui decodes the
    /// display-encoded values the LUT wrote.
    pub fn output_view(&self) -> Option<wgpu::TextureView> {
        self.target.as_ref().map(|t| t.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(SAMPLE_FORMAT), ..Default::default() }))
    }

    #[cfg(test)]
    fn output_texture(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref().map(|t| &t.texture)
    }
}

/// What the viewer looks through: the project's viewing LUT (a look) and
/// then this monitor's calibration LUT from Preferences, made one LUT so
/// the screen runs a single pass (#466). Loaded once per pair of paths.
#[derive(Default)]
pub struct ViewerLut {
    paths: (Option<PathBuf>, Option<PathBuf>),
    pub lut: Option<LutData>,
    /// Why a named LUT could not be loaded, to say so.
    pub error: Option<String>,
}

impl ViewerLut {
    /// Load the `viewing` and `calibration` LUTs when either differs from
    /// those loaded. Returns true when the LUT on screen changed.
    pub fn follow(&mut self, viewing: Option<&Path>, calibration: Option<&Path>) -> bool {
        if (self.paths.0.as_deref(), self.paths.1.as_deref()) == (viewing, calibration) {
            return false;
        }
        self.paths = (viewing.map(Path::to_path_buf), calibration.map(Path::to_path_buf));
        self.error = None;
        let mut load = |what: &str, path: Option<&Path>| {
            let p = path?;
            match klipp_color::lut::load_lut(p) {
                Ok(lut) => Some(lut),
                Err(e) => {
                    tracing::warn!("{what} LUT {}: {e}", p.display());
                    self.error = Some(format!("{what} LUT: {e}"));
                    None
                }
            }
        };
        let viewing = load("viewing", viewing);
        let calibration = load("calibration", calibration);
        self.lut = match (viewing, calibration) {
            (Some(v), Some(c)) => Some(klipp_color::lut::compose(&v, &c)),
            (one, other) => one.or(other),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The viewer LUT shader compiles, with no GPU (#2344).
    #[test]
    fn the_shader_compiles() {
        if let Err(e) = klipp_effects::shader_check::validate(SHADER) {
            panic!("viewer_lut: {e}");
        }
    }

    /// The shader file is byte for byte the string it replaced (#3094):
    /// its length and FNV-1a hash as they were before the move.
    #[test]
    fn the_shader_is_as_it_was_before_the_move() {
        let fnv = SHADER.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        assert_eq!((SHADER.len(), fnv), (1552, 0x233bb004c181e415));
    }

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
    }

    /// A LUT that is not the identity: a warm, contrasty curve, as a show
    /// LUT might be.
    fn warm_lut(n: usize) -> LutData {
        let mut lut = LutData::identity(n);
        for c in lut.data.chunks_exact_mut(3) {
            c[0] = (c[0] * 1.1 + 0.02).min(1.0);
            c[1] = c[1] * c[1];
            c[2] *= 0.8;
        }
        lut
    }

    /// The pass gives what the CPU lookup gives, on display-encoded
    /// bytes sampled through an sRGB view, as the viewer's textures are.
    #[test]
    fn the_gpu_pass_matches_the_cpu_lookup() {
        let Some((device, queue)) = device() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let (w, h) = (64u32, 4u32);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [(i * 4 % 256) as u8, (i * 7 % 256) as u8, (255 - i * 3 % 256) as u8, 255]).collect();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &src,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let lut = warm_lut(17);
        let mut pass = ViewerLutPass::new(&device, &queue, &lut);
        assert!(pass.apply(&device, &queue, &texture.create_view(&Default::default()), (w, h)));
        let got = read_back(&device, &queue, &pass, w, h);
        let mut want = src.clone();
        klipp_color::lut::apply_3d_lut(&mut want, &lut, 1.0);
        let worst = got.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 2, "GPU and CPU lookups differ by {worst} codes");
        assert_ne!(got, src, "the LUT changed nothing");

        // The parked frame's bytes, uploaded by the pass itself: the same.
        let mut bytes_pass = ViewerLutPass::new(&device, &queue, &lut);
        assert!(bytes_pass.apply_bytes(&device, &queue, &src, (w, h)));
        assert_eq!(read_back(&device, &queue, &bytes_pass, w, h), got);
        assert!(!bytes_pass.apply_bytes(&device, &queue, &src, (w, h)), "same size: the output texture is kept");
    }

    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, pass: &ViewerLutPass, w: u32, h: u32) -> Vec<u8> {

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (w * h * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: pass.output_texture().unwrap(), mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer { buffer: &buffer, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let got = buffer.slice(..).get_mapped_range().to_vec();
        got
    }

    #[test]
    fn a_lut_is_loaded_once_per_path_and_a_bad_one_says_why() {
        let dir = std::env::temp_dir().join(format!("klipp-viewer-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("look.cube");
        let table: Vec<[f32; 3]> = warm_lut(5).data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        klipp_color::lut::write_cube(&good, &table, 5, "look").unwrap();
        let bad = dir.join("broken.cube");
        std::fs::write(&bad, "LUT_3D_SIZE 3\n0 0 0\n").unwrap();

        let mut v = ViewerLut::default();
        assert!(v.follow(None, Some(&good)));
        assert!(v.lut.is_some() && v.error.is_none());
        assert!(!v.follow(None, Some(&good)), "the same path is not loaded again");
        assert!(v.follow(None, Some(&bad)));
        assert!(v.lut.is_none() && v.error.is_some(), "a broken LUT is not applied, and says why");
        assert!(v.follow(None, None));
        assert!(v.lut.is_none() && v.error.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #466: the screen looks through the project's viewing LUT and then
    /// the monitor's calibration, as one LUT; either alone is itself.
    #[test]
    fn the_viewing_lut_runs_before_the_calibration() {
        let dir = std::env::temp_dir().join(format!("klipp-viewing-lut-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, lut: &LutData| {
            let path = dir.join(name);
            let table: Vec<[f32; 3]> = lut.data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            klipp_color::lut::write_cube(&path, &table, lut.size as u32, name).unwrap();
            path
        };
        let look = warm_lut(5);
        let mut cal = LutData::identity(5);
        for v in &mut cal.data {
            *v = (*v * 0.9 + 0.05).min(1.0);
        }
        let (look_path, cal_path) = (write("look.cube", &look), write("cal.cube", &cal));
        let mut v = ViewerLut::default();
        assert!(v.follow(Some(&look_path), Some(&cal_path)));
        let both = v.lut.clone().expect("one LUT");
        for rgb in [[0.2, 0.4, 0.6], [0.9, 0.1, 0.5]] {
            let want = klipp_color::lut::lookup_3d(&cal, klipp_color::lut::lookup_3d(&look, rgb));
            let got = klipp_color::lut::lookup_3d(&both, rgb);
            assert!((0..3).all(|c| (want[c] - got[c]).abs() < 2e-3), "look then calibration: {want:?} vs {got:?}");
        }
        assert!(v.follow(Some(&look_path), None));
        let alone = klipp_color::lut::lookup_3d(v.lut.as_ref().unwrap(), [0.2, 0.4, 0.6]);
        let want = klipp_color::lut::lookup_3d(&look, [0.2, 0.4, 0.6]);
        assert!((0..3).all(|c| (want[c] - alone[c]).abs() < 2e-3), "the look alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
