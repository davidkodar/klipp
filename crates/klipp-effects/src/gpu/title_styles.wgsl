// Bounded title ROI operations. Coverage remains integer 0..255; composition
// matches the authored sRGB title stack before returning to working light.
struct Params {
    width: u32, height: u32, frame_width: u32, op: u32,
    x0: u32, y0: u32, offset_x: i32, offset_y: i32,
    color: vec4<f32>,
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>,
    stops: array<vec4<u32>,8>,
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
// Rust's `round`: halves away from zero, where WGSL's rounds them to even.
fn round_away(x: f32) -> f32 {
    let f=floor(abs(x));
    return sign(x)*(f+select(0.0,1.0,abs(x)-f>=0.5));
}
fn round_away3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(round_away(v.x),round_away(v.y),round_away(v.z));
}
fn rounded_rect(point:vec2<f32>,lo:vec2<f32>,hi:vec2<f32>,r:f32)->bool {
    if any(point<lo) || any(point>hi) {return false;}
    if r<=0.0 {return true;}
    let near_lo=point<lo+vec2<f32>(r);let near_hi=point>hi-vec2<f32>(r);
    if !(near_lo.x||near_hi.x) || !(near_lo.y||near_hi.y) {return true;}
    let centre=select(hi-vec2<f32>(r),lo+vec2<f32>(r),near_lo);
    return dot(point-centre,point-centre)<=r*r;
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
fn curve_point(a:vec2<f32>,b:vec2<f32>,c:vec2<f32>,d:vec2<f32>,degree:u32,t:f32)->vec2<f32> {
    let u=1.0-t;
    if degree==1u {return a*u+b*t;}
    if degree==2u {return a*(u*u)+b*(2.0*u*t)+c*(t*t);}
    return a*(u*u*u)+b*(3.0*u*u*t)+c*(3.0*u*t*t)+d*(t*t*t);
}
fn curve_coverage(at:u32,pixel:vec2<f32>)->f32 {
    let ab=load_rgba(at);let cd=load_rgba(at+1u);let degree=u32(load_rgba(at+2u).x);
    var n=1u;
    if degree>1u {
        var dev=length(ab.xy-2.0*ab.zw+cd.xy)*p.row0.x;
        if degree==3u {dev=1.5*max(dev,length(ab.zw-2.0*cd.xy+cd.zw)*p.row0.x);}
        n=1u+u32(min(sqrt(sqrt(3.0*dev*dev)),64.0));
    }
    var a=ab.xy;var coverage=0.0;
    for(var k=1u;k<=n;k+=1u) {
        let b=curve_point(ab.xy,ab.zw,cd.xy,cd.zw,degree,f32(k)/f32(n));
        coverage+=edge_coverage(vec4<f32>(a,b),pixel);a=b;
    }
    return coverage;
}
fn stop_position(i:u32)->f32 {return bitcast<f32>(p.stops[i/2u][(i%2u)*2u]);}
fn stop_color(i:u32)->vec4<f32> {
    let c=p.stops[i/2u][(i%2u)*2u+1u];
    return vec4<f32>(f32(c&255u),f32((c>>8u)&255u),f32((c>>16u)&255u),f32(c>>24u))/255.0;
}
fn fill_color(pos:vec2<f32>)->vec4<f32> {
    if p.offset_y==0 {
        let top=f32(i32(p.row0.y));let bottom=f32(i32(p.row0.w));
        let t=clamp((pos.y-top)/max(bottom-top,1.0),0.0,1.0);
        return floor(clamp(p.color+(p.row2-p.color)*t,vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
    }
    if p.offset_y==2 {
        var band=0i;let size=p.row1.y;
        switch u32(p.row1.x) {
            case 0u:{band=i32(pos.y/size);}
            case 1u:{band=i32(pos.x/size);}
            case 2u:{band=i32(pos.x/size)+i32(pos.y/size);}
            default:{band=i32(floor((pos.x+pos.y)/size));}
        }
        return select(p.color,p.row2,band%2!=0);
    }
    let extent=max(p.row0.zw-p.row0.xy,vec2<f32>(1.0));
    let uv=(pos+vec2<f32>(0.5)-p.row0.xy)/extent;
    var t=0.0;
    if p.row1.w>0.5 {t=length((uv-p.row1.xy)/p.row1.z);}
    else {let span=dot(abs(p.row1.xy),extent);if span>0.0 {t=dot((uv-vec2<f32>(0.5))*extent,p.row1.xy)/span+0.5;}}
    return gradient_colour(t,u32(p.color.x));
}
fn gradient_colour(position:f32,count:u32)->vec4<f32> {
    var t=position;
    // The reference ramp uses 4097 quantized positions.
    t=floor(clamp(t,0.0,1.0)*4096.0+0.5)/4096.0;
    if t<=stop_position(0u) {return stop_color(0u);}
    if t>=stop_position(count-1u) {return stop_color(count-1u);}
    var next=count-1u;
    for(var j=1u;j<count;j+=1u) {if stop_position(j)>t {next=j;break;}}
    let a=stop_color(next-1u);let b=stop_color(next);
    let span=stop_position(next)-stop_position(next-1u);
    var f=1.0;if span>0.0 {f=(t-stop_position(next-1u))/span;}
    let alpha=a.a+(b.a-a.a)*f;
    let x=vec3<f32>(linear(a.r),linear(a.g),linear(a.b))*a.a;
    let y=vec3<f32>(linear(b.r),linear(b.g),linear(b.b))*b.a;
    var rgb=vec3<f32>(0.0);
    if alpha>0.000001 {let v=clamp((x+(y-x)*f)/alpha,vec3<f32>(0.0),vec3<f32>(1.0));rgb=vec3<f32>(encoded(v.r),encoded(v.g),encoded(v.b));}
    return floor(clamp(vec4<f32>(rgb,alpha),vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
}
fn glyph_at(pos:vec2<f32>)->f32 {
    if any(pos<vec2<f32>(0.0)) || any(pos>=p.row0.xy) {return 0.0;}
    return bitcast<f32>(input[u32(pos.y)*u32(p.row0.x)+u32(pos.x)]);
}
fn glyph_store(dst:u32,alpha:f32,px:i32,py:i32) {
    if p.x0==1u {
        let pos=vec2<f32>(f32(px),f32(py));
        if any(pos<p.row2.xy) || any(pos>=p.row2.xy+p.row2.zw) {return;}
        let a=floor(round(alpha*255.0)*round(p.color.a*255.0)/255.0)/255.0;
        store_rgba(dst,over(vec4<f32>(p.color.rgb,a),load_dst(dst)));
    } else {store_rgba(dst,vec4<f32>(p.color.rgb,max(alpha,load_dst(dst).a)));}
}
// Normalized byte channels are zero or normal f16 values. Explicit mantissa
// rounding keeps the CPU sprite boundary even if pack/unpack is optimized away.
fn byte_half(v: vec4<f32>) -> vec4<f32> {
    let bits=bitcast<vec4<u32>>(v);
    return bitcast<vec4<f32>>((bits+vec4<u32>(4095u)+((bits>>vec4<u32>(13u))&vec4<u32>(1u)))&vec4<u32>(0xffffe000u));
}
// Undo premultiplication and round the blur to the CPU painter's byte layer.
fn blurred_byte(raw: vec4<f32>) -> vec4<f32> {
    let alpha=raw.a/255.0;
    var soft=vec4<f32>(0.0,0.0,0.0,floor(clamp(raw.a,0.0,255.0)+0.5)/255.0);
    if alpha>0.0 {soft=vec4<f32>(floor(clamp(raw.rgb/alpha,vec3<f32>(0.0),vec3<f32>(255.0))+vec3<f32>(0.5))/255.0,soft.a);}
    return soft;
}
fn motion_tap(pos: vec2<i32>) -> vec4<f32> {
    if p.op==48u {
        // The header is the original anchor box, not the expanded blur box.
        // Sample the halo too, with the CPU BoxRaster's half-float boundary.
        if any(pos<vec2<i32>(0)) || any(pos>=vec2<i32>(i32(p.width),i32(p.height))) {return vec4<f32>(0.0);}
        return byte_half(blurred_byte(load_rgba(u32(pos.y)*p.width+u32(pos.x)+1u)));
    }
    if any(pos<vec2<i32>(i32(input[0]),i32(input[1]))) || any(pos>=vec2<i32>(i32(input[2]),i32(input[3]))) {return vec4<f32>(0.0);}
    return load_rgba(u32(pos.y)*p.width+u32(pos.x)+1u);
}
fn motion_premult(pos: vec2<i32>) -> vec4<f32> {
    let v=motion_tap(pos);return vec4<f32>(v.rgb*v.a,v.a);
}
// The distance grid is written only by the single reduction invocation.
fn solid_distance(x:i32,y:i32,gw:u32,gh:u32,base:u32) -> u32 {
    if x<0 || y<0 || x>=i32(gw) || y>=i32(gh) {return 0u;}
    return output[base+u32(y)*gw+u32(x)];
}
// An image layer's decoded pixel, packed RGBA8 one to a u32, in byte units.
fn image_px(x: u32, y: u32) -> vec4<f32> {
    let v=input[y*u32(p.offset_x)+x];
    return vec4<f32>(f32(v&255u),f32((v>>8u)&255u),f32((v>>16u)&255u),f32(v>>24u));
}
// `DecodedImage::sample`: bilinear at source pixel coordinates, edges
// clamped, weighted by alpha, straight colour back.
fn image_sample(u0: f32, v0: f32) -> vec4<f32> {
    let iw=u32(p.offset_x);let ih=u32(p.offset_y);
    let u=clamp(u0,0.0,f32(iw-1u));let v=clamp(v0,0.0,f32(ih-1u));
    let x0=u32(floor(u));let y0=u32(floor(v));
    let x1=min(x0+1u,iw-1u);let y1=min(y0+1u,ih-1u);
    let fx=u-f32(x0);let fy=v-f32(y0);
    var out=vec4<f32>(0.0);
    var taps=array<vec3<f32>,4>(vec3<f32>(f32(x0),f32(y0),(1.0-fx)*(1.0-fy)),vec3<f32>(f32(x1),f32(y0),fx*(1.0-fy)),vec3<f32>(f32(x0),f32(y1),(1.0-fx)*fy),vec3<f32>(f32(x1),f32(y1),fx*fy));
    for (var k=0u;k<4u;k+=1u) {
        let px=image_px(u32(taps[k].x),u32(taps[k].y));
        let a=px.a*taps[k].z;
        out=vec4<f32>(out.rgb+px.rgb*a,out.a+a);
    }
    if out.a>0.0 {out=vec4<f32>(out.rgb/out.a,out.a);}
    return out;
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i=gid.x;
    if i>=p.width*p.height { return; }
    let x=i%p.width; let y=i/p.width;
    // MaskRise clips destination rows to the pre-motion painted window.
    // The source may have moved, scaled or grown a blur halo beyond it.
    if p.offset_y==1 && (p.op==41u || p.op==43u || p.op==46u || p.op==47u || p.op==48u)
        && (y<input[1] || y>=input[3]) {store_rgba(i,vec4<f32>(0.0));return;}
    if p.op==49u {
        let bw=input[2]-min(input[0],input[2]);let bh=input[3]-min(input[1],input[3]);
        let cell=max(max(bw,bh)/160u,1u);
        let gw=(bw+cell-1u)/cell;let gh=(bh+cell-1u)/cell;
        if i==0u {
            for(var k=0u;k<4u;k+=1u) {output[k]=input[k];}
            output[4]=gw;output[5]=gh;output[6]=cell;
        }
        if i>=gw*gh {return;}
        let gx=i%gw;let gy=i/gw;var covered=0u;var count=0u;
        for(var row=gy*cell;row<min((gy+1u)*cell,bh);row+=1u) {
            for(var col=gx*cell;col<min((gx+1u)*cell,bw);col+=1u) {
                let raw=load_rgba((row+input[1])*p.width+col+input[0]+1u);
                var alpha=round(raw.a*255.0);
                if p.offset_x==1 {alpha=floor(clamp(raw.a,0.0,255.0)+0.5);}
                covered+=select(0u,1u,alpha>=128.0);count+=1u;
            }
        }
        output[8u+i]=select(0u,1073741823u,covered*2u>count);
    } else if p.op==50u {
        if i!=0u {return;}
        let gw=input[4];let gh=input[5];let cell=input[6];
        let anchor_index=4u+p.width*p.height*4u;let base=anchor_index+2u;
        for(var k=0u;k<gw*gh;k+=1u) {output[base+k]=input[8u+k];}
        for(var row=0i;row<i32(gh);row+=1i) {
            for(var col=0i;col<i32(gw);col+=1i) {
                let index=base+u32(row)*gw+u32(col);
                let d=min(min(solid_distance(col-1i,row,gw,gh,base)+3u,solid_distance(col,row-1i,gw,gh,base)+3u),
                    min(solid_distance(col-1i,row-1i,gw,gh,base)+4u,solid_distance(col+1i,row-1i,gw,gh,base)+4u));
                output[index]=min(output[index],d);
            }
        }
        for(var row=i32(gh)-1i;row>=0i;row-=1i) {
            for(var col=i32(gw)-1i;col>=0i;col-=1i) {
                let index=base+u32(row)*gw+u32(col);
                let d=min(min(solid_distance(col+1i,row,gw,gh,base)+3u,solid_distance(col,row+1i,gw,gh,base)+3u),
                    min(solid_distance(col+1i,row+1i,gw,gh,base)+4u,solid_distance(col-1i,row+1i,gw,gh,base)+4u));
                output[index]=min(output[index],d);
            }
        }
        var best=0u;var depth=0u;
        // Rust max_by_key chooses the last equal maximum.
        for(var k=0u;k<gw*gh;k+=1u) {if output[base+k]>=depth {depth=output[base+k];best=k;}}
        var anchor=(vec2<f32>(f32(input[0]),f32(input[1]))+vec2<f32>(f32(input[2]),f32(input[3])))*0.5;
        if depth>0u {anchor=vec2<f32>(f32(input[0]),f32(input[1]))+(vec2<f32>(f32(best%gw),f32(best/gw))+vec2<f32>(0.5))*f32(cell);}
        output[anchor_index]=bitcast<u32>(anchor.x);output[anchor_index+1u]=bitcast<u32>(anchor.y);
    } else if p.op==44u {
        let v=round(authored_at(i32(x),y)*255.0);
        store_rgba(i+1u,vec4<f32>(v.rgb*(v.a/255.0),v.a));
    } else if p.op==45u {
        var lines=p.height;var len=p.width;var step=1u;var stride=p.width;
        if p.offset_y==1 {lines=p.width;len=p.height;step=p.width;stride=1u;}
        if i>=lines {return;}
        if i==0u {for(var k=0u;k<4u;k+=1u) {output[k]=input[k];}}
        let height=f32(input[3]-min(input[1],input[3]));
        let sigma=max((p.color.x*height)*p.color.y,(p.color.z*height)*p.color.w);
        let radius=select(0u,u32(clamp(floor(sigma/1.7+0.5),1.0,f32(max(p.width,p.height)))),sigma>0.5);
        let base=1u+i*stride;var sum=vec4<f32>(0.0);
        for(var j=0u;j<=min(radius,len-1u);j+=1u) {sum+=load_rgba(base+j*step);}
        var n=f32(min(radius,len-1u)+1u);
        for(var j=0u;j<len;j+=1u) {
            // Correct reciprocal-division error before the next box pass.
            // A few ulps can otherwise flip the final alpha byte at a tie,
            // which subsequent edge/object fades can amplify visibly.
            let quotient=sum/n;
            let residual=fma(-quotient,vec4<f32>(n),sum);
            store_rgba(base+j*step,quotient+residual/n);
            if j+radius+1u<len {sum+=load_rgba(base+(j+radius+1u)*step);n+=1.0;}
            if j>=radius {sum-=load_rgba(base+(j-radius)*step);n-=1.0;}
        }
    } else if p.op==46u || p.op==47u {
        var src=i;
        if p.op==47u {
            // The header retains pre-blur bounds through all six box passes.
            // Do not crop samples to those bounds: the halo moves as well.
            let height=f32(input[3]-min(input[1],input[3]));
            let a=p.row0.x*(height*p.row0.y)*p.row0.zw;
            let b=p.row1.x*(height*p.row1.y)*p.row1.zw;
            let shift=sign(a+b)*floor(abs(a+b)+vec2<f32>(0.5));
            let pos=vec2<f32>(f32(x),f32(y))-shift;
            if any(pos<vec2<f32>(0.0)) || any(pos>=vec2<f32>(f32(p.width),f32(p.height))) {
                store_rgba(i,vec4<f32>(0.0));return;
            }
            src=u32(pos.y)*p.width+u32(pos.x);
        }
        store_rgba(i,blurred_byte(load_rgba(src+1u)));
    } else if p.op==38u {
        if i>=p.height {return;}
        var lo=p.width;var hi=0u;
        for(var col=0u;col<p.width;col+=1u) {
            if load_rgba(i*p.width+col).a*255.0>=0.5 {lo=min(lo,col);hi=col+1u;}
        }
        output[i*2u]=lo;output[i*2u+1u]=hi;
    } else if p.op==39u {
        if i!=0u {return;}
        var x0=p.width;var x1=0u;var y0=p.height;var y1=0u;
        for(var row=0u;row<p.height;row+=1u) {
            let lo=input[row*2u];let hi=input[row*2u+1u];
            if hi>lo {x0=min(x0,lo);x1=max(x1,hi);y0=min(y0,row);y1=row+1u;}
        }
        output[0]=x0;output[1]=y0;output[2]=x1;output[3]=y1;
    } else if p.op==40u {
        if x<input[0] || y<input[1] || x>=input[2] || y>=input[3] {store_rgba(i,vec4<f32>(0.0));}
    } else if p.op==42u {
        store_rgba(i,byte_half(authored_at(i32(x),y)));
    } else if p.op==43u || p.op==48u {
        var pixel=vec4<f32>(0.0);
        if input[2]>input[0] && input[3]>input[1] {
            let height=f32(input[3]-input[1]);
            let a=p.row0.x*(height*p.row0.y)*p.row0.zw;
            let b=p.row1.x*(height*p.row1.y)*p.row1.zw;
            let shift=sign(a+b)*floor(abs(a+b)+vec2<f32>(0.5));
            let lo=vec2<f32>(f32(input[0]),f32(input[1]));
            let hi=vec2<f32>(f32(input[2]),f32(input[3]));
            var anchor=lo+p.row2.zw*(hi-lo);
            if p.offset_x==1 {
                let index=4u+p.width*p.height*4u;
                anchor=vec2<f32>(bitcast<f32>(input[index]),bitcast<f32>(input[index+1u]));
            }
            if abs(p.row2.x-1.0)<0.000001 {
                pixel=motion_tap(vec2<i32>(vec2<f32>(f32(x),f32(y))-shift));
                pixel.a*=p.row2.y;
            } else {
                let src=(vec2<f32>(f32(x),f32(y))+vec2<f32>(0.5)-shift-anchor)/p.row2.x+anchor-vec2<f32>(0.5);
                let base=vec2<i32>(floor(src));let t=src-floor(src);
                var v=motion_premult(base)*((1.0-t.x)*(1.0-t.y));
                v+=motion_premult(base+vec2<i32>(1,0))*(t.x*(1.0-t.y));
                v+=motion_premult(base+vec2<i32>(0,1))*((1.0-t.x)*t.y);
                v+=motion_premult(base+vec2<i32>(1,1))*(t.x*t.y);
                let alpha=clamp(v.a*p.row2.y,0.0,1.0);
                pixel.a=alpha;
                if v.a>0.0 && alpha*255.0>=0.5 {pixel=vec4<f32>(v.rgb/v.a,alpha);}
            }
        }
        store_rgba(i,floor(clamp(pixel,vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0);
    } else if p.op==41u {
        var pixel=vec4<f32>(0.0);
        if input[2]>input[0] && input[3]>input[1] {
            let height=f32(input[3]-input[1]);
            let enter=p.row0.x*(height*p.row0.y)*p.row0.zw;
            let exit=p.row1.x*(height*p.row1.y)*p.row1.zw;
            let shift=enter+exit;
            let delta=vec2<i32>(sign(shift)*floor(abs(shift)+vec2<f32>(0.5)));
            let src=vec2<i32>(i32(x),i32(y))-delta;
            if all(src>=vec2<i32>(i32(input[0]),i32(input[1]))) && all(src<vec2<i32>(i32(input[2]),i32(input[3]))) {
                pixel=load_rgba(u32(src.y)*p.width+u32(src.x)+1u);
            }
        }
        store_rgba(i,pixel);
    } else if p.op==35u {
        if p.offset_x!=0 {store_rgba(i,authored_at(i32(x),y));}
        else {
            let old=load_rgba(i);let rgb=matrix(old.rgb);
            let v=vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),old.a);
            store_rgba(i,vec4<f32>(unpack2x16float(pack2x16float(v.xy)),unpack2x16float(pack2x16float(v.zw))));
        }
    } else if p.op==36u {
        let src=load_rgba(i);let dst=load_dst(i);let sc=matrix(src.rgb);let dc=matrix(dst.rgb);
        let s=vec3<f32>(encoded(sc.x),encoded(sc.y),encoded(sc.z));let d=vec3<f32>(encoded(dc.x),encoded(dc.y),encoded(dc.z));
        store_rgba(i,vec4<f32>(s+d*(1.0-src.a),src.a+dst.a*(1.0-src.a)));
    } else if p.op==51u || p.op==52u {
        // A vector path's painted layer, as `vector_path::paint_filled`
        // draws it: two-by-two samples, even-odd fill and round-capped
        // stroke capsules, each with the CPU's ceil(2x - 0.5) sample rule,
        // then the fill/stroke mix rounded to bytes. The input holds a
        // header and the segments, offset to the painted box as on the CPU.
        // 52 writes the stroke's byte mask for the glow; 51 lays the glow
        // already in `output` (its alpha, blurred) under the mix.
        let bx0=input[2];let by0=input[3];let bx1=input[4];let by1=input[5];
        if x<bx0 || x>=bx1 || y<by0 || y>=by1 {store_rgba(i,vec4<f32>(0.0));return;}
        let n_fill=input[0];let n_stroke=input[1];let r=bitcast<f32>(input[6]);
        let fill=vec4<f32>(bitcast<f32>(input[8]),bitcast<f32>(input[9]),bitcast<f32>(input[10]),bitcast<f32>(input[11]));
        let stroke=vec4<f32>(bitcast<f32>(input[12]),bitcast<f32>(input[13]),bitcast<f32>(input[14]),bitcast<f32>(input[15]));
        let glow=bitcast<f32>(input[16]);
        let lx=x-bx0;let ly=y-by0;
        var fbits=0u;var sbits=0u;
        for (var k=0u;k<4u;k+=1u) {
            let sy=2u*ly+k/2u;let sx=f32(2u*lx+k%2u);
            let ys=(f32(sy)+0.5)*0.5;
            if fill.a>0.0 && p.op==51u {
                var odd=false;
                for (var t=input[input[18]+sy];t<input[input[18]+sy+1u];t+=1u) {
                    let o=32u+4u*input[t];
                    let a=vec2<f32>(bitcast<f32>(input[o]),bitcast<f32>(input[o+1u]));
                    let b=vec2<f32>(bitcast<f32>(input[o+2u]),bitcast<f32>(input[o+3u]));
                    if (a.y>ys)!=(b.y>ys) {
                        let c=a.x+(ys-a.y)*(b.x-a.x)/(b.y-a.y);
                        if ceil(c*2.0-0.5)<=sx {odd=!odd;}
                    }
                }
                if odd {fbits|=1u<<k;}
            }
            if r>0.0 && stroke.a>0.0 {
                for (var t=input[input[19]+sy];t<input[input[19]+sy+1u];t+=1u) {
                    let o=32u+4u*(n_fill+input[t]);
                    let a=vec2<f32>(bitcast<f32>(input[o]),bitcast<f32>(input[o+1u]));
                    let b=vec2<f32>(bitcast<f32>(input[o+2u]),bitcast<f32>(input[o+3u]));
                    let row0=ceil((min(a.y,b.y)-r)*2.0-0.5);let row1=ceil((max(a.y,b.y)+r)*2.0-0.5);
                    if f32(sy)<row0 || f32(sy)>=row1 {continue;}
                    let dx=b.x-a.x;let dy=b.y-a.y;let len=sqrt(dx*dx+dy*dy);
                    var n=vec2<f32>(0.0);
                    if len>1e-6 {n=vec2<f32>(-dy/len*r,dx/len*r);}
                    var lo=1e30;var hi=-1e30;
                    for (var q=0u;q<2u;q+=1u) {
                        let c=select(a,b,q==1u);let d=ys-c.y;
                        if abs(d)<=r {let w=sqrt(max(r*r-d*d,0.0));lo=min(lo,c.x-w);hi=max(hi,c.x+w);}
                    }
                    var corners=array<vec2<f32>,4>(a+n,b+n,b-n,a-n);
                    for (var q=0u;q<4u;q+=1u) {
                        let u=corners[q];let v=corners[(q+1u)%4u];
                        if (u.y>ys)!=(v.y>ys) {let cx=u.x+(ys-u.y)*(v.x-u.x)/(v.y-u.y);lo=min(lo,cx);hi=max(hi,cx);}
                    }
                    if lo<=hi && ceil(lo*2.0-0.5)<=sx && sx<ceil(hi*2.0-0.5) {sbits|=1u<<k;break;}
                }
            }
        }
        if p.op==52u {
            store_rgba(i,vec4<f32>(0.0,0.0,0.0,round_away(f32(countOneBits(sbits))*0.25*stroke.a*255.0)));
            return;
        }
        // Straight sRGB bytes laid over nothing, then the mix over them,
        // with over_blend_chunk's arithmetic.
        var d=vec4<f32>(0.0);
        if glow>0.0 {
            let halo=load_dst(i).a;
            if halo>0.0 {
                let ha=round_away(halo*glow);
                let sa=ha/255.0;
                if sa>0.0 {d=vec4<f32>(round_away3(stroke.rgb*255.0)*sa/sa,sa*255.0);}
            }
        }
        // A gradient fill's colour at the pixel's box fractions, as
        // `GradientKind::position_at` and the ramp give it (#2208).
        var face=fill;
        if p.offset_x>0 && fbits!=0u {
            let g=vec4<f32>(bitcast<f32>(input[20]),bitcast<f32>(input[21]),bitcast<f32>(input[22]),bitcast<f32>(input[23]));
            let pivot=vec2<f32>(bitcast<f32>(input[24]),bitcast<f32>(input[25]));
            let cs=vec2<f32>(bitcast<f32>(input[26]),bitcast<f32>(input[27]));
            let size=vec2<f32>(bitcast<f32>(input[28]),bitcast<f32>(input[29]));
            let anchor=vec2<f32>(bitcast<f32>(input[30]),bitcast<f32>(input[31]));
            let dd=vec2<f32>(f32(x)+0.5,f32(y)+0.5)-pivot;
            let u=(cs.x*dd.x+cs.y*dd.y)/size.x+anchor.x;
            let v=(cs.x*dd.y-cs.y*dd.x)/size.y+anchor.y;
            var t=0.0;
            if g.w>0.5 {
                let a=(u-g.x)/g.z;let b=(v-g.y)/g.z;
                t=sqrt(a*a+b*b);
            } else {
                let half=abs(g.x)*size.x*0.5+abs(g.y)*size.y*0.5;
                if half>0.0 {t=((u-0.5)*size.x*g.x+(v-0.5)*size.y*g.y)/(2.0*half)+0.5;}
            }
            // Non-finite positions read the first stop, as on the CPU.
            if !(t==t) || abs(t)>3.0e38 {t=0.0;}
            face=gradient_colour(clamp(t,0.0,1.0),u32(p.offset_x));
        }
        let fa=f32(countOneBits(fbits))*0.25*face.a;
        let sa=f32(countOneBits(sbits))*0.25*stroke.a;
        let overlap=f32(countOneBits(fbits&sbits))*0.25*face.a*stroke.a;
        let alpha=fa+sa-overlap;
        if (fbits!=0u || sbits!=0u) && alpha>0.0 {
            let src_rgb=round_away3((face.rgb*(fa-overlap)+stroke.rgb*sa)/alpha*255.0);
            let s_a=round_away(alpha*255.0)/255.0;
            if s_a>0.0 {
                let inv=1.0-s_a;let da=d.a/255.0;let out_a=s_a+da*inv;
                if out_a>0.0 {d=vec4<f32>((src_rgb*s_a+d.rgb*da*inv)/out_a,out_a*255.0);}
            }
        }
        if d.a<=0.0 {store_rgba(i,vec4<f32>(0.0));return;}
        // Paths are laid in straight alpha on the CPU, whatever the scene.
        let v=d.rgb;
        let lin=matrix(vec3<f32>(linear(v.x/255.0),linear(v.y/255.0),linear(v.z/255.0)));
        store_rgba(i,vec4<f32>(lin,d.a/255.0));
    } else if p.op==53u {
        // One box pass of `blur_alpha` over the path's box, on alpha bytes:
        // the window clipped to the box, the sum truncated by its count.
        let bx1=u32(p.color.x);let by1=u32(p.color.y);
        if x<p.x0 || x>=bx1 || y<p.y0 || y>=by1 {store_rgba(i,vec4<f32>(0.0));return;}
        let r=u32(p.offset_x);
        var sum=0u;var count=0u;
        if p.offset_y==0 {
            let lo=max(x,p.x0+r)-r;let hi=min(x+r+1u,bx1);
            for (var j=lo;j<hi;j+=1u) {sum+=u32(load_rgba(y*p.width+j).a);}
            count=hi-lo;
        } else {
            let lo=max(y,p.y0+r)-r;let hi=min(y+r+1u,by1);
            for (var j=lo;j<hi;j+=1u) {sum+=u32(load_rgba(j*p.width+x).a);}
            count=hi-lo;
        }
        var out=0u;
        if sum>0u {out=sum/count;}
        store_rgba(i,vec4<f32>(0.0,0.0,0.0,f32(out)));
    } else if p.op==34u || p.op==37u {
        let d=vec2<f32>(f32(x)+0.5,f32(y)+0.5)-p.row1.xy;
        let q=p.row1.xy+vec2<f32>(p.row1.z*d.x+p.row1.w*d.y,-p.row1.w*d.x+p.row1.z*d.y);
        let lo=p.row0.xy-p.row0.zw;let hi=p.row0.xy+p.row0.zw;var colour=vec4<f32>(0.0);
        if rounded_rect(q,lo,hi,p.row2.x) {
            let stroke=p.row2.y>0.0 && !rounded_rect(q,lo+vec2<f32>(p.row2.y),hi-vec2<f32>(p.row2.y),max(p.row2.x-p.row2.y,0.0));
            if p.op==34u {
                colour=p.color;
                if stroke {colour=bitcast<vec4<f32>>(p.stops[0]);}
            } else {
                let extent=max(hi-lo,vec2<f32>(1.0));let uv=(q-lo)/extent;
                var t=0.0;
                if p.color.w>0.5 {t=length((uv-p.color.xy)/p.color.z);}
                else {let span=dot(abs(p.color.xy),extent);if span>0.0 {t=dot((uv-vec2<f32>(0.5))*extent,p.color.xy)/span+0.5;}}
                colour=gradient_colour(t,u32(p.offset_x));
                if stroke {let c=p.frame_width;colour=vec4<f32>(f32(c&255u),f32((c>>8u)&255u),f32((c>>16u)&255u),f32(c>>24u))/255.0;}
                if p.offset_y==0 {colour=vec4<f32>(colour.rgb*colour.a,colour.a);}
            }
        }
        store_rgba(i,colour);
    } else if p.op==31u {
        let v=authored_at(i32(x),y)*255.0;
        store_rgba(i,vec4<f32>(v.rgb*(v.a/255.0),v.a));
    } else if p.op==32u {
        var lines=p.height;var len=p.width;var step=1u;var stride=p.width;
        if p.offset_y==1 {lines=p.width;len=p.height;step=p.width;stride=1u;}
        if i>=lines {return;}
        let base=i*stride;let radius=u32(p.offset_x);var sum=vec4<f32>(0.0);
        for(var j=0u;j<=min(radius,len-1u);j+=1u) {sum+=load_rgba(base+j*step);}
        var n=f32(min(radius,len-1u)+1u);
        for(var j=0u;j<len;j+=1u) {
            store_rgba(base+j*step,sum/n);
            if j+radius+1u<len {sum+=load_rgba(base+(j+radius+1u)*step);n+=1.0;}
            if j>=radius {sum-=load_rgba(base+(j-radius)*step);n-=1.0;}
        }
    } else if p.op==33u {
        let raw=load_rgba(i);let alpha=raw.a/255.0;
        var soft=vec4<f32>(0.0,0.0,0.0,round(clamp(raw.a,0.0,255.0))/255.0);
        if alpha>0.0 {soft=vec4<f32>(round(clamp(raw.rgb/alpha,vec3<f32>(0.0),vec3<f32>(255.0)))/255.0,soft.a);}
        let old=load_dst(i);let rgb=matrix(old.rgb);
        let base=round(clamp(vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),old.a),vec4<f32>(0.0),vec4<f32>(1.0))*255.0)/255.0;
        let u=select((f32(x)+0.5)/f32(p.width),(f32(y)+0.5)/f32(p.height),p.color.x>0.5);
        let t=clamp((abs(u-0.5)*2.0-p.color.y)/max(1.0-p.color.y,0.001),0.0,1.0);let m=t*t*(3.0-2.0*t);
        let a=mix(base.a,soft.a,m);var colour=base.rgb;
        if m>0.0 {colour=vec3<f32>(0.0);if a>0.000001 {colour=mix(base.rgb*base.a,soft.rgb*soft.a,m)/a;}}
        store_rgba(i,round(clamp(vec4<f32>(colour,a),vec4<f32>(0.0),vec4<f32>(1.0))*255.0)/255.0);
    } else if p.op==30u {
        store_rgba(i,over(load_rgba((y+p.y0)*p.frame_width+x+p.x0),load_dst(i)));
    } else if p.op==26u {
        let sx=i32(x)-i32(p.x0);let sy=i32(y)-i32(p.x0);
        var value=0.0;if sx>=0 && sy>=0 && sx<i32(p.row0.x) && sy<i32(p.row0.y) {value=bitcast<f32>(input[u32(sy)*u32(p.row0.x)+u32(sx)]);}
        output[i]=bitcast<u32>(value);
    } else if p.op==27u {
        var value=0.0;
        for(var d=-p.offset_x;d<=p.offset_x;d+=1) {
            let k=u32(abs(d));let weight=bitcast<f32>(p.stops[k/4u][k%4u]);
            var at=i;
            if p.offset_y==0 {at=y*p.width+u32(clamp(i32(x)+d,0,i32(p.width)-1));}
            else {at=u32(clamp(i32(y)+d,0,i32(p.height)-1))*p.width+x;}
            value+=bitcast<f32>(input[at])*weight;
        }
        if p.offset_y==1 {value=floor(clamp(value,0.0,255.0)+0.5);}
        output[i]=bitcast<u32>(value);
    } else if p.op==28u {
        var lines=p.height;var len=p.width;var step=1u;var stride=p.width;
        if p.offset_y==1 {lines=p.width;len=p.height;step=p.width;stride=1u;}
        if i>=lines {return;}
        let base=i*stride;let radius=u32(p.offset_x);var sum=0.0;
        for(var j=0u;j<=min(radius,len-1u);j+=1u) {sum+=bitcast<f32>(input[base+j*step]);}
        let norm=1.0/f32(2u*radius+1u);
        for(var j=0u;j<len;j+=1u) {
            var value=sum*norm;if p.x0==1u {value=floor(clamp(value,0.0,255.0)+0.5);}
            output[base+j*step]=bitcast<u32>(value);
            if j+radius+1u<len {sum+=bitcast<f32>(input[base+(j+radius+1u)*step]);}
            if j>=radius {sum-=bitcast<f32>(input[base+(j-radius)*step]);}
        }
    } else if p.op==29u {
        let px=i32(x)+p.offset_x;let py=i32(y)+p.offset_y;
        if px<0 || py<0 || px>=i32(p.frame_width) || py>=i32(p.y0) || f32(py)<p.row1.x || f32(py)>=p.row1.y {return;}
        var value=0.0;
        if p.row1.w>1.5 {
            let uv=vec2<f32>(f32(x)-f32(p.width-1u)*0.5,f32(y)-f32(p.height-1u)*0.5);
            let src=vec2<f32>(uv.x*p.row0.z+uv.y*p.row0.w,-uv.x*p.row0.w+uv.y*p.row0.z)+p.row0.xy*0.5;
            value=glyph_at(sign(src)*floor(abs(src)+vec2<f32>(0.5)));
        } else if p.row1.w>0.5 {
            let uv=vec2<f32>(f32(x)+0.5-f32(p.width)*0.5,f32(y)+0.5-f32(p.height)*0.5);
            let src=vec2<f32>(uv.x*p.row0.z+uv.y*p.row0.w,uv.y*p.row0.z-uv.x*p.row0.w)+p.row0.xy*0.5-vec2<f32>(0.5);
            let lo=floor(src);let t=src-lo;
            value=glyph_at(lo)*(1.0-t.x)*(1.0-t.y)+glyph_at(lo+vec2<f32>(1.0,0.0))*t.x*(1.0-t.y)+glyph_at(lo+vec2<f32>(0.0,1.0))*(1.0-t.x)*t.y+glyph_at(lo+vec2<f32>(1.0,1.0))*t.x*t.y;
            value=floor(clamp(value,0.0,255.0)+0.5);
        } else {value=bitcast<f32>(input[i]);}
        let alpha=floor(clamp(value*p.row1.z,0.0,255.0)+0.5)/255.0;
        let dst=u32(py)*p.frame_width+u32(px);
        glyph_store(dst,alpha,px,py);
    } else if p.op==24u {
        store_rgba(i,load_rgba((y+p.y0)*p.frame_width+x+p.x0));
    } else if p.op==23u {
        let pos=vec2<u32>(x+p.x0,y+p.y0);
        let uvp=bitcast<vec4<f32>>(p.stops[3]);let extra=bitcast<vec4<f32>>(p.stops[4]);
        let frame=p.width*p.height;
        let coverage=input[frame*4u+i];if coverage==0u {return;}
        let extent=max(p.color.zw-p.color.xy,vec2<f32>(1.0));
        var uv=(vec2<f32>(pos)-p.color.xy)/extent;
        let aspect=extent.x/extent.y;let texaspect=f32(p.offset_x)/f32(p.offset_y);let mode=u32(extra.z);
        if mode==2u {
            if aspect>texaspect {uv.x=(uv.x-0.5)/(texaspect/aspect)+0.5;}
            else if aspect<texaspect {uv.y=(uv.y-0.5)/(aspect/texaspect)+0.5;}
            if any(uv<vec2<f32>(0.0)) || any(uv>vec2<f32>(1.0)) {return;}
        } else if mode==3u {
            if aspect>texaspect {uv.y=(uv.y-0.5)*(aspect/texaspect)+0.5;}
            else if aspect<texaspect {uv.x=(uv.x-0.5)*(texaspect/aspect)+0.5;}
        }
        let c=uv-vec2<f32>(0.5);
        uv=vec2<f32>(0.5)+vec2<f32>(c.x*uvp.y-c.y*uvp.z,c.x*uvp.z+c.y*uvp.y);
        uv=(uv-extra.xy)*uvp.x;
        if mode==1u {uv=uv-floor(uv);} else if any(uv<vec2<f32>(0.0)) || any(uv>vec2<f32>(1.0)) {return;}
        let sample=vec2<u32>(floor(uv*vec2<f32>(f32(p.offset_x-1),f32(p.offset_y-1))+vec2<f32>(0.5)));
        let ti=frame*4u+p.width*p.height+2u*(sample.y*u32(p.offset_x)+sample.x);
        let texture=vec4<f32>(unpack2x16float(input[ti]),unpack2x16float(input[ti+1u]));
        let original=load_rgba(i);
        let authored=vec3<f32>(dot(bitcast<vec4<f32>>(p.stops[0]).xyz,original.rgb),dot(bitcast<vec4<f32>>(p.stops[1]).xyz,original.rgb),dot(bitcast<vec4<f32>>(p.stops[2]).xyz,original.rgb));
        let old=floor(clamp(vec3<f32>(encoded(authored.r),encoded(authored.g),encoded(authored.b)),vec3<f32>(0.0),vec3<f32>(1.0))*255.0+0.5);
        let tex=floor(texture.rgb*255.0+0.5)*bitcast<vec4<f32>>(p.stops[5]).rgb;
        let rgb=floor(clamp(old*(1.0-uvp.w)+tex*(f32(coverage)/255.0)*uvp.w,vec3<f32>(0.0),vec3<f32>(255.0)))/255.0;
        store_rgba(pos.y*p.frame_width+pos.x,vec4<f32>(matrix(vec3<f32>(linear(rgb.r),linear(rgb.g),linear(rgb.b))),f32(coverage)/255.0));
    } else if p.op==22u {
        let pos=vec2<u32>(x+p.x0,y+p.y0);
        if i32(pos.x)>=p.offset_x {return;}
        let coverage=round(clamp(load_rgba(pos.y*p.frame_width+pos.x).a,0.0,1.0)*255.0);
        let c=fill_color(vec2<f32>(pos));
        let alpha=floor(coverage*round(c.a*255.0)/255.0)/255.0;
        store_rgba(i,over(vec4<f32>(c.rgb,alpha),load_dst(i)));
    } else if p.op==19u {
        if i>=p.height {return;}
        var lo=p.width; var hi=0u;
        for(var col=0u;col<p.width;col+=1u) {
            let v=load_rgba(i*p.width+col);
            store_rgba(i*p.width+col,v);
            if v.a>0.0 {lo=min(lo,col);hi=max(hi,col);}
        }
        store_rgba(p.width*p.height+i,vec4<f32>(f32(lo),f32(i),f32(hi),f32(i)));
    } else if p.op==20u {
        if i!=0u {return;}
        var bounds=vec4<f32>(f32(p.width),f32(p.height),0.0,0.0);
        for(var row=0u;row<p.height;row+=1u) {
            let b=load_dst(p.width*p.height+row);
            if b.x<=b.z {bounds=vec4<f32>(min(bounds.xy,b.xy),max(bounds.zw,b.zw));}
        }
        store_rgba(p.width*p.height+p.height,bounds);
    } else if p.op==21u {
        let b=load_rgba(p.width*p.height+p.height);
        let extent=b.zw-b.xy+vec2<f32>(1.0);
        let radius=ceil(sqrt(dot(extent,extent)));
        let pos=vec2<f32>(f32(x),f32(y));
        let d=pos-p.color.xy;
        let source=p.color.xy+vec2<f32>(d.x*p.color.w+d.y*p.color.z,-d.x*p.color.z+d.y*p.color.w);
        // Rust f32::round resolves half-way values away from zero.
        let sample=sign(source)*floor(abs(source)+vec2<f32>(0.5));
        var v=vec4<f32>(0.0);
        if all(b.xy<=b.zw) && all(pos>=floor(p.color.xy)-vec2<f32>(radius)) && all(pos<=ceil(p.color.xy)+vec2<f32>(radius))
            && all(sample>=b.xy) && all(sample<=b.zw) {
            let s=load_rgba(u32(sample.y)*p.width+u32(sample.x));
            if s.a>0.0 {v=s;}
        }
        store_rgba(i,v);
    } else if p.op==17u {
        let pos=vec2<f32>(f32(x+p.x0),f32(y+p.y0));
        let inside=all(pos>=p.row0.xy)&&all(pos<p.row0.xy+p.row0.zw);
        store_rgba(i,select(vec4<f32>(0.0),p.color,inside));
    } else if p.op==18u {
        let inside=x>=p.x0 && y>=p.y0 && x-p.x0<u32(p.offset_x) && y-p.y0<u32(p.offset_y);
        let rgb=matrix(vec3<f32>(linear(p.color.x),linear(p.color.y),linear(p.color.z)));
        store_rgba(i,select(vec4<f32>(0.0),vec4<f32>(rgb,p.color.a),inside));
    } else if p.op==15u {
        let dst=(y+p.y0)*p.frame_width+x+p.x0;
        if p.offset_x==1 {store_rgba(dst,over(p.color,load_dst(dst)));}else{store_rgba(dst,p.color);}
    } else if p.op==14u || p.op==25u {
        let px=i32(x)+p.offset_x; let py=i32(y)+p.offset_y;
        if p.op==14u {
            if px<0 || py<0 || px>=i32(p.frame_width) || py>=i32(p.y0) {return;}
            if f32(py)<p.row1.x || f32(py)>=p.row1.y {return;}
        }
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
                if bins.w>0.5 {
                    let at=u32(range.x)+3u*j;
                    let ab=load_rgba(at);let cd=load_rgba(at+1u);let degree=u32(load_rgba(at+2u).x);
                    var lo=min(ab.y,ab.w);
                    if degree>=2u {lo=min(lo,cd.y);}
                    if degree>=3u {lo=min(lo,cd.w);}
                    let edge_first=u32(clamp(floor((max(low,lo)-bins.x)/bins.y*bins.z),0.0,bins.z-1.0));
                    if bin==edge_first {coverage+=curve_coverage(at,pixel);}
                } else {
                    let edge=load_rgba(u32(range.x)+j);
                    let edge_first=u32(clamp(floor((max(low,min(edge.y,edge.w))-bins.x)/bins.y*bins.z),0.0,bins.z-1.0));
                    if bin==edge_first {coverage+=edge_coverage(edge,pixel);}
                }
            }
        }
        var alpha=floor(clamp(abs(coverage)*255.9,0.0,255.0))/255.0;
        if bins.w>0.5 {alpha=floor(clamp(abs(coverage),0.0,1.0)*255.0+0.5)/255.0;}
        if p.op==25u {output[i]=bitcast<u32>(round(alpha*255.0));return;}
        alpha=floor(round(alpha*255.0)*p.row1.z+0.5)/255.0;
        let dst=u32(py)*p.frame_width+u32(px);
        glyph_store(dst,alpha,px,py);
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
        if p.row1.x>0.5 {
            let u=select(uv.x,uv.y,p.row1.x>1.5);
            a*=1.0-smoothstep(p.row1.y,1.0,abs(u-0.5)*2.0)*(1.0-p.row1.z);
        }
        var alpha=v.a;
        if a<1.0 { alpha=floor(round(alpha*255.0)*a+0.5)/255.0; }
        if p.row0.y>0.5 {
            if abs(p.row0.x-1.0)>=0.000001 {
                // CPU BoxRaster stores byte/255 as f16 before posing. For
                // nonzero bytes these are normal f16 values; discard 13 f32
                // mantissa bits with ties-to-even rounding. Integer rounding
                // prevents drivers eliding a pack2x16float/unpack round-trip.
                let bits=bitcast<u32>(round(alpha*255.0)/255.0);
                let half_alpha=bitcast<f32>((bits+4095u+((bits>>13u)&1u))&0xffffe000u);
                alpha=floor(half_alpha*p.row0.x*255.0+0.5)/255.0;
            }
        } else if abs(p.row0.x-1.0)>=0.001 { alpha=floor(alpha*p.row0.x*255.0+0.5)/255.0; }
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
    } else if p.op==54u {
        // An image layer fitted into its box, as `paint_image_layer` paints
        // it: in `color` the drawn rectangle's left and top and the picture's
        // pixels per frame pixel across and down, the
        // covered pixels in `row0` (x0, y0, x1, y1), the picture's size in
        // the offsets; sRGB-encoded straight alpha, rounded to bytes.
        let fx=f32(x);let fy=f32(y);
        if fx<p.row0.x || fy<p.row0.y || fx>=p.row0.z || fy>=p.row0.w {store_rgba(i,vec4<f32>(0.0));return;}
        let v=((fy+0.5)-p.color.y)*p.color.w-0.5;
        let u=((fx+0.5)-p.color.x)*p.color.z-0.5;
        let s=image_sample(u,v);
        let a=clamp(round_away(s.a),0.0,255.0);
        if a<=0.0 {store_rgba(i,vec4<f32>(0.0));return;}
        store_rgba(i,vec4<f32>(clamp(round_away3(s.rgb),vec3<f32>(0.0),vec3<f32>(255.0))/255.0,a/255.0));
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
        output[i]=select(u32(round(clamp(load_rgba((y+p.y0)*p.frame_width+x+p.x0).a,0.0,1.0)*255.0)),0u,i32(x+p.x0)>=p.offset_x);
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
        let face=vec4<f32>(encoded(rgb.x),encoded(rgb.y),encoded(rgb.z),select(a,0.0,i32(x+p.x0)>=p.offset_x));
        store_rgba(i,over(face,load_dst(i)));
    } else if p.op==7u { // Final title quantization and working-space conversion.
        let v=floor(clamp(load_rgba(i),vec4<f32>(0.0),vec4<f32>(1.0))*255.0+0.5)/255.0;
        let rgb=matrix(vec3<f32>(linear(v.x),linear(v.y),linear(v.z)));
        store_rgba((y+p.y0)*p.frame_width+x+p.x0,vec4<f32>(rgb,v.a));
    }
}
