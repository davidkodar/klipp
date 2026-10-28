
fn at(x: i32, y: i32) -> vec4<f32> {
    let w = i32(params.width);
    let h = i32(params.height);
    return unpack(input[u32(clamp(y, 0, h - 1) * w + clamp(x, 0, w - 1))]);
}

fn sample_premultiplied(sx: f32, sy: f32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    var sum = vec4<f32>(0.0);
    let c00 = at(x0, y0);
    let c10 = at(x0 + 1, y0);
    let c01 = at(x0, y0 + 1);
    let c11 = at(x0 + 1, y0 + 1);
    sum += vec4<f32>(c00.rgb * c00.a, c00.a) * ((1.0 - fx) * (1.0 - fy));
    sum += vec4<f32>(c10.rgb * c10.a, c10.a) * (fx * (1.0 - fy));
    sum += vec4<f32>(c01.rgb * c01.a, c01.a) * ((1.0 - fx) * fy);
    sum += vec4<f32>(c11.rgb * c11.a, c11.a) * (fx * fy);
    if sum.a > 1e-6 { return vec4<f32>(sum.rgb / sum.a, sum.a); }
    return vec4<f32>(0.0);
}

