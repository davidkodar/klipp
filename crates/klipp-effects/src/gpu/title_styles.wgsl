// Bounded title ROI operations. Coverage remains integer 0..255; composition
// matches the authored sRGB title stack before returning to working light.
struct Params {
    width: u32, height: u32, frame_width: u32, op: u32,
    x0: u32, y0: u32, offset_x: i32, offset_y: i32,
    color: vec4<f32>,
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> p: Params;
fn load_rgba(i: u32) -> vec4<f32> {
    return vec4<f32>(bitcast<f32>(input[4u*i]), bitcast<f32>(input[4u*i+1u]), bitcast<f32>(input[4u*i+2u]), bitcast<f32>(input[4u*i+3u]));
}
fn load_dst(i: u32) -> vec4<f32> {
    return vec4<f32>(bitcast<f32>(output[4u*i]), bitcast<f32>(output[4u*i+1u]), bitcast<f32>(output[4u*i+2u]), bitcast<f32>(output[4u*i+3u]));
}
fn store_rgba(i: u32, v: vec4<f32>) {
    output[4u*i] = bitcast<u32>(v.x); output[4u*i+1u] = bitcast<u32>(v.y);
    output[4u*i+2u] = bitcast<u32>(v.z); output[4u*i+3u] = bitcast<u32>(v.w);
}
fn encoded(x: f32) -> f32 {
    if x <= 0.0031308 { return 12.92*x; }
    return 1.055*pow(max(x,0.0),1.0/2.4)-0.055;
}
fn linear(x: f32) -> f32 {
    if x <= 0.04045 { return x/12.92; }
    return pow((x+0.055)/1.055,2.4);
}
fn matrix(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(p.row0.xyz,v),dot(p.row1.xyz,v),dot(p.row2.xyz,v));
}
fn over(s: vec4<f32>, d: vec4<f32>) -> vec4<f32> {
    let a=s.a+d.a*(1.0-s.a);
    if a<=0.0 { return vec4<f32>(0.0); }
    return vec4<f32>((s.rgb*s.a+d.rgb*d.a*(1.0-s.a))/a,a);
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x;
    if i>=p.width*p.height { return; }
    let x=i%p.width; let y=i/p.width;
    if p.op==1u { // Extract coverage directly from the unchanged stack face.
        output[i]=u32(round(clamp(load_rgba((y+p.y0)*p.frame_width+x+p.x0).a,0.0,1.0)*255.0));
    } else if p.op==2u || p.op==8u { // Offset (optionally inverted) coverage.
        let sx=i32(x)-p.offset_x; let sy=i32(y)-p.offset_y;
        var v=0u;
        if sx>=0 && sy>=0 && sx<i32(p.width) && sy<i32(p.height) {
            v=input[u32(sy)*p.width+u32(sx)];
            if p.op==8u { v=255u-min(v,255u); }
        }
        output[i]=v;
    } else if p.op==3u { // Outline ring, saturating subtraction.
        output[i]=output[i]-min(output[i],input[i]);
    } else if p.op==4u { // Inner shadow coverage, integer truncation.
        output[i]=(output[i]*input[i])/255u;
    } else if p.op==5u { // Authored style over the accumulated stack.
        let a=floor(clamp(f32(input[i])*p.color.a,0.0,255.0))/255.0;
        store_rgba(i,over(vec4<f32>(p.color.rgb,a),load_dst(i)));
    } else if p.op==6u { // Face over behind styles.
        let v=load_rgba((y+p.y0)*p.frame_width+x+p.x0);
        let rgb=matrix(v.rgb);
        let face=vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),v.a);
        store_rgba(i,over(face,load_dst(i)));
    } else if p.op==7u { // Final title quantization and working-space conversion.
        let v=floor(clamp(load_rgba(i),vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
        let rgb=matrix(vec3<f32>(linear(v.x),linear(v.y),linear(v.z)));
        store_rgba((y+p.y0)*p.frame_width+x+p.x0,vec4<f32>(rgb,v.a));
    }
}
