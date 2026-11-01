//#include display_dither

// A kept frame's ten-bit words (red, green, blue at bits 0, 10 and 20) to
// display bytes (#3300): the twin of `klipp_engine::cache_store::
// ten_bit_display`, bit for bit. Each code's display value comes from the
// CPU's table, already times 255 and clamped, with whether it is dithered
// (`klipp_effects::dither::quantise`'s test, made there); here only the
// adds. The dither's float steps are made exact rather than left to the
// compiler: a u32 converted in two halves, each exact, joined by one
// correctly rounded add as Rust's `as f32` rounds, and scaled by 2^-32.
struct Params { width: u32, height: u32, _a: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> words: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> codes: array<vec2<f32>, 1024>;

fn exact_f32(v: u32) -> f32 {
    return f32(v >> 16u) * 65536.0 + f32(v & 0xffffu);
}

fn exact_dither(index: u32) -> f32 {
    let a = exact_f32(dither_hash(index)) * 2.3283064365386963e-10;
    let b = exact_f32(dither_hash(index ^ 0x9e3779b9u)) * 2.3283064365386963e-10;
    return a + b - 1.0;
}

fn channel(code: u32, d: f32) -> u32 {
    let c = codes[code];
    return u32(clamp(c.x + 0.5 + d * c.y, 0.0, 255.0));
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let w = words[idx];
    let d = exact_dither(idx);
    let r = channel(w & 1023u, d);
    let g = channel((w >> 10u) & 1023u, d);
    let b = channel((w >> 20u) & 1023u, d);
    output[idx] = r | (g << 8u) | (b << 16u) | (255u << 24u);
}
