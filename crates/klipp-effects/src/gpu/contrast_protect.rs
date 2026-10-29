//! Contrast protection for titles on the GPU (#2734): the twin of
//! [`crate::protect::protect_backdrop`], run on the accumulator just
//! before a protected title is blended onto it.
use super::*;
use crate::protect;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProtectParams {
    width: u32,
    height: u32,
    op: u32,
    scale: u32,
    small_w: u32,
    small_h: u32,
    radius: u32,
    mode: u32,
    /// The working space's luminance weights and the layer's opacity.
    luma_opacity: [f32; 4],
}

impl GpuEffectsPipeline {
    /// Protects the text of the title in `slot` (#2734): darkens or
    /// lightens the picture laid so far round it, as
    /// [`protect::protect_backdrop`] does, by `mode` (1 auto, 2 always;
    /// 0 does nothing). `opacity` is the layer's as it will be blended,
    /// `luma` the working space's weights. Run just before the slot is
    /// blended; `false` when the pass could not run.
    pub fn stack_contrast_protect(&mut self, slot: usize, opacity: f32, mode: u32, luma: [f32; 3]) -> bool {
        if mode == 0 || opacity <= 0.001 {
            return true;
        }
        if !self.stack_ensure_acc() || slot >= self.stack_layers.len() {
            return false;
        }
        let (fw, fh) = self.stack_dims;
        let scale = protect::SCALE;
        let (sw, sh) = (fw.div_ceil(scale), fh.div_ceil(scale));
        let small = u64::from(sw) * u64::from(sh);
        if self.contrast_protect_scratch.as_ref().is_none_or(|(_, _, n)| *n < small) {
            let make = |label| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: small * 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
            self.contrast_protect_scratch = Some((make("contrast protect measure"), make("contrast protect blur"), small));
        }
        let params = |op: u32| ProtectParams {
            width: fw, height: fh, op, scale, small_w: sw, small_h: sh,
            radius: protect::radius_cells(fw, fh), mode,
            luma_opacity: [luma[0], luma[1], luma[2], opacity],
        };
        let acc = self.stack_acc.clone().unwrap();
        let layer = self.stack_layers[slot].clone();
        let (measure, blur, _) = self.contrast_protect_scratch.as_ref().unwrap();
        let small_groups = (small as u32).div_ceil(WORKGROUP_SIZE);
        let pass = |this: &Self, op: u32, read: &wgpu::Buffer, rw: &wgpu::Buffer, groups: u32| {
            this.write_params(&params(op));
            this.stack_pass(&this.pipelines.contrast_protect_pipeline, read, rw, groups, "contrast protect");
        };
        pass(self, 0, &acc, measure, small_groups);
        pass(self, 1, &layer, measure, small_groups);
        for _ in 0..protect::BLUR_ROUNDS {
            pass(self, 2, measure, blur, small_groups);
            pass(self, 3, blur, measure, small_groups);
        }
        pass(self, 4, measure, &acc, (u64::from(fw) * u64::from(fh)).div_ceil(u64::from(WORKGROUP_SIZE)) as u32);
        true
    }
}

pub(super) const SHADER_CONTRAST_PROTECT: &str = concat!(
    "
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    small_w: u32, small_h: u32, radius: u32, mode: u32,
    luma_opacity: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
",
    shader_pixel_storage_wgsl!(),
    "
const TARGET: f32 = 4.5;
const LIGHT_TEXT: f32 = 0.179;
const HALO_FULL: f32 = 0.04;

fn luma(c: vec3<f32>) -> f32 {
    return clamp(dot(c, p.luma_opacity.xyz), 0.0, 1.0);
}

fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[u32(cy) * p.small_w + u32(cx)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if p.op == 4u {
        if i >= p.width * p.height { return; }
        let x = i % p.width; let y = i / p.width;
        let u = (f32(x) + 0.5) / f32(p.scale) - 0.5;
        let v = (f32(y) + 0.5) / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let m = mix(mix(small_at(x0, y0), small_at(x0 + 1, y0), fx), mix(small_at(x0, y0 + 1), small_at(x0 + 1, y0 + 1), fx), fy);
        let a = m.z;
        if a <= 1e-3 { return; }
        let ht = clamp(a / HALO_FULL, 0.0, 1.0);
        let halo = ht * ht * (3.0 - 2.0 * ht);
        let busy = sqrt(max(m.y - m.x * m.x, 0.0));
        let lt = clamp(m.w / a, 0.0, 1.0);
        var px = unpack(output[i]);
        if lt >= LIGHT_TEXT {
            let need = (lt + 0.05) / TARGET - 0.05;
            let cur = min(m.x + busy, 1.0);
            var k = 1.0;
            if cur > need { k = clamp(max(need, 0.0) / cur, 0.12, 1.0); }
            if p.mode == 2u { k = min(k, 0.55); }
            px = vec4<f32>(px.xyz * (1.0 - halo * (1.0 - k)), px.w);
        } else {
            let need = TARGET * (lt + 0.05) - 0.05;
            let cur = max(m.x - busy, 0.0);
            var t = clamp((need - cur) / max(1.0 - cur, 1e-4), 0.0, 0.88);
            if p.mode == 2u { t = max(t, 0.45); }
            px = vec4<f32>(px.xyz + (vec3<f32>(1.0) - px.xyz) * halo * t, px.w);
        }
        output[i] = pack(px);
        return;
    }
    if i >= p.small_w * p.small_h { return; }
    let sx = i % p.small_w; let sy = i / p.small_w;
    if p.op <= 1u {
        // 0: the picture's luminance and its square; 1: the layer's
        // coverage and luminance times coverage, beside them.
        var sum = vec2<f32>(0.0);
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                if p.op == 0u {
                    let lb = luma(c.xyz) * clamp(c.w, 0.0, 1.0);
                    sum += vec2<f32>(lb, lb * lb);
                } else {
                    let a = clamp(c.w * p.luma_opacity.w, 0.0, 1.0);
                    sum += vec2<f32>(a, luma(c.xyz) * a);
                }
                n += 1.0;
            }
        }
        let mean = sum / max(n, 1.0);
        if p.op == 0u {
            output[i] = pack(vec4<f32>(mean, 0.0, 0.0));
        } else {
            let was = unpack(output[i]);
            output[i] = pack(vec4<f32>(was.xy, mean));
        }
    } else if p.op == 2u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx) + d, i32(sy)); }
        output[i] = pack(sum / f32(2 * r + 1));
    } else if p.op == 3u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx), i32(sy) + d); }
        output[i] = pack(sum / f32(2 * r + 1));
    }
}
"
);
