struct View {
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>,
    pivot_focal: vec4<f32>, roi: vec4<f32>, depth: vec4<f32>,
    light: vec4<f32>, surface: vec4<f32>,
    face: vec4<f32>, side: vec4<f32>, bevel: vec4<f32>,
}
struct Pose { anchor_scale: vec4<f32>, offset_alpha: vec4<f32>, turn: vec4<f32> }
@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> poses: array<Pose>;
struct Vertex {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) material: u32, @location(3) glyph: u32,
}
struct Fragment {
    @builtin(position) position: vec4<f32>,
    @location(0) camera: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) @interpolate(flat) material: u32,
    @location(3) @interpolate(flat) alpha: f32,
}
fn rotate(p: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(view.row0.xyz,p),dot(view.row1.xyz,p),dot(view.row2.xyz,p));
}
fn unit(p: vec3<f32>) -> vec3<f32> {return p/max(length(p),1e-12);}
@vertex
fn vertex(v: Vertex) -> Fragment {
    let pose=poses[v.glyph];
    let anchor=pose.anchor_scale.xyz; let scale=pose.anchor_scale.w;
    let q=(v.position-anchor)*scale;
    let s=pose.turn.x; let c=pose.turn.y;
    let positioned=anchor+vec3<f32>(c*q.x-s*q.y,s*q.x+c*q.y,q.z)+vec3<f32>(pose.offset_alpha.xy,0.0);
    var camera=rotate(positioned-view.pivot_focal.xyz);
    let normal=rotate(vec3<f32>(c*v.normal.x-s*v.normal.y,s*v.normal.x+c*v.normal.y,v.normal.z));
    let near=view.depth.x; let far=view.depth.y;
    let ortho=view.depth.z>0.5;
    let f=view.pivot_focal.w;
    var clip: vec4<f32>;
    if ortho {
        let pixel=view.pivot_focal.xy+camera.xy-view.roi.xy;
        clip=vec4<f32>(2.0*pixel.x/view.roi.z-1.0,1.0-2.0*pixel.y/view.roi.w,(camera.z-near)/(far-near),1.0);
    } else {
        camera.z+=f+view.pivot_focal.z;
        let centre=view.pivot_focal.xy-view.roi.xy;
        clip=vec4<f32>((2.0*centre.x/view.roi.z-1.0)*camera.z+2.0*camera.x*f/view.roi.z,
            (1.0-2.0*centre.y/view.roi.w)*camera.z-2.0*camera.y*f/view.roi.w,
            far/(far-near)*camera.z-far*near/(far-near),camera.z);
    }
    var out: Fragment;
    out.position=clip;out.camera=camera;out.normal=normal;
    out.material=v.material;out.alpha=min(pose.offset_alpha.z,1.0);
    return out;
}
@fragment
fn fragment(v: Fragment) -> @location(0) vec4<f32> {
    if v.alpha<=0.001 {discard;}
    var base=view.face.rgb;
    if v.material==1u {base=view.side.rgb;} else if v.material==2u {base=view.bevel.rgb;}
    let n=unit(v.normal);
    var towards=vec3<f32>(0.0,0.0,-1.0);
    if view.depth.z<0.5 {towards=unit(-v.camera);}
    let diffuse=max(dot(n,view.light.xyz),0.0)*view.light.w;
    var spec=0.0;
    if diffuse>0.0 {
        let halfway=unit(view.light.xyz+towards);
        spec=view.surface.y*pow(max(dot(n,halfway),0.0),max(view.surface.z,1.0))*view.light.w;
    }
    return vec4<f32>(base*(view.surface.x+diffuse)+vec3<f32>(spec),v.alpha);
}
