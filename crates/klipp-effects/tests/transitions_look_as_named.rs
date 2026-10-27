//! The transitions of #2765 at their default settings give the look
//! their names promise, measured on the CPU kernel and the GPU's (the
//! viewer's and the export's shader): from neutral vertical bars to
//! neutral horizontal ones, so a colour fringe is the transition's own.

use klipp_core::TRANSITION_SPECS;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::render_transition_linear;

const W: u32 = 192;
const H: u32 = 108;

fn frames() -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for y in 0..H {
        for x in 0..W {
            let v = if (x / 16) % 2 == 0 { 0.6 } else { 0.08 };
            a.extend_from_slice(&[v, v, v, 1.0]);
            let v = if (y / 12) % 2 == 0 { 0.45 } else { 0.03 };
            b.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    (a, b)
}

fn kind(key: &str) -> u32 {
    TRANSITION_SPECS.iter().find(|s| s.string_key == key).unwrap_or_else(|| panic!("no {key}")).kind_id as u32
}

/// `key` at `p` with its default settings, on each path there is.
fn at(key: &str, p: f32) -> Vec<(&'static str, Vec<f32>)> {
    let (from, to) = frames();
    let spec = &TRANSITION_SPECS[kind(key) as usize];
    let d = &spec.default_params;
    let mut out = vec![("cpu", render_transition_linear(&from, &to, kind(key), p, W, H, d.param0, d.param1, d.param2, d.param3, d.color))];
    if std::env::var_os("KLIPP_TEST_SKIP_GPU").is_none() {
        match GpuEffectsPipeline::new() {
            Some(mut gpu) => out.push(("gpu", gpu.render_transition_linear(&from, &to, kind(key), p, W, H, d.param0, d.param1, d.param2, d.param3, d.color))),
            None => assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "KLIPP_TEST_REQUIRE_GPU=1, but there is no GPU"),
        }
    }
    out
}

fn shown(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

fn px(f: &[f32], x: u32, y: u32) -> [f32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [shown(f[i]), shown(f[i + 1]), shown(f[i + 2])]
}

fn mean_diff(a: &[f32], b: &[f32]) -> f32 {
    a.chunks(4).zip(b.chunks(4)).map(|(p, q)| (0..3).map(|c| (shown(p[c]) - shown(q[c])).abs()).sum::<f32>() / 3.0).sum::<f32>() / (W * H) as f32
}

fn brightness(f: &[f32]) -> f32 {
    f.chunks(4).map(|p| (shown(p[0]) + shown(p[1]) + shown(p[2])) / 3.0).sum::<f32>() / (W * H) as f32
}

fn spread(f: &[f32]) -> f32 {
    let m = brightness(f);
    (f.chunks(4).map(|p| ((shown(p[0]) + shown(p[1]) + shown(p[2])) / 3.0 - m).powi(2)).sum::<f32>() / (W * H) as f32).sqrt()
}

/// Whether the pixel shows the incoming shot more than the outgoing one.
fn incoming(out: &[f32], x: u32, y: u32) -> bool {
    let (from, to) = frames();
    let (o, a, b) = (px(out, x, y)[0], px(&from, x, y)[0], px(&to, x, y)[0]);
    (a - b).abs() > 0.2 && (o - b).abs() < (o - a).abs()
}

#[test]
fn a_star_wipe_opens_a_star() {
    for (path, out) in at("StarWipe", 0.3) {
        // How far the opening reaches along each of 72 directions.
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        let reach: Vec<f32> = (0..72)
            .map(|k| {
                let a = k as f32 * std::f32::consts::TAU / 72.0;
                (1..60).map(|r| r as f32).take_while(|r| {
                    let (x, y) = (cx + a.cos() * r, cy + a.sin() * r);
                    x >= 0.0 && y >= 0.0 && (x as u32) < W && (y as u32) < H && (incoming(&out, x as u32, y as u32) || !{
                        let (from, to) = frames();
                        (px(&from, x as u32, y as u32)[0] - px(&to, x as u32, y as u32)[0]).abs() > 0.2
                    })
                }).last().unwrap_or(0.0)
            })
            .collect();
        let (lo, hi) = (reach.iter().cloned().fold(f32::MAX, f32::min), reach.iter().cloned().fold(0.0f32, f32::max));
        println!("StarWipe {path}: reach {lo:.0} to {hi:.0} px");
        assert!(lo > 2.0 && hi / lo > 1.6, "StarWipe {path}: an opening from {lo:.0} to {hi:.0} px is no star");
    }
}

#[test]
fn a_clock_wipe_sweeps_round_the_centre() {
    for (path, out) in at("ClockWipe", 0.25) {
        // A quarter of the way: the quarter clockwise from twelve o'clock.
        let share = |x0: u32, x1: u32, y0: u32, y1: u32| {
            let (mut n, mut on) = (0, 0);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (from, to) = frames();
                    if (px(&from, x, y)[0] - px(&to, x, y)[0]).abs() > 0.2 {
                        n += 1;
                        on += incoming(&out, x, y) as u32;
                    }
                }
            }
            on as f32 / n.max(1) as f32
        };
        let (tr, br, bl, tl) = (share(W / 2 + 4, W, 0, H / 2 - 4), share(W / 2 + 4, W, H / 2 + 4, H), share(0, W / 2 - 4, H / 2 + 4, H), share(0, W / 2 - 4, 0, H / 2 - 4));
        println!("ClockWipe {path}: top right {tr:.2}, bottom right {br:.2}, bottom left {bl:.2}, top left {tl:.2}");
        assert!(tr > 0.6 && br < 0.4 && bl < 0.1 && tl < 0.1, "ClockWipe {path}: {tr:.2} {br:.2} {bl:.2} {tl:.2}");
    }
}

#[test]
fn a_glitch_splits_the_colours_without_blacking_out() {
    for (path, out) in at("GlitchRGBSplit", 0.5) {
        let black = out.chunks(4).filter(|p| p[0] + p[1] + p[2] < 1e-4).count() as f32 / (W * H) as f32;
        // Grey in, so any colour is the split's.
        let fringe = out.chunks(4).filter(|p| (shown(p[0]) - shown(p[2])).abs() > 0.15).count() as f32 / (W * H) as f32;
        println!("GlitchRGBSplit {path}: black {:.1}%, colour fringes {:.1}%", black * 100.0, fringe * 100.0);
        assert!(black < 0.01 && fringe > 0.03, "GlitchRGBSplit {path}: black {:.1}%, fringes {:.1}%", black * 100.0, fringe * 100.0);
    }
}

#[test]
fn a_colorize_flash_tints_the_picture() {
    for (path, out) in at("ColorizeFlash", 0.4) {
        let s = spread(&out);
        let blue = out.chunks(4).map(|p| shown(p[2]) - shown(p[0])).sum::<f32>() / (W * H) as f32;
        println!("ColorizeFlash {path}: spread {s:.3}, blue over red {blue:+.3}");
        assert!(s > 0.05 && blue > 0.05, "ColorizeFlash {path}: spread {s:.3}, tint {blue:+.3}");
    }
}

#[test]
fn a_light_leak_lights_part_of_the_frame() {
    let (from, to) = frames();
    for (path, out) in at("LightLeak", 0.5) {
        let (dissolve, _) = (render_transition_linear(&from, &to, kind("CrossDissolve"), 0.5, W, H, 1.0, 0.0, 0.0, 0.0, [1.0; 4]), ());
        let lifted = out.chunks(4).zip(dissolve.chunks(4)).filter(|(o, d)| (0..3).map(|c| shown(o[c]) - shown(d[c])).sum::<f32>() / 3.0 > 0.08).count() as f32 / (W * H) as f32;
        let wash = brightness(&out) - brightness(&dissolve);
        println!("LightLeak {path}: {:.0}% of the frame lit, brighter overall by {wash:+.3}", lifted * 100.0);
        assert!((0.1..0.7).contains(&lifted) && wash < 0.15, "LightLeak {path}: {:.0}% lit, overall {wash:+.3}", lifted * 100.0);
    }
}

#[test]
fn an_additive_dissolve_is_no_wash() {
    let (from, to) = frames();
    let ceiling = brightness(&from).max(brightness(&to)) + 0.2;
    for (path, out) in at("AdditiveDissolve", 0.5) {
        let b = brightness(&out);
        println!("AdditiveDissolve {path}: brightness {b:.3} (ceiling {ceiling:.3})");
        assert!(b < ceiling && b > brightness(&from).min(brightness(&to)), "AdditiveDissolve {path}: {b:.3}, ceiling {ceiling:.3}");
    }
}

#[test]
fn the_dissolves_start_and_end_on_their_shots() {
    let (from, to) = frames();
    for key in ["LuminanceDissolve", "ZoomOutDissolve", "ZoomInDissolve", "CrossZoom"] {
        for ((path, start), (_, end)) in at(key, 0.0).into_iter().zip(at(key, 1.0)) {
            let (a, b) = (mean_diff(&start, &from), mean_diff(&end, &to));
            println!("{key} {path}: start off the outgoing {a:.3}, end off the incoming {b:.3}");
            assert!(a < 0.01 && b < 0.01, "{key} {path}: start {a:.3}, end {b:.3}");
        }
    }
}
