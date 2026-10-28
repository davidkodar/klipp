//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let xf = f32(x);
    let yf = f32(y);

    if xf < params.crop_left || xf >= params.crop_right || yf < params.crop_top || yf >= params.crop_bottom {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }

    var pixel = unpack(input[idx]);

    if params.feather > 0.0 {
        let dist = min(
            min(xf - params.crop_left, params.crop_right - 1.0 - xf),
            min(yf - params.crop_top, params.crop_bottom - 1.0 - yf),
        );
        if dist < params.feather {
            // Clamped: on the last row and column `dist` goes slightly
            // negative, and the alpha came out negative with it.
            let factor = clamp(dist / params.feather, 0.0, 1.0);
            pixel.w = pixel.w * factor;
        }
    }

    output[idx] = pack(pixel);
}
