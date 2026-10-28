//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(min(params.radius, 2u));

    // Collect neighborhood values (max 5x5 = 25)
    var vals_r: array<f32, 25>;
    var vals_g: array<f32, 25>;
    var vals_b: array<f32, 25>;
    var n = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let p = unpack(input[sy * w + sx]);
            vals_r[n] = p.x;
            vals_g[n] = p.y;
            vals_b[n] = p.z;
            n = n + 1u;
        }
    }

    // Partial selection sort to find median
    let mid = n / 2u;
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_r[j] < vals_r[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_r[i]; vals_r[i] = vals_r[min_idx]; vals_r[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_g[j] < vals_g[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_g[i]; vals_g[i] = vals_g[min_idx]; vals_g[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_b[j] < vals_b[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_b[i]; vals_b[i] = vals_b[min_idx]; vals_b[min_idx] = tmp;
        }
    }

    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(vals_r[mid], vals_g[mid], vals_b[mid], orig.w));
}
