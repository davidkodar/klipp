
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    roi_x: u32, roi_y: u32, roi_w: u32, roi_h: u32,
    small_w: u32, small_h: u32, base: u32, radius: u32,
    tint_shot: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers


fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[p.base + u32(cy) * p.small_w + u32(cx)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if p.op == 3u {
        if i >= p.roi_w * p.roi_h { return; }
        let rec = unpack(input[i]);
        if rec.w <= 0.0 { return; }
        let u = rec.x / f32(p.scale) - 0.5;
        let v = rec.y / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let seen = mix(mix(small_at(x0, y0), small_at(x0 + 1, y0), fx), mix(small_at(x0, y0 + 1), small_at(x0 + 1, y0 + 1), fx), fy);
        let tint = p.tint_shot.xyz;
        let fres = tint + (vec3<f32>(1.0) - tint) * rec.z;
        let at = (p.roi_y + i / p.roi_w) * p.width + p.roi_x + i % p.roi_w;
        var px = unpack(output[at]);
        if px.w <= 0.0 { return; }
        // Rolled off as the face is, so a bright shot does not clip it.
        // The shot as it is shown: a sun over white is white, not brighter.
        var lit = px.xyz + fres * min(seen.xyz, vec3<f32>(1.0)) * p.tint_shot.w * rec.w;
        // A coloured metal keeps its hue whatever it reflects: gold over
        // blue water is a cooler gold, not olive. Pulled towards its own
        // colour at the same brightness, as far as the metal is coloured;
        // silver reflects colours as they are.
        let weights = vec3<f32>(0.2126, 0.7152, 0.0722);
        let chroma = (max(tint.x, max(tint.y, tint.z)) - min(tint.x, min(tint.y, tint.z))) / max(max(tint.x, max(tint.y, tint.z)), 1e-4);
        let own = tint / max(dot(tint, weights), 1e-4) * dot(lit, weights);
        lit = mix(lit, own, clamp(chroma * 1.3, 0.0, 0.85));
        px = vec4<f32>(select(lit, 0.75 + 0.25 * (vec3<f32>(1.0) - exp(-(lit - 0.75) / 0.25)), lit > vec3<f32>(0.75)), px.w);
        output[at] = pack(px);
        return;
    }
    if i >= p.small_w * p.small_h { return; }
    let sx = i % p.small_w; let sy = i / p.small_w;
    if p.op == 0u {
        var sum = vec4<f32>(0.0);
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                sum += vec4<f32>(c.xyz * c.w, c.w);
                n += 1.0;
            }
        }
        output[p.base + i] = pack(sum / max(n, 1.0));
    } else if p.op == 1u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx) + d, i32(sy)); }
        output[i] = pack(sum / f32(2 * r + 1));
    } else if p.op == 2u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) {
            let cy = clamp(i32(sy) + d, 0, i32(p.small_h) - 1);
            sum += unpack(input[u32(cy) * p.small_w + sx]);
        }
        output[p.base + i] = pack(sum / f32(2 * r + 1));
    }
}
