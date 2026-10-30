//! Template flattening and nested compositions (#3073).
//!
//! Two families of scenarios:
//!
//! - `s20_flatten_*`: the time `flatten_sequence` takes for a timeline of
//!   1, 10, 50 and 200 placed instances of one composition template,
//!   with the template's text and colour left as they are or overridden
//!   on every instance. It runs on the CPU, before every render.
//! - `s21_nest_*`: a frame of a precomposition nested 1 to 4 levels deep,
//!   at 1080p and 4K, each level holding the one below plus a title and a
//!   graded, blurred picture of its own; placed plainly or as a template
//!   instance with its title overridden at every level. Timed as the
//!   viewer's frame (composite plus display encode on the GPU), with the
//!   process's own GPU memory read from `nvidia-smi` before the first
//!   frame and after the last.
//!
//! `cargo run --release -p klipp-perf -- --match s2` runs them all, each
//! in a process of its own.

use std::sync::Arc;
use std::time::Instant;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, SequenceId, TemplateInstance, TemplateValue};
use klipp_effects::gpu_context::GpuContext;
use klipp_timeline::flatten::flatten_sequence;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

use crate::stats::{summarise, ScenarioResult};

const FRAMES: usize = 60;
const WARMUP: usize = 10;
const WARMUP_MIN_MS: f64 = 750.0;
/// A frame at 30 fps: real-time playback.
const REALTIME_MS: f64 = 33.3;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 30)
}

/// This process's GPU memory in MB, from `nvidia-smi`; `None` where it
/// can't be read.
fn own_vram_mb() -> Option<f64> {
    let o = std::process::Command::new("nvidia-smi").args(["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"]).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let me = std::process::id().to_string();
    let found: Vec<f64> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split(',').map(str::trim);
            (f.next()? == me).then(|| f.next()?.parse::<f64>().ok()).flatten()
        })
        .collect();
    (!found.is_empty()).then(|| found.iter().sum())
}

/// Every media clip shows the same picture, wherever it is nested: a
/// precomposition's clips are given ids of their own when flattened, so
/// a source keyed by clip id would find none of them. On the GPU run the
/// picture is a decoded frame already on the GPU, as the engine gives
/// one: a frame uploaded from the CPU for every level cost more than the
/// nesting being measured (132 MB a level at UHD).
struct AnyMedia(crate::composite::Make);

impl FrameSource for AnyMedia {
    fn frame(&mut self, clip: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        matches!(clip.kind, ClipKind::Media { .. }).then(|| SourceFrame { pixels: (self.0)(), width, height, pts: None })
    }
}

/// The picture every level shows: on the GPU when `ctx` is given.
fn picture(width: u32, height: u32, ctx: Option<&GpuContext>) -> crate::composite::Make {
    let bytes = crate::composite::picture(width, height);
    match ctx {
        Some(ctx) => crate::composite::decoded_on_gpu(ctx, bytes),
        None => {
            let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
            Box::new(move || SourcePixels::Linear(Arc::clone(&linear)))
        }
    }
}

fn title(text: &str, y: f32) -> ClipKind {
    let tb = klipp_timeline::TextBox { text: text.into(), font_size: 64.0, ..Default::default() };
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() {
        o.transform.center.y = y;
    }
    ClipKind::Title(td)
}

fn track(kind: TrackKind, clip: Clip) -> Track {
    let mut t = Track::new("T", kind);
    t.clips = vec![clip];
    t
}

/// A picture with a three-node grade and a blur on it.
fn graded_picture(secs: f64) -> Clip {
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(secs), tc(0.0), tc(secs));
    let mut g = klipp_nodes::NodeGraph::default();
    let corrector = g.ensure_corrector();
    if let Some(n) = g.nodes.iter_mut().find(|n| n.id == corrector) {
        n.params.exposure = Some(0.2);
        n.params.saturation = Some(1.2);
    }
    c.set_grade(g);
    c.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 })]).expect("a blur is a placed edit").expect("the blur lands on the picture");
    c
}

/// `scene` placed for `secs`: as a plain precomposition, or as a template
/// instance with its title's text and colour overridden.
fn placed(scene: &Sequence, at: f64, secs: f64, overrides: bool, n: usize) -> Clip {
    let mut c = Clip::precomposition(scene.id, tc(at), tc(at + secs));
    c.source_out = tc(secs);
    if overrides {
        let instance = TemplateInstance {
            template_id: SequenceId::new().0,
            bindings: [("text".to_string(), "object:0.text".to_string()), ("color".to_string(), "object:0.color".to_string())].into_iter().collect(),
            overrides: [("text".to_string(), TemplateValue::Text(format!("Card {n}"))), ("color".to_string(), TemplateValue::Color([1.0, 0.8, 0.2, 1.0]))].into_iter().collect(),
            media_slots: Default::default(),
            links: Vec::new(),
        };
        c.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
    }
    c
}

fn scene(width: u32, height: u32, tracks: Vec<Track>) -> Sequence {
    let mut s = Sequence::new_default();
    s.resolution = klipp_core::Resolution { width, height };
    s.tracks = tracks;
    s
}

/// One level of a nest: a title over a graded, blurred picture.
fn leaf(width: u32, height: u32, secs: f64, level: usize) -> Sequence {
    scene(width, height, vec![track(TrackKind::Text, Clip { kind: title(&format!("Level {level}"), 0.2), ..Clip::gap(tc(0.0), tc(secs)) }), track(TrackKind::Video, graded_picture(secs))])
}

/// A precomposition nested `depth` levels deep: the innermost a leaf,
/// each level above its own title and picture with the one below placed
/// over them at the top; the top level placed on the timeline. The
/// sequence flattened, as the viewer renders it.
pub fn nest(depth: usize, width: u32, height: u32, overrides: bool) -> Sequence {
    let secs = 10.0;
    let mut levels = vec![leaf(width, height, secs, 1)];
    for k in 1..depth {
        let mut s = leaf(width, height, secs, k + 1);
        s.tracks.insert(0, track(TrackKind::Video, placed(&levels[k - 1], 0.0, secs, overrides, k)));
        levels.push(s);
    }
    let top = scene(width, height, vec![track(TrackKind::Video, placed(&levels[depth - 1], 0.0, secs, overrides, depth))]);
    let mut all = levels;
    all.push(top.clone());
    flatten_sequence(&top, &all)
}

/// A timeline of `n` instances of one template, a second each, and the
/// project's sequences.
pub fn instances(n: usize, overrides: bool) -> (Sequence, Vec<Sequence>) {
    let template = leaf(1920, 1080, 1.0, 1);
    let clips: Vec<Clip> = (0..n).map(|i| placed(&template, i as f64, 1.0, overrides, i)).collect();
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = clips;
    let top = scene(1920, 1080, vec![v]);
    let all = vec![template, top.clone()];
    (top, all)
}

fn flatten_run(n: usize, overrides: bool) -> ScenarioResult {
    let (top, all) = instances(n, overrides);
    let mut samples = Vec::with_capacity(WARMUP + FRAMES);
    for _ in 0..WARMUP + FRAMES {
        let t0 = Instant::now();
        let flat = flatten_sequence(&top, &all);
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(flat.tracks.len());
    }
    let name = format!("s20_flatten_{n}_instances{}", if overrides { "_overrides" } else { "" });
    // Flattening runs on every change the viewer takes in: well inside a
    // frame.
    let mut result = summarise(&name, 1920, 1080, &samples, WARMUP, 16.7);
    result.warmup_frames = WARMUP;
    result
}

fn nest_run(depth: usize, width: u32, height: u32, overrides: bool, cpu_only: bool) -> ScenarioResult {
    let name = format!("s21_nest_{depth}_levels_{}{}", if width >= 3840 { "uhd" } else { "1080p" }, if overrides { "_overrides" } else { "" });
    let ctx = if cpu_only { None } else { GpuContext::shared() };
    let before = own_vram_mb();
    let seq = nest(depth, width, height, overrides);
    let mut src = AnyMedia(picture(width, height, ctx));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(ctx.is_some());
    let mut fell_back = 0usize;
    let mut frame = |r: &mut Renderer, i: usize, count: bool, fell_back: &mut usize| {
        let t = tc(1.0 + (i % 240) as f64 / 30.0);
        if ctx.is_some() {
            if let Some((px, _)) = r.render_display_u8_on_gpu(&seq, t, width, height, &mut src, RenderOptions::default()) {
                std::hint::black_box(px.len());
                return;
            }
            if count {
                *fell_back += 1;
            }
        }
        let linear = r.render(&seq, t, width, height, &mut src, RenderOptions::default());
        std::hint::black_box(linear.len());
    };
    let warm = Instant::now();
    let mut i = 0;
    while i < WARMUP || warm.elapsed().as_secs_f64() * 1000.0 < WARMUP_MIN_MS {
        frame(&mut r, i, false, &mut fell_back);
        i += 1;
    }
    let mut samples = Vec::with_capacity(FRAMES);
    for k in 0..FRAMES {
        let t0 = Instant::now();
        frame(&mut r, i + k, true, &mut fell_back);
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let after = own_vram_mb();
    let label = if fell_back > 0 { format!("{name} (CPU fallback {fell_back}/{FRAMES})") } else { name };
    let mut result = summarise(&label, width, height, &samples, 0, REALTIME_MS);
    result.warmup_frames = i;
    result.gpu_mem_mb = after;
    eprintln!(
        "{}: GPU memory {} MB at the end{}",
        result.scenario,
        after.map_or("?".into(), |m| format!("{m:.0}")),
        match (before, after) {
            (Some(b), Some(a)) => format!(", {:+.0} MB over the frames' start", a - b),
            _ => String::new(),
        }
    );
    result
}

macro_rules! flatten_scenarios {
    ($($f:ident => $n:expr, $o:expr;)*) => {$(
        pub fn $f(_: bool) -> ScenarioResult { flatten_run($n, $o) }
    )*};
}

macro_rules! nest_scenarios {
    ($($f:ident => $d:expr, $w:expr, $h:expr, $o:expr;)*) => {$(
        pub fn $f(cpu_only: bool) -> ScenarioResult { nest_run($d, $w, $h, $o, cpu_only) }
    )*};
}

flatten_scenarios! {
    flatten_1 => 1, false; flatten_10 => 10, false; flatten_50 => 50, false; flatten_200 => 200, false;
    flatten_1_o => 1, true; flatten_10_o => 10, true; flatten_50_o => 50, true; flatten_200_o => 200, true;
}

nest_scenarios! {
    nest_1_hd => 1, 1920, 1080, false; nest_2_hd => 2, 1920, 1080, false; nest_3_hd => 3, 1920, 1080, false; nest_4_hd => 4, 1920, 1080, false;
    nest_1_uhd => 1, 3840, 2160, false; nest_2_uhd => 2, 3840, 2160, false; nest_3_uhd => 3, 3840, 2160, false; nest_4_uhd => 4, 3840, 2160, false;
    nest_1_hd_o => 1, 1920, 1080, true; nest_2_hd_o => 2, 1920, 1080, true; nest_3_hd_o => 3, 1920, 1080, true; nest_4_hd_o => 4, 1920, 1080, true;
    nest_1_uhd_o => 1, 3840, 2160, true; nest_2_uhd_o => 2, 3840, 2160, true; nest_3_uhd_o => 3, 3840, 2160, true; nest_4_uhd_o => 4, 3840, 2160, true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scenes are what they say: a nest of `depth` levels keeps one
    /// precomposition per level, the innermost holding the picture; a
    /// timeline holds its `n` instances; overrides land on the title.
    #[test]
    fn the_scenes_are_built_as_described() {
        for depth in 1..=4 {
            let flat = nest(depth, 320, 180, true);
            let mut levels = 0;
            let mut at: Option<Arc<Sequence>> = flat.tracks[0].clips[0].precomp.clone();
            while let Some(s) = at {
                levels += 1;
                at = s.all_clips().find_map(|c| c.precomp.clone());
            }
            assert_eq!(levels, depth, "{depth} levels nested");
        }
        let (top, all) = instances(10, true);
        assert_eq!(top.tracks[0].clips.len(), 10);
        let flat = flatten_sequence(&top, &all);
        let inner = flat.tracks[0].clips[3].precomp.clone().expect("the instance's scene");
        let text = inner.all_clips().find_map(|c| match &c.kind {
            ClipKind::Title(t) => t.text().lines().next().map(str::to_owned),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("Card 3"), "the override reaches the title");
    }
}
