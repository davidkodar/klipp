
struct Params {
    width: u32,
    height: u32,
    levels: f32,
    above_count: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The ladder's band edges as linear values, ascending, padded with the
// largest float (`PosterizeLadder`): 256 below white, then 256 above it.
@group(0) @binding(3) var<storage, read> edges: array<f32>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// How many of the 256 edges from `base` are at or below `v`, as the CPU's
// `PosterizeLadder::count` counts them.
fn count_at_or_below(base: u32, v: f32) -> u32 {
    var lo = 0u;
    var hi = 256u;
    loop {
        if lo >= hi { break; }
        let mid = (lo + hi) / 2u;
        if edges[base + mid] <= v { lo = mid + 1u; } else { hi = mid; }
    }
    return lo;
}

// The band `v` falls in, found against the edges the CPU finds it against
// (bit for bit the same band on both routes, #1443), then its value.
// Bands are even on the display scale; above white the ladder carries on
// at the same spacing, instead of every highlight landing on white.
fn band(v: f32) -> f32 {
    let div = params.levels - 1.0;
    let m = f32(count_at_or_below(0u, v));
    var j = count_at_or_below(256u, v);
    if j == params.above_count && params.above_count == 256u {
        // Past the table: the step from the encoding, never below it.
        j = max(j, u32(max(floor((linear_to_srgb_component(v) - 1.0) * params.levels), 0.0)));
    }
    return srgb_to_linear_component(min(m, div) / div + f32(j) / div);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(band(p.x), band(p.y), band(p.z), p.w));
}
