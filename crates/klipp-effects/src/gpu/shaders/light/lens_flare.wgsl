
struct Params {
    width: u32, height: u32, pos_x: f32, pos_y: f32,
    brightness: f32, flare_size: f32, ghost_count: u32, ghost_intensity: f32,
    chromatic_aberration: f32, _pad: u32, _pad2: u32, _pad3: u32,
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
    if idx >= params.width * params.height { return; }
    let px = f32(idx % params.width);
    let py = f32(idx / params.width);
    let w = f32(params.width);
    let h = f32(params.height);
    let fx = params.pos_x * w;
    let fy = params.pos_y * h;
    let cx = w * 0.5;
    let cy = h * 0.5;
    let max_dim = max(w, h);
    let disc_r = params.flare_size * max_dim;
    var add = vec3<f32>(0.0);
    let dist = sqrt((px - fx) * (px - fx) + (py - fy) * (py - fy));
    if dist < disc_r {
        let falloff = 1.0 - dist / disc_r;
        let glow = falloff * falloff * params.brightness;
        add = add + vec3<f32>(glow, glow * 0.9, glow * 0.7);
    }
    for (var gi: u32 = 0u; gi < params.ghost_count; gi = gi + 1u) {
        let t = (f32(gi) + 1.0) / (f32(params.ghost_count) + 1.0);
        let gx = fx + (cx - fx) * t * 2.0;
        let gy = fy + (cy - fy) * t * 2.0;
        let gr = disc_r * (0.3 + t * 0.4);
        let gdist = sqrt((px - gx) * (px - gx) + (py - gy) * (py - gy));
        if gdist < gr {
            let gf = 1.0 - gdist / gr;
            let glow = gf * gf * params.ghost_intensity * params.brightness;
            let ca = params.chromatic_aberration * t;
            add = add + vec3<f32>(glow * (1.0 + ca), glow, glow * (1.0 - ca * 0.5));
        }
    }
    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(orig.xyz + add, orig.w));
}
