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
// Jump an LCG ahead in logarithmic work. This reproduces the CPU stripe
// sequence without running all preceding rows for every output pixel.
fn stripe_seed(steps: u32, initial: u32) -> u32 {
    var n=steps; var a=1664525u; var c=1013904223u;
    var mul=1u; var add=0u;
    while n>0u {
        if (n & 1u)!=0u { add=add*a+c; mul=mul*a; }
        c=(a+1u)*c; a=a*a; n=n>>1u;
    }
    return mul*initial+add;
}
fn authored_at(x: i32, y: u32) -> vec4<f32> {
    let v=load_rgba(y*p.width+u32(clamp(x,0,i32(p.width)-1)));
    let rgb=matrix(v.rgb);
    return floor(clamp(vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),v.a),vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
}
fn overlay_ch(base: f32, blend: f32) -> f32 {
    if base < 0.5 { return 2.0 * base * blend; }
    return 1.0 - 2.0 * (1.0 - base) * (1.0 - blend);
}
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend < 0.5 { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
    var d: f32;
    if base < 0.25 { d = ((16.0 * base - 12.0) * base + 4.0) * base; } else { d = sqrt(base); }
    return base + (2.0 * blend - 1.0) * (d - base);
}
fn dodge_ch(base: f32, blend: f32) -> f32 {
    if blend >= 1.0 { return 1.0; }
    return min(base / (1.0 - blend), 1.0);
}
fn burn_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.0 { return 0.0; }
    return max(1.0 - (1.0 - base) / blend, 0.0);
}
fn blend3(mode: u32, d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return d + s; }
        case 2u: { return d * s; }
        case 3u: { return vec3<f32>(1.0) - (vec3<f32>(1.0) - d) * (vec3<f32>(1.0) - s); }
        case 4u: { return vec3<f32>(overlay_ch(d.x, s.x), overlay_ch(d.y, s.y), overlay_ch(d.z, s.z)); }
        case 5u: { return vec3<f32>(soft_light_ch(d.x, s.x), soft_light_ch(d.y, s.y), soft_light_ch(d.z, s.z)); }
        case 6u: { return vec3<f32>(overlay_ch(s.x, d.x), overlay_ch(s.y, d.y), overlay_ch(s.z, d.z)); }
        case 7u: { return abs(d - s); }
        case 8u: { return d + s - 2.0 * d * s; }
        case 9u: { return vec3<f32>(dodge_ch(d.x, s.x), dodge_ch(d.y, s.y), dodge_ch(d.z, s.z)); }
        case 10u: { return vec3<f32>(burn_ch(d.x, s.x), burn_ch(d.y, s.y), burn_ch(d.z, s.z)); }
        case 11u: { return min(d, s); }
        case 12u: { return max(d, s); }
        default: { return s; }
    }
}

// Integral of clamp(1-x,0,1): an edge's signed pixel area.
fn coverage_integral(x:f32)->f32 {
    if x<=0.0 {return x;}
    if x>=1.0 {return 0.5;}
    return x-0.5*x*x;
}
fn edge_coverage(edge:vec4<f32>,pixel:vec2<f32>)->f32 {
    var a=edge.xy*p.row0.x+p.row0.yz;
    var b=edge.zw*p.row0.x+p.row0.yz;
    var direction=1.0;
    if a.y>b.y {let temp=a; a=b; b=temp; direction=-1.0;}
    let lo=max(a.y,pixel.y); let hi=min(b.y,pixel.y+1.0);
    if hi<=lo || b.y-a.y<0.000001 {return 0.0;}
    let slope=(b.x-a.x)/(b.y-a.y);
    let xa=a.x+(lo-a.y)*slope-pixel.x;
    let xb=a.x+(hi-a.y)*slope-pixel.x;
    var mean=clamp(1.0-(xa+xb)*0.5,0.0,1.0);
    if abs(xb-xa)>0.00001 {mean=(coverage_integral(xb)-coverage_integral(xa))/(xb-xa);}
    return direction*(hi-lo)*mean;
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x;
    if i>=p.width*p.height { return; }
    let x=i%p.width; let y=i/p.width;
    if p.op==14u {
        let px=i32(x)+p.offset_x; let py=i32(y)+p.offset_y;
        if px<0 || py<0 || px>=i32(p.frame_width) || py>=i32(p.y0) {return;}
        var coverage=0.0;
        var pixel=vec2<f32>(f32(x),f32(y));
        if p.row0.w>0.5 {pixel=vec2<f32>(f32(y),f32(p.width)-1.0-f32(x));}
        let bins=load_rgba(0u);
        let low=(pixel.y-p.row0.z)/p.row0.x;
        let high=(pixel.y+1.0-p.row0.z)/p.row0.x;
        let first=u32(clamp(floor((low-bins.x)/bins.y*bins.z),0.0,bins.z-1.0));
        let last=u32(clamp(floor((high-bins.x)/bins.y*bins.z),0.0,bins.z-1.0));
        for(var bin=first;bin<=last;bin+=1u) {
            let range=load_rgba(1u+bin);
            for(var j=0u;j<u32(range.y);j+=1u) {
                let edge=load_rgba(u32(range.x)+j);
                let edge_first=u32(clamp(floor((max(low,min(edge.y,edge.w))-bins.x)/bins.y*bins.z),0.0,bins.z-1.0));
                if bin==edge_first {coverage+=edge_coverage(edge,pixel);}
            }
        }
        let alpha=floor(clamp(abs(coverage)*255.9,0.0,255.0))/255.0;
        let dst=u32(py)*p.frame_width+u32(px);
        store_rgba(dst,vec4<f32>(p.color.rgb,max(alpha,load_dst(dst).a)));
    } else if p.op==13u {
        let matte=load_rgba(i); let object=load_dst(i);
        var value=matte.a;
        if p.offset_x!=0 { value*=dot(matrix(matte.rgb),vec3<f32>(0.2126,0.7152,0.0722)); }
        value=clamp(value,0.0,1.0);
        if p.offset_y!=0 { value=1.0-value; }
        store_rgba(i,vec4<f32>(object.rgb,object.a*value));
    } else if p.op==12u {
        let v=load_rgba(i);
        let uv=(vec2<f32>(f32(x),f32(y))+vec2<f32>(0.5))/vec2<f32>(f32(p.width),f32(p.height));
        var a=1.0;
        if p.color.x>0.0 { a*=smoothstep(0.0,p.color.x,uv.y); }
        if p.color.y>0.0 { a*=smoothstep(0.0,p.color.y,1.0-uv.y); }
        if p.color.z>0.0 { a*=smoothstep(0.0,p.color.z,uv.x); }
        if p.color.w>0.0 { a*=smoothstep(0.0,p.color.w,1.0-uv.x); }
        var alpha=v.a;
        if a<1.0 { alpha=floor(round(alpha*255.0)*a+0.5)/255.0; }
        if abs(p.row0.x-1.0)>=0.001 { alpha=floor(alpha*p.row0.x*255.0+0.5)/255.0; }
        store_rgba(i,vec4<f32>(v.rgb,alpha));
    } else if p.op==10u {
        let s=load_rgba(i); let d=load_dst(i);
        let srgb=matrix(s.rgb); let drgb=matrix(d.rgb);
        var alpha=s.a;
        if abs(p.color.a-1.0)>=0.001 { alpha=floor(alpha*p.color.a*255.0+0.5)/255.0; }
        if p.offset_x==0 {
        store_rgba(i,over(vec4<f32>(encoded(srgb.x),encoded(srgb.y),encoded(srgb.z),alpha),
            vec4<f32>(encoded(drgb.x),encoded(drgb.y),encoded(drgb.z),d.a)));
        } else {
            let a=alpha+d.a*(1.0-alpha);
            var rgb=vec3<f32>(0.0);
            if a>0.0 {
                let mixed=(1.0-d.a)*srgb+d.a*clamp(blend3(u32(p.offset_x),drgb,srgb),vec3<f32>(0.0),vec3<f32>(1.0));
                rgb=(mixed*alpha+drgb*d.a*(1.0-alpha))/a;
            }
            store_rgba(i,vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),a));
        }
    } else if p.op==11u {
        let v=load_rgba(i);
        store_rgba(i,vec4<f32>(matrix(vec3<f32>(linear(v.x),linear(v.y),linear(v.z))),v.a));
    } else if p.op==9u {
        var sx=i32(x);
        if p.offset_y>0 {
            let seed=107078757u+p.x0*2654435761u;
            let r=stripe_seed(y/8u+1u,seed);
            let offset=i32((r>>8u)%u32(2*p.offset_y+1))-p.offset_y;
            sx=clamp(sx-offset,0,i32(p.width)-1);
        }
        let r=authored_at(sx-p.offset_x,y);
        let g=authored_at(sx,y);
        let b=authored_at(sx+p.offset_x,y);
        store_rgba(i,vec4<f32>(r.r,g.g,b.b,max(r.a,max(g.a,b.a))));
    } else if p.op==1u { // Extract coverage directly from the unchanged stack face.
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
        output[i]=select(output[i]-min(output[i],input[i]),0u,i32(x+p.x0)>=p.offset_x);
    } else if p.op==4u { // Inner shadow coverage, integer truncation.
        output[i]=(output[i]*input[i])/255u;
    } else if p.op==5u { // Authored style over the accumulated stack.
        let a=floor(clamp(f32(input[i])*p.color.a,0.0,255.0))/255.0;
        store_rgba(i,over(vec4<f32>(p.color.rgb,a),load_dst(i)));
    } else if p.op==6u { // Face over behind styles.
        let v=load_rgba((y+p.y0)*p.frame_width+x+p.x0);
        let rgb=matrix(v.rgb);
        let a=floor(round(clamp(v.a,0.0,1.0)*255.0)*floor(clamp(p.color.a,0.0,1.0)*255.0+0.5)/255.0)/255.0;
        let face=vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),a);
        store_rgba(i,over(face,load_dst(i)));
    } else if p.op==7u { // Final title quantization and working-space conversion.
        let v=floor(clamp(load_rgba(i),vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
        let rgb=matrix(vec3<f32>(linear(v.x),linear(v.y),linear(v.z)));
        store_rgba((y+p.y0)*p.frame_width+x+p.x0,vec4<f32>(rgb,v.a));
    }
}
