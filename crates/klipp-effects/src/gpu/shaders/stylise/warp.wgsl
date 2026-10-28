//#include pixel_storage
//#include warp_functions

struct Params {
    width: u32,
    height: u32,
    mode: u32,
    direction: u32,
    amplitude: f32,
    wavelength: f32,
    speed: f32,
    angle: f32,
    strength: f32,
    radius: f32,
    center_x: f32,
    center_y: f32,
    band: f32,
    time: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn warp_source(x: f32, y: f32, w: f32, h: f32) -> vec2<f32> {
    let cx = params.center_x * w;
    let cy = params.center_y * h;
    if params.mode == 0u {
        let p = warp_wave(x, y, w, h, params.amplitude * h / 1080.0, 1.0 / max(params.wavelength, 0.001), params.direction, fract(params.time * params.speed));
        return p.xy;
    } else if params.mode == 1u {
        return warp_twirl(x, y, w, h, cx, cy, max(params.radius * h, 1.0), params.angle * 3.14159265 / 180.0, 3.0);
    } else if params.mode == 2u {
        return warp_bulge(x, y, cx, cy, params.radius * h, params.strength);
    } else if params.mode == 3u {
        let p = warp_ripple(x, y, w, h, cx, cy, 1.0 / max(params.wavelength, 0.001), fract(params.time * params.speed) * 6.28318530, 1.0, params.amplitude);
        return p.xy;
    }
    let step = u32(max(params.time * params.speed, 0.0));
    let band_h = max(params.band * h, 1.0);
    let flip = select(-1.0, 1.0, rand3(u32(y / band_h), step, 8u) > 0.5);
    return vec2<f32>(clamp(x + flip * warp_band_shift(y, band_h, step, params.strength, w), 0.0, w - 1.0), y);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    if idx >= w * params.height { return; }
    let p = warp_source(f32(idx % w), f32(idx / w), f32(w), f32(params.height));
    output[idx] = pack(sample_premultiplied(p.x, p.y));
}
//#include displace_sample
