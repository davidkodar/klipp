//! Klipp's HDR reference output: shows the viewer's HDR picture on a
//! surface that can actually carry it.
//!
//! ```text
//! klipp-hdr-out [--fullscreen] [--path <file>]
//! ```
//!
//! The surface is `Rgba16Float` in scRGB — `VK_COLOR_SPACE_EXTENDED_SRGB_
//! LINEAR_EXT` — which is linear and admits values past 1.0, so the HDR
//! display light the viewer publishes goes through unchanged. That is the
//! only HDR colour space wgpu 22 will create; the driver here also offers
//! HDR10 PQ at ten bits, which wgpu discards (see the crate docs).
//!
//! It refuses to run on an 8-bit surface rather than quietly showing a
//! clipped picture: a reference output that silently stops being one is
//! worse than no reference output. `--allow-sdr` overrides that for
//! looking at the plumbing on a machine without an HDR path.

use std::sync::Arc;

use klipp_hdr_out::{FrameReader, ReadError};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

/// The surface format that carries HDR: linear, sixteen bits a channel,
/// values past 1.0 allowed.
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

struct Args {
    fullscreen: bool,
    allow_sdr: bool,
    path: std::path::PathBuf,
}

fn args() -> Args {
    let mut a =
        Args { fullscreen: false, allow_sdr: false, path: klipp_hdr_out::default_path() };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fullscreen" => a.fullscreen = true,
            "--allow-sdr" => a.allow_sdr = true,
            "--path" => {
                if let Some(p) = it.next() {
                    a.path = p.into();
                }
            }
            "--help" | "-h" => {
                eprintln!("klipp-hdr-out [--fullscreen] [--allow-sdr] [--path <file>]");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other:?}");
                std::process::exit(2);
            }
        }
    }
    a
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// The frame as an `Rgba16Float` texture, re-created when the
    /// published geometry changes.
    texture: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)>,
    sampler: wgpu::Sampler,
}

struct App {
    args: Args,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    reader: Option<FrameReader>,
    /// The frame as copied out of shared memory, and its f16 form. Kept
    /// so neither allocates per frame.
    frame: Vec<f32>,
    half: Vec<u16>,
    reported: bool,
}

/// A full-screen triangle pair sampling the frame. No colour maths: the
/// surface is linear and the frame is linear, so the only job is to put
/// the values on the screen untouched.
const SHADER: &str = r#"
@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct Vs { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Vs {
    // Two triangles covering the viewport.
    var p = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let xy = p[i];
    var o: Vs;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    // Texture space is y-down.
    o.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return o;
}

@fragment
fn fs(v: Vs) -> @location(0) vec4<f32> {
    // Straight through. The surface is scRGB — linear, 1.0 = SDR
    // reference white, values above it are the HDR highlights — which is
    // exactly what the viewer published.
    return textureSampleLevel(frame, samp, v.uv, 0.0);
}
"#;

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes().with_title("Klipp HDR output");
        if self.args.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(el.create_window(attrs).expect("could not open a window"));

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("no surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no adapter");

        let caps = surface.get_capabilities(&adapter);
        let hdr = caps.formats.contains(&HDR_FORMAT);
        tracing::info!(
            "adapter {:?}; surface offers {:?}",
            adapter.get_info().name,
            caps.formats,
        );
        if !hdr && !self.args.allow_sdr {
            eprintln!(
                "This surface offers no HDR format ({HDR_FORMAT:?} absent). Refusing to \
                 present: an SDR surface would clip every highlight and the picture would \
                 stop being a reference. Pass --allow-sdr to look at it anyway."
            );
            std::process::exit(1);
        }
        let format = if hdr { HDR_FORMAT } else { caps.formats[0] };
        tracing::info!(
            "presenting as {format:?}{}",
            if format == HDR_FORMAT { " (scRGB, EXTENDED_SRGB_LINEAR)" } else { " (SDR fallback)" },
        );

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor { label: Some("klipp-hdr-out"), ..Default::default() },
            None,
        ))
        .expect("no device");

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("no surface config");
        config.format = format;
        config.view_formats = vec![format];
        surface.configure(&device, &config);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs",
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        self.gpu =
            Some(Gpu { device, queue, surface, config, pipeline, layout, texture: None, sampler });
        self.window = Some(window);
        el.set_control_flow(ControlFlow::Poll);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state.is_pressed() && event.logical_key == Key::Named(NamedKey::Escape) {
                    el.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        self.draw();
        // A monitor feed does not need to spin: the viewer publishes at
        // its own rate and this wakes often enough to keep up with 60 Hz
        // without burning a core polling.
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

impl App {
    /// Attach to the shared frame, or notice it has gone.
    fn poll_writer(&mut self) {
        if self.reader.as_ref().is_some_and(|r| r.writer_present()) {
            return;
        }
        if self.reader.is_some() {
            tracing::info!("the viewer stopped publishing; waiting for it to come back");
            self.reader = None;
            self.reported = false;
        }
        match FrameReader::open(&self.args.path) {
            Ok(r) => {
                tracing::info!(
                    "attached to {} — {}x{}, master peak {} cd/m²",
                    self.args.path.display(),
                    r.width(),
                    r.height(),
                    r.peak_nits(),
                );
                self.reader = Some(r);
            }
            Err(ReadError::Version(v)) if !self.reported => {
                self.reported = true;
                tracing::warn!(
                    "{} was written by a different Klipp (protocol {v}, this build speaks {})",
                    self.args.path.display(),
                    klipp_hdr_out::PROTOCOL_VERSION,
                );
            }
            Err(_) => {}
        }
    }

    fn draw(&mut self) {
        self.poll_writer();
        let Some(gpu) = self.gpu.as_mut() else { return };

        // Upload a new frame if one has been published.
        if let Some(reader) = self.reader.as_mut() {
            let (w, h) = (reader.width(), reader.height());
            if reader.read_latest(&mut self.frame) {
                let frame = &self.frame;
                let stale = !matches!(gpu.texture, Some((_, _, tw, th)) if (tw, th) == (w, h));
                if stale {
                    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("frame"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let view = texture.create_view(&Default::default());
                    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("frame"),
                        layout: &gpu.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                            },
                        ],
                    });
                    gpu.texture = Some((texture, bind, w, h));
                }
                // f32 → f16, through `half` rather than a hand-rolled
                // conversion: this is the presentation path, and getting
                // the rounding or the overflow-to-infinity wrong would
                // clip exactly the highlights the output exists to show.
                // The texture is f16 because that is the surface's own
                // precision, and half of 33 MB crosses the bus.
                self.half.resize(frame.len(), 0);
                for (o, v) in self.half.iter_mut().zip(frame.iter()) {
                    *o = half::f16::from_f32(*v).to_bits();
                }
                if let Some((texture, _, _, _)) = gpu.texture.as_ref() {
                    gpu.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytemuck::cast_slice(&self.half),
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 8),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    );
                }
            }
        }

        let Some((_, bind, _, _)) = gpu.texture.as_ref() else {
            // Nothing published yet: leave the window black rather than
            // showing whatever the surface happened to contain.
            if let Ok(frame) = gpu.surface.get_current_texture() {
                let view = frame.texture.create_view(&Default::default());
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                {
                    let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                }
                gpu.queue.submit(Some(enc.finish()));
                frame.present();
            }
            return;
        };

        let Ok(frame) = gpu.surface.get_current_texture() else { return };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..6, 0..1);
        }
        gpu.queue.submit(Some(enc.finish()));
        frame.present();
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let el = EventLoop::new().expect("no event loop");
    let mut app = App {
        args: args(),
        window: None,
        gpu: None,
        reader: None,
        frame: Vec::new(),
        half: Vec::new(),
        reported: false,
    };
    el.run_app(&mut app).expect("event loop");
}

#[cfg(test)]
mod shader_tests {
    /// The presenter's shader compiles, with no GPU (#2344).
    #[test]
    fn the_shader_compiles() {
        let module = naga::front::wgsl::parse_str(super::SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(super::SHADER)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(super::SHADER)));
    }
}
