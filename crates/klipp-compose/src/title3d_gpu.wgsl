struct View {
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>,
    pivot_focal: vec4<f32>, roi: vec4<f32>, depth: vec4<f32>,
    light: vec4<f32>, surface: vec4<f32>,
    face: vec4<f32>, side: vec4<f32>, bevel: vec4<f32>,
    // A metal (#2706): its tint and whether there is one; roughness,
    // edge, highlights and the studio's share; the sky's share, the
    // anisotropy and its angle; the sky's tint; the key and rim lights
    // (towards the light, strength) and their colours.
    metal: vec4<f32>, metal_a: vec4<f32>, metal_b: vec4<f32>, sky_tint: vec4<f32>,
    key: vec4<f32>, key_color: vec4<f32>, rim: vec4<f32>, rim_color: vec4<f32>,
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
// What a metal sees, its highlights and its roll-off: the flat face's
// (klipp-effects title_styles.wgsl), on the solid's own normals.
fn metal_environment(r: vec3<f32>, rough: f32) -> vec3<f32> {
    let up=-r.y;
    let w=0.015+rough*0.45;
    let floor_=mix(0.012,0.07,smoothstep(0.0,0.2,rough));
    var studio=vec3<f32>(mix(floor_,floor_+0.05,smoothstep(-w,w,up)));
    studio+=vec3<f32>(0.35)*exp(-pow((up+0.3)/(0.12+rough*0.3),2.0));
    studio+=vec3<f32>(1.9)*exp(-pow((up-0.04)/(0.035+rough*0.35),2.0));
    studio+=vec3<f32>(2.4)*smoothstep(0.5-w,0.5+w,up);
    studio+=vec3<f32>(1.1)*smoothstep(0.55-w,0.55+w,r.x)*smoothstep(-0.6,0.2,up);
    studio+=vec3<f32>(0.5)*smoothstep(0.6-w,0.6+w,-r.x)*smoothstep(-0.4,0.3,up);
    let horizon=vec3<f32>(1.0,0.84,0.62)*1.6;
    let zenith=vec3<f32>(0.80,0.84,0.92)*1.1;
    let ground=vec3<f32>(0.30,0.24,0.17)*0.5;
    let above=mix(horizon,zenith,smoothstep(0.0,0.7,up));
    let below=mix(ground*1.4,ground*0.25,smoothstep(0.0,0.5,-up))*mix(0.35,1.0,smoothstep(0.0,0.2,rough));
    let sky=mix(below,above,smoothstep(-w,w,up))*view.sky_tint.rgb;
    return studio*view.metal_a.w+sky*view.metal_b.x;
}
fn metal_light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, rough: f32) -> f32 {
    let a=max(rough,0.035);
    let s=clamp(2.0/(a*a*a*a)-2.0,4.0,4000.0);
    let h=unit(l+v);
    return (s+8.0)/(8.0*3.14159265)*pow(max(dot(n,h),0.0),s)*max(dot(n,l),0.0)*0.12;
}
fn metal_shoulder(x: f32) -> f32 {
    if x<0.75 { return max(x,0.0); }
    return 0.75+0.25*(1.0-exp(-(x-0.75)/0.25));
}
fn metal_hash(q: f32) -> f32 { return fract(sin(q*12.9898)*43758.5453); }
struct Surface { @location(0) color: vec4<f32>, @location(1) metal: vec4<f32> }
@fragment
fn fragment(v: Fragment) -> Surface {
    if v.alpha<=0.001 {discard;}
    var out: Surface;
    out.metal=vec4<f32>(0.0);
    if view.metal.w>0.5 {
        // The solid in metal: every face of it, its own normal reflecting
        // what is round it; the normal and fresnel term kept for the
        // shot's pass.
        let n=unit(v.normal);
        var towards=vec3<f32>(0.0,0.0,-1.0);
        if view.depth.z<0.5 {towards=unit(-v.camera);}
        let rough=view.metal_a.x; let aniso=view.metal_b.y;
        let r=reflect(-towards,n);
        let across=dot(v.position.xy,vec2<f32>(-sin(view.metal_b.z),cos(view.metal_b.z)));
        let streak=metal_hash(floor(across*0.4))*0.6+metal_hash(floor(across*0.1)+7.0)*0.4;
        let env=metal_environment(r,rough+aniso*0.25)*(1.0+aniso*0.8*(streak-0.5));
        let f0=view.metal.rgb;
        let grazing=pow(1.0-clamp(dot(n,towards),0.0,1.0),5.0)*view.metal_a.y;
        let fres=f0+(vec3<f32>(1.0)-f0)*grazing;
        var lin=env*fres;
        lin+=metal_light(n,towards,view.key.xyz,rough+aniso*0.2)*view.key.w*view.metal_a.z*view.key_color.rgb*fres;
        lin+=metal_light(n,towards,view.rim.xyz,rough+aniso*0.2)*view.rim.w*view.metal_a.z*view.rim_color.rgb*fres;
        lin=max(lin,fres*0.1);
        out.color=vec4<f32>(metal_shoulder(lin.x),metal_shoulder(lin.y),metal_shoulder(lin.z),v.alpha);
        out.metal=vec4<f32>(n,grazing);
        return out;
    }
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
    out.color=vec4<f32>(base*(view.surface.x+diffuse)+vec3<f32>(spec),v.alpha);
    return out;
}
