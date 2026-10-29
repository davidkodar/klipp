@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 256u, 0u, id_flat.z);
    let idx = id.x;
    if idx >= params.width * params.height { return; }
    // The window at this pixel, as SHADER_MASK_V2 draws it.
    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);
    let mv = clamp(window_matte(nx, ny, aspect, params.mask_count), 0.0, 1.0);
    let m = vec4<f32>(mv, mv, mv, 1.0);
    // From here, SHADER_MASK_MIX with `m` for the matte it would read.
    var coverage = m.r;
    if params.channel == 1u { coverage = dot(m.rgb, params.luma.rgb); }
    if params.channel == 2u { coverage = m.a; }
    if (bitcast<u32>(coverage) & 0x7f800000u) == 0x7f800000u { coverage = 0.0; }
    coverage = clamp(coverage, 0.0, 1.0);
    let w = clamp(params.mix, 0.0, 1.0) * coverage;
    if params.alpha_one == 0u && w == 0.0 {
        output[idx] = pack(src(idx));
        return;
    }
    if params.alpha_one == 0u && w == 1.0 {
        output[idx] = effect[idx];
        return;
    }
    var value = src(idx);
    if w == 1.0 { value = unpack(effect[idx]); }
    else if w != 0.0 { value = value + (unpack(effect[idx]) - value) * w; }
    if params.alpha_one != 0u { value.a = 1.0; }
    output[idx] = pack(value);
}
