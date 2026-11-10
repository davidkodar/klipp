
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    small_w: u32, small_h: u32, radius: u32, mode: u32,
    luma_opacity: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
//#include pixel_storage

const TARGET: f32 = 4.5;
const LIGHT_TEXT: f32 = 0.179;
// The share of a title's ink that must be light text for the title to be
// light text (protect::LIGHT_SHARE).
const LIGHT_SHARE: f32 = 0.1;
// The least share that counts by the light ink's need (protect::MIN_LIGHT_SHARE).
const MIN_LIGHT_SHARE: f32 = 0.02;
// A cell covered this much is solid ink, no edge of it (protect::SOLID).
const SOLID: f32 = 0.98;
const HALO_FULL: f32 = 0.04;

fn luma(c: vec3<f32>) -> f32 {
    return clamp(dot(c, p.luma_opacity.xyz), 0.0, 1.0);
}

// The light text's darkening `k` and the dark text's lightening `t` a
// cell's measures ask for (protect::pull).
fn pull_k_in(lb: f32, busy: f32, lt: f32, mode: u32) -> f32 {
    let need = (lt + 0.05) / TARGET - 0.05;
    let cur = min(lb + busy, 1.0);
    var k = 1.0;
    if cur > need { k = clamp(max(need, 0.0) / cur, 0.12, 1.0); }
    if mode == 2u { k = min(k, 0.55); }
    return k;
}

fn pull_t_in(lb: f32, busy: f32, lt: f32, mode: u32) -> f32 {
    let need = TARGET * (lt + 0.05) - 0.05;
    let cur = max(lb - busy, 0.0);
    var t = clamp((need - cur) / max(1.0 - cur, 1e-4), 0.0, 0.88);
    if mode == 2u { t = max(t, 0.45); }
    return t;
}

fn pull_k(lb: f32, busy: f32, lt: f32) -> f32 { return pull_k_in(lb, busy, lt, p.mode); }
fn pull_t(lb: f32, busy: f32, lt: f32) -> f32 { return pull_t_in(lb, busy, lt, p.mode); }

var<workgroup> wk: array<vec4<f32>, 256>;
var<workgroup> wany: array<vec2<f32>, 256>;
var<workgroup> wsum: array<vec4<f32>, 256>;
var<workgroup> wall: array<vec4<f32>, 256>;
var<workgroup> wneed: array<vec4<f32>, 256>;
var<workgroup> wlt: f32;
var<workgroup> wlight: f32;

fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[u32(cy) * p.small_w + u32(cx)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if p.op == 5u {
        // One workgroup: the whole title's pull (protect::title_pull), the
        // hardest part's need, light text's least k and dark text's
        // greatest t, over the cells the halo covers whole (xy) or, when
        // none is, over any the text reaches (zw), into the cell after the
        // grid as (k, t).
        let n = p.small_w * p.small_h;
        // First whether the whole title is light or dark text, and the
        // luminance its need is for (protect::title_tone), from the ink
        // that meets the picture (cells under SOLID; all, when every cell
        // is solid): light when the cells reading as light text hold
        // LIGHT_SHARE of it, or MIN_LIGHT_SHARE and need more protecting
        // than the dark ink, at their luminance; else dark, at the dark
        // ink's. Edge sums (x lt, y coverage, z light lt, w light
        // coverage), the same over all ink, and the needs (light, dark;
        // edge then all).
        var sum = vec4<f32>(0.0);
        var all = vec4<f32>(0.0);
        var need = vec4<f32>(0.0);
        for (var c = li; c < n; c += 256u) {
            let m = unpack(output[c]);
            if m.z <= 1e-3 { continue; }
            let own = clamp(m.w / m.z, 0.0, 1.0);
            let busy = sqrt(max(m.y - m.x * m.x, 0.0));
            let is_light = m.w / m.z >= LIGHT_TEXT;
            var tone = vec4<f32>(m.w, m.z, 0.0, 0.0);
            var nd = vec2<f32>(0.0);
            if is_light {
                tone = vec4<f32>(m.w, m.z, m.w, m.z);
                nd = vec2<f32>((1.0 - pull_k_in(m.x, busy, own, 1u)) * m.z, 0.0);
            } else {
                nd = vec2<f32>(0.0, pull_t_in(m.x, busy, own, 1u) * m.z);
            }
            all += tone;
            need += vec4<f32>(0.0, 0.0, nd);
            if m.z < SOLID {
                sum += tone;
                need += vec4<f32>(nd, 0.0, 0.0);
            }
        }
        wsum[li] = sum;
        wall[li] = all;
        wneed[li] = need;
        workgroupBarrier();
        for (var half = 128u; half > 0u; half >>= 1u) {
            if li < half {
                wsum[li] += wsum[li + half];
                wall[li] += wall[li + half];
                wneed[li] += wneed[li + half];
            }
            workgroupBarrier();
        }
        if li == 0u {
            var t = wsum[0];
            var nd = wneed[0].xy;
            if t.y <= 0.0 {
                t = wall[0];
                nd = wneed[0].zw;
            }
            var t_lt = 1.0;
            var t_light = 1.0;
            if t.y > 0.0 {
                let dark_cov = max(t.y - t.w, 1e-6);
                let needs_more = t.w >= MIN_LIGHT_SHARE * t.y && t.w > 0.0 && nd.x / t.w > nd.y / dark_cov;
                if t.w >= LIGHT_SHARE * t.y || needs_more {
                    t_lt = clamp(t.z / t.w, 0.0, 1.0);
                } else {
                    t_light = 0.0;
                    t_lt = clamp((t.x - t.z) / dark_cov, 0.0, 1.0);
                }
            }
            wlt = t_lt;
            wlight = t_light;
        }
        workgroupBarrier();
        let lt = wlt;
        let light = wlight > 0.5;
        var acc = vec4<f32>(1.0, 0.0, 1.0, 0.0);
        var seen = vec2<f32>(0.0);
        for (var c = li; c < n; c += 256u) {
            let m = unpack(output[c]);
            if m.z <= 1e-3 { continue; }
            let busy = sqrt(max(m.y - m.x * m.x, 0.0));
            let full = m.z >= HALO_FULL;
            if light {
                let k = pull_k(m.x, busy, lt);
                acc.z = min(acc.z, k);
                if full { acc.x = min(acc.x, k); }
            } else {
                let t = pull_t(m.x, busy, lt);
                acc.w = max(acc.w, t);
                if full { acc.y = max(acc.y, t); }
            }
            seen.y = 1.0;
            if full { seen.x = 1.0; }
        }
        wk[li] = acc;
        wany[li] = seen;
        workgroupBarrier();
        for (var half = 128u; half > 0u; half >>= 1u) {
            if li < half {
                let o = wk[li + half];
                let a = wk[li];
                wk[li] = vec4<f32>(min(a.x, o.x), max(a.y, o.y), min(a.z, o.z), max(a.w, o.w));
                wany[li] = max(wany[li], wany[li + half]);
            }
            workgroupBarrier();
        }
        if li == 0u {
            let r = wk[0];
            var kt = vec2<f32>(1.0, 0.0);
            if wany[0].x > 0.5 { kt = r.xy; } else if wany[0].y > 0.5 { kt = r.zw; }
            // The pull, and the title light (1) or dark (0) text.
            output[n] = pack(vec4<f32>(kt, select(0.0, 1.0, light), 0.0));
        }
        return;
    }
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if p.op == 4u {
        if i >= p.width * p.height { return; }
        let x = i % p.width; let y = i / p.width;
        let u = (f32(x) + 0.5) / f32(p.scale) - 0.5;
        let v = (f32(y) + 0.5) / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let m = mix(mix(small_at(x0, y0), small_at(x0 + 1, y0), fx), mix(small_at(x0, y0 + 1), small_at(x0 + 1, y0 + 1), fx), fy);
        let a = m.z;
        if a <= 1e-3 { return; }
        let ht = clamp(a / HALO_FULL, 0.0, 1.0);
        let halo = ht * ht * (3.0 - 2.0 * ht);
        // The whole title's pull, and whether it is light text, from op 5,
        // evenly through the halo.
        let title = unpack(input[p.small_w * p.small_h]);
        let kt = title.xy;
        var px = unpack(output[i]);
        if title.z > 0.5 {
            px = vec4<f32>(px.xyz * (1.0 - halo * (1.0 - kt.x)), px.w);
        } else {
            px = vec4<f32>(px.xyz + (vec3<f32>(1.0) - px.xyz) * halo * kt.y, px.w);
        }
        output[i] = pack(px);
        return;
    }
    if i >= p.small_w * p.small_h { return; }
    let sx = i % p.small_w; let sy = i / p.small_w;
    if p.op <= 1u {
        // 0: the picture's luminance and its square; 1: the layer's
        // coverage and luminance times coverage, beside them.
        var sum = vec2<f32>(0.0);
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                if p.op == 0u {
                    let lb = luma(c.xyz) * clamp(c.w, 0.0, 1.0);
                    sum += vec2<f32>(lb, lb * lb);
                } else {
                    let a = clamp(c.w * p.luma_opacity.w, 0.0, 1.0);
                    sum += vec2<f32>(a, luma(c.xyz) * a);
                }
                n += 1.0;
            }
        }
        let mean = sum / max(n, 1.0);
        if p.op == 0u {
            output[i] = pack(vec4<f32>(mean, 0.0, 0.0));
        } else {
            let was = unpack(output[i]);
            output[i] = pack(vec4<f32>(was.xy, mean));
        }
    } else if p.op == 2u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx) + d, i32(sy)); }
        output[i] = pack(sum / f32(2 * r + 1));
    } else if p.op == 3u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx), i32(sy) + d); }
        output[i] = pack(sum / f32(2 * r + 1));
    }
}
