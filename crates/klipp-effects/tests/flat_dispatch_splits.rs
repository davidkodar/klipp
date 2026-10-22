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
use klipp_effects::gpu_scopes::{HistogramJob, HistogramLayout, ScopeInput, ScopeJob, ScopeTransfer, VectorscopeJob, WaveformJob, WaveformMode};
use klipp_effects::registry::effect_types;
use klipp_effects::{apply_effects_with_luma, equalize_cdf_of, equalize_histogram, BuiltinEffect};

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

/// Every scope draws the same dispatched in 2D as in a row, bit for bit,
/// the limit lowered to 2: a flat-indexed scope shader rebuilds its index
/// from both dimensions, and a 2D one isn't dispatched as if flat.
#[test]
fn every_scope_draws_the_same_dispatched_in_2d() {
    let Some(mut g) = pipeline() else { return };
    let (w, h) = (61, 37);
    let input = frame(w, h);
    let src = ScopeInput::Linear { rgba: &input, width: w, height: h, matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], luma: YW, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let wave = |mode, envelope| ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope, glow: true, out_w: 96, out_h: 64 });
    let jobs = [
        wave(WaveformMode::Luma, true),
        wave(WaveformMode::Parade, false),
        wave(WaveformMode::RgbOverlay, false),
        ScopeJob::Vectorscope(VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: true, out_w: 64, out_h: 64 }),
        ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Parade, log: true, fill: 0.5, glow: true, out_w: 96, out_h: 64 }),
    ];
    for (i, job) in jobs.iter().enumerate() {
        let row = g.scopes().render(&src, 1, std::slice::from_ref(job));
        let split = {
            let _cap = WorkgroupCapForTest::new(2);
            g.scopes().render(&src, 1, std::slice::from_ref(job))
        };
        assert!(row[0].is_some(), "scope {i} drew");
        assert!(row == split, "scope {i} drawn differently dispatched in 2D");
    }
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
/// dimensions with its own workgroup size. A direct dispatch of `(n, 1,
/// 1)`, written over one line or several, is refused unless it says why
/// it is safe (`flat-ok:`), and indirect arguments written as a row (a
/// `y` of `1u`) are refused: they are split as `split_workgroups` splits.
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
        let mut rest = src.as_str();
        while let Some(at) = rest.find(".dispatch_workgroups(") {
            let after = &rest[at + ".dispatch_workgroups(".len()..];
            let end = after.find(");").unwrap_or(after.len());
            let args: Vec<&str> = after[..end].split(',').map(str::trim).filter(|a| !a.is_empty()).collect();
            let line_end = after[end..].find('\n').map_or(after.len(), |n| end + n);
            if args.len() >= 3 && args[args.len() - 2..] == ["1", "1"] && !after[end..line_end].contains("flat-ok:") {
                let line = src[..src.len() - rest.len() + at].lines().count();
                found.push(format!("{path}:{line}: a row of workgroups dispatched unsplit: ({})", args.join(", ")));
            }
            rest = &after[end..];
        }
        if src.contains("dispatch_workgroups_indirect(") {
            for (i, line) in src.lines().enumerate() {
                let Some(k) = line.trim().strip_prefix("args[").and_then(|l| l.split_once("] = 1u;")).and_then(|(k, _)| k.parse::<usize>().ok()) else { continue };
                if k % 3 == 1 {
                    found.push(format!("{path}:{}: an indirect dispatch written as a row: {}", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(entries > 100, "the scan found the shaders: {entries}");
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// A frame really past the limit (4200 × 4100: 67,266 workgroups of 256)
/// is drawn to its last pixel: Equalize's histogram counts every pixel
/// once, as the CPU's does, and a plain effect's first and last rows are
/// the CPU's. Its buffers are past wgpu's default limits, so it needs the
/// adapter's own (`device_limits`); an adapter offering less skips it.
#[test]
fn a_frame_past_the_limit_is_drawn_to_its_last_pixel() {
    let Some(mut g) = pipeline() else { return };
    let (w, h): (u32, u32) = (4200, 4100);
    assert!((w * h).div_ceil(256) > 65_535);
    let bytes = u64::from(w * h) * 16;
    let limits = g.device().limits();
    if u64::from(limits.max_storage_buffer_binding_size) < bytes || limits.max_buffer_size < bytes {
        eprintln!("skipped: the adapter binds {} MiB, a {w}×{h} frame is {} MiB", limits.max_storage_buffer_binding_size >> 20, bytes >> 20);
        return;
    }
    let input = frame(w, h);
    let (hist, cdf) = g.equalize_histogram_and_cdf(&input, w, h).expect("a GPU histogram");
    assert_eq!(hist.iter().map(|&c| u64::from(c)).sum::<u64>(), u64::from(w * h), "every pixel counted once");
    let cpu_cdf = equalize_cdf_of(&equalize_histogram(&input, YW), (w * h) as usize);
    let worst = cdf.iter().zip(&cpu_cdf).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-3, "the CDF is the CPU's: {worst}");
    let row = (w * 4) as usize;
    for fx in [BuiltinEffect::Invert { mix: 0.7 }, BuiltinEffect::Gamma { gamma: 1.4 }] {
        let gpu = g.apply_effects(std::slice::from_ref(&fx), &input, w, h);
        for (name, at) in [("first", 0), ("last", input.len() - row)] {
            let cpu = apply_effects_with_luma(std::slice::from_ref(&fx), input[at..at + row].to_vec(), w, 1, YW);
            let off = gpu[at..at + row].iter().zip(&cpu).filter(|(a, b)| (*a - *b).abs() > 1e-4).count();
            assert_eq!(off, 0, "{fx:?}: the {name} row is the CPU's");
        }
    }
}
