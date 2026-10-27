//! Each stylised transition of #2764 shows its own character at the cut,
//! at its default settings: measured as how far its frame there is from
//! a plain cross dissolve's, on two smooth, photo-like HD frames (where
//! a small displacement or a faint overlay disappears), on the CPU
//! kernel and the GPU shader alike.

use klipp_core::TRANSITION_SPECS;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::render_transition_linear;

const W: u32 = 1920;
const H: u32 = 1080;

/// A bright outdoor shot (sky, sea, a sunlit wall) and a medium shot of a
/// figure against a soft background: broad gradients, a few edges, and
/// the fine texture footage has.
fn frames() -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (Vec::with_capacity((W * H * 4) as usize), Vec::with_capacity((W * H * 4) as usize));
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let c = if fy < 0.4 {
                [0.35 + 0.3 * fy, 0.5 + 0.2 * fy, 0.85]
            } else if fx < 0.6 {
                [0.05 + 0.1 * fx, 0.2 + 0.1 * fy, 0.35]
            } else {
                [0.75 - 0.2 * fy, 0.6 - 0.2 * fy, 0.45]
            };
            // Texture, as footage has: leaves, waves, brickwork.
            let t = 0.05 * ((x as f32 * 0.21).sin() * (y as f32 * 0.17).sin() + 0.5 * (x as f32 * 0.047 + y as f32 * 0.031).sin());
            a.extend_from_slice(&[c[0] + t, c[1] + t, c[2] + t, 1.0]);
            let d = ((fx - 0.5) / 0.18).powi(2) + ((fy - 0.55) / 0.42).powi(2);
            let c = if d < 1.0 { [0.45, 0.28, 0.2] } else { [0.12 + 0.1 * fy, 0.14, 0.12 + 0.15 * fx] };
            let t = 0.04 * ((x as f32 * 0.13).sin() * (y as f32 * 0.19).sin());
            b.extend_from_slice(&[c[0] + t, c[1] + t, c[2] + t, 1.0]);
        }
    }
    (a, b)
}

fn shown(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

/// Mean difference as shown, 0 to 1.
fn diff(a: &[f32], b: &[f32]) -> f32 {
    a.chunks(4).zip(b.chunks(4)).map(|(p, q)| (0..3).map(|c| (shown(p[c]) - shown(q[c])).abs()).sum::<f32>() / 3.0).sum::<f32>() / (W * H) as f32
}

/// The look-alikes: each must differ from a cross dissolve at the cut by
/// at least this, about five levels a pixel on average.
const OWN_LOOK: f32 = 0.02;
const KINDS: &[&str] = &["Ripple", "WaveDistortion", "VHSTracking", "ProjectorFlicker", "BokehDissolve", "ZoomInDissolve", "ZoomOutDissolve", "CrossZoom", "PageTurn", "PageTurnReverse"];

#[test]
fn each_shows_its_own_look_at_the_cut() {
    let (from, to) = frames();
    let id = |key: &str| TRANSITION_SPECS.iter().find(|s| s.string_key == key).unwrap_or_else(|| panic!("{key}")).kind_id;
    let mut gpu = if std::env::var_os("KLIPP_TEST_SKIP_GPU").is_some() { None } else { GpuEffectsPipeline::new() };
    assert!(gpu.is_some() || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "KLIPP_TEST_REQUIRE_GPU=1, but there is no GPU");
    let mut failed = Vec::new();
    for key in KINDS {
        let d = &TRANSITION_SPECS[id(key) as usize].default_params;
        // Its most distinct moment about the cut: where it peaks.
        for p in [0.4f32, 0.5, 0.6] {
            let plain = render_transition_linear(&from, &to, id("CrossDissolve") as u32, p, W, H, 1.0, 0.0, 0.0, 0.0, [1.0; 4]);
            let cpu = render_transition_linear(&from, &to, id(key) as u32, p, W, H, d.param0, d.param1, d.param2, d.param3, d.color);
            let mut paths = vec![("cpu", diff(&cpu, &plain))];
            if let Some(g) = gpu.as_mut() {
                let out = g.render_transition_linear(&from, &to, id(key) as u32, p, W, H, d.param0, d.param1, d.param2, d.param3, d.color);
                paths.push(("gpu", diff(&out, &plain)));
            }
            for (path, own) in paths {
                println!("{key} {path} p {p}: off a cross dissolve {own:.3}");
                if p == 0.5 && own < OWN_LOOK {
                    failed.push(format!("{key} {path}: {own:.3}"));
                }
            }
        }
    }
    assert!(failed.is_empty(), "these read as a plain cross dissolve at the cut: {failed:?}");
}

/// A page turn turns the page: beside the fold, at the cut, the page's
/// pale back curled over the outgoing shot, both ways round.
#[test]
fn a_page_turn_shows_the_page_curled_over() {
    let (from, to) = frames();
    let mut gpu = if std::env::var_os("KLIPP_TEST_SKIP_GPU").is_some() { None } else { GpuEffectsPipeline::new() };
    for (key, band) in [("PageTurn", (0.25f32, 0.45f32)), ("PageTurnReverse", (0.55, 0.75))] {
        let spec = TRANSITION_SPECS.iter().find(|s| s.string_key == key).unwrap();
        let d = &spec.default_params;
        let kind = spec.kind_id as u32;
        let mut outs = vec![("cpu", render_transition_linear(&from, &to, kind, 0.5, W, H, d.param0, d.param1, d.param2, d.param3, d.color))];
        if let Some(g) = gpu.as_mut() {
            outs.push(("gpu", g.render_transition_linear(&from, &to, kind, 0.5, W, H, d.param0, d.param1, d.param2, d.param3, d.color)));
        }
        // The band beside the fold, against the outgoing shot there: the
        // paper's back is paler and flatter.
        let mean = |f: &[f32]| {
            let (mut s, mut n) = (0.0, 0.0);
            for y in (0..H).step_by(4) {
                for x in ((band.0 * W as f32) as u32..(band.1 * W as f32) as u32).step_by(4) {
                    let i = ((y * W + x) * 4) as usize;
                    s += (shown(f[i]) + shown(f[i + 1]) + shown(f[i + 2])) / 3.0;
                    n += 1.0;
                }
            }
            s / n
        };
        for (path, out) in outs {
            let (paper, shot) = (mean(&out), mean(&from));
            println!("{key} {path}: beside the fold {paper:.3}, the outgoing shot there {shot:.3}");
            assert!(paper > shot + 0.1, "{key} {path}: no turned page beside the fold ({paper:.3} against {shot:.3})");
        }
    }
}
