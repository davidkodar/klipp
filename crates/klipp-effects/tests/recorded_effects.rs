//! The colour effects are recorded into one encoder (node-graph M2 step
//! 7): a run of them is one submit, whatever its length, and gives the
//! same picture as each effect run on its own. The counts include the
//! one submit `apply_effects`'s download makes.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails instead.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::BuiltinEffect as E;
const W: u32 = 32;
const H: u32 = 16;
fn colour() -> Vec<E> {
    vec![
        E::Opacity { value: 0.8 },
        E::Invert { mix: 0.3 },
        E::Posterize { levels: 6.0 },
        E::ChannelMixer { red_red: 0.9, red_green: 0.1, red_blue: 0.0, green_red: 0.0, green_green: 1.0, green_blue: 0.0, blue_red: 0.0, blue_green: 0.1, blue_blue: 0.9 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 10.0, saturation: 1.1, value: 0.95 },
        E::Levels { in_black: 0.05, in_white: 0.95, gamma: 1.1, out_black: 0.0, out_white: 1.0 },
        E::Vibrance { amount: 0.2 },
        E::ColorFade { color: [20, 40, 60, 255], duration_secs: 1.0, mode: Default::default(), progress: 0.5 },
    ]
}
fn pipeline() -> Option<GpuEffectsPipeline> {
    let g = GpuEffectsPipeline::new();
    if g.is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required but no GPU");
        eprintln!("UNVERIFIED: no GPU");
    }
    g
}

fn picture() -> Vec<f32> {
    (0..W * H * 4).map(|i| if i % 4 == 3 { 0.25 + (i % 7) as f32 / 10.0 } else { (i % 97) as f32 / 97.0 }).collect()
}

/// Submits `apply_effects` makes for `fx`, and its picture.
fn run(g: &mut GpuEffectsPipeline, fx: &[E]) -> (u64, Vec<f32>) {
    let before = g.submit_count();
    let out = g.apply_effects(fx, &picture(), W, H);
    (g.submit_count() - before, out)
}

#[test]
fn a_run_of_colour_effects_is_one_submit() {
    let Some(mut g) = pipeline() else { return };
    // One for the run, one for the download.
    assert_eq!(run(&mut g, &colour()[..1]).0, 2);
    assert_eq!(run(&mut g, &colour()).0, 2, "nine effects, one run");
    // Identities record nothing, and a run of only identities submits nothing.
    let idle = [E::Opacity { value: 1.0 }, E::Gamma { gamma: 1.0 }, E::Invert { mix: 0.0 }];
    assert_eq!(run(&mut g, &idle).0, 1, "only the download");
    // Seventy passes overflow the 64 slots once: two runs.
    let seventy: Vec<E> = colour().into_iter().cycle().take(70).collect();
    assert_eq!(run(&mut g, &seventy).0, 3);
    // Equalize reads the frame back mid-chain, a named break: the four
    // recorded before it, its readback and pass, the five after.
    let mut eq = colour();
    eq.insert(4, E::Equalize { strength: 0.5 });
    assert_eq!(run(&mut g, &eq).0, 5);
}

#[test]
fn a_recorded_run_draws_what_each_effect_alone_draws() {
    let Some(mut g) = pipeline() else { return };
    // Each effect in a run of its own, one after the other: each pass
    // reads its own parameters there, whatever the slots do.
    let one_by_one = |g: &mut GpuEffectsPipeline, fx: &[E]| {
        fx.iter().fold(picture(), |px, e| g.apply_effects(std::slice::from_ref(e), &px, W, H))
    };
    let together = run(&mut g, &colour()).1;
    assert_eq!(together, one_by_one(&mut g, &colour()), "nine in one run");
    // Across the slots' overflow too.
    let seventy: Vec<E> = colour().into_iter().cycle().take(70).collect();
    assert_eq!(run(&mut g, &seventy).1, one_by_one(&mut g, &seventy), "seventy, across a refill");
    // And the picture is not the input: the effects ran.
    assert_ne!(together, picture());
}
