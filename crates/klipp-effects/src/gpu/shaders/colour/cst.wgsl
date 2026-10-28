
struct Params {
    width: u32,
    height: u32,
    flags: u32,
    _pad: u32,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// The table from `base`'s linear value at `code`, carried on past the
// ends (`curve_decode`).
fn curve_decode(base: u32, code: f32) -> f32 {
    let x = code * 255.0;
    let i = min(u32(max(floor(x), 0.0)), 254u);
    let f = x - f32(i);
    let a = tables[base + i];
    return a + (tables[base + i + 1u] - a) * f;
}

// Its inverse, the segment found by halving (`curve_encode`).
fn curve_encode(base: u32, v: f32) -> f32 {
    var lo = 0u;
    var hi = 255u;
    loop {
        if lo + 1u >= hi { break; }
        let mid = (lo + hi) / 2u;
        if tables[base + mid] <= v { lo = mid; } else { hi = mid; }
    }
    let a = tables[base + lo];
    let b = tables[base + lo + 1u];
    var f = 0.0;
    if b != a { f = (v - a) / (b - a); }
    return (f32(lo) + f) / 255.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    var c = p.xyz;
    if (params.flags & 1u) != 0u {
        c = vec3<f32>(
            curve_decode(0u, linear_to_srgb_component(c.x)),
            curve_decode(0u, linear_to_srgb_component(c.y)),
            curve_decode(0u, linear_to_srgb_component(c.z)),
        );
    }
    if (params.flags & 4u) != 0u {
        c = vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
    }
    if (params.flags & 2u) != 0u {
        c = vec3<f32>(
            srgb_to_linear_component(curve_encode(256u, c.x)),
            srgb_to_linear_component(curve_encode(256u, c.y)),
            srgb_to_linear_component(curve_encode(256u, c.z)),
        );
    }
    output[idx] = pack(vec4<f32>(c, p.w));
}
