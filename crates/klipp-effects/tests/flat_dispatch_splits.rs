//! Flat-indexed shaders dispatched past the per-dimension limit (#2341).
//!
//! A shader that runs one invocation per pixel, its workgroups in a row,
//! needed `ceil(w·h / 256)` workgroups along x: 129,600 at 8K, past
//! wgpu's 65,535, a validation error. The dispatch now splits over x and
//! y (`split_workgroups`), and each such shader rebuilds its index from
//! both. These hold that: every effect draws the same split as unsplit,
//! and no shader or dispatch is left flat.

use std::path::Path;

use klipp_effects::gpu::{split_workgroups, GpuEffectsPipeline, WorkgroupCapForTest};
use klipp_effects::registry::effect_types;

const YW: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn pipeline() -> Option<GpuEffectsPipeline> {
    let mut g = GpuEffectsPipeline::new()?;
    g.set_luma_coefs(YW);
    Some(g)
}

/// A frame with every channel moving, so each effect has something to do.
fn frame(w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [x, y, (x * 7.0 + y * 3.0).fract(), 0.4 + 0.6 * x]
        })
        .collect()
}

#[test]
fn a_dispatch_splits_only_past_the_limit_and_covers_every_workgroup() {
    assert_eq!(split_workgroups(0), (0, 1));
    assert_eq!(split_workgroups(65_535), (65_535, 1));
    for n in [65_536, 129_600, 1_000_000, 65_535 * 3 + 1] {
        let (x, y) = split_workgroups(n);
        assert!(x <= 65_535 && y <= 65_535, "{n}: {x} × {y}");
        assert!(x * y >= n && x * y - n < y, "{n}: {x} × {y} covers it, with less than a row spare");
    }
    let _cap = WorkgroupCapForTest::new(4);
    assert_eq!(split_workgroups(10), (4, 3));
}

/// Every effect draws the same picture dispatched in 2D as in a row, bit
/// for bit: the limit lowered to 2 on this thread, so a small frame
/// splits, with a few workgroups spare past its end.
#[test]
fn every_effect_draws_the_same_dispatched_in_2d() {
    let Some(mut g) = pipeline() else { return };
    let (w, h) = (61, 37);
    let input = frame(w, h);
    let mut unsteady = Vec::new();
    let mut differ = Vec::new();
    for t in effect_types() {
        let fx = [(t.sample)()];
        let row = g.apply_effects(&fx, &input, w, h);
        if g.apply_effects(&fx, &input, w, h) != row {
            // Different each run whatever the dispatch: nothing to compare.
            unsteady.push(t.desc.id);
            continue;
        }
        let split = {
            let _cap = WorkgroupCapForTest::new(2);
            g.apply_effects(&fx, &input, w, h)
        };
        if split.iter().map(|v| v.to_bits()).ne(row.iter().map(|v| v.to_bits())) {
            differ.push(t.desc.id);
        }
    }
    assert!(differ.is_empty(), "drawn differently dispatched in 2D: {differ:?}");
    assert!(unsteady.len() <= 2, "too many effects differ run to run to be checked: {unsteady:?}");
}

/// Every source file of the GPU shaders and their dispatches, by path.
fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in [manifest.join("src"), manifest.join("../klipp-compose/src")] {
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs" || x == "wgsl") {
                    out.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
    }
    out
}

/// No flat shader indexes by `gid.x` alone, and none dispatches a row of
/// workgroups unsplit. A flat shader is a compute entry with a workgroup
/// one dimension wide whose body doesn't use its id's `y` (one that does
/// indexes in 2D and is dispatched so); it must rebuild its id from both
/// dimensions with its own workgroup size.
#[test]
fn no_shader_or_dispatch_is_left_flat() {
    let mut found = Vec::new();
    let mut entries = 0;
    for (path, src) in sources() {
        let mut rest = src.as_str();
        while let Some(at) = rest.find("@workgroup_size(") {
            let after = &rest[at + "@workgroup_size(".len()..];
            let size = &after[..after.find(')').unwrap()];
            let body_end = after.find("\n}").unwrap_or(after.len());
            let (sig_and_body, next) = after.split_at(body_end);
            rest = next;
            if size.contains(',') {
                continue;
            }
            let Some(sig_end) = sig_and_body.find('{') else { continue };
            let (sig, body) = sig_and_body.split_at(sig_end);
            let Some(g) = sig.find("@builtin(global_invocation_id) ") else { continue };
            let name: String = sig[g + "@builtin(global_invocation_id) ".len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            entries += 1;
            if let Some(id) = name.strip_suffix("_flat") {
                let rebuilt = format!("let {id} = vec3<u32>({id}_flat.x + {id}_flat.y * nwg.x * {}u, 0u, {id}_flat.z);", size.trim());
                if !body.contains(&rebuilt) {
                    found.push(format!("{path}: an entry of @workgroup_size({size}) rebuilds its id with another size, or not at all"));
                }
            } else if !body.contains(&format!("{name}.y")) {
                found.push(format!("{path}: an entry of @workgroup_size({size}) indexes by {name}.x alone"));
            }
        }
        for (i, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(".dispatch_workgroups(") && code.trim_end().ends_with(", 1, 1);") {
                found.push(format!("{path}:{}: a row of workgroups dispatched unsplit: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(entries > 100, "the scan found the shaders: {entries}");
    assert!(found.is_empty(), "{}", found.join("\n"));
}
