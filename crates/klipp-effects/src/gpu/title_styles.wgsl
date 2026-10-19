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
    // The reference ramp uses 4097 quantized positions.
    t=floor(clamp(t,0.0,1.0)*4096.0+0.5)/4096.0;
    let count=u32(p.color.x);
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
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x;
    if i>=p.width*p.height { return; }
    let x=i%p.width; let y=i/p.width;
    if p.op==30u {
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
        if p.row1.w>0.5 {
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
