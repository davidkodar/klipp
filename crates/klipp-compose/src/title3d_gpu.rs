//! Cached solid glyphs rasterized at the CPU reference's 2×2 sample grid.
//! Geometry uploads only on cache misses; frames upload camera/light/pose data.
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_title::text3d_mesh::Part;
use klipp_title::text3d_render::{Pose, Scene};
use std::{collections::VecDeque, sync::Arc};
use wgpu::util::DeviceExt;

const MAX_VERTICES: usize = 1_000_000;
const CACHE_BYTES: u64 = 64 << 20;
const ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Uint32,3=>Uint32];
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    p: [f32; 3],
    n: [f32; 3],
    material: u32,
    glyph: u32,
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuPose {
    anchor_scale: [f32; 4],
    offset_alpha: [f32; 4],
    turn: [f32; 4],
}
impl From<Pose> for GpuPose {
    fn from(p: Pose) -> Self {
        Self {
            anchor_scale: [p.anchor[0], p.anchor[1], p.anchor[2], p.scale],
            offset_alpha: [p.offset[0], p.offset[1], p.alpha, 0.0],
            turn: [p.rotation.sin(), p.rotation.cos(), 0.0, 0.0],
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewParams {
    rotation: [[f32; 4]; 3],
    pivot_focal: [f32; 4],
    roi: [f32; 4],
    depth: [f32; 4],
    light: [f32; 4],
    surface: [f32; 4],
    face: [f32; 4],
    side: [f32; 4],
    bevel: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveParams {
    roi: [u32; 4],
    frame_width: u32,
    pad: [u32; 3],
    matrix: [[f32; 4]; 3],
}
struct Geometry {
    id: u64,
    buffer: Arc<wgpu::Buffer>,
    count: u32,
    bytes: u64,
}
struct Targets {
    _color: wgpu::Texture,
    _depth: wgpu::Texture,
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    size: (u32, u32),
}
pub(crate) struct GpuText3d {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    resolve: wgpu::ComputePipeline,
    resolve_layout: wgpu::BindGroupLayout,
    view_params: wgpu::Buffer,
    resolve_params: wgpu::Buffer,
    poses: wgpu::Buffer,
    geometry: VecDeque<Geometry>,
    targets: Option<Targets>,
    frame: (u32, u32),
    uploads: u64,
}
fn entry(
    binding: u32,
    ty: wgpu::BindingType,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty,
        count: None,
    }
}
fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}
fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}
impl GpuText3d {
    pub(crate) fn new(gpu: &GpuEffectsPipeline) -> Self {
        let device = gpu.device();
        let queue = gpu.queue();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("3D title view"),
            entries: &[
                entry(0, uniform(), wgpu::ShaderStages::VERTEX_FRAGMENT),
                entry(1, storage(true), wgpu::ShaderStages::VERTEX),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("3D title"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("3D title"),
            source: wgpu::ShaderSource::Wgsl(include_str!("title3d_gpu.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("3D title"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vertex",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRS,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fragment",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("3D title resolve"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    wgpu::ShaderStages::COMPUTE,
                ),
                entry(1, storage(false), wgpu::ShaderStages::COMPUTE),
                entry(2, uniform(), wgpu::ShaderStages::COMPUTE),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("3D title resolve"),
            bind_group_layouts: &[&resolve_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("3D title resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("title3d_resolve.wgsl").into()),
        });
        let resolve = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("3D title resolve"),
            layout: Some(&pl),
            module: &shader,
            entry_point: "resolve",
            compilation_options: Default::default(),
            cache: None,
        });
        let buffer = |size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("3D title parameters"),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let view_params = buffer(
            std::mem::size_of::<ViewParams>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let resolve_params = buffer(
            std::mem::size_of::<ResolveParams>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let poses = buffer(48, wgpu::BufferUsages::STORAGE);
        Self {
            device,
            queue,
            pipeline,
            layout,
            resolve,
            resolve_layout,
            view_params,
            resolve_params,
            poses,
            geometry: VecDeque::new(),
            targets: None,
            frame: (0, 0),
            uploads: 0,
        }
    }
    pub(crate) fn uploads(&self) -> u64 {
        self.uploads
    }
    pub(crate) fn same_device(&self, gpu: &GpuEffectsPipeline) -> bool {
        Arc::ptr_eq(&self.device, &gpu.device())
    }
    fn geometry(&mut self, id: u64, scene: &Scene) -> Option<(Arc<wgpu::Buffer>, u32)> {
        if let Some(i) = self.geometry.iter().position(|g| g.id == id) {
            let g = self.geometry.remove(i)?;
            let out = (g.buffer.clone(), g.count);
            self.geometry.push_front(g);
            return Some(out);
        }
        let mut vertices = Vec::new();
        // The CPU reference draws walls/bevels first, then planar faces.
        for (i, m) in scene.meshes.iter().enumerate() {
            for tri in &m.tris {
                for k in 0..3 {
                    vertices.push(Vertex {
                        p: tri.p[k],
                        n: tri.n[k],
                        material: if tri.part == Part::Bevel { 2 } else { 1 },
                        glyph: i as u32,
                    });
                }
                if vertices.len() > MAX_VERTICES {
                    return None;
                }
            }
        }
        for (i, m) in scene.meshes.iter().enumerate() {
            for face in [&m.front, &m.back] {
                for tri in klipp_title::text3d_triangles::triangulate(
                    face,
                    (MAX_VERTICES - vertices.len()) / 3,
                )? {
                    for p in tri {
                        vertices.push(Vertex {
                            p,
                            n: face.normal,
                            material: 0,
                            glyph: i as u32,
                        });
                    }
                }
            }
        }
        if vertices.is_empty()
            || vertices
                .iter()
                .any(|v| v.p.iter().chain(&v.n).any(|x| !x.is_finite()))
        {
            return None;
        }
        let bytes = vertices.len() as u64 * 32;
        if bytes > CACHE_BYTES {
            return None;
        }
        while self.geometry.len() >= 4
            || self.geometry.iter().map(|g| g.bytes).sum::<u64>() + bytes > CACHE_BYTES
        {
            self.geometry.pop_back()?;
        }
        let buffer = Arc::new(
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("cached 3D title geometry"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
        );
        let count = vertices.len() as u32;
        self.geometry.push_front(Geometry {
            id,
            buffer: buffer.clone(),
            count,
            bytes,
        });
        self.uploads += 1;
        Some((buffer, count))
    }
    fn targets(&mut self, w: u32, h: u32) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.size.0 >= w && t.size.1 >= h)
        {
            return;
        }
        let (w, h) = (w.div_ceil(64) * 64, h.div_ceil(64) * 64);
        let texture = |format, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("3D title sample grid"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = texture(
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        self.targets = Some(Targets {
            color: color.create_view(&Default::default()),
            depth: depth.create_view(&Default::default()),
            _color: color,
            _depth: depth,
            size: (w, h),
        });
    }
    /// Return the exact reference solid bounds, or decline before recording.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        gpu: &GpuEffectsPipeline,
        slot: usize,
        id: u64,
        scene: &Scene,
        width: u32,
        height: u32,
        matrix: [[f32; 3]; 3],
    ) -> Option<[u32; 4]> {
        if gpu.is_device_lost()
            || !self.same_device(gpu)
            || !scene.sprites.is_empty()
            || scene.texture.is_some()
        {
            return None;
        }
        let layer = gpu.stack_layer_buffer(slot)?;
        if width == 0 || height == 0 || layer.size() < u64::from(width) * u64::from(height) * 16 {
            return None;
        }
        let (mut view, roi) = project(scene, width, height)?;
        if roi[2] == 0 || roi[3] == 0 {
            gpu.flush();
            let mut e = self.device.create_command_encoder(&Default::default());
            e.clear_buffer(&layer, 0, None);
            self.queue.submit(Some(e.finish()));
            return Some(roi);
        }
        let (sw, sh) = (roi[2].checked_mul(2)?, roi[3].checked_mul(2)?);
        if sw.div_ceil(64) * 64 > self.device.limits().max_texture_dimension_2d
            || sh.div_ceil(64) * 64 > self.device.limits().max_texture_dimension_2d
        {
            return None;
        }
        if roi[2].checked_mul(roi[3])?.div_ceil(256)
            > self.device.limits().max_compute_workgroups_per_dimension
            || matrix.iter().flatten().any(|v| !v.is_finite())
        {
            return None;
        }
        let (vertices, count) = self.geometry(id, scene)?;
        if self.frame != (width, height) {
            self.targets = None;
            self.frame = (width, height);
        }
        self.targets(sw, sh);
        let poses: Vec<GpuPose> = (0..scene.meshes.len())
            .map(|i| scene.poses.get(i).copied().unwrap_or_default().into())
            .collect();
        let pose_bytes = bytemuck::cast_slice(&poses);
        if bytemuck::cast_slice::<GpuPose, f32>(&poses)
            .iter()
            .any(|v| !v.is_finite())
            || pose_bytes.is_empty()
            || pose_bytes.len() as u64
                > u64::from(self.device.limits().max_storage_buffer_binding_size)
        {
            return None;
        }
        gpu.flush();
        if self.poses.size() < pose_bytes.len() as u64 {
            self.poses = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("3D title glyph poses"),
                size: pose_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        view.roi = roi.map(|v| v as f32);
        self.queue
            .write_buffer(&self.view_params, 0, bytemuck::bytes_of(&view));
        self.queue.write_buffer(&self.poses, 0, pose_bytes);
        self.queue.write_buffer(
            &self.resolve_params,
            0,
            bytemuck::bytes_of(&ResolveParams {
                roi,
                frame_width: width,
                pad: [0; 3],
                matrix: matrix.map(|r| [r[0], r[1], r[2], 0.0]),
            }),
        );
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("3D title"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.view_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.poses.as_entire_binding(),
                },
            ],
        });
        let targets = self.targets.as_ref()?;
        let resolve_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("3D title resolve"),
            layout: &self.resolve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&targets.color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: layer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.resolve_params.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("3D title frame"),
            });
        encoder.clear_buffer(&layer, 0, None);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D title surfaces"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
            pass.draw(0..count, 0..1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("3D title resolve"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve);
            pass.set_bind_group(0, &resolve_bg, &[]);
            klipp_effects::gpu::dispatch_flat(&mut pass, (roi[2] * roi[3]).div_ceil(256));
        }
        self.queue.submit(Some(encoder.finish()));
        Some(roi)
    }
}

fn linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn project(scene: &Scene, width: u32, height: u32) -> Option<(ViewParams, [u32; 4])> {
    let t = scene.settings;
    let [x, y, z] = t.rotation.map(f32::to_radians);
    let (sx, cx, sy, cy, sz, cz) = (x.sin(), x.cos(), y.sin(), y.cos(), z.sin(), z.cos());
    let rotation = [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx, 0.0],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx, 0.0],
        [-sy, cy * sx, cy * cx, 0.0],
    ];
    let ortho = t.camera.orthographic;
    let focal = if ortho {
        0.0
    } else {
        height as f32 * 0.5 / (t.camera.fov.clamp(1.0, 170.0).to_radians() * 0.5).tan()
    };
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for (i, m) in scene.meshes.iter().enumerate() {
        let pose = scene.poses.get(i).copied().unwrap_or_default();
        if pose.alpha <= 0.001 {
            continue;
        }
        let (s, c) = (pose.rotation.sin(), pose.rotation.cos());
        let mut grow = |p: [f32; 3]| -> Option<()> {
            let q = std::array::from_fn::<_, 3, _>(|k| (p[k] - pose.anchor[k]) * pose.scale);
            let p = [
                pose.anchor[0] + c * q[0] - s * q[1] + pose.offset[0],
                pose.anchor[1] + s * q[0] + c * q[1] + pose.offset[1],
                pose.anchor[2] + q[2],
            ];
            let q = std::array::from_fn::<_, 3, _>(|k| p[k] - scene.pivot[k]);
            let mut cam = rotation.map(|r| r[0] * q[0] + r[1] * q[1] + r[2] * q[2]);
            if !ortho {
                cam[2] += focal + scene.pivot[2];
                if cam[2] < focal * 0.02 {
                    return None;
                }
            }
            let k = if ortho { 1.0 } else { focal / cam[2] };
            let screen = [
                scene.pivot[0] + cam[0] * k,
                scene.pivot[1] + cam[1] * k,
                cam[2],
            ];
            if screen.iter().any(|v| !v.is_finite()) {
                return None;
            }
            for d in 0..3 {
                lo[d] = lo[d].min(screen[d]);
                hi[d] = hi[d].max(screen[d]);
            }
            Some(())
        };
        for tri in &m.tris {
            for p in tri.p {
                grow(p)?;
            }
        }
        for f in [&m.front, &m.back] {
            for contour in &f.contours {
                for p in contour {
                    grow([p[0], p[1], f.z])?;
                }
            }
        }
    }
    let roi = if lo[0] > hi[0] {
        [0; 4]
    } else {
        let x0 = (lo[0].floor() as i64)
            .saturating_sub(1)
            .clamp(0, i64::from(width)) as u32;
        let y0 = (lo[1].floor() as i64)
            .saturating_sub(1)
            .clamp(0, i64::from(height)) as u32;
        let x1 = (hi[0].ceil() as i64)
            .saturating_add(2)
            .clamp(0, i64::from(width)) as u32;
        let y1 = (hi[1].ceil() as i64)
            .saturating_add(2)
            .clamp(0, i64::from(height)) as u32;
        [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
    };
    let (az, el) = (t.light.azimuth.to_radians(), t.light.elevation.to_radians());
    let mut direction = [el.cos() * az.cos(), el.cos() * az.sin(), -el.sin()];
    let len = direction
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt()
        .max(1e-12);
    for v in &mut direction {
        *v /= len;
    }
    let color = |c: [u8; 4]| [linear(c[0]), linear(c[1]), linear(c[2]), 1.0];
    let near = if ortho {
        lo[2] - 1.0
    } else {
        (lo[2] - 1.0).max(focal * 0.02)
    };
    let far = hi[2] + 1.0;
    let view = ViewParams {
        rotation,
        pivot_focal: [scene.pivot[0], scene.pivot[1], scene.pivot[2], focal],
        roi: roi.map(|v| v as f32),
        depth: [near, far, if ortho { 1.0 } else { 0.0 }, 0.0],
        light: [direction[0], direction[1], direction[2], t.light.intensity],
        surface: [t.light.ambient, t.light.specular, t.light.shininess, 0.0],
        face: color(scene.face),
        side: color(scene.side),
        bevel: color(scene.bevel),
    };
    if bytemuck::cast_slice::<ViewParams, f32>(&[view])
        .iter()
        .any(|v| !v.is_finite())
        || (roi[2] > 0 && roi[3] > 0 && far <= near)
    {
        return None;
    }
    Some((view, roi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_title::text3d_mesh::{contours_of, extrude, BevelPx};

    /// Both 3D title shaders compile, with no GPU (#2344).
    #[test]
    fn the_shaders_compile() {
        for (name, source) in [("title3d_gpu", include_str!("title3d_gpu.wgsl")), ("title3d_resolve", include_str!("title3d_resolve.wgsl"))] {
            if let Err(e) = klipp_effects::shader_check::validate(source) {
                panic!("{name}: {e}");
            }
        }
    }
    #[test]
    fn gpu_solids_preserve_cpu_holes_lighting_and_coverage() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else {
            assert!(
                std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            return;
        };
        let mut solid = GpuText3d::new(&gpu);
        let contour = contours_of(vec![
            vec![[80.0, 45.0], [240.0, 45.0], [240.0, 135.0], [80.0, 135.0]],
            vec![[125.0, 70.0], [125.0, 110.0], [195.0, 110.0], [195.0, 70.0]],
        ]);
        let meshes = [extrude(
            &contour,
            25.0,
            Some(BevelPx {
                size: 3.0,
                segments: 3,
                round: true,
            }),
            None,
            25.0,
        )];
        for (turn, orthographic, alpha) in [
            (0.0, false, 1.0),
            (35.0, false, 1.0),
            (-55.0, false, 0.4),
            (28.0, true, 1.0),
            (20.0, false, 0.7),
        ] {
            let mut settings = klipp_timeline::Text3d {
                rotation: [10.0, turn, 4.0],
                ..Default::default()
            };
            settings.camera.orthographic = orthographic;
            let poses = [if turn == 20.0 {
                Pose {
                    anchor: [160.0, 90.0, 12.5],
                    scale: 0.9,
                    rotation: 0.1,
                    offset: [2.3, -1.2],
                    alpha,
                }
            } else {
                Pose {
                    alpha,
                    ..Default::default()
                }
            }];
            let scene = Scene {
                meshes: &meshes,
                sprites: &[],
                settings: &settings,
                pivot: [160.0, 90.0, 12.5],
                face: [180, 210, 240, 255],
                side: [50, 80, 130, 255],
                bevel: [200, 180, 100, 255],
                poses: &poses,
                texture: None,
            };
            let (expected, bounds) = klipp_title::text3d_render::render_boxed(&scene, 320, 180, 2);
            assert!(gpu.stack_begin(320, 180, 1));
            let before = gpu.readback_stats();
            let roi = solid
                .render(
                    &gpu,
                    0,
                    42,
                    &scene,
                    320,
                    180,
                    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                )
                .unwrap();
            let b = bounds.unwrap();
            assert_eq!(
                roi,
                [
                    b[0] as u32,
                    b[1] as u32,
                    (b[2] - b[0]) as u32,
                    (b[3] - b[1]) as u32
                ]
            );
            assert_eq!(before, gpu.readback_stats());
            assert_eq!(solid.uploads, 1);
            assert!(gpu.stack_blend(0, 1.0, 0));
            assert!(gpu.stack_finish_into_chain(true));
            let actual = klipp_effects::linear_f32_rgba_to_srgb_u8(&gpu.stack_download_linear());
            let errors: Vec<_> = actual
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .map(|(a, b)| {
                    let alpha = (f32::from(a[3]) - f32::from(b[3])).abs();
                    (0..3)
                        .map(|c| {
                            (f32::from(a[c]) * f32::from(a[3]) / 255.0
                                - f32::from(b[c]) * f32::from(b[3]) / 255.0)
                                .abs()
                        })
                        .fold(alpha, f32::max)
                })
                .collect();
            let worst = errors.iter().copied().fold(0.0f32, f32::max);
            let changed = errors.iter().filter(|e| **e > 2.0).count();
            eprintln!("turn={turn}, ortho={orthographic}, alpha={alpha}: worst={worst}, pixels>2={changed}");
            let (view, _) = project(&scene, 320, 180).unwrap();
            let screen = |p: [f32; 3]| {
                let pose = poses[0];
                let q = std::array::from_fn::<_, 3, _>(|k| (p[k] - pose.anchor[k]) * pose.scale);
                let (sn, cs) = (pose.rotation.sin(), pose.rotation.cos());
                let p = [
                    pose.anchor[0] + cs * q[0] - sn * q[1] + pose.offset[0],
                    pose.anchor[1] + sn * q[0] + cs * q[1] + pose.offset[1],
                    pose.anchor[2] + q[2],
                ];
                let q = std::array::from_fn::<_, 3, _>(|k| p[k] - scene.pivot[k]);
                let cam = view
                    .rotation
                    .map(|r| r[0] * q[0] + r[1] * q[1] + r[2] * q[2]);
                let k = if orthographic {
                    1.0
                } else {
                    view.pivot_focal[3] / (cam[2] + view.pivot_focal[3] + scene.pivot[2])
                };
                [scene.pivot[0] + cam[0] * k, scene.pivot[1] + cam[1] * k]
            };
            let mut edges = Vec::new();
            for m in &meshes {
                let triangles =
                    m.tris
                        .iter()
                        .map(|t| t.p)
                        .chain([&m.front, &m.back].into_iter().flat_map(|f| {
                            klipp_title::text3d_triangles::triangulate(f, 100000).unwrap()
                        }));
                for t in triangles {
                    let t = t.map(screen);
                    for j in 0..3 {
                        edges.push((t[j], t[(j + 1) % 3]));
                    }
                }
            }
            for (i, error) in errors.iter().enumerate().filter(|(_, e)| **e > 2.0) {
                let mut distance = f32::MAX;
                for dy in [0.25, 0.75] {
                    for dx in [0.25, 0.75] {
                        let p = [(i % 320) as f32 + dx, (i / 320) as f32 + dy];
                        for (a, b) in &edges {
                            let d = [b[0] - a[0], b[1] - a[1]];
                            let len = d[0] * d[0] + d[1] * d[1];
                            if len == 0.0 {
                                continue;
                            }
                            let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len)
                                .clamp(0.0, 1.0);
                            distance = distance
                                .min((p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1]));
                        }
                    }
                }
                // Hardware fixed-point coverage/depth at triangle seams differs
                // from the reference floating-point scan converter. Only permit
                // one of the four samples to differ very close to an edge.
                assert!(
                    distance <= 1.0 / 32.0,
                    "interior mismatch at {i}: {distance}"
                );
                eprintln!(
                    "pixel ({},{}): GPU {:?}, CPU {:?}, error {}",
                    i % 320,
                    i / 320,
                    &actual[i * 4..i * 4 + 4],
                    &expected[i * 4..i * 4 + 4],
                    error
                );
            }
            assert!(changed <= 8 && worst <= 65.0, "excessive edge differences");
        }
    }
}
