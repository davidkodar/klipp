//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    aspect_ratio: f32,
    bar_color: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let frame_aspect = f32(w) / f32(h);
    let target_aspect = params.aspect_ratio;

    var bar_height = 0.0;
    var bar_width = 0.0;
    if frame_aspect > target_aspect {
        let visible_w = f32(h) * target_aspect;
        bar_width = (f32(w) - visible_w) / 2.0;
    } else {
        let visible_h = f32(w) / target_aspect;
        bar_height = (f32(h) - visible_h) / 2.0;
    }

    let in_bar = f32(y) < bar_height || f32(y) >= f32(h) - bar_height
              || f32(x) < bar_width || f32(x) >= f32(w) - bar_width;

    let v = unpack(input[idx]);
    if in_bar {
        // bar_color 0 = black, non-zero = white. Linear black = 0.0,
        // linear white = 1.0 (same as sRGB 0/1 endpoints).
        let bv = select(0.0, 1.0, params.bar_color != 0u);
        let op = params.opacity;
        let orig_rgb = vec3<f32>(v.x, v.y, v.z);
        let bar_rgb = vec3<f32>(bv);
        let mixed = orig_rgb * (1.0 - op) + bar_rgb * op;
        let na = v.w * (1.0 - op) + op;
        output[idx] = pack(vec4<f32>(mixed.x, mixed.y, mixed.z, na));
    } else {
        output[idx] = pack(v);
    }
}
