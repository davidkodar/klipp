
@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct Vs { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Vs {
    // Two triangles covering the viewport.
    var p = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let xy = p[i];
    var o: Vs;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    // Texture space is y-down.
    o.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return o;
}

@fragment
fn fs(v: Vs) -> @location(0) vec4<f32> {
    // Straight through. The surface is scRGB — linear, 1.0 = SDR
    // reference white, values above it are the HDR highlights — which is
    // exactly what the viewer published.
    return textureSampleLevel(frame, samp, v.uv, 0.0);
}
