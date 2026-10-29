
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var lut: texture_3d<f32>;

struct VOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    return o;
}

fn encode(c: f32) -> f32 {
    if (c <= 0.0031308) { return 12.92 * c; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn at(p: vec3<i32>) -> vec3<f32> {
    return textureLoad(lut, p, 0).rgb;
}

// Trilinear, as `klipp_color::lut::apply_3d_lut` does on the CPU.
fn lookup(rgb: vec3<f32>) -> vec3<f32> {
    let n = i32(textureDimensions(lut).x);
    let p = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * f32(n - 1);
    let i0 = vec3<i32>(floor(p));
    let i1 = min(i0 + vec3<i32>(1), vec3<i32>(n - 1));
    let f = p - floor(p);
    let c00 = mix(at(vec3<i32>(i0.x, i0.y, i0.z)), at(vec3<i32>(i1.x, i0.y, i0.z)), f.x);
    let c10 = mix(at(vec3<i32>(i0.x, i1.y, i0.z)), at(vec3<i32>(i1.x, i1.y, i0.z)), f.x);
    let c01 = mix(at(vec3<i32>(i0.x, i0.y, i1.z)), at(vec3<i32>(i1.x, i0.y, i1.z)), f.x);
    let c11 = mix(at(vec3<i32>(i0.x, i1.y, i1.z)), at(vec3<i32>(i1.x, i1.y, i1.z)), f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(v.pos.xy), 0);
    let e = vec3<f32>(encode(c.r), encode(c.g), encode(c.b));
    return vec4<f32>(clamp(lookup(e), vec3<f32>(0.0), vec3<f32>(1.0)), c.a);
}
