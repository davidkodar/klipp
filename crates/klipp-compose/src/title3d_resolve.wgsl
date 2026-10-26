struct Params {
    roi: vec4<u32>, frame_width: u32, _pad0: u32, _pad1: u32, _pad2: u32,
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>,
    // A metal's record for the shot (#2706): whether to write it, how
    // far a bevel's slope reaches, how the solid's box is stretched over
    // the frame; the box's centre and the frame's.
    metal: vec4<f32>, centres: vec4<f32>,
}
@group(0) @binding(0) var samples: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var normals: texture_2d<f32>;
// not-pixels: a metal's record, where each pixel sees the shot.
@group(0) @binding(4) var<storage, read_write> record: array<vec4<f32>>;
fn encoded(x: f32) -> f32 {
    let v=clamp(x,0.0,1.0);
    if v<=0.0031308 {return 12.92*v;}
    return 1.055*pow(v,1.0/2.4)-0.055;
}
fn linear(v: f32) -> f32 {
    if v<=0.04045 {return v/12.92;}
    return pow((v+0.055)/1.055,2.4);
}
@compute @workgroup_size(256)
fn resolve(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i=gid.x;if i>=p.roi.z*p.roi.w {return;}
    let xy=vec2<u32>(i%p.roi.z,i/p.roi.z);
    var rgb=vec3<f32>(0.0);var coverage=0.0;var metal=vec4<f32>(0.0);
    for(var y=0u;y<2u;y++) {for(var x=0u;x<2u;x++) {
        let at=vec2<i32>(xy*2u+vec2<u32>(x,y));
        let v=textureLoad(samples,at,0);
        rgb+=v.rgb*v.a;coverage+=v.a;
        metal+=textureLoad(normals,at,0)*v.a;
    }}
    if p.metal.x>0.5 {
        // Where this pixel sees the shot, as the flat face's record has it.
        var rec=vec4<f32>(0.0);
        if coverage>0.0 {
            let m=metal/coverage;
            let n=m.xyz/max(length(m.xyz),1e-6);
            let fp=vec2<f32>(f32(xy.x+p.roi.x)+0.5,f32(xy.y+p.roi.y)+0.5);
            let at=p.centres.zw+(fp-p.centres.xy)*p.metal.zw+n.xy/max(-n.z,0.25)*p.metal.y;
            rec=vec4<f32>(at,m.w,1.0);
        }
        record[i]=rec;
    }
    var result=vec4<f32>(0.0);
    if coverage>0.0 {
        rgb/=coverage;
        let code=floor(vec3<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z))*255.0+0.5)/255.0;
        let light=vec3<f32>(linear(code.x),linear(code.y),linear(code.z));
        result=vec4<f32>(dot(p.row0.xyz,light),dot(p.row1.xyz,light),dot(p.row2.xyz,light),floor(coverage*0.25*255.0+0.5)/255.0);
    }
    output[(xy.y+p.roi.y)*p.frame_width+xy.x+p.roi.x]=result;
}
