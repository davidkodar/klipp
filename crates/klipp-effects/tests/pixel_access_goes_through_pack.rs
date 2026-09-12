//! Every shader reads and writes pixels through `unpack`/`pack`.
//!
//! Those two are identity functions today, which is exactly why this
//! test exists. They are the hook for changing how a frame is *stored* —
//! half floats being the obvious candidate, at half the bandwidth — and
//! on the day that changes, a shader that reached into a pixel buffer
//! directly would read half-packed bits as a float and produce garbage.
//! Nothing would catch it: the types still line up, naga still
//! validates, and while `pack`/`unpack` are identity the wrong code
//! gives the right answer.
//!
//! So the invariant is enforced here, on the source, while it is still
//! free to enforce. Buffers that are *not* pixels — packed 10-bit HDR10
//! codes, sRGB u8 words, LUT tables, mask geometry — are declared as
//! something other than `array<vec4<f32>>` and are none of this test's
//! business.

/// A pixel binding is one declared `array<vec4<f32>>`. Anything else is
/// a different kind of buffer and is skipped.
fn pixel_bindings(shader: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in shader.lines() {
        let line = line.trim();
        if !line.starts_with("@group(") || !line.contains("array<vec4<f32>>") {
            continue;
        }
        // `@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;`
        if let Some(rest) = line.split("var<").nth(1) {
            if let Some(decl) = rest.split('>').nth(1) {
                if let Some(name) = decl.trim().split(':').next() {
                    names.push(name.trim().to_string());
                }
            }
        }
    }
    names
}

/// Lines that touch a pixel binding without going through the hook.
fn violations(name: &str, shader: &str) -> Vec<String> {
    let bindings = pixel_bindings(shader);
    if bindings.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, raw) in shader.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        for b in &bindings {
            let index = format!("{b}[");
            if !line.contains(&index) {
                continue;
            }
            // A store: `<binding>[i] = …` must assign `pack(…)`.
            let store = line.split_once('=').filter(|(lhs, _)| {
                lhs.trim_end().ends_with(']') && lhs.trim_start().starts_with(b)
            });
            if let Some((_, rhs)) = store {
                let rhs = rhs.trim();
                // A straight copy of one pixel buffer into another moves
                // the stored representation as it is, which stays
                // correct whatever that representation becomes.
                let copy_from_pixel_buffer = bindings
                    .iter()
                    .any(|other| rhs.starts_with(&format!("{other}[")));
                if !rhs.starts_with("pack(") && !copy_from_pixel_buffer {
                    out.push(format!("{name}:{}: store bypasses pack(): {line}", i + 1));
                }
                continue;
            }
            // A load: every `<binding>[i]` must sit inside `unpack(`.
            for (at, _) in line.match_indices(&index) {
                let before = &line[..at];
                let copy_target = bindings.iter().any(|other| {
                    before.trim_start().starts_with(&format!("{other}["))
                        && before.contains('=')
                });
                if !before.ends_with("unpack(") && !copy_target {
                    out.push(format!("{name}:{}: load bypasses unpack(): {line}", i + 1));
                    break;
                }
            }
        }
    }
    out
}

#[test]
fn every_shader_reads_and_writes_pixels_through_the_hook() {
    let mut all = Vec::new();
    for (name, src) in klipp_effects::gpu::shader_sources() {
        all.extend(violations(name, src));
    }
    if !all.is_empty() {
        panic!(
            "{} pixel accesses bypass unpack()/pack(). Each one reads or writes a \
             frame buffer directly, which is correct only while those two are \
             identity functions:\n  {}",
            all.len(),
            all.join("\n  "),
        );
    }
}

/// The lint has to be able to fail, or it proves nothing.
#[test]
fn the_lint_catches_a_direct_access() {
    let bad = "
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn main() {
    let p = input[idx];
    output[idx] = vec4<f32>(p.x, p.y, p.z, 1.0);
}
";
    let found = violations("fixture", bad);
    assert_eq!(found.len(), 2, "expected one load and one store: {found:?}");

    let good = "
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn main() {
    let p = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(p.x, p.y, p.z, 1.0));
}
";
    assert!(violations("fixture", good).is_empty(), "{:?}", violations("fixture", good));

    // A buffer of packed codes is not a pixel buffer and is left alone.
    let codes = "
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(256)
fn main() {
    let packed = input[idx];
    output[idx] = pack(vec4<f32>(0.0));
}
";
    assert!(violations("fixture", codes).is_empty(), "{:?}", violations("fixture", codes));
}
