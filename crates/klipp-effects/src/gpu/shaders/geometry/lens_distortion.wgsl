//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    distortion: f32,
    scale: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_pixel(sx: f32, sy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    let cx0 = u32(clamp(x0, 0, i32(w) - 1));
    let cy0 = u32(clamp(y0, 0, i32(h) - 1));
    let cx1 = u32(clamp(x1, 0, i32(w) - 1));
    let cy1 = u32(clamp(y1, 0, i32(h) - 1));
    let c00 = unpack(input[cy0 * w + cx0]);
    let c10 = unpack(input[cy0 * w + cx1]);
    let c01 = unpack(input[cy1 * w + cx0]);
    let c11 = unpack(input[cy1 * w + cx1]);
    let top = mix(c00, c10, fx);
    let bot = mix(c01, c11, fx);
    return mix(top, bot, fy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;
    let cx = f32(w) / 2.0;
    let cy = f32(h) / 2.0;
    let nx = (f32(x) - cx) / cx;
    let ny = (f32(y) - cy) / cy;
    let r2 = nx * nx + ny * ny;
    let factor = 1.0 + params.distortion * r2;
    let sx = (nx * factor / params.scale) * cx + cx;
    let sy = (ny * factor / params.scale) * cy + cy;
    output[idx] = pack(sample_pixel(sx, sy, w, h));
}
