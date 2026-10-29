//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> res: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> trust: array<vec2<f32>>;
@group(0) @binding(6) var<storage, read_write> partial: array<f32>;
var<workgroup> acc: array<f32, (THREADS * NORMAL)>;
@compute @workgroup_size(THREADS)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if state[20] > 0.5 { return; }
    var s: array<f32, NORMAL>;
    for (var j = 0u; j < NORMAL; j++) { s[j] = 0.0; }
    let cut = state[17];
    var i = wg.x * THREADS + li;
    loop {
        if i >= p.n { break; }
        if seen[i].z > 0.5 {
            let o = i * POINT;
            let e = res[i];
            var t = vec2<f32>(0.0);
            for (var c = 0u; c < 2u; c++) {
                let r = e[c] / cut;
                if abs(r) < 1.0 {
                    let w = (1.0 - r * r) * (1.0 - r * r);
                    t[c] = w;
                    var k = 0u;
                    for (var a = 0u; a < 8u; a++) {
                        let sa = points[o + 4u + c * 8u + a];
                        s[36u + a] += w * sa * e[c];
                        for (var b = a; b < 8u; b++) { s[k] += w * sa * points[o + 4u + c * 8u + b]; k++; }
                    }
                }
            }
            trust[i] = t;
        }
        i += GROUPS * THREADS;
    }
    for (var j = 0u; j < NORMAL; j++) { acc[li * NORMAL + j] = s[j]; }
    workgroupBarrier();
    var stride = THREADS / 2u;
    loop {
        if stride == 0u { break; }
        if li < stride {
            for (var j = 0u; j < NORMAL; j++) { acc[li * NORMAL + j] += acc[(li + stride) * NORMAL + j]; }
        }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if li == 0u {
        for (var j = 0u; j < NORMAL; j++) { partial[wg.x * NORMAL + j] = acc[j]; }
    }
}
