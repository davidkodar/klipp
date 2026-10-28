
struct Params {
    width: u32,
    height: u32,
    crop_zoom: f32,
    _pad: u32,
}

struct Homography3x3 {
    h0: f32, h1: f32, h2: f32,
    h3: f32, h4: f32, h5: f32,
    h6: f32, h7: f32, h8: f32,
    _p0: f32, _p1: f32, _p2: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> homography: Homography3x3;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = idx % params.width;
    let py = idx / params.width;

    // Apply crop/zoom: center and scale
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let x = (f32(px) - cx) / params.crop_zoom + cx;
    let y = (f32(py) - cy) / params.crop_zoom + cy;

    // Apply homography (perspective divide for completeness, but h6/h7 ~ 0 for affine)
    let w_div = homography.h6 * x + homography.h7 * y + homography.h8;
    let src_x = (homography.h0 * x + homography.h1 * y + homography.h2) / w_div;
    let src_y = (homography.h3 * x + homography.h4 * y + homography.h5) / w_div;

    output[idx] = pack(sample_bilinear(src_x, src_y));
}
