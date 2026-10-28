
fn overlay_ch(base: f32, blend: f32) -> f32 {
    if base < 0.5 { return 2.0 * base * blend; }
    return 1.0 - 2.0 * (1.0 - base) * (1.0 - blend);
}
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend < 0.5 { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
    var d: f32;
    if base < 0.25 { d = ((16.0 * base - 12.0) * base + 4.0) * base; } else { d = sqrt(base); }
    return base + (2.0 * blend - 1.0) * (d - base);
}
fn dodge_ch(base: f32, blend: f32) -> f32 {
    if blend >= 1.0 { return 1.0; }
    return min(base / (1.0 - blend), 1.0);
}
fn burn_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.0 { return 0.0; }
    return max(1.0 - (1.0 - base) / blend, 0.0);
}
fn blend3(mode: u32, d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return d + s; }
        case 2u: { return d * s; }
        case 3u: { return vec3<f32>(1.0) - (vec3<f32>(1.0) - d) * (vec3<f32>(1.0) - s); }
        case 4u: { return vec3<f32>(overlay_ch(d.x, s.x), overlay_ch(d.y, s.y), overlay_ch(d.z, s.z)); }
        case 5u: { return vec3<f32>(soft_light_ch(d.x, s.x), soft_light_ch(d.y, s.y), soft_light_ch(d.z, s.z)); }
        case 6u: { return vec3<f32>(overlay_ch(s.x, d.x), overlay_ch(s.y, d.y), overlay_ch(s.z, d.z)); }
        case 7u: { return abs(d - s); }
        case 8u: { return d + s - 2.0 * d * s; }
        case 9u: { return vec3<f32>(dodge_ch(d.x, s.x), dodge_ch(d.y, s.y), dodge_ch(d.z, s.z)); }
        case 10u: { return vec3<f32>(burn_ch(d.x, s.x), burn_ch(d.y, s.y), burn_ch(d.z, s.z)); }
        case 11u: { return min(d, s); }
        case 12u: { return max(d, s); }
        default: { return s; }
    }
}
