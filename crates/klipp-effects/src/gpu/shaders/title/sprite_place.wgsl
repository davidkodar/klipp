//#include pixel_storage

struct Params {
    width: u32, height: u32, y0: u32, y1: u32,
    dx: i32, dy: i32, opacity: f32, scale: f32,
    anchor_x: f32, anchor_y: f32, under_r: f32, under_g: f32,
    under_b: f32, under_a: f32, focus_axis: u32, band: f32,
    edge_opacity: f32, fade_top: f32, fade_bottom: f32, fade_left: f32,
    fade_right: f32, bx0: u32, by0: u32, bw: u32,
    bh: u32, over: u32, _pad1: u32, _pad2: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
    a0: vec4<f32>, a1: vec4<f32>, a2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> sprite: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb_c(c: f32) -> f32 {
    if (c <= 0.0031308) { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// The layer's fade by where it is on the frame (#592):
// `klipp_core::title_motion::ScreenFade`, its factor down the frame
// (`along_y`) times its factor across it (`along_x`). A focus band runs
// down the frame on axis 1, across it on axis 2; 0 is none.
fn focus_factor(u: f32) -> f32 {
    let d = abs(u - 0.5) * 2.0;
    return 1.0 - smoothstep(params.band, 1.0, d) * (1.0 - params.edge_opacity);
}
fn shape_alpha(x: i32, y: i32) -> f32 {
    let uy = (f32(y) + 0.5) / f32(params.height);
    let ux = (f32(x) + 0.5) / f32(params.width);
    var ay = 1.0;
    if params.fade_top > 0.0 { ay = ay * smoothstep(0.0, params.fade_top, uy); }
    if params.fade_bottom > 0.0 { ay = ay * smoothstep(0.0, params.fade_bottom, 1.0 - uy); }
    if params.focus_axis == 1u { ay = ay * focus_factor(uy); }
    var ax = 1.0;
    if params.fade_left > 0.0 { ax = ax * smoothstep(0.0, params.fade_left, ux); }
    if params.fade_right > 0.0 { ax = ax * smoothstep(0.0, params.fade_right, 1.0 - ux); }
    if params.focus_axis == 2u { ax = ax * focus_factor(ux); }
    return ay * ax;
}

// The sprite at frame pixel (x, y), straight: colour in sRGB-encoded
// 0..255 units, alpha 0..1; nothing outside its box (#190).
fn texel(x: i32, y: i32) -> vec4<f32> {
    let bx = x - i32(params.bx0);
    let by = y - i32(params.by0);
    if bx < 0 || by < 0 || bx >= i32(params.bw) || by >= i32(params.bh) {
        return vec4<f32>(0.0);
    }
    let i = u32(by) * params.bw + u32(bx);
    let rg = unpack2x16float(sprite[i * 2u]);
    let ba = unpack2x16float(sprite[i * 2u + 1u]);
    return vec4<f32>(rg.x * 255.0, rg.y * 255.0, ba.x * 255.0, ba.y);
}

// A tap of the sprite, premultiplied, in its sRGB-encoded 0..255 units
// (alpha 0..1).
fn tap(x: i32, y: i32) -> vec4<f32> {
    let t = texel(x, y);
    return vec4<f32>(t.xyz * t.w, t.w);
}

// CPU image::resize Triangle: vertical float pass, horizontal pass rounded
// to byte premultiplied RGBA, then integer unpremultiplication.
fn resized_colour(x:i32,y:i32)->vec4<f32> {
    if x<0 || y<0 || x>=i32(params._pad1) || y>=i32(params._pad2) {return vec4<f32>(0.0);}
    let ratio=vec2<f32>(f32(params.bw)/f32(params._pad1),f32(params.bh)/f32(params._pad2));
    let support=max(ratio,vec2<f32>(1.0));
    let centre=(vec2<f32>(f32(x),f32(y))+0.5)*ratio;
    let left=clamp(vec2<i32>(floor(centre-support)),vec2<i32>(0),vec2<i32>(i32(params.bw)-1,i32(params.bh)-1));
    let right=clamp(vec2<i32>(ceil(centre+support)),left+1,vec2<i32>(i32(params.bw),i32(params.bh)));
    var wx=0.0;var wy=0.0;
    for(var sx=left.x;sx<right.x;sx=sx+1) {wx+=max(1.0-abs((f32(sx)-(centre.x-0.5))/support.x),0.0);}
    for(var sy=left.y;sy<right.y;sy=sy+1) {wy+=max(1.0-abs((f32(sy)-(centre.y-0.5))/support.y),0.0);}
    var total=vec4<f32>(0.0);
    for(var sx=left.x;sx<right.x;sx=sx+1) {
        var vertical=vec4<f32>(0.0);
        for(var sy=left.y;sy<right.y;sy=sy+1) {
            let t=texel(sx,sy);
            let bytes=round(vec4<f32>(t.rgb,t.a*255.0));
            let weight=max(1.0-abs((f32(sy)-(centre.y-0.5))/support.y),0.0)/wy;
            vertical+=bytes*weight;
        }
        let weight=max(1.0-abs((f32(sx)-(centre.x-0.5))/support.x),0.0)/wx;
        total+=vertical*weight;
    }
    let pre=floor(clamp(total,vec4<f32>(0.0),vec4<f32>(255.0))+0.5);
    var rgb=pre.rgb;
    if pre.a>0.0 {rgb=min(floor((pre.rgb*255.0+floor(pre.a/2.0))/pre.a),vec3<f32>(255.0));}
    return vec4<f32>(rgb,pre.a/255.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    var rgb = vec3<f32>(0.0);
    var a8 = 0.0;
    if params._pad1>0u && params._pad2>0u {
        let t=resized_colour(x-params.dx,y-params.dy);
        rgb=t.rgb;a8=t.a*255.0;
    } else if params.scale == 1.0 {
        // Unscaled: the pixel as it is.
        let t = texel(x - params.dx, y - params.dy);
        rgb = t.xyz;
        a8 = t.w * 255.0;
    } else {
        // The sprite scaled about the anchor: the source position of
        // this pixel's centre, sampled bilinearly, as the CPU twin does.
        let fx = (f32(x) + 0.5 - f32(params.dx) - params.anchor_x) / params.scale + params.anchor_x - 0.5;
        let fy = (f32(y) + 0.5 - f32(params.dy) - params.anchor_y) / params.scale + params.anchor_y - 0.5;
        let x0f = floor(fx);
        let y0f = floor(fy);
        let tx = fx - x0f;
        let ty = fy - y0f;
        let x0 = i32(x0f);
        let y0 = i32(y0f);
        let s = tap(x0, y0) * ((1.0 - tx) * (1.0 - ty))
          + tap(x0 + 1, y0) * (tx * (1.0 - ty))
          + tap(x0, y0 + 1) * ((1.0 - tx) * ty)
          + tap(x0 + 1, y0 + 1) * (tx * ty);
        if s.w > 0.0 {
            rgb = s.xyz / s.w;
        }
        a8 = s.w * 255.0;
    }
    // A posed glyph's byte coverage scaled by its alpha and rounded half
    // away from zero, as the CPU rasteriser scales it (#2203): the byte
    // recovered first, so 4/255 read back as 3.9999998 still scales from 4.
    let posed = params.over >= 2u && params.opacity < 1.0;
    if posed {
        a8 = round(a8);
    }
    // Unrounded (#190), as the CPU twin: the opacity and the fade
    // applied; scaled, no colour under an alpha that rounds to nothing.
    a8 = clamp(a8 * params.opacity * shape_alpha(x, y), 0.0, 255.0);
    if posed {
        let whole = floor(a8);
        a8 = whole + select(0.0, 1.0, a8 - whole >= 0.5);
    }
    if params.scale != 1.0 && a8 < 0.5 {
        rgb = vec3<f32>(0.0);
    }
    if params.over == 2u {
        let old=unpack(output[idx]);
        let alpha=max(old.a,a8/255.0);
        output[idx]=pack(vec4<f32>(to_ws(vec3<f32>(params.under_r,params.under_g,params.under_b)),alpha));
        return;
    }
    if params.over == 3u {
        // Recover byte coverage from half-float sprite storage before the
        // saturating subtraction. Do not multiply by the text fill alpha.
        let old=unpack(output[idx]);
        let alpha=max(round(old.a*255.0)-round(a8),0.0)/255.0;
        output[idx]=pack(vec4<f32>(old.rgb,alpha));
        return;
    }
    if params.over == 1u || params.over == 4u {
        // Over what the layer holds, as the title renderer lays its
        // layers (`over_blend`): straight alpha, in sRGB-encoded units.
        // Undo the working-space matrix before authored-space blending.
        let sa = a8 / 255.0;
        if sa <= 0.0 { return; }
        let d = unpack(output[idx]);
        let da = clamp(d.w, 0.0, 1.0);
        let out_a = sa + da * (1.0 - sa);
        let authored = vec3<f32>(dot(params.a0.xyz, d.xyz), dot(params.a1.xyz, d.xyz), dot(params.a2.xyz, d.xyz));
        let dc = vec3<f32>(linear_to_srgb_c(max(authored.x, 0.0)), linear_to_srgb_c(max(authored.y, 0.0)), linear_to_srgb_c(max(authored.z, 0.0))) * 255.0;
        let c = (rgb * sa + dc * da * (1.0 - sa)) / out_a;
        let lc = vec3<f32>(srgb_to_linear_c(c.x / 255.0), srgb_to_linear_c(c.y / 255.0), srgb_to_linear_c(c.z / 255.0));
        output[idx] = pack(vec4<f32>(to_ws(lc), out_a));
        return;
    }
    let lin = vec3<f32>(srgb_to_linear_c(rgb.x / 255.0), srgb_to_linear_c(rgb.y / 255.0), srgb_to_linear_c(rgb.z / 255.0));
    if params.under_a <= 0.0 {
        output[idx] = pack(vec4<f32>(to_ws(lin), a8 / 255.0));
        return;
    }
    // Over the full-frame colour beneath, in linear light.
    let sa = a8 / 255.0;
    let ua = params.under_a * (1.0 - sa);
    let oa = sa + ua;
    var o = vec3<f32>(0.0);
    if oa > 0.0 {
        o = (lin * sa + vec3<f32>(params.under_r, params.under_g, params.under_b) * ua) / oa;
    }
    output[idx] = pack(vec4<f32>(to_ws(o), oa));
}
