//! Every effect under hostile values (#2995): each number of each effect
//! set to what isn't a number, an overflow either way, a huge value
//! either way, zero, and the ends of its hard range, taken through the
//! guard every write passes, then rendered on the CPU and the GPU. No
//! render panics, takes more than its bound, leaves a pixel that isn't a
//! number, falls back from the GPU where the effect as reset doesn't, or
//! raises a GPU validation or out-of-memory error. Then every number at
//! the top of its range at once, and at the bottom, at full HD.
//!
//! Its own binary: it reads the process-wide GPU error counters.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::registry::{effect_types, guard_entry, EffectType, NotFinite, ParamDesc, ParamKind};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// A small frame for one value at a time; full HD for the extremes.
const SMALL: (u32, u32) = (160, 90);
const FULL: (u32, u32) = (1920, 1080);
/// The most one render may take: a value inside the range that makes an
/// effect run away (a radius per pixel, a loop per unit) shows here.
const SMALL_BOUND: Duration = Duration::from_secs(2);
const FULL_BOUND: Duration = Duration::from_secs(20);

/// A gradient with hard blocks, every value a number.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
                let block = if ((u * 8.0) as u32 + (t * 4.0) as u32).is_multiple_of(2) { 1.0 } else { 0.4 };
                v.extend_from_slice(&[u * block, t * block, (1.0 - u) * block, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn sequence(effect: &BuiltinEffect, size: (u32, u32)) -> Sequence {
    let clip = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }).effect(effect.clone()).build();
    let mut s = Sequence::new_default();
    s.resolution = Resolution { width: size.0, height: size.1 };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    s.tracks = vec![track];
    s
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The effects whose numbers are theirs to stress: not a source drawn
/// from a file or another clip, nor a plug-in's.
fn stressed() -> Vec<&'static EffectType> {
    effect_types().filter(|t| !matches!((t.sample)(), BuiltinEffect::Frei0r { .. } | BuiltinEffect::MediaSource { .. } | BuiltinEffect::TitleSource { .. })).collect()
}

fn numbers(t: &EffectType) -> impl Iterator<Item = &ParamDesc> {
    t.desc.params.iter().filter(|d| matches!(d.kind, ParamKind::Float | ParamKind::Whole) && !d.derived && d.hard().is_some())
}

fn slot<'a>(entry: &'a mut serde_json::Value, d: &ParamDesc) -> Option<&'a mut serde_json::Value> {
    match d.index {
        Some(i) => entry.get_mut(d.field).and_then(|a| a.get_mut(i)),
        None => entry.get_mut(d.field),
    }
}

/// `t`'s sample with `set` written to its numbers, through the guard.
fn hostile(t: &EffectType, set: &dyn Fn(&ParamDesc) -> Option<serde_json::Value>) -> Result<BuiltinEffect, String> {
    let mut entry = serde_json::to_value((t.sample)()).map_err(|e| e.to_string())?;
    for d in numbers(t) {
        if let (Some(v), Some(s)) = (set(d), slot(&mut entry, d)) {
            *s = v;
        }
    }
    guard_entry(&mut entry, NotFinite::Default)?;
    serde_json::from_value(entry).map_err(|e| format!("doesn't read after the guard: {e}"))
}

struct Run {
    cpu: Renderer,
    gpu: Renderer,
    gpu_ok: bool,
    failures: Vec<String>,
    slowest: (Duration, String),
}

impl Run {
    fn new() -> Self {
        let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        cpu.set_gpu_enabled(false);
        let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        gpu.set_gpu_enabled(true);
        let probe = sequence(&BuiltinEffect::Gamma { gamma: 1.0 }, SMALL);
        let gpu_ok = gpu.render_display_u8_on_gpu(&probe, tc(1.0), SMALL.0, SMALL.1, &mut Chart, RenderOptions::default()).is_some();
        assert!(gpu_ok || !required(), "required, but the GPU stack declined a plain frame");
        if !gpu_ok {
            eprintln!("UNVERIFIED: no GPU stack, the CPU only");
        }
        Run { cpu, gpu, gpu_ok, failures: Vec::new(), slowest: (Duration::ZERO, String::new()) }
    }

    /// Whether the GPU renders `effect` without falling back.
    fn gpu_holds(&mut self, effect: &BuiltinEffect) -> bool {
        let seq = sequence(effect, SMALL);
        let r = catch_unwind(AssertUnwindSafe(|| self.gpu.render_display_u8_on_gpu(&seq, tc(1.0), SMALL.0, SMALL.1, &mut Chart, RenderOptions::default())));
        matches!(r, Ok(Some(_))) && klipp_effects::residency::last_frame().fallback_count() == 0
    }

    fn time(&mut self, took: Duration, bound: Duration, what: &str) {
        if took > self.slowest.0 {
            self.slowest = (took, what.to_owned());
        }
        if took > bound {
            self.failures.push(format!("{what}: {took:?}, over {bound:?}"));
        }
    }

    /// One case on both: `gpu_baseline` says whether the effect as reset
    /// stays on the GPU, so a fallback here is the value's.
    fn case(&mut self, what: &str, effect: &BuiltinEffect, size: (u32, u32), bound: Duration, gpu_baseline: bool) {
        let seq = sequence(effect, size);
        let start = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| self.cpu.render(&seq, tc(1.0), size.0, size.1, &mut Chart, RenderOptions::default()))) {
            Err(_) => self.failures.push(format!("{what} (CPU): panicked")),
            Ok(px) => {
                if let Some(at) = px.iter().position(|v| !v.is_finite()) {
                    self.failures.push(format!("{what} (CPU): pixel value {} at {at} isn't a number", px[at]));
                }
            }
        }
        self.time(start.elapsed(), bound, &format!("{what} (CPU)"));
        if !self.gpu_ok {
            return;
        }
        let start = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| self.gpu.render_display_u8_on_gpu(&seq, tc(1.0), size.0, size.1, &mut Chart, RenderOptions::default()))) {
            Err(_) => self.failures.push(format!("{what} (GPU): panicked")),
            Ok(None) if gpu_baseline => self.failures.push(format!("{what} (GPU): the stack declined it: {}", self.gpu.last_decline().unwrap_or("no reason given"))),
            Ok(_) => {
                let fell = klipp_effects::residency::last_frame();
                if gpu_baseline && fell.fallback_count() > 0 {
                    self.failures.push(format!("{what} (GPU): fell back: {:?}", fell.events));
                }
            }
        }
        self.time(start.elapsed(), bound, &format!("{what} (GPU)"));
    }
}

#[test]
fn every_effect_survives_hostile_values() {
    let (validation, oom) = (klipp_effects::gpu_context::validation_errors(), klipp_effects::gpu_context::out_of_memory_errors());
    let mut run = Run::new();
    let types = stressed();
    let (mut cases, mut on_gpu) = (0, 0);
    for t in &types {
        let baseline = run.gpu_ok && run.gpu_holds(&(t.sample)());
        on_gpu += usize::from(baseline);
        for d in numbers(t) {
            let (lo, hi) = d.hard().expect("filtered");
            let values: [(&str, serde_json::Value); 8] = [
                ("not a number", serde_json::Value::Null),
                ("overflow", serde_json::json!(1e300)),
                ("overflow down", serde_json::json!(-1e300)),
                ("huge", serde_json::json!(1e30)),
                ("huge negative", serde_json::json!(-1e30)),
                ("zero", serde_json::json!(0.0)),
                ("its minimum", serde_json::json!(lo)),
                ("its maximum", serde_json::json!(hi)),
            ];
            for (name, v) in values {
                let what = format!("{}.{} {name}", t.tag, d.key);
                let v = if d.kind == ParamKind::Whole { v.as_f64().map_or(v.clone(), |n| serde_json::json!(n.clamp(-1e18, 1e18).round() as i64)) } else { v };
                match hostile(t, &|p| (p.key == d.key).then(|| v.clone())) {
                    Ok(effect) => run.case(&what, &effect, SMALL, SMALL_BOUND, baseline),
                    Err(e) => run.failures.push(format!("{what}: {e}")),
                }
                cases += 1;
            }
        }
        // Every number at once at each end of its range, at full size.
        for (name, end) in [("all at their maximum", 1usize), ("all at their minimum", 0)] {
            let what = format!("{} {name}", t.tag);
            let pick = |d: &ParamDesc| {
                let (lo, hi) = d.hard()?;
                let v = if end == 1 { hi } else { lo };
                Some(if d.kind == ParamKind::Whole { serde_json::json!(v.round() as i64) } else { serde_json::json!(v) })
            };
            match hostile(t, &pick) {
                Ok(effect) => run.case(&what, &effect, FULL, FULL_BOUND, baseline),
                Err(e) => run.failures.push(format!("{what}: {e}")),
            }
            cases += 1;
        }
    }
    eprintln!("{cases} cases over {} effects, {on_gpu} of them on the GPU stack; slowest {:?} ({})", types.len(), run.slowest.0, run.slowest.1);
    let validation = klipp_effects::gpu_context::validation_errors() - validation;
    let oom = klipp_effects::gpu_context::out_of_memory_errors() - oom;
    if validation > 0 {
        run.failures.push(format!("{validation} GPU validation errors"));
    }
    if oom > 0 {
        run.failures.push(format!("{oom} GPU out-of-memory errors"));
    }
    assert!(types.len() >= 60, "only {} effects stressed", types.len());
    assert!(!run.gpu_ok || on_gpu >= types.len() / 2, "only {on_gpu} of {} effects stay on the GPU stack as reset", types.len());
    assert!(run.failures.is_empty(), "{} failures:\n{}", run.failures.len(), run.failures.join("\n"));
}
