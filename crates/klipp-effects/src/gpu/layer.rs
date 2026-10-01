//! Resident layer effects. Every spill samples the unchanged layer, and
//! reverse src-over stacking keeps the first spill nearest the picture.
use super::{BlurParams, GpuEffectsPipeline, WORKGROUP_SIZE};

pub(super) struct LayerGpu {
    rows: wgpu::ComputePipeline,
    bounds: wgpu::ComputePipeline,
    corners: wgpu::ComputePipeline,
    cast: wgpu::ComputePipeline,
    over: wgpu::ComputePipeline,
    fill_small: wgpu::ComputePipeline,
    fill_over: wgpu::ComputePipeline,
    scratch: Vec<wgpu::Buffer>,
    size: u64,
}

impl LayerGpu {
    pub(super) fn new(
        device: &wgpu::Device,
        single: &wgpu::PipelineLayout,
        pair: &wgpu::PipelineLayout,
    ) -> Self {
        let build = |layout: &wgpu::PipelineLayout, source: &str, entry: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(entry),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(layout),
                module: &module,
                entry_point: entry,
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            rows: build(single, ROWS, "rows"),
            bounds: build(single, BOUNDS, "bounds"),
            cast: build(single, CAST, "make_cast"),
            corners: build(pair, CORNERS, "corners"),
            over: build(pair, OVER, "over"),
            fill_small: build(pair, FILL_SMALL, "fill_small"),
            fill_over: build(pair, FILL_OVER, "fill_over"),
            scratch: Vec::new(),
            size: 0,
        }
    }

    pub(super) fn ensure_buffers(&mut self, device: &wgpu::Device, size: u64) {
        if self.size >= size {
            return;
        }
        self.scratch = (0..2)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("layer_scratch"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        self.size = size;
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    radius: f32,
    feather: f32,
    offset_x: i32,
    offset_y: i32,
    opacity: f32,
    pad: u32,
    color: [f32; 4],
}

impl Params {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            radius: 0.0,
            feather: 0.0,
            offset_x: 0,
            offset_y: 0,
            opacity: 0.0,
            pad: 0,
            color: [0.0; 4],
        }
    }
}

/// [`crate::layer::fill_bars`]'s parameters, as its two shaders read them.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillParams {
    width: u32,
    height: u32,
    small_w: u32,
    small_h: u32,
    zoom: f32,
    darken: f32,
    pad: [u32; 2],
}

impl GpuEffectsPipeline {
    /// [`crate::layer::fill_bars`] on resident buffers: the picture's box
    /// found as the corners find it, the fill built an eighth the size,
    /// blurred there, and laid under the picture at full size (#680).
    /// Always writes `output`; with no bars it is the input.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn layer_fill(
        &self,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        blur: f32,
        zoom: f32,
        darken: f32,
    ) {
        let (sw, sh) = crate::layer::fill_size(width, height);
        let bounds = self.buf_c.as_ref().unwrap();
        let (small, spare) = (&self.layer_gpu.scratch[0], &self.layer_gpu.scratch[1]);
        self.write_params(&Params::new(width, height));
        self.dispatch(&self.layer_gpu.rows, input, &self.layer_gpu.scratch[1], height.div_ceil(WORKGROUP_SIZE));
        self.dispatch(&self.layer_gpu.bounds, &self.layer_gpu.scratch[1], bounds, 1);
        let fill = FillParams { width, height, small_w: sw, small_h: sh, zoom, darken: darken.clamp(0.0, 1.0), pad: [0; 2] };
        self.write_params(&fill);
        self.dispatch_pair(&self.layer_gpu.fill_small, input, bounds, small, (sw * sh).div_ceil(WORKGROUP_SIZE));
        let sigma = blur.max(0.0) * sw as f32;
        if sigma >= 0.5 {
            for radius in crate::blur::boxes_for_gaussian(sigma) {
                if radius == 0 {
                    continue;
                }
                self.write_params(&BlurParams { width: sw, height: sh, radius: radius as i32, _pad: 0 });
                self.dispatch(&self.blur_h_pipeline, small, spare, (sw * sh).div_ceil(WORKGROUP_SIZE));
                self.dispatch(&self.blur_v_pipeline, spare, small, (sw * sh).div_ceil(WORKGROUP_SIZE));
            }
        }
        self.write_params(&fill);
        self.dispatch_pair(&self.layer_gpu.fill_over, input, small, output, (width * height).div_ceil(WORKGROUP_SIZE));
    }

    pub(super) fn layer_corners(
        &self,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        radius: f32,
        feather: f32,
    ) {
        let mut params = Params::new(width, height);
        params.radius = radius.clamp(0.0, 0.5);
        params.feather = feather.max(0.0);
        self.write_params(&params);
        let rows = &self.layer_gpu.scratch[0];
        let bounds = self.buf_c.as_ref().unwrap();
        // One lane scans each row, then one lane reduces the row boxes.
        // No frame download, global atomics, or per-pixel frame scan.
        self.dispatch(
            &self.layer_gpu.rows,
            input,
            rows,
            height.div_ceil(WORKGROUP_SIZE),
        );
        self.dispatch(&self.layer_gpu.bounds, rows, bounds, 1);
        self.dispatch_pair(
            &self.layer_gpu.corners,
            input,
            bounds,
            output,
            (width * height).div_ceil(WORKGROUP_SIZE),
        );
    }

    pub(super) fn layer_spills(
        &self,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        run: &[crate::layer::Spill],
    ) -> bool {
        let wg = (width * height).div_ceil(WORKGROUP_SIZE);
        let cast = self.buf_c.as_ref().unwrap();
        let mut under = &self.layer_gpu.scratch[0];
        let mut next = &self.layer_gpu.scratch[1];
        let mut drawn = false;
        for spill in run.iter().rev() {
            let mut params = Params::new(width, height);
            params.opacity = spill.opacity.clamp(0.0, 1.0);
            params.offset_x = (spill.offset_x * width as f32).round() as i32;
            params.offset_y = (spill.offset_y * width as f32).round() as i32;
            let sigma = spill.blur.max(0.0) * width as f32;
            if params.opacity <= 0.001
                || (params.offset_x == 0 && params.offset_y == 0 && sigma < 0.5)
            {
                continue;
            }
            params.color = [
                crate::cpu::decode(spill.color[0] as f32 / 255.0),
                crate::cpu::decode(spill.color[1] as f32 / 255.0),
                crate::cpu::decode(spill.color[2] as f32 / 255.0),
                0.0,
            ];
            self.write_params(&params);
            self.dispatch(&self.layer_gpu.cast, input, cast, wg);
            if sigma >= 0.5 {
                for radius in crate::blur::boxes_for_gaussian(sigma) {
                    if radius == 0 {
                        continue;
                    }
                    self.write_params(&BlurParams {
                        width,
                        height,
                        radius: radius as i32,
                        _pad: 0,
                    });
                    self.dispatch(&self.blur_h_pipeline, cast, output, wg);
                    self.dispatch(&self.blur_v_pipeline, output, cast, wg);
                }
            }
            if drawn {
                self.write_params(&params);
                self.dispatch_pair(&self.layer_gpu.over, cast, under, next, wg);
                std::mem::swap(&mut under, &mut next);
            } else {
                let mut encoder = self.encoder(Some("layer_first_spill"));
                encoder.copy_buffer_to_buffer(
                    cast,
                    0,
                    under,
                    0,
                    u64::from(width) * u64::from(height) * 16,
                );
                self.submit(encoder);
                drawn = true;
            }
        }
        if drawn {
            self.write_params(&Params::new(width, height));
            self.dispatch_pair(&self.layer_gpu.over, input, under, output, wg);
        }
        drawn
    }
}

pub(super) const ROWS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn rows(@builtin(global_invocation_id) gid: vec3<u32>) {
    let y = gid.x;
    if y >= params.height { return; }
    var left = params.width;
    var right = 0u;
    for (var x = 0u; x < params.width; x++) {
        if unpack(input[y * params.width + x]).a > 0.001 {
            left = min(left, x);
            right = x + 1u;
        }
    }
    output[y] = vec4<f32>(f32(left), f32(y), f32(right), f32(y + 1u));
}
"#
);

pub(super) const BOUNDS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>; // not-pixels: per-row opaque bounds
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn bounds(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x != 0u { return; }
    var box = vec4<f32>(f32(params.width), f32(params.height), 0.0, 0.0);
    for (var y = 0u; y < params.height; y++) {
        let row = input[y];
        if row.z > row.x { box = vec4<f32>(min(box.xy, row.xy), max(box.zw, row.zw)); }
    }
    output[0] = box;
}
"#
);

pub(super) const CAST: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn make_cast(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width) - params.offset_x;
    let y = i32(idx / params.width) - params.offset_y;
    var a = 0.0;
    if x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height) {
        a = unpack(input[u32(y) * params.width + u32(x)]).a * params.opacity;
    }
    output[idx] = pack(vec4<f32>(params.color.rgb, a));
}
"#
);

pub(super) const CORNERS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn corners(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let box = second[0];
    let short = min(box.z - box.x, box.w - box.y);
    let r = params.radius * short;
    var pixel = unpack(input[idx]);
    if box.z > box.x && r > 0.5 {
        let pos = vec2<f32>(f32(idx % params.width), f32(idx / params.width)) + vec2<f32>(0.5);
        let dxy = max(max(box.xy + vec2<f32>(r) - pos, pos - (box.zw - vec2<f32>(r))), vec2<f32>(0.0));
        let d = length(dxy) - r;
        pixel.a *= clamp(0.5 - d / max(params.feather * short, 1.0), 0.0, 1.0);
    }
    output[idx] = pack(pixel);
}
"#
);

pub(super) const OVER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct Params {
    width: u32, height: u32, radius: f32, feather: f32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    color: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn over(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let top = unpack(input[idx]);
    let bottom = unpack(second[idx]);
    let a = top.a + bottom.a * (1.0 - top.a);
    var pixel = bottom;
    if a > 0.0 {
        pixel = vec4<f32>((top.rgb * top.a + bottom.rgb * bottom.a * (1.0 - top.a)) / a, a);
    }
    output[idx] = pack(pixel);
}
"#
);

/// Linear between the four pixels round `(x, y)`, clamped to `lo..=hi`:
/// `crate::layer::bilinear`, for a buffer bound as `buf`.
macro_rules! fill_bilinear_wgsl {
    ($buf:literal) => {
        concat!(
            "fn bilinear_", $buf, "(x_in: f32, y_in: f32, w: u32, lo: vec2<f32>, hi: vec2<f32>) -> vec4<f32> {\n",
            "    let x = clamp(x_in, lo.x, hi.x);\n",
            "    let y = clamp(y_in, lo.y, hi.y);\n",
            "    let ix = floor(x);\n",
            "    let iy = floor(y);\n",
            "    let tx = x - ix;\n",
            "    let ty = y - iy;\n",
            "    let x0 = u32(ix); let y0 = u32(iy);\n",
            "    let x1 = u32(min(ix + 1.0, hi.x)); let y1 = u32(min(iy + 1.0, hi.y));\n",
            "    let a = unpack(", $buf, "[y0 * w + x0]); let b = unpack(", $buf, "[y0 * w + x1]);\n",
            "    let c = unpack(", $buf, "[y1 * w + x0]); let d = unpack(", $buf, "[y1 * w + x1]);\n",
            "    let top = a + (b - a) * tx;\n",
            "    let bottom = c + (d - c) * tx;\n",
            "    return top + (bottom - top) * ty;\n",
            "}\n",
        )
    };
}

pub(super) const FILL_SMALL: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
"#,
    fill_bilinear_wgsl!("input"),
    r#"
@compute @workgroup_size(256)
fn fill_small(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.small_w * params.small_h { return; }
    let box = second[0];
    let wf = f32(params.width);
    let hf = f32(params.height);
    // Nothing opaque, or a picture that fills the frame: no bars, and an
    // empty fill says so to fill_over.
    if !(box.z > box.x) || (box.x <= 0.0 && box.y <= 0.0 && box.z >= wf && box.w >= hf) {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let scale = max(wf / (box.z - box.x), hf / (box.w - box.y)) * max(params.zoom, 1.0);
    let cx = (box.x + box.z) * 0.5;
    let cy = (box.y + box.w) * 0.5;
    let fx = (f32(idx % params.small_w) + 0.5) * wf / f32(params.small_w);
    let fy = (f32(idx / params.small_w) + 0.5) * hf / f32(params.small_h);
    let c = bilinear_input(cx + (fx - wf * 0.5) / scale - 0.5, cy + (fy - hf * 0.5) / scale - 0.5, params.width, box.xy, box.zw - vec2<f32>(1.0));
    output[idx] = pack(vec4<f32>(c.rgb, 1.0));
}
"#
);

pub(super) const FILL_OVER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    r#"
struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
"#,
    fill_bilinear_wgsl!("second"),
    r#"
@compute @workgroup_size(256)
fn fill_over(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let rx = f32(params.small_w) / f32(params.width);
    let ry = f32(params.small_h) / f32(params.height);
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let hi = vec2<f32>(f32(params.small_w - 1u), f32(params.small_h - 1u));
    let bg = bilinear_second((x + 0.5) * rx - 0.5, (y + 0.5) * ry - 0.5, params.small_w, vec2<f32>(0.0), hi);
    if bg.a < 0.5 {
        output[idx] = input[idx];
        return;
    }
    let px = unpack(input[idx]);
    let keep = 1.0 - params.darken;
    output[idx] = pack(vec4<f32>(px.rgb * px.a + bg.rgb * keep * (1.0 - px.a), 1.0));
}
"#
);
