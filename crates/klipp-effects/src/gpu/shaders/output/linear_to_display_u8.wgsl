//#include pixel_storage
//#include hdr_display_light
//#include display_dither

// `view_op` 1 is HDR monitoring's preview: the frame is display light
// (1.0 = 203 cd/m²) rather than SDR linear, and the light above reference
// white rolls into the top so its structure survives a screen that cannot
// show it. `klipp_color::hdr_view::hdr_to_sdr_preview` is the twin.
struct Params {
    width: u32, height: u32, hdr_tonemap: u32, exposure: f32,
    view_op: u32, scene_kind: u32, scene_scale: f32, peak_nits: f32,
    luma: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

/// The hook `shader_hdr_display_light_wgsl` asks for. Here the weights
/// travel in the params rather than a baked table: this shader's layout
/// has three bindings and every effect shares it.
fn h_working_luma() -> vec3<f32> {
    return params.luma.xyz;
}

/// `hdr_to_sdr_preview`: identity to the knee, then a shoulder that
/// approaches 1 without reaching it.
fn hdr_to_sdr_preview(display_light: f32) -> f32 {
    let knee = 0.75;
    let v = max(display_light, 0.0);
    if v <= knee {
        return v;
    }
    let t = (v - knee) / (1.0 - knee);
    return knee + (1.0 - knee) * (t / (1.0 + t));
}

fn linear_to_srgb_c(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.0031308) { return x * 12.92; }
    return 1.055 * pow(x, 1.0/2.4) - 0.055;
}

// Narkowicz ACES fit; constants shared with klipp_color::hdr::filmic.
fn filmic(x: f32) -> f32 {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    var rgb = max(px.xyz, vec3<f32>(0.0));
    if (params.hdr_tonemap != 0u) {
        rgb = vec3<f32>(filmic(rgb.x * params.exposure), filmic(rgb.y * params.exposure), filmic(rgb.z * params.exposure));
    }
    if (params.view_op == 1u) {
        // A scene-referred clip's own last step first, where the ordinary
        // SDR path would have tone-mapped it: monitoring wants the HDR
        // display light the export writes, not the SDR picture.
        if (params.scene_kind < 3u) {
            rgb = hdr_display_light(
                rgb, params.scene_kind, params.scene_scale, params.peak_nits,
            );
        }
        rgb = vec3<f32>(
            hdr_to_sdr_preview(rgb.x),
            hdr_to_sdr_preview(rgb.y),
            hdr_to_sdr_preview(rgb.z),
        );
    }
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let r = quantise_u8(linear_to_srgb_c(rgb.x), d);
    let g = quantise_u8(linear_to_srgb_c(rgb.y), d);
    let b = quantise_u8(linear_to_srgb_c(rgb.z), d);
    let a = u32(clamp(px.w * 255.0 + 0.5, 0.0, 255.0));
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
