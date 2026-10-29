
struct Params {
    width: u32, height: u32, mix: f32, channel: u32,
    has_mask: u32, alpha_one: u32, mask_count: u32, pad1: u32,
    luma: vec4<f32>,
};
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> effect: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> records: array<MaskRecord>;
@group(0) @binding(3) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read> points: array<vec2<f32>>;
