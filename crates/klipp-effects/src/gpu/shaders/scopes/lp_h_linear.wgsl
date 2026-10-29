//#include scope_lp_params

@group(0) @binding(0) var<storage, read> src: array<vec4<f32>>;
fn lp_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    switch p.transfer {
        case 1u: { return pow(c, 1.0 / 2.4); }
        case 2u: {
            let y = clamp(v * p.scale, 0.0, 1.0);
            let n = pow(y, 0.1593017578125);
            return pow((0.8359375 + 18.8515625 * n) / (1.0 + 18.6875 * n), 78.84375);
        }
        default: {
            if (c <= 0.0031308) { return c * 12.92; }
            return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
        }
    }
}
fn tap(x: u32, y: u32) -> vec3<f32> {
    let c = src[y * p.width + x].rgb;
    let m = vec3<f32>(dot(p.m0.xyz, c), dot(p.m1.xyz, c), dot(p.m2.xyz, c));
    return vec3<f32>(lp_encode(m.r), lp_encode(m.g), lp_encode(m.b));
}
//#include scope_lp_h
