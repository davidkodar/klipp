
@group(0) @binding(1) var<storage, read> fa: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> fb: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> fwd: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> bwd: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> count: array<u32>;
@group(0) @binding(6) var<storage, read_write> out: array<vec4<f32>>;
