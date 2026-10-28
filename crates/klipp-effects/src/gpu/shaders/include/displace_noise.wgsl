
fn dn_hash(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    var h = (bitcast<u32>(x) * 0x8da6b343u) ^ (bitcast<u32>(y) * 0xd8163841u) ^ (bitcast<u32>(z) * 0xcb1ab31fu) ^ (seed * 0x165667b1u);
    h = (h ^ (h >> 15u)) * 0x2c1b3c6du;
    h = (h ^ (h >> 12u)) * 0x297a2d39u;
    h = h ^ (h >> 15u);
    return f32(h) / 4294967295.0;
}

fn dn_value(p: vec3<f32>, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (vec3<f32>(3.0) - f * 2.0);
    let ix = i32(i.x);
    let iy = i32(i.y);
    let iz = i32(i.z);
    let x00 = mix(dn_hash(ix, iy, iz, seed), dn_hash(ix + 1, iy, iz, seed), u.x);
    let x10 = mix(dn_hash(ix, iy + 1, iz, seed), dn_hash(ix + 1, iy + 1, iz, seed), u.x);
    let x01 = mix(dn_hash(ix, iy, iz + 1, seed), dn_hash(ix + 1, iy, iz + 1, seed), u.x);
    let x11 = mix(dn_hash(ix, iy + 1, iz + 1, seed), dn_hash(ix + 1, iy + 1, iz + 1, seed), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

fn dn_fbm(p: vec3<f32>, seed: u32) -> f32 {
    let n = dn_value(p, seed) * 0.65 + dn_value(p * 2.03 + vec3<f32>(17.0, 31.0, 7.0), seed) * 0.35;
    return n * 2.0 - 1.0;
}

fn dn_offset(x: f32, y: f32, amount: f32, size: f32, speed: f32, direction: u32, seed: u32, time: f32) -> vec2<f32> {
    let p = vec3<f32>(x / max(size, 1.0), y / max(size, 1.0), time * speed);
    var d = vec2<f32>(0.0);
    if direction != 2u { d.x = dn_fbm(p, seed) * amount; }
    if direction != 1u { d.y = dn_fbm(p + vec3<f32>(41.3, 7.9, 0.0), seed + 1u) * amount; }
    return d;
}
