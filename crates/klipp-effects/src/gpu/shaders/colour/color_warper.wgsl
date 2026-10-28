
struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    hue0: f32, hue1: f32, hue2: f32, hue3: f32,
    hue4: f32, hue5: f32, hue6: f32, hue7: f32,
    hue8: f32, hue9: f32, hue10: f32, hue11: f32,
    sat0: f32, sat1: f32, sat2: f32, sat3: f32,
    sat4: f32, sat5: f32, sat6: f32, sat7: f32,
    sat8: f32, sat9: f32, sat10: f32, sat11: f32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers


fn get_hue_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.hue0; } case 1u: { return params.hue1; }
        case 2u: { return params.hue2; } case 3u: { return params.hue3; }
        case 4u: { return params.hue4; } case 5u: { return params.hue5; }
        case 6u: { return params.hue6; } case 7u: { return params.hue7; }
        case 8u: { return params.hue8; } case 9u: { return params.hue9; }
        case 10u: { return params.hue10; } default: { return params.hue11; }
    }
}
fn get_sat_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.sat0; } case 1u: { return params.sat1; }
        case 2u: { return params.sat2; } case 3u: { return params.sat3; }
        case 4u: { return params.sat4; } case 5u: { return params.sat5; }
        case 6u: { return params.sat6; } case 7u: { return params.sat7; }
        case 8u: { return params.sat8; } case 9u: { return params.sat9; }
        case 10u: { return params.sat10; } default: { return params.sat11; }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let hsl = picture_hsl(p.xyz);
    let h_deg = hsl.h * 360.0;
    let sector_f = h_deg / 30.0;
    let sector_low = u32(floor(sector_f)) % 12u;
    let sector_high = (sector_low + 1u) % 12u;
    let t = fract(sector_f);
    let blend = (1.0 - cos(t * 3.14159265)) * 0.5;
    let hue_shift = get_hue_shift(sector_low) * (1.0 - blend) + get_hue_shift(sector_high) * blend;
    let sat_shift = get_sat_shift(sector_low) * (1.0 - blend) + get_sat_shift(sector_high) * blend;
    let sat_weight = clamp(hsl.s, 0.0, 1.0);
    let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
    let new_s = clamp(hsl.s + sat_shift * sat_weight, 0.0, 1.0);
    let rgb = picture_rgb(new_h, new_s, hsl.v, hsl.magnitude);
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
