
@group(0) @binding(1) var<storage, read> fwd: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> bwd: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> count: array<atomic<u32>>;
