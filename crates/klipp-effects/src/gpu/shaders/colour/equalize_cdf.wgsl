
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> hist: array<u32>;
@group(0) @binding(1) var<storage, read_write> cdf: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(1)
fn main() {
    let total = f32(params.width * params.height);
    var first = 0u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        if hist[i] > 0u {
            first = i;
            break;
        }
    }
    var min_cum = 0u;
    for (var i = 0u; i <= first; i = i + 1u) {
        min_cum = min_cum + hist[i];
    }
    var cum = 0u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        cum = cum + hist[i];
        if total > f32(min_cum) {
            let above = select(cum - min_cum, 0u, cum < min_cum);
            cdf[i] = clamp(f32(above) / (total - f32(min_cum)), 0.0, 1.0);
        } else {
            cdf[i] = f32(i) / 255.0;
        }
    }
}
