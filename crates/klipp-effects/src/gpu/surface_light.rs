//! A pinned layer taking its surface's light and texture on the GPU
//! (#2751): the twin of [`crate::surface_light::take_surface`], run on the
//! layer just before it is blended onto the accumulator under it.
use super::*;
use crate::surface_light as light;

/// The pass's buffers: the grid, its blur, the row sums, a frame of
/// factors, and the cell and pixel counts they hold.
pub(super) struct Scratch {
    grid: wgpu::Buffer,
    blur: wgpu::Buffer,
    rows: wgpu::Buffer,
    frame: wgpu::Buffer,
    cells: u64,
    pixels: u64,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceParams {
    width: u32,
    height: u32,
    op: u32,
    scale: u32,
    small_w: u32,
    small_h: u32,
    radius: u32,
    shade: f32,
    texture: f32,
    _pad: [u32; 3],
    /// The working space's luminance weights.
    luma: [f32; 4],
}

impl GpuEffectsPipeline {
    /// The layer in `slot` given the light and texture of what is laid
    /// under it (#2751), as [`light::take_surface`] gives it: `shade` and
    /// `texture` 0 to 1, `luma` the working space's weights. Run just
    /// before the slot is blended; `false` when the pass could not run.
    pub fn stack_take_surface(&mut self, slot: usize, shade: f32, texture: f32, luma: [f32; 3]) -> bool {
        if shade <= 0.0 && texture <= 0.0 {
            return true;
        }
        if !self.stack_ensure_acc() || slot >= self.stack_layers.len() {
            return false;
        }
        let (fw, fh) = self.stack_dims;
        let scale = light::SCALE;
        let (sw, sh) = (fw.div_ceil(scale), fh.div_ceil(scale));
        let (cells, pixels) = (u64::from(sw) * u64::from(sh), u64::from(fw) * u64::from(fh));
        if self.surface_light_scratch.as_ref().is_none_or(|s| s.cells < cells || s.pixels < pixels) {
            let make = |label, n: u64| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: n.max(1) * 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
            self.surface_light_scratch = Some(Scratch {
                grid: make("surface light grid", cells),
                blur: make("surface light blur", cells),
                rows: make("surface light rows", u64::from(sh)),
                frame: make("surface light frame", pixels),
                cells,
                pixels,
            });
        }
        let params = |op: u32| SurfaceParams {
            width: fw, height: fh, op, scale, small_w: sw, small_h: sh,
            radius: light::radius_cells(fw, fh), shade, texture, _pad: [0; 3],
            luma: [luma[0], luma[1], luma[2], 0.0],
        };
        let acc = self.stack_acc.clone().unwrap();
        let layer = self.stack_layers[slot].clone();
        let s = self.surface_light_scratch.as_ref().unwrap();
        let groups = |n: u64| n.div_ceil(u64::from(WORKGROUP_SIZE)) as u32;
        let pass = |this: &Self, op: u32, read: &wgpu::Buffer, rw: &wgpu::Buffer, n: u64| {
            this.write_params(&params(op));
            this.stack_pass(&this.pipelines.surface_light_pipeline, read, rw, groups(n), "surface light");
        };
        pass(self, 0, &acc, &s.grid, cells);
        pass(self, 1, &layer, &s.grid, cells);
        for _ in 0..light::BLUR_ROUNDS {
            pass(self, 2, &s.grid, &s.blur, cells);
            pass(self, 3, &s.blur, &s.grid, cells);
        }
        pass(self, 4, &s.grid, &s.rows, u64::from(sh));
        pass(self, 5, &s.rows, &s.blur, 1);
        pass(self, 6, &s.blur, &s.grid, cells);
        pass(self, 7, &acc, &s.frame, pixels);
        pass(self, 8, &s.grid, &s.frame, pixels);
        pass(self, 9, &s.frame, &layer, pixels);
        true
    }
}

pub(super) const SHADER_SURFACE_LIGHT: &str = concat!(
    "
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    small_w: u32, small_h: u32, radius: u32, shade: f32,
    texture: f32, _p0: u32, _p1: u32, _p2: u32,
    luma: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
",
    shader_pixel_storage_wgsl!(),
    "
const MAX_RATIO: f32 = 4.0;
const EPS: f32 = 1e-3;

fn luma(c: vec3<f32>) -> f32 {
    return max(dot(c, p.luma.xyz), 0.0);
}

fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[u32(cy) * p.small_w + u32(cx)]);
}

fn ratio(a: f32, b: f32) -> f32 {
    return clamp((a + EPS) / (b + EPS), 1.0 / MAX_RATIO, MAX_RATIO);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    let cells = p.small_w * p.small_h;
    let pixels = p.width * p.height;
    if p.op <= 1u {
        // 0: each cell's picture luminance; 1: how much the layer covers
        // it, beside it.
        if i >= cells { return; }
        let sx = i % p.small_w; let sy = i / p.small_w;
        var sum = 0.0;
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                if p.op == 0u {
                    sum += luma(c.xyz) * clamp(c.w, 0.0, 1.0);
                } else {
                    sum += clamp(c.w, 0.0, 1.0);
                }
                n += 1.0;
            }
        }
        let mean = sum / max(n, 1.0);
        if p.op == 0u {
            output[i] = pack(vec4<f32>(mean, 0.0, 0.0, 0.0));
        } else {
            let was = unpack(output[i]);
            output[i] = pack(vec4<f32>(was.x, 0.0, mean, 0.0));
        }
    } else if p.op <= 3u {
        // The luminance blurred across (2) and down (3), the coverage kept.
        if i >= cells { return; }
        let sx = i32(i % p.small_w); let sy = i32(i / p.small_w);
        var sum = 0.0;
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) {
            if p.op == 2u { sum += small_at(sx + d, sy).x; } else { sum += small_at(sx, sy + d).x; }
        }
        output[i] = pack(vec4<f32>(sum / f32(2 * r + 1), 0.0, small_at(sx, sy).z, 0.0));
    } else if p.op == 4u {
        // Each row's local light weighted by coverage, and its coverage.
        if i >= p.small_h { return; }
        var sum = vec2<f32>(0.0);
        for (var x = 0u; x < p.small_w; x++) {
            let c = unpack(input[i * p.small_w + x]);
            sum += vec2<f32>(c.x * c.z, c.z);
        }
        output[i] = pack(vec4<f32>(sum, 0.0, 0.0));
    } else if p.op == 5u {
        // The rows summed.
        if i != 0u { return; }
        var sum = vec2<f32>(0.0);
        for (var y = 0u; y < p.small_h; y++) { sum += unpack(input[y]).xy; }
        output[0] = pack(vec4<f32>(sum, 0.0, 0.0));
    } else if p.op == 6u {
        // The surface's light into every cell; -1 where nothing is covered.
        if i >= cells { return; }
        let t = unpack(input[0]).xy;
        let was = unpack(output[i]);
        var mean = -1.0;
        if t.y > EPS { mean = t.x / t.y; }
        output[i] = pack(vec4<f32>(was.xyz, mean));
    } else if p.op == 7u {
        // The picture's own luminance, at each pixel.
        if i >= pixels { return; }
        let c = unpack(input[i]);
        output[i] = pack(vec4<f32>(luma(c.xyz) * clamp(c.w, 0.0, 1.0), 0.0, 0.0, 0.0));
    } else if p.op == 8u {
        // The factor at each pixel: its local light against the surface's,
        // and its luminance against its local light.
        if i >= pixels { return; }
        let mean = unpack(input[0]).w;
        let was = unpack(output[i]);
        if mean < 0.0 {
            output[i] = pack(vec4<f32>(was.x, 1.0, 0.0, 0.0));
            return;
        }
        let x = i % p.width; let y = i / p.width;
        let u = (f32(x) + 0.5) / f32(p.scale) - 0.5;
        let v = (f32(y) + 0.5) / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let top = small_at(x0, y0).x + (small_at(x0 + 1, y0).x - small_at(x0, y0).x) * fx;
        let bottom = small_at(x0, y0 + 1).x + (small_at(x0 + 1, y0 + 1).x - small_at(x0, y0 + 1).x) * fx;
        let local = top + (bottom - top) * fy;
        let f = clamp(pow(ratio(local, mean), p.shade) * pow(ratio(was.x, local), p.texture), 0.1, 4.0);
        output[i] = pack(vec4<f32>(was.x, f, 0.0, 0.0));
    } else if p.op == 9u {
        // The layer multiplied by it, its alpha kept.
        if i >= pixels { return; }
        let px = unpack(output[i]);
        if px.w <= 0.0 { return; }
        output[i] = pack(vec4<f32>(px.xyz * unpack(input[i]).y, px.w));
    }
}
"
);
