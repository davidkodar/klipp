//! The planar tracker's shaders (#2754): each a mirror of its CPU
//! reference in `crate::planar`, so the two agree within float rounding
//! and the histogram's resolution (the robust scale is a histogram
//! median here, an exact one there).

/// Partial sums each reduction's workgroups write, and how many of them.
pub(super) const GROUPS: u32 = 64;
/// Threads in a reduction's workgroup.
pub(super) const THREADS: u32 = 64;
/// Bins of the residual and strength histograms.
pub(super) const BINS: u32 = 1024;
/// Floats a template point holds: u, the two values, the two channels'
/// eight steepest-descent terms, the two strengths.
pub(super) const POINT: u32 = 22;
/// Floats the alignment's state holds: the map (9), the light (8), the
/// cut, the conditioning, a failure flag, whether the level has
/// converged, and spare.
pub(super) const STATE: u32 = 24;
/// Partial sums of the light fit, per workgroup: the brightness's 4×4
/// (10) and right side (4), the edges' 2×2 (3) and right side (2), the
/// weight.
pub(super) const LIGHT: u32 = 20;
/// Partial sums of the normal equations, per workgroup: the 8×8's upper
/// half (36) and the right side (8).
pub(super) const NORMAL: u32 = 44;

/// The parameters every pass takes, as a uniform.
const PARAMS: &str = "
struct P {
    w: u32,
    h: u32,
    n: u32,
    wgs: u32,
    // The region: its centre and half-size in the finest pixels, the
    // level's scale, and which level.
    cx: f32,
    cy: f32,
    s: f32,
    k: f32,
    // The coarse search's radius and the point step it reads.
    radius: i32,
    step: u32,
    shift_x: f32,
    shift_y: f32,
}
";

/// Between pixels, bilinearly; `ok` false outside the level.
const SAMPLE: &str = "
fn chan_at(c: u32, x: i32, y: i32) -> f32 {
    let xi = clamp(x, 0, i32(p.w) - 1);
    let yi = clamp(y, 0, i32(p.h) - 1);
    let i = u32(yi) * p.w + u32(xi);
    if c == 0u { return c0[i]; }
    return c1[i];
}

struct Sampled { v: f32, ok: bool }

fn sample(c: u32, x: f32, y: f32) -> Sampled {
    if !(x >= 0.0 && y >= 0.0 && x <= f32(p.w - 1u) && y <= f32(p.h - 1u)) {
        return Sampled(0.0, false);
    }
    let x0 = i32(floor(x));
    let y0 = i32(floor(y));
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let top = chan_at(c, x0, y0) * (1.0 - fx) + chan_at(c, x0 + 1, y0) * fx;
    let bottom = chan_at(c, x0, y0 + 1) * (1.0 - fx) + chan_at(c, x0 + 1, y0 + 1) * fx;
    return Sampled(top * (1.0 - fy) + bottom * fy, true);
}

// The map, row by row, from the state.
fn map_point(u: vec2<f32>) -> vec3<f32> {
    let w = state[6] * u.x + state[7] * u.y + state[8];
    if w <= 1.0e-12 { return vec3<f32>(0.0, 0.0, -1.0); }
    return vec3<f32>((state[0] * u.x + state[1] * u.y + state[2]) / w, (state[3] * u.x + state[4] * u.y + state[5]) / w, 1.0);
}
";

/// Each pass's source, by label.
pub(super) fn sources() -> Vec<(&'static str, String)> {
    vec![
        // The frame's brightness, from bytes four to a word.
        (
            "planar_luma",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> bytes: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    if gid.x >= p.w || gid.y >= p.h {{ return; }}
    let i = gid.y * p.w + gid.x;
    out[i] = f32((bytes[i / 4u] >> (8u * (i % 4u))) & 255u) / 255.0;
}}
"
            ),
        ),
        // Blurred down the rows and every other row taken: `halve`'s first pass.
        (
            "planar_halve_rows",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// p.w × p.h is the source; the out is p.w × ceil(p.h / 2).
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let h2 = (p.h + 1u) / 2u;
    if gid.x >= p.w || gid.y >= h2 {{ return; }}
    var kk = array<f32, 5>(1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0);
    var sum = 0.0;
    for (var k = 0; k < 5; k++) {{
        let y = clamp(2 * i32(gid.y) + k - 2, 0, i32(p.h) - 1);
        sum += kk[k] * src[u32(y) * p.w + gid.x];
    }}
    out[gid.y * p.w + gid.x] = sum;
}}
"
            ),
        ),
        // Then across the columns and every other column taken.
        (
            "planar_halve_cols",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// p.w × p.h is the source (already halved down); the out is ceil(p.w / 2) × p.h.
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let w2 = (p.w + 1u) / 2u;
    if gid.x >= w2 || gid.y >= p.h {{ return; }}
    var kk = array<f32, 5>(1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0);
    var sum = 0.0;
    for (var k = 0; k < 5; k++) {{
        let x = clamp(2 * i32(gid.x) + k - 2, 0, i32(p.w) - 1);
        sum += kk[k] * src[gid.y * p.w + u32(x)];
    }}
    out[gid.y * w2 + gid.x] = sum;
}}
"
            ),
        ),
        // The strength of the edges, central differences: `edges`.
        (
            "planar_edges",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
fn at(x: i32, y: i32) -> f32 {{
    return src[u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1))];
}}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    if gid.x >= p.w || gid.y >= p.h {{ return; }}
    let x = i32(gid.x);
    let y = i32(gid.y);
    let gx = (at(x + 1, y) - at(x - 1, y)) * 0.5;
    let gy = (at(x, y + 1) - at(x, y - 1)) * 0.5;
    out[gid.y * p.w + gid.x] = sqrt(gx * gx + gy * gy);
}}
"
            ),
        ),
        // A template's points: `Template::new`, from the cells the CPU
        // chose, and a histogram of each channel's strength.
        (
            "planar_template",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> cells: array<vec2<i32>>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> points: array<f32>;
@group(0) @binding(5) var<storage, read_write> hist: array<atomic<u32>>;
fn chan_at(c: u32, x: i32, y: i32) -> f32 {{
    let i = u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
    if c == 0u {{ return c0[i]; }}
    return c1[i];
}}
// A strength's bin, on a log scale from 2^-24 to 2^40, sixteen to a power of two.
fn strength_bin(v: f32) -> u32 {{
    if v <= 0.0 {{ return 0u; }}
    return u32(clamp((log2(v) + 24.0) * 16.0, 0.0, 1023.0));
}}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {{
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n {{ return; }}
    let cell = cells[i];
    let x = cell.x;
    let y = cell.y;
    let a = (f32(x) / p.k - p.cx) / p.s;
    let b = (f32(y) / p.k - p.cy) / p.s;
    let per_u = p.s * p.k;
    let o = i * {POINT}u;
    points[o] = a;
    points[o + 1u] = b;
    for (var c = 0u; c < 2u; c++) {{
        points[o + 2u + c] = chan_at(c, x, y);
        let gx = (chan_at(c, x + 1, y) - chan_at(c, x - 1, y)) * 0.5 * per_u;
        let gy = (chan_at(c, x, y + 1) - chan_at(c, x, y - 1)) * 0.5 * per_u;
        var d0 = array<f32, 8>(a, b, 1.0, 0.0, 0.0, 0.0, -a * a, -a * b);
        var d1 = array<f32, 8>(0.0, 0.0, 0.0, a, b, 1.0, -a * b, -b * b);
        var strength = 0.0;
        for (var j = 0u; j < 8u; j++) {{
            let sd = gx * d0[j] + gy * d1[j];
            points[o + 4u + c * 8u + j] = sd;
            strength += sd * sd;
        }}
        points[o + 20u + c] = strength;
        atomicAdd(&hist[c * {BINS}u + strength_bin(strength)], 1u);
    }}
}}
"
            ),
        ),
        // Each channel's median strength from its histogram: the
        // template's `strong` threshold.
        (
            "planar_strong",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read_write> strong: array<f32>;
@compute @workgroup_size(1)
fn main() {{
    // The channels' count (the parameters read, as every pass's are
    // bound).
    let channels = 2u + min(p.n, 0u);
    for (var c = 0u; c < channels; c++) {{
        var total = 0u;
        for (var b = 0u; b < {BINS}u; b++) {{ total += atomicLoad(&hist[c * {BINS}u + b]); }}
        var seen = 0u;
        var at = 0u;
        for (var b = 0u; b < {BINS}u; b++) {{
            seen += atomicLoad(&hist[c * {BINS}u + b]);
            if seen * 2u > total {{ at = b; break; }}
        }}
        // The bin's floor, so the median point itself counts as strong.
        strong[c] = select(exp2(f32(at) / 16.0 - 24.0), 0.0, at == 0u);
        for (var b = 0u; b < {BINS}u; b++) {{ atomicStore(&hist[c * {BINS}u + b], 0u); }}
    }}
}}
"
            ),
        ),
        // Each point sampled through the map: `align_level`'s first loop.
        (
            "planar_sample",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> seen: array<vec4<f32>>;
{SAMPLE}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {{
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n || state[20] > 0.5 {{ return; }}
    let o = i * {POINT}u;
    let m = map_point(vec2<f32>(points[o], points[o + 1u]));
    if m.z < 0.0 {{ seen[i] = vec4<f32>(0.0); return; }}
    let a = sample(0u, m.x, m.y);
    let b = sample(1u, m.x, m.y);
    seen[i] = vec4<f32>(a.v, b.v, select(0.0, 1.0, a.ok && b.ok), 0.0);
}}
"
            ),
        ),
        // The light fit's sums, per workgroup, weighted by the last
        // step's trust.
        (
            "planar_light_partial",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> trust: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read_write> partial: array<f32>;
@group(0) @binding(5) var<storage, read> state: array<f32>;
var<workgroup> acc: array<f32, {}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {{
    // Converged: the rest of the level's steps do nothing.
    if state[20] > 0.5 {{ return; }}
    var s: array<f32, {LIGHT}>;
    for (var j = 0u; j < {LIGHT}u; j++) {{ s[j] = 0.0; }}
    var i = wg.x * {THREADS}u + li;
    loop {{
        if i >= p.n {{ break; }}
        let v = seen[i];
        if v.z > 0.5 {{
            let o = i * {POINT}u;
            let u = vec2<f32>(points[o], points[o + 1u]);
            let t = trust[i];
            // Brightness: rows [T, 1, u, v].
            var r0 = array<f32, 4>(points[o + 2u], 1.0, u.x, u.y);
            var k = 0u;
            for (var a = 0u; a < 4u; a++) {{
                for (var b = a; b < 4u; b++) {{ s[k] += t.x * r0[a] * r0[b]; k++; }}
            }}
            for (var a = 0u; a < 4u; a++) {{ s[10u + a] += t.x * r0[a] * v.x; }}
            // Edges: rows [T, 1].
            let e = points[o + 3u];
            s[14] += t.y * e * e;
            s[15] += t.y * e;
            s[16] += t.y;
            s[17] += t.y * e * v.y;
            s[18] += t.y * v.y;
            s[19] += t.x;
        }}
        i += {GROUPS}u * {THREADS}u;
    }}
    for (var j = 0u; j < {LIGHT}u; j++) {{ acc[li * {LIGHT}u + j] = s[j]; }}
    workgroupBarrier();
    var stride = {THREADS}u / 2u;
    loop {{
        if stride == 0u {{ break; }}
        if li < stride {{
            for (var j = 0u; j < {LIGHT}u; j++) {{ acc[li * {LIGHT}u + j] += acc[(li + stride) * {LIGHT}u + j]; }}
        }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if li == 0u {{
        for (var j = 0u; j < {LIGHT}u; j++) {{ partial[wg.x * {LIGHT}u + j] = acc[j]; }}
    }}
}}
",
                THREADS * LIGHT
            ),
        ),
        // The light solved from the sums: brightness by gain, offset and
        // slope; edges by gain and offset.
        (
            "planar_light_solve",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
{SOLVE}
var<workgroup> sums: array<f32, {LIGHT}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(local_invocation_index) li: u32) {{
    if state[20] > 0.5 {{ return; }}
    // Each sum over the groups by a thread of its own, then one solves.
    if li < {LIGHT}u {{
        var t = 0.0;
        for (var g = 0u; g < p.wgs; g++) {{ t += partial[g * {LIGHT}u + li]; }}
        sums[li] = t;
    }}
    workgroupBarrier();
    if li != 0u {{ return; }}
    var s: array<f32, {LIGHT}>;
    for (var j = 0u; j < {LIGHT}u; j++) {{ s[j] = sums[j]; }}
    // Brightness.
    var a: array<array<f32, 8>, 8>;
    var b: array<f32, 8>;
    for (var r = 0u; r < 8u; r++) {{ for (var c = 0u; c < 8u; c++) {{ a[r][c] = select(0.0, 1.0, r == c); }} b[r] = 0.0; }}
    var k = 0u;
    for (var r = 0u; r < 4u; r++) {{
        for (var c = r; c < 4u; c++) {{ a[r][c] = s[k]; a[c][r] = s[k]; k++; }}
        b[r] = s[10u + r];
    }}
    let x = solve(a, b, 4u);
    if s[19] >= 16.0 && x.ok && x.v[0] >= 0.3 && x.v[0] <= 3.0 {{
        state[9] = x.v[0]; state[10] = x.v[1]; state[11] = x.v[2]; state[12] = x.v[3];
    }} else {{
        state[9] = 1.0; state[10] = 0.0; state[11] = 0.0; state[12] = 0.0;
    }}
    // Edges.
    for (var r = 0u; r < 8u; r++) {{ for (var c = 0u; c < 8u; c++) {{ a[r][c] = select(0.0, 1.0, r == c); }} b[r] = 0.0; }}
    a[0][0] = s[14]; a[0][1] = s[15]; a[1][0] = s[15]; a[1][1] = s[16];
    b[0] = s[17]; b[1] = s[18];
    let y = solve(a, b, 2u);
    if s[16] >= 16.0 && y.ok && y.v[0] >= 0.3 && y.v[0] <= 3.0 {{
        state[13] = y.v[0]; state[14] = y.v[1];
    }} else {{
        state[13] = 1.0; state[14] = 0.0;
    }}
}}
"
            ),
        ),
        // The residuals under the light, and a histogram of the strong
        // points' sizes.
        (
            "planar_residual",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> state: array<f32>;
@group(0) @binding(4) var<storage, read_write> res: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read> strong: array<f32>;
// A residual's bin, on a square-root scale over 0 to 2.
fn residual_bin(r: f32) -> u32 {{
    return u32(clamp(sqrt(abs(r) / 2.0) * 1024.0, 0.0, 1023.0));
}}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {{
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n {{ return; }}
    let v = seen[i];
    if v.z < 0.5 || state[20] > 0.5 {{ return; }}
    let o = i * {POINT}u;
    let u = vec2<f32>(points[o], points[o + 1u]);
    let r0 = (v.x - state[10] - state[11] * u.x - state[12] * u.y) / state[9] - points[o + 2u];
    let r1 = (v.y - state[14]) / state[13] - points[o + 3u];
    res[i] = vec2<f32>(r0, r1);
    if points[o + 20u] >= strong[0] {{ atomicAdd(&hist[residual_bin(r0)], 1u); }}
    if points[o + 21u] >= strong[1] {{ atomicAdd(&hist[residual_bin(r1)], 1u); }}
}}
"
            ),
        ),
        // The median residual from the histogram: the Tukey cut. Too few
        // residuals to read a spread from fails the alignment.
        (
            "planar_cut",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
@compute @workgroup_size(1)
fn main() {{
    // The fewest residuals a spread is read from (the parameters read,
    // as every pass's are bound).
    let fewest = 16u + min(p.n, 0u);
    if state[20] > 0.5 {{ return; }}
    var total = 0u;
    for (var b = 0u; b < {BINS}u; b++) {{ total += atomicLoad(&hist[b]); }}
    if total < fewest {{ state[19] = 1.0; }}
    var seen = 0u;
    var at = 0u;
    for (var b = 0u; b < {BINS}u; b++) {{
        seen += atomicLoad(&hist[b]);
        if seen * 2u > total {{ at = b; break; }}
    }}
    let mid = (f32(at) + 0.5) / 1024.0;
    let median = mid * mid * 2.0;
    state[17] = 4.685 * max(median * 1.4826, 0.004);
    for (var b = 0u; b < {BINS}u; b++) {{ atomicStore(&hist[b], 0u); }}
}}
"
            ),
        ),
        // The weighted normal equations' sums, per workgroup, and each
        // point's trust for the next step's light.
        (
            "planar_normal_partial",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> res: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> trust: array<vec2<f32>>;
@group(0) @binding(6) var<storage, read_write> partial: array<f32>;
var<workgroup> acc: array<f32, {}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {{
    if state[20] > 0.5 {{ return; }}
    var s: array<f32, {NORMAL}>;
    for (var j = 0u; j < {NORMAL}u; j++) {{ s[j] = 0.0; }}
    let cut = state[17];
    var i = wg.x * {THREADS}u + li;
    loop {{
        if i >= p.n {{ break; }}
        if seen[i].z > 0.5 {{
            let o = i * {POINT}u;
            let e = res[i];
            var t = vec2<f32>(0.0);
            for (var c = 0u; c < 2u; c++) {{
                let r = e[c] / cut;
                if abs(r) < 1.0 {{
                    let w = (1.0 - r * r) * (1.0 - r * r);
                    t[c] = w;
                    var k = 0u;
                    for (var a = 0u; a < 8u; a++) {{
                        let sa = points[o + 4u + c * 8u + a];
                        s[36u + a] += w * sa * e[c];
                        for (var b = a; b < 8u; b++) {{ s[k] += w * sa * points[o + 4u + c * 8u + b]; k++; }}
                    }}
                }}
            }}
            trust[i] = t;
        }}
        i += {GROUPS}u * {THREADS}u;
    }}
    for (var j = 0u; j < {NORMAL}u; j++) {{ acc[li * {NORMAL}u + j] = s[j]; }}
    workgroupBarrier();
    var stride = {THREADS}u / 2u;
    loop {{
        if stride == 0u {{ break; }}
        if li < stride {{
            for (var j = 0u; j < {NORMAL}u; j++) {{ acc[li * {NORMAL}u + j] += acc[(li + stride) * {NORMAL}u + j]; }}
        }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if li == 0u {{
        for (var j = 0u; j < {NORMAL}u; j++) {{ partial[wg.x * {NORMAL}u + j] = acc[j]; }}
    }}
}}
",
                THREADS * NORMAL
            ),
        ),
        // The step solved and the map updated by its inverse; the
        // Hessian's conditioning kept.
        (
            "planar_step",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
{SOLVE}
var<workgroup> sums: array<f32, {NORMAL}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(local_invocation_index) li: u32) {{
    if state[19] > 0.5 || state[20] > 0.5 {{ return; }}
    // Each sum over the groups by a thread of its own, then one solves.
    if li < {NORMAL}u {{
        var t = 0.0;
        for (var g = 0u; g < p.wgs; g++) {{ t += partial[g * {NORMAL}u + li]; }}
        sums[li] = t;
    }}
    workgroupBarrier();
    if li != 0u {{ return; }}
    var s: array<f32, {NORMAL}>;
    for (var j = 0u; j < {NORMAL}u; j++) {{ s[j] = sums[j]; }}
    var a: array<array<f32, 8>, 8>;
    var b: array<f32, 8>;
    var k = 0u;
    for (var r = 0u; r < 8u; r++) {{
        for (var c = r; c < 8u; c++) {{ a[r][c] = s[k]; a[c][r] = s[k]; k++; }}
        b[r] = s[36u + r];
    }}
    let x = solve(a, b, 8u);
    if !x.ok {{ state[19] = 1.0; return; }}
    var d = x.v;
    // A step this small is the CPU's stop: the level is done.
    var moved = 0.0;
    for (var j = 0u; j < 8u; j++) {{ moved += d[j] * d[j]; }}
    if moved < 1.0e-10 {{ state[20] = 1.0; }}
    // The step as a map, inverted by its adjugate.
    let m = mat3x3<f32>(vec3<f32>(1.0 + d[0], d[3], d[6]), vec3<f32>(d[1], 1.0 + d[4], d[7]), vec3<f32>(d[2], d[5], 1.0));
    let det = determinant(m);
    if abs(det) < 1.0e-12 {{ state[19] = 1.0; return; }}
    let inv = adjugate(m) * (1.0 / det);
    let g = mat3x3<f32>(vec3<f32>(state[0], state[3], state[6]), vec3<f32>(state[1], state[4], state[7]), vec3<f32>(state[2], state[5], state[8]));
    let n = g * inv;
    state[0] = n[0][0]; state[1] = n[1][0]; state[2] = n[2][0];
    state[3] = n[0][1]; state[4] = n[1][1]; state[5] = n[2][1];
    state[6] = n[0][2]; state[7] = n[1][2]; state[8] = n[2][2];
}}
"
            ),
        ),
        // The last step's Hessian's conditioning, once a level is done:
        // `align_level`'s, which only its last step's matters for.
        (
            "planar_condition",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
{EIGEN}
@compute @workgroup_size(1)
fn main() {{
    var s: array<f32, {NORMAL}>;
    for (var j = 0u; j < {NORMAL}u; j++) {{ s[j] = 0.0; }}
    for (var g = 0u; g < p.wgs; g++) {{
        for (var j = 0u; j < {NORMAL}u; j++) {{ s[j] += partial[g * {NORMAL}u + j]; }}
    }}
    var a: array<array<f32, 8>, 8>;
    var k = 0u;
    for (var r = 0u; r < 8u; r++) {{
        for (var c = r; c < 8u; c++) {{ a[r][c] = s[k]; a[c][r] = s[k]; k++; }}
    }}
    state[18] = conditioning(a);
}}
"
            ),
        ),
        // How alike a template's brightness and the frame's are under the
        // map: per workgroup, the sums ZNCC reads.
        (
            "planar_zncc",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> partial: array<f32>;
{SAMPLE}
var<workgroup> acc: array<f32, {}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {{
    var s = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    var i = wg.x * {THREADS}u + li;
    loop {{
        if i >= p.n {{ break; }}
        let o = i * {POINT}u;
        let m = map_point(vec2<f32>(points[o], points[o + 1u]));
        if m.z > 0.0 {{
            let b = sample(0u, m.x, m.y);
            if b.ok {{
                let a = points[o + 2u];
                s[0] += 1.0; s[1] += a; s[2] += b.v; s[3] += a * a; s[4] += b.v * b.v; s[5] += a * b.v;
            }}
        }}
        i += {GROUPS}u * {THREADS}u;
    }}
    for (var j = 0u; j < 6u; j++) {{ acc[li * 6u + j] = s[j]; }}
    workgroupBarrier();
    var stride = {THREADS}u / 2u;
    loop {{
        if stride == 0u {{ break; }}
        if li < stride {{ for (var j = 0u; j < 6u; j++) {{ acc[li * 6u + j] += acc[(li + stride) * 6u + j]; }} }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if li == 0u {{ for (var j = 0u; j < 6u; j++) {{ partial[wg.x * 6u + j] = acc[j]; }} }}
}}
",
                THREADS * 6
            ),
        ),
        // The coarse search: for each shift of the map (one workgroup
        // each), the summed difference of the brightness over every
        // `step`th point, and how many were read.
        (
            "planar_search",
            format!(
                "{PARAMS}
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> costs: array<f32>;
{SAMPLE}
var<workgroup> acc: array<f32, {}>;
@compute @workgroup_size({THREADS})
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {{
    let side = u32(2 * p.radius + 1);
    let shift = vec2<f32>(f32(i32(wg.x % side) - p.radius), f32(i32(wg.x / side) - p.radius));
    var sum = 0.0;
    var n = 0.0;
    var j = li;
    loop {{
        let i = j * p.step;
        if i >= p.n {{ break; }}
        let o = i * {POINT}u;
        let m = map_point(vec2<f32>(points[o], points[o + 1u]));
        if m.z > 0.0 {{
            let v = sample(0u, m.x + shift.x, m.y + shift.y);
            if v.ok {{ sum += abs(v.v - points[o + 2u]); n += 1.0; }}
        }}
        j += {THREADS}u;
    }}
    acc[li * 2u] = sum;
    acc[li * 2u + 1u] = n;
    workgroupBarrier();
    var stride = {THREADS}u / 2u;
    loop {{
        if stride == 0u {{ break; }}
        if li < stride {{ acc[li * 2u] += acc[(li + stride) * 2u]; acc[li * 2u + 1u] += acc[(li + stride) * 2u + 1u]; }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if li == 0u {{ costs[wg.x * 2u] = acc[0]; costs[wg.x * 2u + 1u] = acc[1]; }}
}}
",
                THREADS * 2
            ),
        ),
    ]
}

/// Gaussian elimination with partial pivoting on the top-left `n` × `n`
/// of an 8×8: `homography::solve`.
const SOLVE: &str = "
struct Solved { v: array<f32, 8>, ok: bool }

fn solve(a_in: array<array<f32, 8>, 8>, b_in: array<f32, 8>, n: u32) -> Solved {
    var a = a_in;
    var b = b_in;
    for (var col = 0u; col < n; col++) {
        var best = col;
        var big = abs(a[col][col]);
        for (var r = col + 1u; r < n; r++) {
            if abs(a[r][col]) > big { big = abs(a[r][col]); best = r; }
        }
        if big < 1.0e-12 { return Solved(b, false); }
        if best != col {
            let row = a[col]; a[col] = a[best]; a[best] = row;
            let t = b[col]; b[col] = b[best]; b[best] = t;
        }
        for (var r = col + 1u; r < n; r++) {
            let f = a[r][col] / a[col][col];
            for (var c = col; c < n; c++) { a[r][c] -= f * a[col][c]; }
            b[r] -= f * b[col];
        }
    }
    var x: array<f32, 8>;
    for (var j = 0u; j < 8u; j++) { x[j] = 0.0; }
    for (var k = 0u; k < n; k++) {
        let r = n - 1u - k;
        var sum = b[r];
        for (var c = r + 1u; c < n; c++) { sum -= a[r][c] * x[c]; }
        x[r] = sum / a[r][r];
    }
    return Solved(x, true);
}

fn adjugate(m: mat3x3<f32>) -> mat3x3<f32> {
    let a = m[0]; let b = m[1]; let c = m[2];
    return transpose(mat3x3<f32>(cross(b, c), cross(c, a), cross(a, b)));
}
";

/// The smallest over the largest eigenvalue of a symmetric 8×8, by
/// Jacobi rotations: `planar::eigenvalues`.
const EIGEN: &str = "
fn conditioning(a_in: array<array<f32, 8>, 8>) -> f32 {
    var a = a_in;
    for (var sweep = 0; sweep < 30; sweep++) {
        var off = 0.0;
        for (var i = 0u; i < 8u; i++) { for (var j = 0u; j < 8u; j++) { if i != j { off += a[i][j] * a[i][j]; } } }
        if off < 1.0e-20 { break; }
        for (var pp = 0u; pp < 8u; pp++) {
            for (var q = pp + 1u; q < 8u; q++) {
                if abs(a[pp][q]) < 1.0e-30 { continue; }
                let theta = (a[q][q] - a[pp][pp]) / (2.0 * a[pp][q]);
                var t = sign(theta) / (abs(theta) + sqrt(theta * theta + 1.0));
                if theta == 0.0 { t = 1.0; }
                let c = 1.0 / sqrt(t * t + 1.0);
                let s = t * c;
                for (var k = 0u; k < 8u; k++) {
                    let akp = a[k][pp]; let akq = a[k][q];
                    a[k][pp] = c * akp - s * akq; a[k][q] = s * akp + c * akq;
                }
                for (var k = 0u; k < 8u; k++) {
                    let apk = a[pp][k]; let aqk = a[q][k];
                    a[pp][k] = c * apk - s * aqk; a[q][k] = s * apk + c * aqk;
                }
            }
        }
    }
    var lo = 3.0e38;
    var hi = 0.0;
    for (var i = 0u; i < 8u; i++) { lo = min(lo, a[i][i]); hi = max(hi, a[i][i]); }
    if hi <= 0.0 { return 0.0; }
    return max(lo / hi, 0.0);
}
";
