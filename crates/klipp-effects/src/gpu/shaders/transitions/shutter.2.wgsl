 { colour = rgb / covered; }
    output[idx] = pack(vec4<f32>(colour, covered / f32(max(params.n, 1u))));
}
