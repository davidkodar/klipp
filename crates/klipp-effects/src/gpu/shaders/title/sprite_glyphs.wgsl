//#include pixel_storage

struct Params { width: u32, height: u32, y0: u32, y1: u32, n: u32, ux0: u32, uy0: u32, uw: u32, uh: u32, bx0: u32, by0: u32, bw: u32, bh: u32, _p0: u32, _p1: u32, _p2: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> sprite: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> units: array<u32>;
@group(0) @binding(4) var<storage, read> glyphs: array<vec4<f32>>; // not-pixels: glyph poses (moved rect, pose, anchor), four vec4 each

// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn unit_at(x: i32, y: i32) -> u32 {
    let mx = x - i32(params.ux0);
    let my = y - i32(params.uy0);
    if mx < 0 || my < 0 || mx >= i32(params.uw) || my >= i32(params.uh) { return 0xFFFFu; }
    let i = u32(my) * params.uw + u32(mx);
    let word = units[i >> 1u];
    if (i & 1u) == 0u { return word & 0xFFFFu; }
    return (word >> 16u) & 0xFFFFu;
}

// Glyph `g`'s pixel at (x, y), premultiplied, colour in sRGB-encoded
// 0..255 units; nothing outside the sprite's box or the glyph (#190).
fn tap(x: i32, y: i32, g: u32) -> vec4<f32> {
    let bx = x - i32(params.bx0);
    let by = y - i32(params.by0);
    if bx < 0 || by < 0 || bx >= i32(params.bw) || by >= i32(params.bh) || unit_at(x, y) != g {
        return vec4<f32>(0.0);
    }
    let i = u32(by) * params.bw + u32(bx);
    let rg = unpack2x16float(sprite[i * 2u]);
    let ba = unpack2x16float(sprite[i * 2u + 1u]);
    let a = ba.y;
    return vec4<f32>(rg.x * 255.0 * a, rg.y * 255.0 * a, ba.x * 255.0 * a, a);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    var acc = vec4<f32>(0.0);
    for (var g = 0u; g < params.n; g = g + 1u) {
        let moved = glyphs[g * 4u];
        if f32(x) < moved.x || f32(x) >= moved.z || f32(y) < moved.y || f32(y) >= moved.w { continue; }
        let pose = glyphs[g * 4u + 1u];
        let anch = glyphs[g * 4u + 2u];
        // A turn, clockwise radians about the anchor (an animator's).
        let turn = glyphs[g * 4u + 3u].x;
        var u = f32(x) + 0.5 - pose.x - anch.x;
        var v = f32(y) + 0.5 - pose.y - anch.y;
        if turn != 0.0 {
            let c = cos(turn);
            let sn = sin(turn);
            let ur = u * c + v * sn;
            v = v * c - u * sn;
            u = ur;
        }
        let fx = u / pose.z + anch.x - 0.5;
        let fy = v / pose.z + anch.y - 0.5;
        let x0f = floor(fx);
        let y0f = floor(fy);
        let tx = fx - x0f;
        let ty = fy - y0f;
        let x0 = i32(x0f);
        let y0 = i32(y0f);
        let s = tap(x0, y0, g) * ((1.0 - tx) * (1.0 - ty))
              + tap(x0 + 1, y0, g) * (tx * (1.0 - ty))
              + tap(x0, y0 + 1, g) * ((1.0 - tx) * ty)
              + tap(x0 + 1, y0 + 1, g) * (tx * ty);
        if s.w <= 0.0 { continue; }
        // Unrounded (#190), as the CPU twin.
        let a = clamp(s.w * pose.w, 0.0, 1.0);
        if a * 255.0 < 0.5 { continue; }
        let rgb = s.xyz / s.w;
        acc = vec4<f32>(rgb * a + acc.xyz * (1.0 - a), a + acc.w * (1.0 - a));
    }
    if acc.w <= 0.0 {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let rgb = acc.xyz / acc.w;
    output[idx] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rgb.x / 255.0), srgb_to_linear_c(rgb.y / 255.0), srgb_to_linear_c(rgb.z / 255.0))), acc.w));
}
