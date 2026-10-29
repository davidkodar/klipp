
@group(0) @binding(1) var<storage, read> from_l: array<f32>;
@group(0) @binding(2) var<storage, read> to_l: array<f32>;
@group(0) @binding(3) var<storage, read> coarse: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> raw: array<vec4<f32>>;
