
struct Params {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    luma_sigma: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, max_kernel: u32,
    chroma_sigma: f32,
    _pad0: u32, _pad1: u32, _pad2: u32,
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
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);

    let center = unpack(input[idx]);
    let coefs = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
    let center_luma = dot(coefs, center.xyz);

    let kernel_size = min(i32(ceil(params.spatial_sigma * 2.0)), i32(params.max_kernel));
    let spatial_denom = 2.0 * params.spatial_sigma * params.spatial_sigma;
    // A sigma of 0 leaves that part as it is (#1039).
    let luma_on = params.luma_sigma > 0.0;
    let chroma_on = params.chroma_sigma > 0.0;
    let luma_denom = 2.0 * params.luma_sigma * params.luma_sigma;
    let chroma_denom = 2.0 * params.chroma_sigma * params.chroma_sigma;

    var sum_y = 0.0;
    var total_y = 0.0;
    var sum_c = vec3<f32>(0.0);
    var total_c = 0.0;

    if luma_on || chroma_on {
        for (var dy = -kernel_size; dy <= kernel_size; dy = dy + 1) {
            for (var dx = -kernel_size; dx <= kernel_size; dx = dx + 1) {
                let sx = u32(clamp(x + dx, 0, i32(w) - 1));
                let sy = u32(clamp(y + dy, 0, i32(h) - 1));
                let n = unpack(input[sy * w + sx]);
                let n_luma = dot(coefs, n.xyz);

                let spatial_dist = f32(dx * dx + dy * dy);
                let spatial_w = exp(-spatial_dist / spatial_denom);
                let luma_diff = center_luma - n_luma;
                let d2 = luma_diff * luma_diff;
                if luma_on {
                    let wl = spatial_w * exp(-d2 / luma_denom);
                    sum_y += n_luma * wl;
                    total_y += wl;
                }
                if chroma_on {
                    let wc = spatial_w * exp(-d2 / chroma_denom);
                    sum_c = sum_c + (n.xyz - vec3<f32>(n_luma)) * wc;
                    total_c += wc;
                }
            }
        }
    }

    var out_y = center_luma;
    if luma_on { out_y = sum_y * (1.0 / total_y); }
    var out_c = center.xyz - vec3<f32>(center_luma);
    if chroma_on { out_c = sum_c * (1.0 / total_c); }
    output[idx] = pack(vec4<f32>(vec3<f32>(out_y) + out_c, center.w));
}
