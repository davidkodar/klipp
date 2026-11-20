// Klipp's decoded sRGB bytes as linear floats, straight alpha, through the
// tables Klipp's own CPU expansion uses: the same picture to the bit.
// lut[0..256] is each colour byte's light, lut[256..512] each alpha byte's.

@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> lut: array<f32, 512>;
@group(0) @binding(3) var<uniform> count: vec4<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x + id.y * count.y;
    if (i >= count.x) {
        return;
    }
    let p = src[i];
    dst[i] = vec4<f32>(lut[p & 0xffu], lut[(p >> 8u) & 0xffu], lut[(p >> 16u) & 0xffu], lut[256u + (p >> 24u)]);
}
