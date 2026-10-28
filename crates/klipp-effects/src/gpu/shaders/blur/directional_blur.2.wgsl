 { colour = pre / sum.w; }
    output[idx] = pack(vec4<f32>(colour, sum.w * inv));
}
