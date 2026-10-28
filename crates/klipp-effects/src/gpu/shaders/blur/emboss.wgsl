
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    strength: f32,
    mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }

    let x = idx % w;
    let y = idx / w;
    let orig = unpack(input[idx]);

    // Skip border pixels
    if x == 0u || y == 0u || x >= w - 1u || y >= h - 1u {
        output[idx] = input[idx];
        return;
    }

    // Sample in light direction and opposite direction
    let sx1 = clamp(i32(round(f32(x) + params.dx)), 0, i32(w) - 1);
    let sy1 = clamp(i32(round(f32(y) + params.dy)), 0, i32(h) - 1);
    let sx2 = clamp(i32(round(f32(x) - params.dx)), 0, i32(w) - 1);
    let sy2 = clamp(i32(round(f32(y) - params.dy)), 0, i32(h) - 1);

    let p1 = unpack(input[u32(sy1) * w + u32(sx1)]);
    let p2 = unpack(input[u32(sy2) * w + u32(sx2)]);

    // Emboss: difference scaled around 0.18 (perceptual mid-grey
    // anchor in linear). sRGB version pivoted at 0.5 sRGB (~0.21
    // linear) — close enough that the slider feel stays familiar.
    let emboss = vec3<f32>(0.18) + (p1.xyz - p2.xyz) * params.strength;

    // Mix with original in linear light.
    let mixed = orig.xyz + (emboss - orig.xyz) * params.mix;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
