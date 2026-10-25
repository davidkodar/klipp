//! GPU-accelerated NV12 → RGBA conversion with optional resize.
//!
//! Uses a standalone wgpu device so it can run on the decoder thread
//! without sharing the main effects pipeline's device/queue.
//! This replaces the CPU-bound swscale path for hardware-decoded frames.

use tracing::{info, warn};

/// NV12 → RGBA GPU converter. Owns its own wgpu device + queue.
///
/// Device and queue are `Arc`-wrapped so consumers that want to hold a
/// conversion's output buffer on-GPU (`convert_nv12_to_rgba_on_gpu`)
/// can extend the lifetime across threads without cloning the
/// underlying wgpu handles.
pub struct GpuNv12Converter {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    // Reusable buffers — recreated when dimensions change
    y_buf: Option<wgpu::Buffer>,
    uv_buf: Option<wgpu::Buffer>,
    output_buf: Option<wgpu::Buffer>,
    staging_buf: Option<wgpu::Buffer>,
    /// Packed RGB10 code values for scene-referred sources (camera log,
    /// and the rare 8-bit PQ/HLG) on the CPU-readback path.
    hdr_buf: Option<wgpu::Buffer>,
    hdr_staging_buf: Option<wgpu::Buffer>,
    /// Bound as the codes output on display-referred frames, which never
    /// write it.
    hdr_dummy_buf: wgpu::Buffer,
    /// Bound as the linear output whenever the caller wants the sRGB-u8
    /// hand-off instead, which never writes it.
    linear_dummy_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    // Cached dimensions to avoid re-creating buffers
    cached_y_size: u64,
    cached_uv_size: u64,
    cached_output_size: u64,
    /// The GPU-resident outputs handed to callers, kept to be handed out
    /// again once no caller holds them (#2107).
    resident_rgba: ResidentPool,
    resident_codes: ResidentPool,
}

/// Buffers of one size handed out as `Arc`s and taken back when nothing
/// else holds them (#2107). A 4K frame's output was a fresh 33 MB buffer
/// for every frame of every stream, made and freed again a moment later.
/// One handed out again is written by a pass submitted on the same queue
/// after the passes of whoever read it last, so it is never written
/// under them; and only once it has been free for a few frames, so a
/// reader that let go of it before submitting its read has submitted it.
pub(crate) struct ResidentPool {
    label: &'static str,
    size: u64,
    /// Each buffer and the last take at which something else held it.
    buffers: Vec<(std::sync::Arc<wgpu::Buffer>, u64)>,
    takes: u64,
}

impl ResidentPool {
    /// A queue's frames, the frames held while they are shown and a few
    /// in flight. Past it a buffer is made fresh, as before, and not kept.
    const CAP: usize = 24;
    /// Takes a buffer must stay free for before it is handed out again.
    const QUIET_TAKES: u64 = 4;

    pub(crate) fn new(label: &'static str) -> Self {
        Self { label, size: 0, buffers: Vec::new(), takes: 0 }
    }

    /// A `size`-byte buffer nothing else holds: one kept, or one made.
    pub(crate) fn take(&mut self, device: &wgpu::Device, size: u64) -> std::sync::Arc<wgpu::Buffer> {
        if size != self.size {
            self.buffers.clear();
            self.size = size;
        }
        self.takes += 1;
        let now = self.takes;
        for (buffer, held_at) in &mut self.buffers {
            if std::sync::Arc::strong_count(buffer) > 1 {
                *held_at = now;
            }
        }
        if let Some((free, held_at)) = self.buffers.iter_mut().find(|(_, held_at)| now - *held_at >= Self::QUIET_TAKES) {
            *held_at = now;
            return std::sync::Arc::clone(free);
        }
        let made = std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        if self.buffers.len() < Self::CAP {
            self.buffers.push((std::sync::Arc::clone(&made), now));
        }
        made
    }

    /// How many buffers it keeps. For tests.
    pub(crate) fn kept(&self) -> usize {
        self.buffers.len()
    }
}

/// What the GPU-resident NV12 conversion produced — the same shape as the
/// P010 converter's, so the decoder treats both alike.
pub type Nv12GpuOutput = crate::p010_gpu::P010GpuOutput;

/// Params uniform for the NV12→RGBA shader (must match WGSL struct layout).
///
/// 12 u32s = 48 bytes, 16-byte aligned — fits the WGSL uniform
/// alignment rules without padding.
///
/// `primaries_matrix` carries the pre-computed source-primaries →
/// working-space matrix as 3 rows × `vec4<f32>` (12 floats; the 4th
/// component of each row is padding for WGSL's `vec3<f32>` alignment).
/// The engine builds it via
/// `klipp_color::camera_log::primaries_to_working_matrix(primaries,
/// working_colorspace)` and passes it through `pack_primaries_matrix`.
/// Sliding the working-space choice into the matrix keeps the shader
/// simple — one matmul per pixel regardless of which working space
/// the project is in.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Nv12Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,
    uv_stride: u32,
    rotation: u32, // 0, 90, 180, 270
    /// `[range, matrix_id, transfer_id, primaries_id]` from
    /// `klipp_core::color_ids::ids(SourceColorspace)`. Drives the
    /// shader's branching for limited/full range, BT.709/BT.601/
    /// BT.2020 matrix, and EOTF selection. `primaries_id` is no
    /// longer consumed by the shader — gamut conversion now goes
    /// through `primaries_matrix` — but the field is preserved on
    /// the API boundary for future per-primaries dispatch (e.g.
    /// gamut-mapping modes that DO need to know the source identity).
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    /// Write linear working-space f32 to the `linear` binding instead
    /// of sRGB-u8 to `output`. The shader computes the linear value
    /// either way — this only decides which of them is thrown away.
    ///
    /// The u8 hand-off is right for the viewer, which is showing an
    /// 8-bit picture anyway. It is wrong for a render: quantising to
    /// 8-bit RGB here costs real precision, because YUV→RGB is a
    /// matrix and its output does not fit back into 8 bits.
    write_linear: u32,
    /// Source-primaries → working-space matrix, packed as 3 rows ×
    /// vec4 (12 floats). Row 0 is dotted against (R, G, B, _) to
    /// produce out.R, etc.
    primaries_matrix: [[f32; 4]; 3],
}

/// Per-shader code that runs after the shared colourspace prelude and
/// helpers ([`crate::color_wgsl::WGSL_PRELUDE`] plus
/// [`crate::color_wgsl::WGSL_HELPERS`]). Reads NV12 (8-bit) Y/UV
/// planes, dispatches the colourspace stages from the shared
/// helpers, re-encodes as sRGB-u8 for the existing CPU/GPU
/// hand-off in `klipp-engine::video_decoder::rgba_u8_to_linear`.
///
/// Source rotation is folded into the same pass — zero-cost since
/// it just remaps the source coordinate before sampling.
const SHADER_NV12_BODY: &str = "
@group(0) @binding(0) var<storage, read> y_plane: array<u32>;
@group(0) @binding(1) var<storage, read> uv_plane: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;
// Packed RGB10 code values for scene-referred sources; dummy otherwise.
@group(0) @binding(4) var<storage, read_write> hdr_codes: array<u32>;
// Linear working-space output, for a caller that wants the frame at full
// precision rather than the sRGB-u8 hand-off; dummy otherwise.
@group(0) @binding(5) var<storage, read_write> linear: array<vec4<f32>>;

struct Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,
    uv_stride: u32,
    rotation: u32,
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    write_linear: u32,
    // Source-primaries → working-space matrix (3 rows × vec4; the .w
    // of each row is unused padding for WGSL's vec3 alignment).
    primaries_row0: vec4<f32>,
    primaries_row1: vec4<f32>,
    primaries_row2: vec4<f32>,
}

// One source pixel of a display-referred frame, as linear working-space
// RGB: the same reads and the same four stages as `main` below, for the
// downscale to average (#744).
fn display_working_at(sx: u32, sy: u32) -> vec3<f32> {
    let yb = sy * params.y_stride + sx;
    let y_val = f32((y_plane[yb / 4u] >> ((yb % 4u) * 8u)) & 0xFFu);
    let cw = (params.src_width + 1u) / 2u;
    let ch = (params.src_height + 1u) / 2u;
    let fx = clamp(f32(sx) * 0.5, 0.0, f32(cw - 1u));
    let fy = clamp(f32(sy) * 0.5 - 0.25, 0.0, f32(ch - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, cw - 1u);
    let y1 = min(y0 + 1u, ch - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    var u_acc = 0.0;
    var v_acc = 0.0;
    for (var j = 0u; j < 2u; j = j + 1u) {
        let cy = select(y0, y1, j == 1u);
        let wy = select(1.0 - ty, ty, j == 1u);
        for (var i = 0u; i < 2u; i = i + 1u) {
            let cx = select(x0, x1, i == 1u);
            let w = select(1.0 - tx, tx, i == 1u) * wy;
            let ub = cy * params.uv_stride + cx * 2u;
            u_acc = u_acc + f32((uv_plane[ub / 4u] >> ((ub % 4u) * 8u)) & 0xFFu) * w;
            let vb = ub + 1u;
            v_acc = v_acc + f32((uv_plane[vb / 4u] >> ((vb % 4u) * 8u)) & 0xFFu) * w;
        }
    }
    let yuv = yuv_normalise(y_val, u_acc, v_acc, params.color_range, 255.0);
    let rgb_linear = display_to_linear(yuv_to_rgb_display(yuv, params.matrix_id), params.transfer_id);
    return vec3<f32>(
        dot(params.primaries_row0.xyz, rgb_linear),
        dot(params.primaries_row1.xyz, rgb_linear),
        dot(params.primaries_row2.xyz, rgb_linear),
    );
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = gid.y;
    if (dx >= params.dst_width || dy >= params.dst_height) { return; }

    // A downscale of a display-referred frame averages the footprint in
    // linear light (#744); see `RESIZE_WGSL`. A scene-referred one keeps
    // the nearest lookup below, which its CPU twin matches.
    let scene_referred = params.transfer_id == TRANSFER_PQ || params.transfer_id == TRANSFER_HLG || params.transfer_id >= 100u;
    if (!scene_referred) {
        let fp = resize_footprint(dx, dy);
        let nx = resize_samples(fp.y);
        let ny = resize_samples(fp.w);
        // Shrinking on a side averages; growing is Lanczos-3 (#2557).
        let upscale = nx == 1u && ny == 1u && (fp.y < 0.999 || fp.w < 0.999);
        if (nx > 1u || ny > 1u || upscale) {
            var working: vec3<f32>;
            if (upscale) {
                working = resize_lanczos(fp);
            } else {
                var acc = vec3<f32>(0.0);
                for (var j = 0u; j < ny; j = j + 1u) {
                    let py = resize_tap(fp.z, fp.w, j, ny, params.src_height);
                    for (var i = 0u; i < nx; i = i + 1u) {
                        acc = acc + display_working_at(resize_tap(fp.x, fp.y, i, nx, params.src_width), py);
                    }
                }
                working = acc / f32(nx * ny);
            }
            if params.write_linear != 0u {
                linear[dy * params.dst_width + dx] = pack(vec4<f32>(working, 1.0));
                return;
            }
            let srgb = linear_to_srgb_vec(working);
            let r = u32(clamp(srgb.x * 255.0 + 0.5, 0.0, 255.0));
            let g = u32(clamp(srgb.y * 255.0 + 0.5, 0.0, 255.0));
            let b = u32(clamp(srgb.z * 255.0 + 0.5, 0.0, 255.0));
            output[dy * params.dst_width + dx] = r | (g << 8u) | (b << 16u) | (255u << 24u);
            return;
        }
    }

    // Source coord lookup (rotation + nearest-neighbour resize).
    var sx: u32;
    var sy: u32;
    if params.rotation == 0u {
        sx = dx * params.src_width / params.dst_width;
        sy = dy * params.src_height / params.dst_height;
    } else if params.rotation == 90u {
        sx = dy * params.src_width / params.dst_height;
        sy = (params.dst_width - 1u - dx) * params.src_height / params.dst_width;
    } else if params.rotation == 180u {
        sx = (params.dst_width - 1u - dx) * params.src_width / params.dst_width;
        sy = (params.dst_height - 1u - dy) * params.src_height / params.dst_height;
    } else { // 270
        sx = (params.dst_height - 1u - dy) * params.src_width / params.dst_height;
        sy = dx * params.src_height / params.dst_width;
    }
    sx = min(sx, params.src_width - 1u);
    sy = min(sy, params.src_height - 1u);

    // Read raw 8-bit Y / U / V samples via byte access on the u32-
    // packed planes.
    let y_byte_idx = sy * params.y_stride + sx;
    let y_word = y_plane[y_byte_idx / 4u];
    let y_shift = (y_byte_idx % 4u) * 8u;
    let y_val = f32((y_word >> y_shift) & 0xFFu);

    // Chroma is reconstructed, not repeated. See
    // `YuvSamples::chroma_at`, the CPU twin, for why the vertical
    // coordinate carries a quarter-sample offset and the horizontal one
    // does not: NV12 is 4:2:0 sited per H.264 `chroma_sample_loc_type =
    // 0`, co-sited horizontally and centred between luma rows. These two
    // must stay identical or `sw_decode_parity` fails.
    let cw = (params.src_width + 1u) / 2u;
    let ch = (params.src_height + 1u) / 2u;
    let fx = clamp(f32(sx) * 0.5, 0.0, f32(cw - 1u));
    let fy = clamp(f32(sy) * 0.5 - 0.25, 0.0, f32(ch - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, cw - 1u);
    let y1 = min(y0 + 1u, ch - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    // Interleaved plane: the U/V pair for chroma column x starts at byte
    // 2x, so a horizontal neighbour is two bytes away, not one.
    var u_acc = 0.0;
    var v_acc = 0.0;
    for (var j = 0u; j < 2u; j = j + 1u) {
        let cy = select(y0, y1, j == 1u);
        let wy = select(1.0 - ty, ty, j == 1u);
        for (var i = 0u; i < 2u; i = i + 1u) {
            let cx = select(x0, x1, i == 1u);
            let wx = select(1.0 - tx, tx, i == 1u);
            let w = wx * wy;
            let ub = cy * params.uv_stride + cx * 2u;
            let uw = uv_plane[ub / 4u];
            u_acc = u_acc + f32((uw >> ((ub % 4u) * 8u)) & 0xFFu) * w;
            let vb = ub + 1u;
            let vw = uv_plane[vb / 4u];
            v_acc = v_acc + f32((vw >> ((vb % 4u) * 8u)) & 0xFFu) * w;
        }
    }
    let u_val = u_acc;
    let v_val = v_acc;

    // Four-stage colourspace pipeline from the shared WGSL helpers:
    // 1. Range normalise (limited 16/235 vs full 0/255).
    // 2. YUV → display-encoded RGB via the source's matrix.
    // 3. Inverse EOTF → linear-light in source primaries.
    // 4. Source primaries → working space via the uniform matrix.
    // Then re-encode to sRGB for the u8 hand-off.
    let yuv = yuv_normalise(y_val, u_val, v_val, params.color_range, 255.0);
    let rgb_display = yuv_to_rgb_display(yuv, params.matrix_id);
    var rgb_srgb: vec3<f32>;
    // The working-space linear value, before the display encode below
    // throws its precision away. For a scene-referred source this is
    // scene-linear and *not* tone-mapped: the tone map is a display
    // step, and a grade wants the scene.
    var rgb_working_linear: vec3<f32>;
    let is_log = params.transfer_id >= 100u;
    if (params.transfer_id == TRANSFER_PQ || params.transfer_id == TRANSFER_HLG || is_log) {
        // Scene-referred: the same route as the P010 decoder. 8-bit log
        // (e.g. 8-bit XAVC S-Log3) lands in 10-bit code space by the same
        // mapping — 64 and 876 are 16 and 219 scaled by 2^(10-8).
        var signal = rgb_display;
        if (is_log && params.color_range == 0u) {
            signal = (vec3<f32>(64.0) + signal * 876.0) / 1023.0;
        }
        // Clamp to the code range, after the remap: camera logs record
        // real highlights above legal white (S-Log3 940 is ~18x grey, 1023
        // ~38x), so a legal-range clamp would flatten the top stop.
        // PQ/HLG signal is legal-range, so for them this is the same clamp.
        signal = clamp(signal, vec3<f32>(0.0), vec3<f32>(1.0));
        let code = vec3<u32>(signal * 1023.0 + vec3<f32>(0.5));
        hdr_codes[dy * params.dst_width + dx] = code.x | (code.y << 10u) | (code.z << 20u);
        var scale = 100.0;
        var exposure = HDR_DISPLAY_EXPOSURE;
        if (is_log) {
            scale = 1.0;
            exposure = LOG_DISPLAY_EXPOSURE;
        }
        let scene = display_to_linear(vec3<f32>(code) / 1023.0, params.transfer_id) * scale;
        let working = vec3<f32>(
            dot(params.primaries_row0.xyz, scene),
            dot(params.primaries_row1.xyz, scene),
            dot(params.primaries_row2.xyz, scene),
        );
        rgb_working_linear = working;
        rgb_srgb = linear_to_srgb_vec(tone_map_hdr_vec(working, exposure));
    } else {
        let rgb_linear = display_to_linear(rgb_display, params.transfer_id);
        let rgb_working = vec3<f32>(
            dot(params.primaries_row0.xyz, rgb_linear),
            dot(params.primaries_row1.xyz, rgb_linear),
            dot(params.primaries_row2.xyz, rgb_linear),
        );
        rgb_working_linear = rgb_working;
        rgb_srgb = linear_to_srgb_vec(rgb_working);
    }

    // Full precision for a caller that is grading or rendering; the
    // sRGB-u8 hand-off below only for one that is showing 8 bits.
    if params.write_linear != 0u {
        linear[dy * params.dst_width + dx] = pack(vec4<f32>(rgb_working_linear, 1.0));
        return;
    }

    let ri = u32(clamp(rgb_srgb.x * 255.0 + 0.5, 0.0, 255.0));
    let gi = u32(clamp(rgb_srgb.y * 255.0 + 0.5, 0.0, 255.0));
    let bi = u32(clamp(rgb_srgb.z * 255.0 + 0.5, 0.0, 255.0));
    let pixel = ri | (gi << 8u) | (bi << 16u) | (255u << 24u);

    output[dy * params.dst_width + dx] = pixel;
}
";

/// WGSL shared by the NV12 and P010 decode shaders for a downscale (#744):
/// where an output pixel's footprint lies in the source, through the
/// rotation, and how many samples across it to average.
///
/// The nearest-neighbour lookup each shader keeps for 1:1 and upscales
/// takes one source pixel per output pixel. Shrinking 4K to 1080 that
/// way drops three pixels in four, and fine detail aliases: the viewer
/// shimmered on it, and the export could not use this shader at all and
/// went back through the CPU. A downscale instead averages an `n x m`
/// grid of samples spread evenly over the footprint, in linear light: a
/// box filter at integer ratios, close to an area filter otherwise. The
/// grid is capped at 8 a side, so a thumbnail of a 4K frame stays cheap.
///
/// An upscale is Lanczos-3 (#2557), the kernel swscale's export path used,
/// over 6 x 6 source pixels: a 1080p source in a 4K export was refused here
/// for nearest's blockiness, and went down to the CPU to be scaled there,
/// at 44 ms a frame. Unlike the downscale it filters the sRGB-encoded
/// values, as swscale does, not linear light: on a test chart that matches
/// swscale's export to a level (as 1:1 does), where in linear light hard
/// edges came out up to 61 levels off it, and a Lanczos kernel's ringing
/// round a hard edge looks worse in linear light besides.
pub(crate) const RESIZE_WGSL: &str = "
// Lanczos-3 at distance x: sinc(x) sinc(x / 3) inside three pixels.
fn lanczos3(x: f32) -> f32 {
    let ax = abs(x);
    if (ax < 0.00001) { return 1.0; }
    if (ax >= 3.0) { return 0.0; }
    let px = 3.14159265358979 * ax;
    return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}

// Output pixel `fp` (a footprint under one source pixel on some side, an
// upscale) as linear working-space RGB: Lanczos-3 over the 6 x 6 source
// pixels round its centre, normalised, on their sRGB-encoded values (see
// `RESIZE_WGSL`'s doc), the ringing held to the encoded range.
fn resize_lanczos(fp: vec4<f32>) -> vec3<f32> {
    let cx = fp.x + fp.y * 0.5 - 0.5;
    let cy = fp.z + fp.w * 0.5 - 0.5;
    let ix = i32(floor(cx));
    let iy = i32(floor(cy));
    let max_x = i32(params.src_width) - 1;
    let max_y = i32(params.src_height) - 1;
    var acc = vec3<f32>(0.0);
    var total = 0.0;
    for (var j = -2; j <= 3; j = j + 1) {
        let wy = lanczos3(cy - f32(iy + j));
        let py = u32(clamp(iy + j, 0, max_y));
        for (var i = -2; i <= 3; i = i + 1) {
            let w = lanczos3(cx - f32(ix + i)) * wy;
            acc = acc + linear_to_srgb_vec(max(display_working_at(u32(clamp(ix + i, 0, max_x)), py), vec3<f32>(0.0))) * w;
            total = total + w;
        }
    }
    let e = clamp(acc / total, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(pow((e + 0.055) / 1.055, vec3<f32>(2.4)), e / 12.92, e <= vec3<f32>(0.04045));
}

// The source footprint of output pixel (dx, dy): x0, width, y0, height,
// in source pixels, through the rotation the nearest lookup uses.
fn resize_footprint(dx: u32, dy: u32) -> vec4<f32> {
    var a: u32; var a_n: u32; var b: u32; var b_n: u32;
    if (params.rotation == 0u) {
        a = dx; a_n = params.dst_width; b = dy; b_n = params.dst_height;
    } else if (params.rotation == 90u) {
        a = dy; a_n = params.dst_height; b = params.dst_width - 1u - dx; b_n = params.dst_width;
    } else if (params.rotation == 180u) {
        a = params.dst_width - 1u - dx; a_n = params.dst_width; b = params.dst_height - 1u - dy; b_n = params.dst_height;
    } else {
        a = params.dst_height - 1u - dy; a_n = params.dst_height; b = dx; b_n = params.dst_width;
    }
    let sw = f32(params.src_width) / f32(a_n);
    let sh = f32(params.src_height) / f32(b_n);
    return vec4<f32>(f32(a) * sw, sw, f32(b) * sh, sh);
}

// Samples to take across a footprint `span` source pixels wide.
fn resize_samples(span: f32) -> u32 {
    return clamp(u32(ceil(span - 0.001)), 1u, 8u);
}

// The source pixel under sample `i` of `n` across [x0, x0 + span).
fn resize_tap(x0: f32, span: f32, i: u32, n: u32, extent: u32) -> u32 {
    return min(u32(x0 + (f32(i) + 0.5) / f32(n) * span), extent - 1u);
}
";

/// Full shader source: shared prelude + helpers + the pixel storage hook
/// + NV12-specific body.
///
/// The storage hook is here because the `linear` output is a frame buffer
/// in the chain's own format — it gets copied into `buf_a` — so its store
/// has to go through `pack` like every other pixel write in the codebase.
/// It is assembled at runtime from three pieces; [`nv12_shader`] builds
/// it once, and `gpu::shader_sources` lists that, so the WGSL validation
/// and `pixel_access_goes_through_pack` check it like the rest.
fn nv12_shader_source() -> String {
    let mut s = String::with_capacity(
        crate::color_wgsl::WGSL_PRELUDE.len()
            + crate::color_wgsl::WGSL_HELPERS.len()
            + SHADER_NV12_BODY.len()
            + RESIZE_WGSL.len()
            + 256,
    );
    s.push_str(crate::color_wgsl::WGSL_PRELUDE);
    s.push_str(crate::color_wgsl::WGSL_HELPERS);
    s.push_str(crate::gpu::shader_pixel_storage_source());
    s.push_str(RESIZE_WGSL);
    s.push_str(SHADER_NV12_BODY);
    s
}

/// [`nv12_shader_source`], built once.
pub(crate) fn nv12_shader() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(nv12_shader_source)
}

impl GpuNv12Converter {
    /// How many GPU-resident output buffers the converter keeps to hand
    /// out again (#2107). For tests.
    #[doc(hidden)]
    pub fn resident_buffers_kept(&self) -> usize {
        self.resident_rgba.kept()
    }

    /// Create a new GPU NV12 converter. Shares the process-wide
    /// `GpuContext` with every other GPU-using type in the engine
    /// (the effects pipeline and the P010 converter), so buffers it
    /// produces can be bound directly as grade-chain inputs without
    /// a device-to-device copy.
    ///
    /// Returns `None` if no suitable GPU adapter exists (falls back
    /// to CPU conversion at the caller). When the shared context's
    /// initial setup has already failed for this process, this will
    /// not retry — see `GpuContext::shared`.
    pub fn new() -> Option<Self> {
        let ctx = crate::gpu_context::GpuContext::shared()?;
        Self::with_context(ctx)
    }

    /// Create an NV12 converter that reuses an existing `GpuContext`.
    /// The decoder thread holds this converter; buffers it produces
    /// via [`Self::convert_nv12_to_rgba_on_gpu`] live on `ctx.device`
    /// and can be bound directly as compute-shader inputs by any
    /// other pipeline built on the same context (phase 4's grade
    /// chain).
    pub fn with_context(ctx: &crate::gpu_context::GpuContext) -> Option<Self> {
        let device = ctx.device.clone();
        let queue = ctx.queue.clone();

        // Bind group layout: y_plane (read), uv_plane (read), output (read_write), params (uniform)
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("nv12_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Linear working-space output.
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Push error scope to catch shader compilation errors
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12_to_rgba"),
            source: wgpu::ShaderSource::Wgsl(nv12_shader().into()),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("nv12_to_rgba"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });

        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            crate::shader_check::shader_failed("NV12 converter", &err.to_string());
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_params"),
            size: std::mem::size_of::<Nv12Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let hdr_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_hdr_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let linear_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_linear_dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        info!("NV12 GPU converter initialized");

        Some(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            y_buf: None,
            uv_buf: None,
            output_buf: None,
            staging_buf: None,
            hdr_buf: None,
            hdr_staging_buf: None,
            hdr_dummy_buf,
            linear_dummy_buf,
            params_buf,
            cached_y_size: 0,
            cached_uv_size: 0,
            cached_output_size: 0,
            resident_rgba: ResidentPool::new("nv12_out_gpu_resident"),
            resident_codes: ResidentPool::new("nv12_hdr_codes_gpu_resident"),
        })
    }

    /// Clone the Arc-wrapped wgpu device. Callers constructing a
    /// `GpuFrame` from `convert_nv12_to_rgba_on_gpu` output pair this
    /// with `queue()`.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Clone the Arc-wrapped wgpu queue. See [`Self::device`].
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    /// Ensure Y plane buffer is large enough.
    fn ensure_y_buf(&mut self, size: u64) {
        if size <= self.cached_y_size {
            return;
        }
        // Round up to multiple of 4 for u32 storage access
        let size = (size + 3) & !3;
        self.y_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_y"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_y_size = size;
    }

    /// Ensure UV plane buffer is large enough.
    fn ensure_uv_buf(&mut self, size: u64) {
        if size <= self.cached_uv_size {
            return;
        }
        let size = (size + 3) & !3;
        self.uv_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_uv"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_uv_size = size;
    }

    /// Ensure output + staging buffers are large enough for RGBA output.
    fn ensure_output_buf(&mut self, size: u64) {
        if size <= self.cached_output_size {
            return;
        }
        self.output_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_out"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_staging"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cached_output_size = size;
    }

    /// Convert NV12 frame to RGBA with optional resize and rotation on the GPU.
    ///
    /// - `y_data`: Y plane bytes (src_height rows, each y_stride bytes)
    /// - `uv_data`: interleaved UV plane bytes (src_height/2 rows, each uv_stride bytes)
    /// - `src_width`, `src_height`: source frame dimensions
    /// - `y_stride`, `uv_stride`: byte stride per row for each plane
    /// - `dst_width`, `dst_height`: desired output dimensions (caller must swap for 90°/270°)
    /// - `rotation`: source rotation in degrees (0, 90, 180, 270)
    /// - `color_ids`: `[range, matrix_id, transfer_id, primaries_id]`
    ///   from `klipp_core::color_ids::ids(SourceColorspace)`.
    /// - `primaries_matrix`: source-primaries → working-space 3x3,
    ///   row-major. Pass [`crate::color_wgsl::pack_primaries_matrix`]
    ///   the output of `klipp_color::camera_log::primaries_to_working_matrix`
    ///   to get the working-space matrix; the helper packs to the
    ///   3-row vec4 layout the shader expects. For pure BT.709 SDR
    ///   sources targeting `Rec709Linear` working space, identity
    ///   ([`crate::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED`])
    ///   is correct.
    ///
    /// Returns the RGBA pixel data (dst_width * dst_height * 4 bytes).
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_rgba(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Vec<u8> {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);
        self.ensure_output_buf(output_size);
        // Scene-referred sources write codes too; give them a real buffer
        // so nothing is written out of bounds.
        let hdr = crate::p010_gpu::is_hdr_transfer(color_ids[2]);
        if hdr && self.hdr_buf.as_ref().is_none_or(|b| b.size() < output_size) {
            self.hdr_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nv12_hdr"),
                size: output_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
        }

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        let output_buf = self.output_buf.as_ref().unwrap();
        let hdr_binding = if hdr { self.hdr_buf.as_ref().unwrap() } else { &self.hdr_dummy_buf };

        // Upload planes
        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        // Write params
        let params = Nv12Params {
            src_width,
            src_height,
            dst_width,
            dst_height,
            y_stride,
            uv_stride,
            rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        // Create bind group
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: y_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uv_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: hdr_binding.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
                },
            ],
        });

        // Dispatch compute shader with 2D workgroups (16×16 threads)
        let workgroups_x = dst_width.div_ceil(16);
        let workgroups_y = dst_height.div_ceil(16);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }

        // Copy output to staging buffer for readback
        let staging = self.staging_buf.as_ref().unwrap();
        encoder.copy_buffer_to_buffer(output_buf, 0, staging, 0, output_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        // Map + poll-loop + read. `Maintain::Wait` and
        // `WaitForSubmissionIndex` both unreliably deadlock on a
        // device shared with a busy UI thread — see
        // `gpu.rs::poll_until_mapped` for the writeup. Inline the
        // same polling pattern here.
        let buffer_slice = staging.slice(..output_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Nv12Convert);
        let map_result = {
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(2);
            loop {
                self.device.poll(wgpu::Maintain::Poll);
                match rx.try_recv() {
                    Ok(v) => break Some(v),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if start.elapsed() >= timeout { break None; }
                        std::thread::sleep(std::time::Duration::from_micros(100));
                    }
                }
            }
        };
        crate::gpu::mark_gpu_stage(crate::gpu::GpuStage::Idle);

        match map_result {
            Some(Ok(())) => {
                let data = buffer_slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                result
            }
            _ => {
                warn!("NV12 GPU converter: readback failed, returning zeros");
                vec![0u8; output_size as usize]
            }
        }
    }

    /// GPU-resident variant of [`Self::convert_nv12_to_rgba`]: runs
    /// the NV12→RGBA compute shader and returns the output
    /// `wgpu::Buffer` directly, without downloading to CPU. The
    /// returned buffer is freshly allocated per call (so holding it
    /// across subsequent calls is safe — no internal aliasing) and
    /// flagged `STORAGE | COPY_SRC` so it can both feed another
    /// compute shader (phase 4's grade chain) and be copied into a
    /// staging buffer for readback via `Frame::into_cpu()`.
    ///
    /// Pair with [`Self::device`] / [`Self::queue`] to assemble a
    /// `klipp_engine::GpuFrame` — that's the canonical way to hand a
    /// GPU-resident frame across the engine boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_rgba_on_gpu(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
        // `color_ids` is `[range, matrix_id, transfer_id, primaries_id]`.
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> std::sync::Arc<wgpu::Buffer> {
        self.convert_nv12_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        )
        .rgba
    }

    /// GPU-resident conversion returning the u8 buffer and, for
    /// scene-referred sources (camera log, PQ, HLG), the packed RGB10 code
    /// values the grade reads. Both freshly allocated for the caller.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_on_gpu(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Nv12GpuOutput {
        let y_size = y_data.len() as u64;
        let uv_size = uv_data.len() as u64;
        let output_size = (dst_width * dst_height * 4) as u64;

        self.ensure_y_buf(y_size);
        self.ensure_uv_buf(uv_size);
        let hdr_codes = crate::p010_gpu::is_hdr_transfer(color_ids[2]).then(|| self.resident_codes.take(&self.device, output_size));
        let hdr_binding: &wgpu::Buffer = match hdr_codes.as_ref() {
            Some(b) => b,
            None => &self.hdr_dummy_buf,
        };

        // The caller's to hold: a buffer nothing else holds now, from the
        // pool, not the internal cache the readback paths share.
        let output_buf = self.resident_rgba.take(&self.device, output_size);

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();

        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = Nv12Params {
            src_width,
            src_height,
            dst_width,
            dst_height,
            y_stride,
            uv_stride,
            rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 0,
            primaries_matrix,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: y_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uv_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: hdr_binding.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.linear_dummy_buf.as_entire_binding(),
                },
            ],
        });

        let workgroups_x = dst_width.div_ceil(16);
        let workgroups_y = dst_height.div_ceil(16);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        Nv12GpuOutput { rgba: output_buf, hdr_codes }
    }

    /// [`Self::convert_nv12_on_gpu`] at full precision: the frame lands as
    /// linear working-space f32 rather than sRGB u8, ready to be a grade
    /// chain's input slot.
    ///
    /// This is the render's route. The u8 hand-off is the right shape for
    /// the viewer, which is showing eight bits anyway, but it quantises —
    /// YUV→RGB is a matrix and its output does not fit back into 8 bits —
    /// and a delivered frame should not pay that. The shader computes the
    /// linear value either way; `write_linear` only decides which one is
    /// discarded.
    ///
    /// The buffer comes back on **this converter's device**, so a caller
    /// that means to hand it to the grade chain must have built the
    /// converter with [`Self::with_context`] on the same context the chain
    /// uses. Rotation and resize happen here, in the same pass, so the
    /// caller does not need a separate CPU pass for either.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_to_linear_on_gpu(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> std::sync::Arc<wgpu::Buffer> {
        self.ensure_y_buf(y_data.len() as u64);
        self.ensure_uv_buf(uv_data.len() as u64);

        // 16 bytes a pixel, and the caller takes ownership, so this is
        // not the internal cache.
        let linear_buf = std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_linear_gpu_resident"),
            size: (dst_width as u64) * (dst_height as u64) * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));

        let y_buf = self.y_buf.as_ref().unwrap();
        let uv_buf = self.uv_buf.as_ref().unwrap();
        self.queue.write_buffer(y_buf, 0, y_data);
        self.queue.write_buffer(uv_buf, 0, uv_data);

        let params = Nv12Params {
            src_width,
            src_height,
            dst_width,
            dst_height,
            y_stride,
            uv_stride,
            rotation,
            color_range: color_ids[0],
            matrix_id: color_ids[1],
            transfer_id: color_ids[2],
            primaries_id: color_ids[3],
            write_linear: 1,
            primaries_matrix,
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12_to_linear_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: y_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uv_buf.as_entire_binding() },
                // The u8 output is the one going unwritten this time.
                wgpu::BindGroupEntry { binding: 2, resource: self.linear_dummy_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.hdr_dummy_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: linear_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nv12_to_linear") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(dst_width.div_ceil(16), dst_height.div_ceil(16), 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        linear_buf
    }

    /// Convert one scene-referred NV12 frame and read back its packed RGB10
    /// code values. `None` for a display-referred transfer or a failed
    /// readback, so the caller can fall back to `convert_nv12_to_rgba`.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_nv12_hdr_codes(
        &mut self,
        y_data: &[u8],
        uv_data: &[u8],
        src_width: u32,
        src_height: u32,
        y_stride: u32,
        uv_stride: u32,
        dst_width: u32,
        dst_height: u32,
        rotation: u32,
        color_ids: [u32; 4],
        primaries_matrix: [[f32; 4]; 3],
    ) -> Option<Vec<u32>> {
        if !crate::p010_gpu::is_hdr_transfer(color_ids[2]) {
            return None;
        }
        let out = self.convert_nv12_on_gpu(
            y_data, uv_data, src_width, src_height, y_stride, uv_stride,
            dst_width, dst_height, rotation, color_ids, primaries_matrix,
        );
        let codes = out.hdr_codes?;
        let size = codes.size();
        if self.hdr_staging_buf.as_ref().is_none_or(|b| b.size() < size) {
            self.hdr_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nv12_hdr_staging"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let staging = self.hdr_staging_buf.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("nv12_hdr_readback"),
        });
        encoder.copy_buffer_to_buffer(&codes, 0, staging, 0, size);
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = staging.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        match crate::gpu::poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let words = bytemuck::cast_slice::<u8, u32>(&slice.get_mapped_range()).to_vec();
                staging.unmap();
                Some(words)
            }
            _ => {
                warn!("NV12 GPU converter: code readback failed");
                None
            }
        }
    }
}
