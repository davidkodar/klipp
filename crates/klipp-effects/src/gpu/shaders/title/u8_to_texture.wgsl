
struct Params { width: u32, height: u32, _p0: u32, _p1: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let packed = input[gid.y * params.width + gid.x];
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), unpack4x8unorm(packed));
}
