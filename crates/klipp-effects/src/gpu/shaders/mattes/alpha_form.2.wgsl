 { rgb = p.xyz / p.w; }
    }
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
