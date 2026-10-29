//#include scope_lp_params

@group(0) @binding(0) var<storage, read> src: array<u32>;
fn tap(x: u32, y: u32) -> vec3<f32> {
    let v = src[y * p.width + x];
    return vec3<f32>(f32(v & 0xffu), f32((v >> 8u) & 0xffu), f32((v >> 16u) & 0xffu)) / 255.0;
}
//#include scope_lp_h
