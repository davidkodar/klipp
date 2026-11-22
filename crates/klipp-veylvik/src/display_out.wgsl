// Klipp's display bytes into the Veylvik viewer's texture: through the
// viewer's LUT when one is set, then premultiplied as that viewer takes
// them. `KlippDisplay::finish` is the twin, step for step, so the two
// round alike.

struct Params { width: u32, height: u32, n: u32, lut_on: u32 }
@group(0) @binding(0) var<storage, read> bytes: array<u32>;
@group(0) @binding(1) var<storage, read> lut: array<f32>;
@group(0) @binding(2) var out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var<uniform> params: Params;

fn at(r: u32, g: u32, b: u32, ch: u32) -> f32 {
    let n = params.n;
    return lut[(b * n * n + g * n + r) * 3u + ch];
}

// `klipp_color::lut::apply_3d_lut` at full opacity.
fn look(c: vec3<u32>) -> vec3<u32> {
    let n1 = f32(params.n - 1u);
    let rgb = vec3<f32>(c) / 255.0;
    let i = clamp(rgb * n1, vec3<f32>(0.0), vec3<f32>(n1));
    let lo = min(vec3<u32>(floor(i)), vec3<u32>(params.n - 2u));
    let hi = lo + vec3<u32>(1u);
    let f = i - vec3<f32>(lo);
    var o = vec3<u32>(0u);
    for (var ch = 0u; ch < 3u; ch++) {
        let c00 = at(lo.x, lo.y, lo.z, ch) + (at(hi.x, lo.y, lo.z, ch) - at(lo.x, lo.y, lo.z, ch)) * f.x;
        let c01 = at(lo.x, lo.y, hi.z, ch) + (at(hi.x, lo.y, hi.z, ch) - at(lo.x, lo.y, hi.z, ch)) * f.x;
        let c10 = at(lo.x, hi.y, lo.z, ch) + (at(hi.x, hi.y, lo.z, ch) - at(lo.x, hi.y, lo.z, ch)) * f.x;
        let c11 = at(lo.x, hi.y, hi.z, ch) + (at(hi.x, hi.y, hi.z, ch) - at(lo.x, hi.y, hi.z, ch)) * f.x;
        let c0 = c00 + (c10 - c00) * f.y;
        let c1 = c01 + (c11 - c01) * f.y;
        let v = c0 + (c1 - c0) * f.z;
        let original = rgb[ch];
        let result = original + (v - original) * 1.0;
        o[ch] = u32(clamp(result, 0.0, 1.0) * 255.0);
    }
    return o;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let v = bytes[gid.y * params.width + gid.x];
    var c = vec3<u32>(v & 255u, (v >> 8u) & 255u, (v >> 16u) & 255u);
    let a = v >> 24u;
    if (params.lut_on == 1u) { c = look(c); }
    if (a < 255u) { c = (c * a + vec3<u32>(127u)) / 255u; }
    textureStore(out, vec2<i32>(gid.xy), vec4<f32>(vec3<f32>(c), f32(a)) / 255.0);
}
