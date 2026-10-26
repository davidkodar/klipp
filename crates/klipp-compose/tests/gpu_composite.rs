//! The GPU stack's frame is the CPU renderer's frame: the same layers,
//! blends and opacities, encoded to display bytes, within a level.

#[path = "support/cpu_reference.rs"]
mod cpu_reference;
use cpu_reference::{no_gpu, CpuReference};
use klipp_timeline::GradeAccess;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture(seed: f32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[(fx + seed).fract() * 0.8, fy * 0.6, ((fx + fy) * 0.5 + seed).fract() * 0.7, 1.0]);
        }
    }
    Arc::new(v)
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(
        linear,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    )
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// Two video layers with a blend mode and opacity, a title, a generator.
fn stack() -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Stack".into(), font_size: 160.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    let mut gen = Track::new("Gen", TrackKind::Video);
    let mut g = clip(ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 80, 20, 255] }), 0.0, 5.0);
    g.blend_mode = klipp_core::BlendMode::Multiply;
    g = TestClip::of(g).effects([klipp_effects::BuiltinEffect::Opacity { value: 0.4 }]).build();
    gen.clips = vec![g];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    t.blend_mode = klipp_core::BlendMode::Screen;
    t = TestClip::of(t).effects([klipp_effects::BuiltinEffect::Opacity { value: 0.5 }]).build();
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(t.id, picture(0.3));
    src.0.insert(b.id, picture(0.0));
    top.clips = vec![t];
    base.clips = vec![b];
    seq.tracks = vec![text, gen, top, base];
    (seq, src)
}

#[test]
fn the_gpu_stack_is_the_renderers_frame() {
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "GPU stack differs from the CPU renderer by {d} levels");
}

/// #1279: in an ACEScg or a BT.2020 project the stack's frame is the CPU
/// renderer's, through the project's own display transform: the title
/// and the generator, authored in BT.709, placed into the working space as
/// the CPU puts them there, the blends in it, and the project's encode at
/// the end. The bar sees a working space: the same layers in Rec.709 are
/// another picture.
#[test]
fn the_gpu_stack_is_the_renderers_frame_in_a_wide_working_space() {
    use klipp_core::WorkingColorspace as Ws;
    let (seq, mut src) = stack();
    let shown = |linear: &[f32], ws: Ws| klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
    let mut rec709 = renderer();
    let narrow = rec709.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
    for ws in [Ws::AcesCG, Ws::Bt2020Linear] {
        let mut r = renderer();
        r.set_working_colorspace(ws);
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
            if r.last_decline().is_none() {
                no_gpu("no usable GPU");
                return;
            }
            panic!("{ws:?}: the GPU stack declined it: {:?}", r.last_decline());
        };
        assert_eq!(r.last_decline(), None, "{ws:?}: taken by the stack");
        let cpu = shown(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()), ws);
        let d = worst(&cpu, &gpu);
        assert!(d <= 1, "{ws:?}: the GPU stack differs from the CPU renderer by {d} levels");
        let off = worst(&shown(&narrow, ws), &gpu);
        assert!(off > 1, "{ws:?}: the Rec.709 picture is only {off} levels off: the bar wouldn't see the working space");
    }
}

#[test]
fn the_gpu_stack_declines_what_it_cannot_do_yet() {
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let hdr = RenderOptions { hdr_peak_nits: Some(1000.0), ..Default::default() };
    assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, hdr).is_none());
}

/// The stack asks the clip's picture whether it holds a frei0r plug-in,
/// not its v1 list (M4 step 12a): a clip whose list the test cleared but
/// whose picture keeps the plug-in is still declined. Read from the list,
/// the stack would have taken it and left the plug-in out.
#[test]
fn the_stack_reads_the_frei0r_plug_in_from_the_picture() {
    let frei0r = klipp_effects::BuiltinEffect::Frei0r { type_id: "frei0r:/x/invert0r.so:invert0r".into(), params: vec![] };
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    if r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none() {
        no_gpu("the GPU stack declined the plain stack (no usable GPU?)");
        return;
    }
    let mut diverged = seq.clone();
    let base = diverged.tracks.last_mut().and_then(|t| t.clips.first_mut()).expect("the base clip");
    *base = klipp_timeline::test_clip::TestClip::of(base.clone()).effect(frei0r).build();
    base.diverge_fields_for_test(|effects, enabled, _| {
        effects.clear();
        enabled.clear();
    });
    assert!(base.builtin_effects().is_empty(), "the list as the test left it");
    assert!(r.render_display_u8_on_gpu(&diverged, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none(), "the picture's frei0r on the stack");
    assert_eq!(r.last_decline(), Some("frei0r plug-in"));
}

/// The stack has no frei0r: a frame with a frei0r plug-in, on a clip or
/// on an adjustment layer, is declined (counted, and said), so the
/// timeline render, where the plug-in is a node, draws it. Run on the
/// stack, the plug-in would have been left out (M4 step 5).
#[test]
fn the_gpu_stack_declines_a_frei0r_plug_in() {
    let frei0r = || klipp_effects::BuiltinEffect::Frei0r { type_id: "frei0r:/x/invert0r.so:invert0r".into(), params: vec![] };
    let (seq, mut src) = stack();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    if r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none() {
        no_gpu("the GPU stack declined the plain stack (no usable GPU?)");
        return;
    }
    // On a clip.
    let mut on_clip = seq.clone();
    let base = on_clip.tracks.last_mut().and_then(|t| t.clips.first_mut()).expect("the base clip");
    base.set_effect_list(vec![frei0r()], vec![true]);
    assert!(r.render_display_u8_on_gpu(&on_clip, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none(), "a clip's frei0r on the stack");
    assert_eq!(r.last_decline(), Some("frei0r plug-in"));
    // On an adjustment layer over the stack.
    let mut adjusted = seq.clone();
    let mut adj = Track::new("Adj", TrackKind::Video);
    let mut layer = clip(ClipKind::AdjustmentLayer, 0.0, 5.0);
    layer.set_effect_list(vec![frei0r()], vec![true]);
    adj.clips = vec![layer];
    adjusted.tracks.insert(0, adj);
    assert!(r.render_display_u8_on_gpu(&adjusted, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none(), "an adjustment layer's frei0r on the stack");
    assert_eq!(r.last_decline(), Some("frei0r plug-in"));
}

/// A frame with something for every effect to act on: gradients across
/// and down, a band of highlights up to 4 (glows, bloom and soft clips
/// gate on light above 1), patches of saturated primaries and secondaries,
/// a hard checker (edges for sharpening, streaks and keys), a band at
/// half alpha (an alpha-over has something to show through), and clear
/// margins down each side.
fn rich() -> Arc<Vec<f32>> {
    let patches: [[f32; 3]; 6] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 0.0, 1.0]];
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let mut px = [fx * 0.9, fy * 0.7, (1.0 - fx) * 0.6, 1.0];
            if fy < 0.2 {
                // Highlights: a ramp to 4 across the top.
                let l = 0.5 + fx * 3.5;
                px = [l, l * 0.9, l * 0.8, 1.0];
            } else if (0.4..0.55).contains(&fy) {
                let p = patches[((fx * 6.0) as usize).min(5)];
                px = [p[0], p[1], p[2], 1.0];
            } else if (0.65..0.8).contains(&fy) {
                let on = ((x / 8) + (y / 8)) % 2 == 0;
                let c = if on { 0.95 } else { 0.05 };
                px = [c, c, c, 1.0];
            }
            if fy > 0.9 {
                px[3] = 0.5;
            }
            // Transparent margins down each side: a shadow or a glow
            // around the picture has empty pixels to draw into.
            if !(24..W - 24).contains(&x) {
                px = [0.0, 0.0, 0.0, 0.0];
            }
            v.extend_from_slice(&px);
        }
    }
    Arc::new(v)
}

/// The probe's sequence: one clip of [`rich`] on top of a dark backdrop,
/// so a change isn't diluted by layers over it, and an effect that draws
/// outside the picture (a shadow) or through it (opacity) has something to
/// draw over. The probed clip is the top track's.
fn probe_stack() -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut top = Track::new("V2", TrackKind::Video);
    let mut back = Track::new("V1", TrackKind::Video);
    let t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(t.id, rich());
    src.0.insert(b.id, Arc::new(picture(0.6).iter().enumerate().map(|(i, v)| if i % 4 == 3 { *v } else { v * 0.3 }).collect()));
    top.clips = vec![t];
    back.clips = vec![b];
    seq.tracks = vec![top, back];
    (seq, src)
}

/// The probed clip of [`probe_stack`]'s sequence.
fn probed(seq: &mut Sequence) -> &mut Clip {
    seq.tracks.first_mut().and_then(|t| t.clips.first_mut()).expect("the probed clip")
}

/// The values a catalog parameter is tried at, strongest first: near each
/// end of its soft range (its range without one), then nearer the middle.
fn tries(p: &klipp_effects::registry::ParamDesc) -> Vec<f32> {
    let (lo, hi) = p.soft.or(p.range).unwrap_or((p.default - 1.0, p.default + 1.0));
    [0.95, 0.05, 0.8, 0.2].iter().map(|s| lo + (hi - lo) * s).filter(|v| (v - p.default).abs() > 1e-6).collect()
}

/// `t`'s settings to try through its saved form: every number at once,
/// then one parameter at a time, strongest first; the sample last.
fn effect_tries(t: &klipp_effects::registry::EffectType) -> Vec<klipp_effects::BuiltinEffect> {
    // A LUT reads a file and a transform converts between named spaces:
    // not numbers to turn up (#1746).
    match t.tag {
        "lut_3d" => return vec![klipp_effects::BuiltinEffect::Lut3D { path: rotating_lut().to_string_lossy().into_owned(), opacity: 1.0, tetrahedral: true, prepared: None }],
        "colour_space_transform" => {
            return vec![klipp_effects::BuiltinEffect::ColourSpaceTransform { input_gamut: 2, input_curve: 1, output_gamut: 1, output_curve: 0, prepared: None }];
        }
        _ => {}
    }
    let mut out = Vec::new();
    // Every number at once, at 80% of its range: an effect that needs two
    // settings together (a shadow's distance and its opacity) acts.
    let mut all = serde_json::to_value((t.sample)()).expect("an effect serialises");
    for p in t.desc.params.iter().filter(|p| p.kind == klipp_effects::registry::ParamKind::Float) {
        let (lo, hi) = p.soft.or(p.range).unwrap_or((p.default - 1.0, p.default + 1.0));
        let to = serde_json::json!(lo + (hi - lo) * 0.8);
        match (p.index, all.get_mut(p.field)) {
            (Some(i), Some(serde_json::Value::Array(a))) if i < a.len() => a[i] = to,
            (None, Some(field)) if field.is_number() => *field = to,
            _ => {}
        }
    }
    if let Ok(e) = serde_json::from_value(all) {
        out.push(e);
    }
    for p in t.desc.params.iter().filter(|p| p.kind == klipp_effects::registry::ParamKind::Float) {
        for value in tries(p) {
            let mut v = serde_json::to_value((t.sample)()).expect("an effect serialises");
            let to = serde_json::json!(value);
            let set = match (p.index, v.get_mut(p.field)) {
                (Some(i), Some(serde_json::Value::Array(a))) if i < a.len() => {
                    a[i] = to;
                    true
                }
                (None, Some(field)) if field.is_number() => {
                    *field = to;
                    true
                }
                _ => false,
            };
            if let (true, Ok(e)) = (set, serde_json::from_value(v)) {
                out.push(e);
            }
        }
    }
    out.push((t.sample)());
    out
}

/// A Transform that shrinks the picture to 60%, so an effect that draws
/// beyond it (a shadow, a glow's spill) has room to.
fn shrink() -> klipp_effects::BuiltinEffect {
    klipp_effects::BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

/// The kinds the probe can't show changing in a one-clip fixture, and
/// why. Held exactly: a kind that starts drawing here must leave the list,
/// and a new kind that can't must join it with its reason.
const UNPROVEN: &[(&str, &str)] = &[
    ("effect klipp.fx.temporal_denoise", "compares a frame with its neighbours; the fixture's source is one still frame"),
    ("effect klipp.fx.speed_ramp", "blends toward the media's next frame; the fixture's source is one still frame"),
    ("effect klipp.fx.deflicker", "draws from a deflicker analysis of the media, which the fixture has none of"),
    ("effect klipp.fx.video_stabilization", "warps by a stabilisation analysis, which the fixture has none of"),
    ("effect klipp.fx.rolling_shutter_correction", "warps by a motion analysis, which the fixture has none of"),
    ("effect klipp.fx.grade_compound", "a marker: alone, with no compound's grade beside it, it runs nothing; a clip with a grade compound is declined by the stack (\"grade compound\") and drawn in grade_compound_render"),
    ("effect klipp.fx.merge", "lays the picture wired into its fg port over; the probe's effect list wires none, so alone it passes the picture (drawn with one in merge_render)"),
    ("grade node Reroute", "passes its input on by design"),
    ("grade node InputTransform", "records the source's format; the decoder has already decoded it, so it passes the picture on"),
    ("grade node Mask", "makes a matte its consumers read; alone in a chain it draws the picture as it is"),
    ("grade node DifferenceKey", "keys against a clean-plate file the fixture has none of"),
];

/// A grade node's settings that make it act, for a kind whose settings
/// aren't plain numbers; `None` for the rest, which are tried number by
/// number.
fn node_setup(kind: klipp_nodes::NodeKind, lut: &std::path::Path) -> Option<klipp_nodes::NodeParams> {
    use klipp_core::color_grade::{ColorMatchSettings, CurveSet, FilmGrainSettings, HueSatCurves, LayerMixerInput, SoftClipSettings};
    use klipp_nodes::{NodeKind as K, NodeParams};
    let p = NodeParams::default();
    Some(match kind {
        K::RGBCurves => NodeParams { curves: Some(CurveSet { master: vec![[0.0, 0.0], [0.4, 0.7], [1.0, 1.0]], ..CurveSet::default() }), ..p },
        K::HueSatCurves => NodeParams { huesat: Some(HueSatCurves { hue_vs_sat: vec![[0.0, 0.1], [0.5, 0.1], [1.0, 0.1]], ..HueSatCurves::default() }), ..p },
        K::SoftClip => NodeParams { soft_clip: Some(SoftClipSettings { enabled: true, threshold: 0.6, knee: 0.2 }), ..p },
        K::FilmGrain => NodeParams { grain: Some(FilmGrainSettings { enabled: true, amount: 0.6, ..FilmGrainSettings::default() }), ..p },
        K::Lut3D => NodeParams { lut_path: Some(lut.to_path_buf()), lut_opacity: Some(1.0), ..p },
        K::PrimariesMatrix => NodeParams { primaries_from: Some(klipp_core::WorkingColorspace::Rec709Linear), primaries_to: Some(klipp_core::WorkingColorspace::Bt2020Linear), ..p },
        K::ColorMatch => NodeParams { color_match: Some(ColorMatchSettings { exposure: 0.7, ..ColorMatchSettings::default() }), ..p },
        K::LayerMixer => NodeParams {
            layer_mixer: Some(klipp_nodes::LayerMixerParams {
                // Half of input 0 over all of input 1: a mix of the two,
                // whichever is drawn on top.
                inputs: vec![LayerMixerInput { opacity: 0.5, ..LayerMixerInput::default() }, LayerMixerInput::default()],
            }),
            ..p
        },
        K::Compound => {
            // Inside: Source → a stop up → Output.
            let mut bright = NodeParams::default();
            bright.apply_override("exposure", 1.0);
            let (source, up, output) = (node(K::Source, NodeParams::default()), node(K::Exposure, bright), node(K::Output, NodeParams::default()));
            let edges = vec![
                klipp_nodes::Edge { from_node: source.id, from_port: 0, to_node: up.id, to_port: 0 },
                klipp_nodes::Edge { from_node: up.id, from_port: 0, to_node: output.id, to_port: 0 },
            ];
            let inner = klipp_nodes::NodeGraph { output_node_id: output.id, nodes: vec![source, up, output], edges, ..klipp_nodes::NodeGraph::default() };
            NodeParams { compound_subgraph: Some(inner), ..p }
        }
        _ => return None,
    })
}

/// A 2-point LUT that sends each channel to the next (R from G, G from
/// B, B from R), written for the Lut3D probe.
fn rotating_lut() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("klipp-probe-rotate-{}.cube", std::process::id()));
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                text.push_str(&format!("{g} {b} {r}\n"));
            }
        }
    }
    std::fs::write(&path, text).expect("the probe LUT written");
    path
}

fn node(kind: klipp_nodes::NodeKind, params: klipp_nodes::NodeParams) -> klipp_nodes::Node {
    klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A grade node's settings to try: its own setup when it has one
/// ([`node_setup`]), else one keyframable number at a time, strongest
/// first.
fn node_tries(t: &klipp_nodes::registry::NodeType, lut: &std::path::Path) -> Vec<klipp_nodes::NodeParams> {
    if let Some(p) = node_setup(t.kind, lut) {
        return vec![p];
    }
    let mut out = Vec::new();
    for p in t.desc.params.iter().filter(|p| p.kind == klipp_effects::registry::ParamKind::Float && p.keyframable) {
        for value in tries(p) {
            let mut params = klipp_nodes::NodeParams::default();
            params.apply_override(p.key, value);
            out.push(params);
        }
    }
    out.push(klipp_nodes::NodeParams::default());
    out
}

/// One probe's verdict: drawn by the stack as the timeline draws it, or
/// declined with a reason, both fine; anything else, what went wrong.
enum Verdict {
    Fine,
    Unshown,
    Wrong(String),
}

/// The first of `cases` (sequences with the kind set up) that changes the
/// frame from `plain`, judged: the stack draws it within a level of the
/// timeline render, or declines it with a reason.
fn judge(cases: impl IntoIterator<Item = Sequence>, plain: &[u8], src: &mut Flat) -> Verdict {
    for s in cases {
        let mut r = renderer();
        let cpu = display(&r.cpu_render(&s, tc(1.0), W, H, src, RenderOptions::default()));
        if worst(&cpu, plain) <= 1 {
            continue;
        }
        return match r.render_display_u8_on_gpu(&s, tc(1.0), W, H, src, RenderOptions::default()) {
            Some((gpu, _)) => match worst(&cpu, &gpu) {
                d if d > 1 => Verdict::Wrong(format!("the stack drew it {d} levels from the timeline render")),
                _ => Verdict::Fine,
            },
            None if r.last_decline().is_none() => Verdict::Wrong("declined with no reason".into()),
            None => Verdict::Fine,
        };
    }
    Verdict::Unshown
}

/// Every probe's outcome against [`UNPROVEN`]: anything wrong fails, and
/// the unshown set must be the list, exactly.
fn hold(wrong: Vec<String>, unshown: Vec<String>, probed: &[String]) {
    let pinned: Vec<&str> = UNPROVEN.iter().map(|(k, _)| *k).filter(|k| probed.iter().any(|p| p == k)).collect();
    let newly: Vec<&String> = unshown.iter().filter(|u| !pinned.contains(&u.as_str())).collect();
    let now_drawn: Vec<&&str> = pinned.iter().filter(|k| !unshown.iter().any(|u| u == *k)).collect();
    assert!(wrong.is_empty(), "drawn by the stack other than the timeline draws it:\n{}", wrong.join("\n"));
    assert!(newly.is_empty(), "not shown to change anything, and not listed in UNPROVEN with a reason: {newly:?}");
    assert!(now_drawn.is_empty(), "listed in UNPROVEN but now shown to change the frame; take them off: {now_drawn:?}");
}

/// Nothing the GPU stack draws is silently absent (M4 step 5). For every
/// catalog effect, and every single-input grade node, on a clip of a rich
/// frame over a backdrop, set so it changes the frame, the stack either
/// draws the frame the timeline render draws (within a level), or declines
/// it with a reason. A new kind the stack can't run fails here, not in the
/// viewer.
///
/// The control: each kind is set until the timeline render differs from
/// the plain one by more than a level; a stack that left it out would
/// otherwise look the same as one that drew it. The few kinds that can't
/// change a one-clip fixture are [`UNPROVEN`], with their reasons, held
/// exactly.
///
/// Grade nodes with more than one input or output are wired whole by
/// [`every_multi_port_grade_node_is_drawn_by_the_stack_or_declined`] (#1484).
#[test]
fn every_effect_and_grade_node_is_drawn_by_the_stack_or_declined() {
    let (seq, mut src) = probe_stack();
    let mut r = renderer();
    if r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none() {
        no_gpu("the GPU stack declined the plain probe (no usable GPU?)");
        return;
    }
    let plain = display(&renderer().cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let (mut wrong, mut unshown, mut probed_names) = (Vec::new(), Vec::new(), Vec::new());
    let mut verdict = |name: String, v: Verdict| {
        match v {
            Verdict::Fine => {}
            Verdict::Unshown => unshown.push(name.clone()),
            Verdict::Wrong(why) => wrong.push(format!("{name}: {why}")),
        }
        probed_names.push(name);
    };
    // The probe shrunk to 60%: what an effect after the shrink is judged
    // against, never the full-size frame (the shrink alone changes that).
    let shrunk = {
        let mut s = seq.clone();
        probed(&mut s).set_effect_list(vec![shrink()], vec![true]);
        display(&renderer().cpu_render(&s, tc(1.0), W, H, &mut src, RenderOptions::default()))
    };
    let with = |list: Vec<klipp_effects::BuiltinEffect>| {
        let mut s = seq.clone();
        let n = list.len();
        probed(&mut s).set_effect_list(list, vec![true; n]);
        s
    };
    for t in klipp_effects::registry::effect_types().filter(|t| t.tag != "frei0r") {
        // On the full frame first; an effect that draws beyond the picture
        // (a shadow, a glow's spill) after a shrink, against the shrunk.
        let full = judge(effect_tries(t).into_iter().map(|e| with(vec![e])), &plain, &mut src);
        let v = match full {
            Verdict::Unshown => judge(effect_tries(t).into_iter().map(|e| with(vec![shrink(), e])), &shrunk, &mut src),
            v => v,
        };
        verdict(format!("effect {}", t.desc.id), v);
    }
    // frei0r with a real plug-in when one is installed (its decline by the
    // stack is held by the_gpu_stack_declines_a_frei0r_plug_in).
    match saturator() {
        Some(type_id) => {
            let mut s = seq.clone();
            probed(&mut s).set_effect_list(vec![klipp_effects::BuiltinEffect::Frei0r { type_id, params: vec![klipp_frei0r::Frei0rValue::Double(0.9)] }], vec![true]);
            verdict("effect klipp.fx.frei0r".into(), judge([s], &plain, &mut src));
        }
        None => eprintln!("UNVERIFIED: effect klipp.fx.frei0r (the saturat0r plug-in isn't installed)"),
    }
    let lut = rotating_lut();
    for t in klipp_nodes::registry::node_types().filter(|t| t.desc.inputs.len() == 1 && t.desc.outputs.len() == 1) {
        let mut cases = Vec::new();
        for params in node_tries(t, &lut) {
            let mut s = seq.clone();
            let clip = probed(&mut s);
            let source = clip.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).map(|n| n.id);
            if clip.insert_grade_node(source, node(t.kind, params)) {
                cases.push(s);
            }
        }
        let name = format!("grade node {:?}", t.kind);
        if cases.is_empty() {
            verdict(name, Verdict::Wrong("couldn't be placed".into()));
            continue;
        }
        verdict(name, judge(cases, &plain, &mut src));
    }
    hold(wrong, unshown, &probed_names);
}

/// The saturat0r frei0r plug-in's type id, registered, when installed.
fn saturator() -> Option<String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).map(|h| h.join(".frei0r-1/lib"));
    let dirs = home.into_iter().chain(["/usr/local/lib/frei0r-1", "/usr/lib/frei0r-1"].map(std::path::PathBuf::from));
    let info = dirs.map(|d| d.join("saturat0r.so")).filter(|p| p.exists()).find_map(|p| klipp_frei0r::host::scan_library(&p).into_iter().next())?;
    klipp_effects::frei0r_effect::_register_for_test(info.clone());
    Some(info.type_id())
}

/// The grade nodes [`every_effect_and_grade_node_is_drawn_by_the_stack_or_declined`]
/// can't place, each wired whole (#1484): every input fed, input 0 the
/// clip's source and every other input that source a stop brighter, the
/// first output joined to the grade's output, set so it acts. The RGB
/// splitter emits its input on every port by design, so it is probed with
/// its combiner, a stop up on the red branch. The same rule: shown to
/// change the picture, then drawn as the timeline draws it or declined
/// with a reason.
#[test]
fn every_multi_port_grade_node_is_drawn_by_the_stack_or_declined() {
    use klipp_nodes::NodeKind as K;
    let (seq, mut src) = probe_stack();
    let mut r = renderer();
    if r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none() {
        no_gpu("the GPU stack declined the plain probe (no usable GPU?)");
        return;
    }
    let plain = display(&renderer().cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let lut = rotating_lut();
    let (mut wrong, mut unshown, mut probed_names) = (Vec::new(), Vec::new(), Vec::new());
    let bright = || {
        let mut p = klipp_nodes::NodeParams::default();
        p.apply_override("exposure", 1.0);
        node(K::Exposure, p)
    };
    for t in klipp_nodes::registry::node_types().filter(|t| (t.desc.inputs.len() > 1 || t.desc.outputs.len() > 1) && t.kind != K::RGBCombiner) {
        let name = format!("grade node {:?}", t.kind);
        let mut cases = Vec::new();
        for params in node_tries(t, &lut) {
            let mut s = seq.clone();
            let placed = probed(&mut s).edit_grade(|g| {
                let source = g.nodes.iter().find(|n| n.kind == K::Source)?.id;
                let out = g.output_node_id;
                g.edges.retain(|e| e.to_node != out);
                let it = node(t.kind, params);
                let it_id = it.id;
                if t.kind == K::RGBSplitter {
                    // Splitter → (a stop up on R) → combiner.
                    let (up, comb) = (bright(), node(K::RGBCombiner, klipp_nodes::NodeParams::default()));
                    let (up_id, comb_id) = (up.id, comb.id);
                    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: it_id, to_port: 0 });
                    g.edges.push(klipp_nodes::Edge { from_node: it_id, from_port: 0, to_node: up_id, to_port: 0 });
                    g.edges.push(klipp_nodes::Edge { from_node: up_id, from_port: 0, to_node: comb_id, to_port: 0 });
                    g.edges.push(klipp_nodes::Edge { from_node: it_id, from_port: 1, to_node: comb_id, to_port: 1 });
                    g.edges.push(klipp_nodes::Edge { from_node: it_id, from_port: 2, to_node: comb_id, to_port: 2 });
                    g.edges.push(klipp_nodes::Edge { from_node: comb_id, from_port: 0, to_node: out, to_port: 0 });
                    g.nodes.extend([it, up, comb]);
                    return Some(());
                }
                let other = bright();
                let other_id = other.id;
                if t.kind == K::Output {
                    // The grade's own Output (#898): the source into its
                    // picture, a stop brighter into its alpha, read as the
                    // coverage it carries.
                    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: other_id, to_port: 0 });
                    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: out, to_port: 0 });
                    g.edges.push(klipp_nodes::Edge { from_node: other_id, from_port: 0, to_node: out, to_port: 1 });
                    g.nodes.push(other);
                    return Some(());
                }
                g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: other_id, to_port: 0 });
                for port in 0..t.desc.inputs.len() as u8 {
                    let from = if port == 0 { source } else { other_id };
                    g.edges.push(klipp_nodes::Edge { from_node: from, from_port: 0, to_node: it_id, to_port: port });
                }
                g.edges.push(klipp_nodes::Edge { from_node: it_id, from_port: 0, to_node: out, to_port: 0 });
                g.nodes.extend([other, it]);
                Some(())
            });
            if placed.is_some() {
                cases.push(s);
            }
        }
        if cases.is_empty() {
            wrong.push(format!("{name}: couldn't be wired"));
            continue;
        }
        match judge(cases, &plain, &mut src) {
            Verdict::Fine => {}
            Verdict::Unshown => unshown.push(name.clone()),
            Verdict::Wrong(why) => wrong.push(format!("{name}: {why}")),
        }
        probed_names.push(name);
    }
    hold(wrong, unshown, &probed_names);
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

/// Both paths on `seq` at 1 s, as display bytes; `None` when the GPU
/// stack declined.
fn both(seq: &Sequence, src: &mut Flat) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut r = renderer();
    let (gpu, _) = r.render_display_u8_on_gpu(seq, tc(1.0), W, H, src, RenderOptions::default())?;
    let cpu = display(&r.cpu_render(seq, tc(1.0), W, H, src, RenderOptions::default()));
    Some((cpu, gpu))
}

fn assert_match(name: &str, seq: &Sequence, src: &mut Flat) {
    let Some((cpu, gpu)) = both(seq, src) else {
        no_gpu(&format!("{name}: the GPU stack declined (no usable GPU?)"));
        return;
    };
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "{name}: GPU stack differs from the CPU renderer by {d} levels");
}

#[test]
fn a_luma_matte_with_feather_and_choke_matches() {
    let (mut seq, mut src) = stack();
    let source = seq.tracks[3].clips[0].id;
    seq.tracks[2].clips[0].matte_source = Some(klipp_timeline::MatteBinding {
        source_clip_id: source,
        channel: klipp_nodes::params::MatteChannel::Luminance,
        invert: true,
        feather_px: 3.0,
        choke_px: 2.0,
    });
    seq.tracks[3].clips[0].consumed_as_matte = true;
    assert_match("luma matte", &seq, &mut src);
}

#[test]
fn a_stencil_masks_the_layers_below_it() {
    let (mut seq, mut src) = stack();
    // The generator becomes a stencil over the two video layers.
    seq.tracks[1].clips[0].matte_stencil = Some(klipp_timeline::StencilSettings {
        channel: klipp_nodes::params::MatteChannel::Alpha,
        invert: false,
        feather_px: 0.0,
        choke_px: 0.0,
    });
    assert_match("stencil", &seq, &mut src);
}

#[test]
fn a_transition_between_two_clips_matches() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(a.id, picture(0.0));
    src.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(2.0),
        alignment: Default::default(),
        user_params: Default::default(),
    });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    assert_match("cross dissolve", &seq, &mut src);
}

/// #2668: a transition whose outgoing shot is seen through a title's
/// alpha on the track above (text-shaped video) runs on the GPU stack, as
/// the CPU composite draws it; it went to the CPU composite, 70-80 ms a
/// frame at 1080p, for every frame of the transition.
#[test]
fn a_transition_with_a_matted_side_is_drawn_on_the_gpu() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "KLIPP".into(), font_size: 200.0, ..Default::default() });
    let mut shape = clip(ClipKind::Title(td), 0.0, 2.0);
    shape.consumed_as_matte = true;
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    a.matte_source = Some(klipp_timeline::MatteBinding {
        source_clip_id: shape.id,
        channel: klipp_nodes::params::MatteChannel::Alpha,
        invert: false,
        feather_px: 2.0,
        choke_px: 0.0,
    });
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(a.id, picture(0.0));
    src.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::DipToWhite,
        duration: tc(2.0),
        alignment: Default::default(),
        user_params: Default::default(),
    });
    text.clips = vec![shape];
    v.clips = vec![a, b];
    seq.tracks = vec![text, v];
    for kind in [klipp_core::TransitionKind::DipToWhite, klipp_core::TransitionKind::CrossDissolve] {
        seq.tracks[1].transitions[0].kind = kind;
        let mut r = renderer();
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
            no_gpu(&format!("{kind:?}: the GPU stack declined: {:?}", r.last_decline()));
            return;
        };
        let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= 1, "{kind:?}: GPU stack differs from the CPU renderer by {d} levels");
    }
}

/// #2705: a clip flipped about its anchor (a negative scale in its
/// Transform) draws the same on the GPU stack as on the CPU, and is the
/// unflipped picture mirrored.
#[test]
fn a_flipped_clip_is_mirrored_on_both_paths() {
    let placed = |sx: f32, sy: f32| {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut v = Track::new("V1", TrackKind::Video);
        let c = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0))
            .effects([klipp_effects::BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0, scale_x: sx, scale_y: sy, rotation: 0.0,
                anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
            }])
            .build();
        let mut src = Flat::default();
        src.0.insert(c.id, picture(0.0));
        v.clips = vec![c];
        seq.tracks = vec![v];
        (seq, src)
    };
    // The transform maps in pixel corners: a flip about the frame's middle
    // takes column x to W - x, so the first row and column have no twin.
    let mirrored = |img: &[u8], x_axis: bool, y_axis: bool| {
        let (w, h) = (W as usize, H as usize);
        let mut out = vec![0u8; img.len()];
        for y in 1..h {
            for x in 1..w {
                let (sx, sy) = (if x_axis { w - x } else { x }, if y_axis { h - y } else { y });
                out[(y * w + x) * 4..][..4].copy_from_slice(&img[(sy * w + sx) * 4..][..4]);
            }
        }
        out
    };
    let inner = |img: &[u8]| {
        let (w, h) = (W as usize, H as usize);
        let mut v = img.to_vec();
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 {
                    v[(y * w + x) * 4..][..4].fill(0);
                }
            }
        }
        v
    };
    let (seq, mut src) = placed(0.6, 0.6);
    let plain = display(&renderer().cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    for (name, sx, sy, x_axis, y_axis) in [("flip x", -0.6, 0.6, true, false), ("flip y", 0.6, -0.6, false, true), ("flip both", -0.6, -0.6, true, true)] {
        let (seq, mut src) = placed(sx, sy);
        assert_match(name, &seq, &mut src);
        let cpu = display(&renderer().cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
        assert!(worst(&cpu, &plain) > 50, "{name}: not flipped at all");
        let d = worst(&inner(&cpu), &mirrored(&plain, x_axis, y_axis));
        assert!(d <= 1, "{name}: not the picture mirrored, {d} levels off");
    }
}

/// #985: the piece transitions, at their defaults, sliding and posed
/// (panels tilting away, halves turning, the shatter falling), with the
/// camera's shutter shut and open, are the same in the viewer (the GPU
/// stack) and the export (the CPU); the open shutter blurs the posed ones.
#[test]
fn the_piece_transitions_match() {
    use klipp_core::piece_transition::{PieceMotion, PieceParams};
    let posed = |kind, motion| PieceParams { motion, shadow: true, ..PieceParams::read(kind, &klipp_core::spec_for_kind(kind).default_params) }.to_user(kind);
    let cases = [
        (klipp_core::TransitionKind::SlideStrips, klipp_core::spec_for_kind(klipp_core::TransitionKind::SlideStrips).default_params, 0.0),
        (klipp_core::TransitionKind::SlidePanels, klipp_core::spec_for_kind(klipp_core::TransitionKind::SlidePanels).default_params, 0.0),
        (klipp_core::TransitionKind::PartHalves, klipp_core::spec_for_kind(klipp_core::TransitionKind::PartHalves).default_params, 0.0),
        (klipp_core::TransitionKind::SlidePanels, posed(klipp_core::TransitionKind::SlidePanels, PieceMotion::Tilt), 0.0),
        (klipp_core::TransitionKind::PartHalves, posed(klipp_core::TransitionKind::PartHalves, PieceMotion::Turn), 0.0),
        (klipp_core::TransitionKind::Shatter, klipp_core::spec_for_kind(klipp_core::TransitionKind::Shatter).default_params, 0.0),
        (klipp_core::TransitionKind::Shatter, klipp_core::spec_for_kind(klipp_core::TransitionKind::Shatter).default_params, 270.0),
    ];
    let mut sharp_frame = None;
    for (kind, user_params, shutter_deg) in cases {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut v = Track::new("V1", TrackKind::Video);
        let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
        let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
        let mut src = Flat::default();
        src.0.insert(a.id, picture(0.0));
        src.0.insert(b.id, picture(0.5));
        v.transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            clip_a_id: a.id,
            clip_b_id: b.id,
            kind,
            duration: tc(2.0),
            alignment: Default::default(),
            user_params,
        });
        v.clips = vec![a, b];
        seq.tracks = vec![v];
        seq.camera.shutter_deg = shutter_deg;
        seq.camera.shutter_samples = 6;
        // Part way in and past the cut: the transition runs from 1 s to 3 s.
        for t in [1.7, 2.3] {
            let mut r = renderer();
            let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
                no_gpu(&format!("{kind:?}: the GPU stack declined (no usable GPU?)"));
                return;
            };
            let cpu = display(&r.cpu_render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
            assert!(cpu != display(&picture(0.0)) && cpu != display(&picture(0.5)), "{kind:?} at {t} s: under way");
            let d = worst(&cpu, &gpu);
            assert!(d <= 1, "{kind:?} with a {shutter_deg} degree shutter at {t} s: GPU stack differs from the CPU renderer by {d} levels");
            if kind == klipp_core::TransitionKind::Shatter && t == 2.3 {
                match sharp_frame.take() {
                    None => sharp_frame = Some(cpu),
                    Some(sharp) => assert!(worst(&sharp, &cpu) > 8, "the open shutter blurs the falling shards"),
                }
            }
        }
    }
}

/// #1653: a transition's colour is picked in BT.709, as a title's is, so
/// in an ACEScg or a BT.2020 project a dip's held colour is the colour
/// picked, put into the working space, not the same numbers read as the
/// working space's; the GPU stack's dip is the CPU's.
#[test]
fn a_dip_holds_the_colour_picked_in_a_wide_working_space() {
    use klipp_core::WorkingColorspace as Ws;
    let picked = [0.9f32, 0.2, 0.1, 1.0];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(a.id, picture(0.0));
    src.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::DipToBlack,
        duration: tc(2.0),
        alignment: Default::default(),
        // Held at the colour across the middle of the dip.
        user_params: klipp_core::TransitionUserParams { param0: 0.3, color: picked, ..Default::default() },
    });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    let shown = |linear: &[f32], ws: Ws| klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
    for ws in [Ws::AcesCG, Ws::Bt2020Linear] {
        let mut r = renderer();
        r.set_working_colorspace(ws);
        let cpu = r.cpu_render(&seq, tc(2.0), W, H, &mut src, RenderOptions::default());
        let m = klipp_color::primaries::working_space_conversion_matrix(Ws::Rec709Linear, ws).unwrap();
        let want = klipp_color::primaries::matrix3_mul_vec(m, [0, 1, 2].map(|c| klipp_color::transfer::srgb_to_linear(picked[c])));
        let i = (((H / 2) * W + W / 2) * 4) as usize;
        for c in 0..3 {
            assert!((cpu[i + c] - want[c]).abs() < 2e-3, "{ws:?}: the dip holds {:?}, the colour picked is {want:?} there", &cpu[i..i + 3]);
        }
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()) else {
            eprintln!("UNVERIFIED: the GPU stack declined ({:?})", r.last_decline());
            continue;
        };
        let d = worst(&shown(&cpu, ws), &gpu);
        assert!(d <= 1, "{ws:?}: the stack's dip differs from the CPU's by {d} levels");
    }
}

#[test]
fn an_adjustment_layer_with_a_grade_and_an_effect_matches() {
    let (mut seq, mut src) = stack();
    let mut adj = Track::new("Adj", TrackKind::Video);
    let mut a = clip(ClipKind::AdjustmentLayer, 0.0, 5.0);
    a.set_grade({
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node {
            id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
        };
        let nodes = vec![
            node(NodeKind::Source, NodeParams::default()),
            node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() }),
            node(NodeKind::Output, NodeParams::default()),
        ];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    });
    a = TestClip::of(a).effects([klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }, klipp_effects::BuiltinEffect::Opacity { value: 0.7 }]).build();
    adj.clips = vec![a];
    seq.tracks.insert(1, adj);
    assert_match("adjustment layer", &seq, &mut src);
}

#[test]
fn a_title_with_an_effect_and_a_faded_layer_match() {
    let (mut seq, mut src) = stack();
    {
        let effect_clip = &mut seq.tracks[0].clips[0];
        *effect_clip = TestClip::of(effect_clip.clone()).effects([klipp_effects::BuiltinEffect::GaussianBlur { radius: 1.5 }]).build();
    }
    seq.tracks[2].opacity = 0.6;
    assert_match("title effect", &seq, &mut src);
}

/// The timeline grade over the stack: the accumulator graded after the
/// last layer, as `render_stack` grades it.
#[test]
fn a_timeline_grade_over_the_stack_matches() {
    let (mut seq, mut src) = stack();
    seq.timeline_grade_slot().set(Some({
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node {
            id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
        };
        let nodes = vec![
            node(NodeKind::Source, NodeParams::default()),
            node(NodeKind::Exposure, NodeParams { exposure: Some(-0.7), ..Default::default() }),
            node(NodeKind::Output, NodeParams::default()),
        ];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    }));
    assert_match("timeline grade", &seq, &mut src);
}

/// A frame with nothing on screen (a gap, a strobe's dark pieces) is the
/// stack's clear canvas, graded by the timeline grade and finished on the
/// GPU, where it fell back to the CPU compositor (#2377).
#[test]
fn an_empty_frame_is_drawn_and_graded_on_the_stack() {
    let (mut seq, mut src) = stack();
    let end = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_out.to_secs_f64()).fold(0.0, f64::max);
    let gap = tc(end + 0.5);
    for graded in [false, true] {
        if graded {
            seq.timeline_grade_slot().set(Some({
                use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
                let node = |kind: NodeKind, params: NodeParams| Node {
                    id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
                };
                let nodes = vec![
                    node(NodeKind::Source, NodeParams::default()),
                    node(NodeKind::ColorWheels, NodeParams { offset_master: Some(0.15), ..Default::default() }),
                    node(NodeKind::Output, NodeParams::default()),
                ];
                let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
                let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
                NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
            }));
        }
        let mut r = renderer();
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, gap, W, H, &mut src, RenderOptions::default()) else {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "graded={graded}: the stack declined an empty frame");
            return;
        };
        let events = klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "graded={graded}: {events:?}");
        let cpu = display(&r.cpu_render(&seq, gap, W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= 1, "graded={graded}: the empty frame differs from the CPU renderer by {d} levels");
    }
}

/// The timeline's LUT over the stack: applied on the accumulator before
/// the premultiply, where `render_stack` applies it.
fn test_lut() -> klipp_color::lut::LutData {
    let size = 5usize;
    let step = 1.0 / (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let (rv, gv, bv) = (r as f32 * step, g as f32 * step, b as f32 * step);
                data.extend_from_slice(&[
                    (rv * 0.5 + 0.4 * gv * bv).clamp(0.0, 1.0),
                    (gv * 0.7 + 0.2 * rv).clamp(0.0, 1.0),
                    (bv * 0.45 + 0.3 * rv * gv).clamp(0.0, 1.0),
                ]);
            }
        }
    }
    klipp_color::lut::LutData {
        size,
        data,
        input_space: klipp_core::WorkingColorspace::Rec709Linear,
        output_space: klipp_core::WorkingColorspace::Rec709Linear,
    }
}

#[test]
fn a_global_lut_over_the_stack_matches() {
    let (seq, mut src) = stack();
    let lut = test_lut();
    let mut r = renderer();
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: 0.9 }));
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "LUT over the stack: GPU differs from the CPU renderer by {d} levels");
    let mut plain = renderer();
    let (no_lut, _) = plain.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).unwrap();
    assert!(worst(&no_lut, &gpu) > 20, "the LUT is not reaching the stack's frame");
}

/// The timeline's LUT in an ACEScg project. It runs before the display
/// transform, on AP1; an untagged `.cube` is Rec.709, so the lookup is
/// bracketed as a Lut3D node's is. Both sides used to look it up on
/// sRGB-encoded AP1 (#422).
#[test]
fn a_global_lut_in_an_acescg_project_is_bracketed_on_both_sides() {
    use klipp_core::WorkingColorspace::AcesCG;
    let (seq, mut src) = stack();
    let lut = test_lut();
    let ap1_display = |linear: &[f32]| {
        klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, AcesCG, klipp_core::SdrTransfer::Srgb)
    };
    // The CPU renderer applies the LUT as the node does: the stack
    // without it, then the node's bracketed lookup. Straight alpha on
    // both, since the renderer's LUT goes on before the premultiply.
    let straight = RenderOptions { keep_alpha: true, ..Default::default() };
    // Tetrahedral on both, stated rather than left to the default the
    // expected value below assumes.
    let mut plain = renderer();
    plain.set_lut_tetrahedral(true);
    plain.set_working_colorspace(AcesCG);
    let mut want = plain.cpu_render(&seq, tc(1.0), W, H, &mut src, straight);
    klipp_color::apply_3d_lut_linear_ws(&mut want, &lut, 0.9, AcesCG, true);
    let mut r = renderer();
    r.set_lut_tetrahedral(true);
    r.set_working_colorspace(AcesCG);
    r.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: 0.9 }));
    let d = worst(&ap1_display(&want), &ap1_display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, straight)));
    assert!(d <= 1, "the CPU renderer's LUT is not the node's bracketed lookup: {d} levels");
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
    let d = worst(&ap1_display(&cpu), &gpu);
    assert!(d <= 1, "LUT in ACEScg: GPU differs from the CPU renderer by {d} levels");
}

/// A rolling title mid-roll: the resting sprite placed by the GPU over
/// the clip below, against the CPU renderer's shifted sprite.
#[test]
fn a_rolling_title_placed_on_the_gpu_matches() {
    let (mut seq, mut src) = stack();
    let td = klipp_timeline::TitleData::single_text_fx(
        klipp_timeline::TextBox { text: "Rolling\ncredits\nover the\npicture".into(), font_size: 48.0, ..Default::default() },
        klipp_timeline::LayerFx { loops: vec![klipp_timeline::TitleLoop::Scroll { direction: klipp_timeline::ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }], ..Default::default() },
    );
    seq.tracks[0].clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    for t in [1.0, 2.5, 4.0] {
        let mut r = renderer();
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
            no_gpu("the GPU stack declined (no usable GPU?)");
            return;
        };
        let cpu = display(&r.cpu_render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= 1, "rolling title at {t}s: GPU differs from the CPU renderer by {d} levels");
    }
}

/// A rolling title with an edge fade and a focus band gone soft (#592):
/// faded and blurred where it is, the same on the GPU stack as on the
/// CPU renderer an export uses.
#[test]
fn a_soft_focus_roll_on_the_gpu_matches() {
    use klipp_core::title_motion::{EdgeFade, FocusBand, RollShape};
    let (mut seq, mut src) = stack();
    let td = klipp_timeline::TitleData::single_text_fx(
        klipp_timeline::TextBox { text: "Rolling\ncredits\nover the\npicture".into(), font_size: 48.0, ..Default::default() },
        klipp_timeline::LayerFx {
            loops: vec![klipp_timeline::TitleLoop::Scroll {
                direction: klipp_timeline::ScrollDirection::Up,
                speed_px_per_sec: 0.0,
                shape: RollShape { focus: Some(FocusBand { band: 0.3, edge_opacity: 0.3, blur: 8.0 }) },
            }],
            edge_fade: EdgeFade { top: 0.15, bottom: 0.15, ..EdgeFade::NONE },
            ..Default::default()
        },
    );
    seq.tracks[0].clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    for t in [1.0, 2.5, 4.0] {
        let mut r = renderer();
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
            no_gpu("the GPU stack declined (no usable GPU?)");
            return;
        };
        let cpu = display(&r.cpu_render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= 2, "soft-focus roll at {t}s: GPU differs from the CPU renderer by {d} levels");
    }
}

/// A decoded frame on the GPU that is not the timeline's size: sRGB
/// bytes `w` by `h`, seeding by the GPU fit, read back as linear.
struct OtherSize {
    srgb: Arc<Vec<u8>>,
    w: u32,
    h: u32,
}

impl klipp_compose::GpuPixels for OtherSize {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_fit_from_srgb8(&self.srgb, self.w, self.h, width, height)
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.srgb))
    }
}

/// A base of the timeline's size on the CPU and one clip above it from
/// the GPU at its own size.
struct Mixed {
    base: (ClipId, Arc<Vec<f32>>),
    top: (ClipId, Arc<Vec<u8>>, u32, u32),
}

impl FrameSource for Mixed {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        if clip.id == self.base.0 {
            return Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.base.1)), width, height, pts: None });
        }
        let (_, srgb, w, h) = &self.top;
        Some(SourceFrame {
            pixels: SourcePixels::Gpu(Arc::new(OtherSize { srgb: Arc::clone(srgb), w: *w, h: *h })),
            width: *w,
            height: *h,
            pts: None,
        })
    }
}

/// A clip of another size over one of the timeline's is fitted on the
/// GPU as the CPU fits it: half again as large (bilinear), four times
/// as large (an area average), and portrait (bars either side). The
/// stack used to decline such a frame, which sent a 1440p overlay on a
/// 1080p timeline down the CPU path every frame.
#[test]
fn a_layer_of_another_size_is_fitted_on_the_gpu() {
    for (w, h) in [(480u32, 270u32), (1280, 720), (90, 160), (200, 180)] {
        let srgb: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 7 % 256) as u8, 255]))
            .collect();
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut top = Track::new("V2", TrackKind::Video);
        let t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
        let mut base = Track::new("V1", TrackKind::Video);
        let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
        let mut src = Mixed { base: (b.id, picture(0.0)), top: (t.id, Arc::new(srgb), w, h) };
        top.clips = vec![t];
        base.clips = vec![b];
        seq.tracks = vec![top, base];

        let mut r = renderer();
        let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
            panic!("{w}x{h}: the GPU stack declined a layer of another size");
        };
        let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
        let d = worst(&cpu, &gpu);
        assert!(d <= 1, "{w}x{h}: GPU fit differs from the CPU fit by {d} levels");
    }
}

/// One Media clip whose frame is on the GPU, carrying BOTH a primary
/// grade and a GPU-capable builtin effect (a reframe `Transform`). The
/// stack must grade it and apply the effect on the GPU — the grade must
/// not fall back to the CPU because an effect follows it — and its frame
/// must be the CPU renderer's. This is the resident path playback rides
/// for a graded, reframed clip (the case a static reframe or an animated
/// zoom's baked per-frame `Transform` produces).
struct OneGpu {
    id: ClipId,
    srgb: Arc<Vec<u8>>,
}

impl FrameSource for OneGpu {
    fn frame(&mut self, clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        if clip.id != self.id {
            return None;
        }
        Some(SourceFrame {
            pixels: SourcePixels::Gpu(Arc::new(OtherSize { srgb: Arc::clone(&self.srgb), w: width, h: height })),
            width,
            height,
            pts: None,
        })
    }
}

#[test]
fn a_graded_clip_with_a_gpu_effect_stays_on_the_stack() {
    let srgb: Vec<u8> = (0..H)
        .flat_map(|y| (0..W).flat_map(move |x| [(x * 255 / W) as u8, (y * 255 / H) as u8, ((x + y) * 7 % 256) as u8, 255]))
        .collect();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let mut c = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    c.set_grade({
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node {
            id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
        };
        let nodes = vec![
            node(NodeKind::Source, NodeParams::default()),
            node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() }),
            node(NodeKind::Output, NodeParams::default()),
        ];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    });
    c = TestClip::of(c).effects([klipp_effects::BuiltinEffect::Transform {
        position_x: 12.0,
        position_y: -8.0,
        scale_x: 1.25,
        scale_y: 1.25,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    }]).build();
    let id = c.id;
    v.clips = vec![c];
    seq.tracks = vec![v];
    let mut src = OneGpu { id, srgb: Arc::new(srgb) };

    let mut r = renderer();
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "graded + GPU effect: GPU stack differs from the CPU renderer by {d} levels");
}

/// Scene-referred codes, 10-bit, ramping down the frame with a hue turning
/// across it, at `w` by `h`: black to peak, no two channels alike.
fn scene_codes(w: u32, h: u32) -> Arc<klipp_color::hdr::SceneCodes> {
    Arc::new(klipp_color::hdr::SceneCodes::Packed10(
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let (v, t) = (y * 1023 / h, x * 1023 / w);
                    klipp_color::hdr::pack_rgb10(v, (v * 3 / 4 + t / 4).min(1023), (v / 2 + t / 2).min(1023))
                })
            })
            .collect(),
    ))
}

/// [`scene_codes`] as a source deeper than 10 bits gives them: the same
/// ramps in 16-bit codes, between the 10-bit steps (#1226).
fn deep_scene_codes(w: u32, h: u32) -> Arc<klipp_color::hdr::SceneCodes> {
    Arc::new(klipp_color::hdr::SceneCodes::Deep16(
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let (v, t) = (y * 65_535 / h, x * 65_535 / w);
                    klipp_color::hdr::pack_rgb16(v as u16, (v * 3 / 4 + t / 4).min(65_535) as u16, (v / 2 + t / 2).min(65_535) as u16)
                })
            })
            .collect(),
    ))
}

/// Scene-referred clips' codes, each in its transfer and at its own size;
/// every other clip a flat linear picture. `linear` hands the scene clips
/// over decoded to scene light instead, with no transfer: the same picture
/// without its display step, for the control. `deep` gives them 16-bit
/// codes.
struct Scenes {
    scene: HashMap<ClipId, (klipp_color::hdr::HdrTransfer, u32, u32)>,
    flat: Flat,
    linear: bool,
    deep: bool,
}

impl FrameSource for Scenes {
    fn frame(&mut self, clip: &Clip, secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let Some(&(transfer, w, h)) = self.scene.get(&clip.id) else { return self.flat.frame(clip, secs, width, height) };
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let codes = if self.deep { deep_scene_codes(w, h) } else { scene_codes(w, h) };
        let pixels = if self.linear {
            SourcePixels::Linear(Arc::new(codes.to_scene_linear_rgba(transfer, identity)))
        } else {
            SourcePixels::SceneReferred { codes, transfer, source_to_working: identity }
        };
        Some(SourceFrame { pixels, width: w, height: h, pts: None })
    }
}

/// A half-stop grade, so a layer's grade runs a node of its own.
fn half_stop(c: &mut Clip) {
    c.set_grade({
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node {
            id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
        };
        let nodes = vec![
            node(NodeKind::Source, NodeParams::default()),
            node(NodeKind::Exposure, NodeParams { exposure: Some(-0.5), ..Default::default() }),
            node(NodeKind::Output, NodeParams::default()),
        ];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
    });
}

/// A scene-referred clip cropped, moved and fading in over an SDR base
/// layer, a title over both: `(sequence, source)`, the scene clip at
/// `size` in `transfer`.
fn scene_over_base(transfer: klipp_color::hdr::HdrTransfer, size: (u32, u32), graded: bool) -> (Sequence, Scenes) {
    use klipp_effects::BuiltinEffect;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Log".into(), font_size: 120.0, ..Default::default() });
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 5.0)];
    let mut top = Track::new("V2", TrackKind::Video);
    let mut t = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    if graded {
        half_stop(&mut t);
    }
    t = TestClip::of(t)
        .effects([
            BuiltinEffect::Crop { left: 0.1, right: 0.2, top: 0.05, bottom: 0.15, feather: 0.0 },
            BuiltinEffect::Transform {
                position_x: 14.0,
                position_y: -6.0,
                scale_x: 0.85,
                scale_y: 0.85,
                rotation: 4.0,
                anchor_x: 0.5,
                anchor_y: 0.5,
                rotation_x: 0.0,
                rotation_y: 0.0,
                perspective: 0.0,
                focal_ref: None,
            },
        ])
        .lane("opacity", 0.0, 0.2)
        .lane("opacity", 2.0, 1.0)
        .build();
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Scenes { scene: HashMap::new(), flat: Flat::default(), linear: false, deep: false };
    src.scene.insert(t.id, (transfer, size.0, size.1));
    src.flat.0.insert(b.id, picture(0.1));
    top.clips = vec![t];
    base.clips = vec![b];
    seq.tracks = vec![text, top, base];
    (seq, src)
}

/// Whether the stack takes a plain SDR frame here at all: asked first, so
/// a decline of a scene-referred one after it is a failure, not a skip.
fn stack_available() -> bool {
    let (seq, mut src) = stack();
    renderer().render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_some()
}

/// A log, HLG or PQ clip with a crop, a transform and a fade, under a
/// title, is placed on the GPU stack: its codes decoded to the working
/// space as its grade's chain starts, its display step in its plan before
/// its effects (#1276). It used to be refused part-way, sending every
/// such frame to the CPU composite. Held to the CPU render within a
/// level, at the sequence's size and fitted from another, graded and not;
/// the control is the same picture handed over without its display step,
/// which the bar must tell apart.
#[test]
fn a_scene_referred_clip_with_a_crop_transform_and_fade_stays_on_the_stack() {
    use klipp_color::hdr::HdrTransfer;
    if !stack_available() {
        eprintln!("UNVERIFIED: the GPU stack declined a plain frame (no usable GPU?)");
        return;
    }
    let transfers = [("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)), ("HLG", HdrTransfer::Hlg), ("PQ", HdrTransfer::Pq)];
    for (name, transfer) in transfers {
        for size in [(W, H), (480, 270), (90, 160)] {
            for graded in [false, true] {
                let case = format!("{name} at {}x{}, graded {graded}", size.0, size.1);
                let (seq, mut src) = scene_over_base(transfer, size, graded);
                let mut r = renderer();
                let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
                    panic!("{case}: the GPU stack declined it: {:?}", r.last_decline());
                };
                assert_eq!(r.last_decline(), None, "{case}: taken by the stack");
                let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
                let d = worst(&cpu, &gpu);
                assert!(d <= 1, "{case}: the GPU stack differs from the CPU renderer by {d} levels");
                // The control: without its display step the frame is
                // another picture, and the bar says so.
                src.linear = true;
                let undisplayed = display(&renderer().cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
                let off = worst(&undisplayed, &gpu);
                assert!(off > 1, "{case}: a frame without its display step is only {off} levels off: the bar wouldn't see it missing");
            }
        }
    }
}

/// #1226: a deeper source's 16-bit codes are placed on the stack as 10-bit
/// ones are, decoded on the GPU, and held to the CPU render the same way.
#[test]
fn a_deep_scene_referred_clip_stays_on_the_stack() {
    use klipp_color::hdr::HdrTransfer;
    if !stack_available() {
        eprintln!("UNVERIFIED: the GPU stack declined a plain frame (no usable GPU?)");
        return;
    }
    for (name, transfer) in [("S-Log3", HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)), ("HLG", HdrTransfer::Hlg), ("PQ", HdrTransfer::Pq)] {
        for graded in [false, true] {
            let case = format!("16-bit {name}, graded {graded}");
            let (seq, mut src) = scene_over_base(transfer, (W, H), graded);
            src.deep = true;
            let mut r = renderer();
            let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
                panic!("{case}: the GPU stack declined it: {:?}", r.last_decline());
            };
            assert_eq!(r.last_decline(), None, "{case}: taken by the stack");
            let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
            let d = worst(&cpu, &gpu);
            assert!(d <= 1, "{case}: the GPU stack differs from the CPU renderer by {d} levels");
        }
    }
}

/// A transition between a scene-referred clip and an SDR one is drawn by
/// the stack, both sides placed there (#1276).
#[test]
fn a_transition_from_a_scene_referred_clip_stays_on_the_stack() {
    if !stack_available() {
        eprintln!("UNVERIFIED: the GPU stack declined a plain frame (no usable GPU?)");
        return;
    }
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut src = Scenes { scene: HashMap::new(), flat: Flat::default(), linear: false, deep: false };
    src.scene.insert(a.id, (klipp_color::hdr::HdrTransfer::Hlg, W, H));
    src.flat.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(2.0),
        alignment: Default::default(),
        user_params: Default::default(),
    });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    let mut r = renderer();
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()) else {
        panic!("the GPU stack declined the transition: {:?}", r.last_decline());
    };
    assert_eq!(r.last_decline(), None);
    let cpu = display(&r.cpu_render(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "the transition differs from the CPU renderer by {d} levels");
}

/// A scene-referred layer whose grade fails is refused after its effects
/// and display step were queued for the run, and leaves none of them
/// behind (#1276): an SDR layer graded next on the same renderer is its
/// CPU render, not tone-mapped, cropped or moved by what was queued.
#[test]
fn a_scene_referred_layer_refused_part_way_leaves_nothing_queued() {
    if !stack_available() {
        eprintln!("UNVERIFIED: the GPU stack declined a plain frame (no usable GPU?)");
        return;
    }
    let (seq, mut src) = scene_over_base(klipp_color::hdr::HdrTransfer::Hlg, (W, H), true);
    let mut r = renderer();
    {
        let _fault = klipp_compose::fault::fail_grade(seq.tracks[1].clips[0].id);
        assert!(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).is_none(), "a failed grade is refused");
        assert_eq!(r.last_decline(), Some("a scene-referred layer's display step didn't run"));
    }
    // The base alone, graded, with no effects of its own: anything left
    // queued would run in its grade.
    let mut plain = seq.clone();
    plain.tracks.drain(..2);
    half_stop(&mut plain.tracks[0].clips[0]);
    let Some((gpu, _)) = r.render_display_u8_on_gpu(&plain, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        panic!("the GPU stack declined the SDR layer: {:?}", r.last_decline());
    };
    let cpu = display(&renderer().cpu_render(&plain, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "the SDR layer after the refusal differs from its CPU render by {d} levels");
}

/// #1850: a still generator's layer is kept between frames, by its data
/// and size: the next frame draws the same, a colour changed in the
/// sequence is drawn at once, and so is another size. A living gradient,
/// which moves, is drawn anew each frame.
#[test]
fn a_still_generator_is_kept_and_a_changed_one_redrawn() {
    let at = |seq: &Sequence, r: &mut Renderer, t: f64, w: u32, h: u32| r.render_display_u8_on_gpu(seq, tc(t), w, h, &mut Flat::default(), RenderOptions::default()).map(|(px, _)| px);
    let one = |data: klipp_core::GeneratorData| {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut gen = Track::new("Gen", TrackKind::Video);
        gen.clips = vec![clip(ClipKind::Generator(data), 0.0, 5.0)];
        seq.tracks = vec![gen];
        seq
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let blue = one(klipp_core::GeneratorData::SolidColor { color: [20, 40, 220, 255] });
    let Some(first) = at(&blue, &mut r, 1.0, W, H) else {
        no_gpu("no GPU stack");
        return;
    };
    assert_eq!(at(&blue, &mut r, 2.0, W, H).unwrap(), first, "the next frame draws the same");
    // The same clip, its colour changed: drawn in the new colour at once.
    let mut red = blue.clone();
    red.tracks[0].clips[0].kind = ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [220, 40, 20, 255] });
    let changed = at(&red, &mut r, 2.0, W, H).unwrap();
    let fresh = at(&red, &mut Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new()), 2.0, W, H).unwrap();
    assert_eq!(changed, fresh, "the changed colour, as a fresh renderer draws it");
    assert!(worst(&changed, &first) > 100, "control: the colours differ");
    // Another size is drawn at that size.
    let small = at(&red, &mut r, 2.0, W / 2, H / 2).unwrap();
    assert_eq!(small.len(), (W / 2 * H / 2 * 4) as usize);
    // A living gradient drifts: two times, two pictures.
    let living = one(klipp_core::GeneratorData::LivingGradient { a: [20, 40, 220, 255], b: [250, 200, 120, 255], angle_deg: 0.0, drift_deg_per_sec: 45.0, glow: 0.0, grain: klipp_core::DEFAULT_GENERATOR_GRAIN });
    let (a, b) = (at(&living, &mut r, 1.0, W, H).unwrap(), at(&living, &mut r, 2.0, W, H).unwrap());
    assert!(worst(&a, &b) > 10, "a moving generator is drawn anew");
}

/// #1141: particles over a clip are the same in the viewer (the GPU
/// stack) and the export (the CPU), for each kind, small and large.
#[test]
fn particles_match() {
    for style in 0..5u32 {
        for (size, dir_, sp) in [(1.0f32, None, None), (12.0, None, None), (18.05, None, None), (2.45, Some(324.0f32), None), (2.5, None, Some(1350.0f32)), (17.0, Some(37.0), Some(400.0))] {
            let size: f32 = size;
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: W, height: H };
            let mut v = Track::new("V1", TrackKind::Video);
            let mut a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
            let fx = match klipp_effects::particles::preset(style) {
                klipp_effects::BuiltinEffect::Particles { style, amount, speed, direction, spread, lifetime, opacity, color, seed, time, .. } => klipp_effects::BuiltinEffect::Particles { style, amount, size, speed: sp.unwrap_or(speed), direction: dir_.unwrap_or(direction), spread, lifetime, opacity, color, seed, time },
                e => e,
            };
            a = TestClip::of(a).effects([fx]).build();
            let mut src = Flat::default();
            src.0.insert(a.id, busy());
            v.clips = vec![a];
            seq.tracks = vec![v];
            let mut r = renderer();
            // A frame before, on the same renderer: what it keeps between
            // frames must not hold the particles where they were.
            let _ = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
            let _ = r.render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
            let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default()) else {
                no_gpu("the GPU stack declined");
                return;
            };
            let cpu = display(&r.cpu_render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default()));
            let over: Vec<usize> = (0..cpu.len()).filter(|&i| cpu[i].abs_diff(gpu[i]) > 1).collect();
            for &i in over.iter().take(4) {
                let p = i / 4;
                eprintln!("style {style} size {size}: ({}, {}) channel {} CPU {} GPU {}", p as u32 % W, p as u32 / W, i % 4, cpu[i], gpu[i]);
            }
            assert!(over.is_empty(), "style {style} size {size}: {} values more than a level apart", over.len());
        }
    }
}

/// A busy picture with highlights over 1 and grain: the UI parity
/// harness's source.
fn busy() -> Arc<Vec<f32>> {
    let (w, h) = (W, H);
    let frame = 12u32;
    let pan = frame * 2;
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let xs = x + pan;
            let (fx, fy) = ((xs % w) as f32 / w as f32, y as f32 / h as f32);
            let check = if (xs / 8 + y / 8).is_multiple_of(2) { 0.15 } else { 0.0 };
            let bright = (0.6..0.75).contains(&fx) && (0.2..0.4).contains(&fy);
            let lift = if bright { 1.5 } else { 0.0 };
            let hash = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ frame.wrapping_mul(83492791)) % 1000;
            let grain = (hash as f32 / 1000.0 - 0.5) * 0.06;
            v.extend_from_slice(&[fx * 0.8 + check + lift + grain, fy * 0.7 + lift + grain, (1.0 - fx) * 0.6 + check + lift + grain, 1.0]);
        }
    }
    Arc::new(v)
}

/// #239: a grade gated by a matte sequence (the subject matte pass's
/// output) takes each frame's own matte as the clip plays, the same on
/// the GPU stack (the viewer) and the CPU (the export); a frame whose
/// matte isn't made yet is drawn with the nearest one, but said, as a
/// failing decline naming the sequence, so an export refuses it.
#[test]
fn a_matte_sequence_gates_the_grade_frame_by_frame_on_both_routes() {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, matte_sequence, params::NodeParams};
    let dir = std::env::temp_dir().join(format!("klipp-matte-seq-render-{}-{}", std::process::id(), ClipId::new().0));
    let (mw, mh) = (32u32, 18u32);
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    matte_sequence::create(&dir, &manifest).unwrap();
    // A bar a quarter of the frame wide, moving right a column a frame.
    for f in 0..=60i64 {
        let bar: Vec<u8> = (0..mw * mh).map(|i| if ((i % mw) as i64 - f / 3).rem_euclid(mw as i64) < (mw / 4) as i64 { 255 } else { 0 }).collect();
        matte_sequence::put(&dir, &manifest, f * 40_000, &bar).unwrap();
    }
    let mut g = NodeGraph::default();
    let corr = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.saturation = Some(0.0);
    let matte = NodeId::new_v4();
    let params = NodeParams { matte_image_path: Some(dir.clone()), ..Default::default() };
    g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
    g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });

    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    a.set_grade(g);
    let mut src = Flat::default();
    src.0.insert(a.id, picture(0.2));
    v.clips = vec![a];
    seq.tracks = vec![v];

    let mut r = renderer();
    let mut frames = Vec::new();
    for t in [1.0, 2.0] {
        let cpu = display(&r.cpu_render(&seq, tc(t), W, H, &mut src, RenderOptions::default()));
        assert!(r.frame_declines().iter().all(|d| !d.fails()), "at {t} s: {:?}", r.frame_declines());
        if let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) {
            let d = worst(&cpu, &gpu);
            assert!(d <= 1, "at {t} s: the stack differs from the CPU by {d} levels");
        } else {
            eprintln!("UNVERIFIED: the GPU stack declined ({:?})", r.last_decline());
        }
        frames.push(cpu);
    }
    assert!(worst(&frames[0], &frames[1]) > 20, "the matte moved with the frame: the grey bar is elsewhere");

    // The frame at 2 s not made yet: the nearest stands in, and is said.
    std::fs::remove_file(matte_sequence::frame_path(&dir, 50 * 40_000)).unwrap();
    let mut r = renderer();
    let (_, found) = r.render_inspected(&seq, tc(2.0), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    let mut routes = vec![("CPU", found.declines)];
    if r.render_display_u8_on_gpu(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()).is_some() {
        routes.push(("GPU", r.frame_declines().to_vec()));
    }
    for (route, declines) in routes {
        let missing: Vec<_> = declines.iter().filter(|d| d.outcome == klipp_nodes::DeclineOutcome::FileMissing).collect();
        assert_eq!(missing.len(), 1, "{route}: {declines:?}");
        assert!(missing[0].fails() && missing[0].shown(), "{route}: an export refuses a stand-in matte, and the viewer says so");
        assert!(missing[0].reason.contains("2.000 s"), "{route}: {}", missing[0].reason);
        assert!(missing[0].why.contains("the one at 1.960000 s stands in") && missing[0].why.contains(&dir.display().to_string()), "{route}: {}", missing[0].why);
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A flat source whose media has a subject matte in `dir` (#985 phase 3).
struct Matted(Flat, Option<std::path::PathBuf>);

impl FrameSource for Matted {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.frame(clip, source_secs, width, height)
    }

    fn subject_matte_dir(&mut self, _clip: &Clip, _model: u32) -> Option<std::path::PathBuf> {
        self.1.clone()
    }
}

/// #985 phase 3: a shatter cutting round the outgoing shot's subject, its
/// matte a tracked sequence: the viewer (the GPU stack) and the export
/// (the CPU) draw the same frame, the subject still in place half way;
/// with no matte made, both draw the plain pieces and decline, so the
/// viewer says why and an export refuses.
#[test]
fn a_transition_cuts_round_the_subject_on_both_routes() {
    use klipp_core::piece_transition::{PieceParams, SubjectCut};
    use klipp_nodes::matte_sequence;
    let dir = std::env::temp_dir().join(format!("klipp-transition-subject-{}-{}", std::process::id(), ClipId::new().0));
    let (mw, mh) = (32u32, 18u32);
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    matte_sequence::create(&dir, &manifest).unwrap();
    // A disc in the middle, every frame of the first four seconds.
    let disc: Vec<u8> = (0..mw * mh).map(|i| { let (x, y) = ((i % mw) as f32 + 0.5 - 16.0, (i / mw) as f32 + 0.5 - 9.0); if x * x + y * y < 36.0 { 255 } else { 0 } }).collect();
    for f in 0..=100i64 {
        matte_sequence::put(&dir, &manifest, f * 40_000, &disc).unwrap();
    }
    let kind = klipp_core::TransitionKind::Shatter;
    let user_params = PieceParams { subject: Some(SubjectCut::default()), ..PieceParams::read(kind, &klipp_core::spec_for_kind(kind).default_params) }.to_user(kind);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut flat = Flat::default();
    flat.0.insert(a.id, picture(0.0));
    flat.0.insert(b.id, picture(0.5));
    v.transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), clip_a_id: a.id, clip_b_id: b.id, kind, duration: tc(2.0), alignment: Default::default(), user_params });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    let outgoing = display(&picture(0.0));
    let centre = (((H / 2) * W + W / 2) * 4) as usize;

    let mut src = Matted(flat, Some(dir.clone()));
    let mut r = renderer();
    // 1.9 s: 45% through, the shards most of the way gone, the subject still.
    let (cpu, found) = r.cpu_render_inspected(&seq, tc(1.9), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    let cpu = display(&cpu);
    assert!(found.declines.iter().all(|d| !d.fails()), "{:?}", found.declines);
    assert_eq!(&cpu[centre..centre + 3], &outgoing[centre..centre + 3], "the subject in place");
    assert!(cpu != outgoing, "the rest under way");
    // The viewer's route must take the frame wherever there is a GPU: a
    // decline would leave "the viewer draws what the export does" unheld.
    match r.render_display_u8_on_gpu(&seq, tc(1.9), W, H, &mut src, RenderOptions::default()) {
        Some((gpu, _)) => {
            assert!(r.frame_declines().iter().all(|d| !d.fails()), "GPU: {:?}", r.frame_declines());
            let d = worst(&cpu, &gpu);
            assert!(d <= 1, "the stack differs from the CPU by {d} levels");
        }
        None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined a subject transition: {:?}", r.last_decline()),
    }

    // No matte made: the plain pieces, said on both routes.
    src.1 = None;
    let mut r = renderer();
    let (plain, found) = r.cpu_render_inspected(&seq, tc(1.9), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    let plain = display(&plain);
    let mut routes = vec![("CPU", found.declines, plain.clone())];
    match r.render_display_u8_on_gpu(&seq, tc(1.9), W, H, &mut src, RenderOptions::default()) {
        Some((gpu, _)) => routes.push(("GPU", r.frame_declines().to_vec(), gpu)),
        None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined: {:?}", r.last_decline()),
    }
    for (route, declines, frame) in routes {
        let missing: Vec<_> = declines.iter().filter(|d| d.outcome == klipp_nodes::DeclineOutcome::FileMissing).collect();
        assert_eq!(missing.len(), 1, "{route}: {declines:?}");
        assert!(missing[0].fails() && missing[0].reason.contains("track its subject"), "{route}: {}", missing[0].reason);
        assert!(worst(&frame, &plain) <= 1, "{route}: the plain pieces");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A source whose clips' frames are their own size (a portrait phone
/// clip), each with a subject matte (#985 phase 3).
struct Shaped {
    frames: HashMap<ClipId, (u32, u32, Arc<Vec<f32>>)>,
    dir: std::path::PathBuf,
    /// Every clip's media's length in seconds at 25 fps, when known.
    length: Option<f64>,
}

impl FrameSource for Shaped {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        self.frames.get(&clip.id).map(|(w, h, p)| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width: *w, height: *h, pts: None })
    }

    fn subject_matte_dir(&mut self, _clip: &Clip, _model: u32) -> Option<std::path::PathBuf> {
        Some(self.dir.clone())
    }

    fn media_range(&mut self, _clip: &Clip) -> Option<(f64, f64)> {
        self.length.map(|l| (0.0, l))
    }

    fn media_fps(&mut self, _clip: &Clip) -> Option<f64> {
        self.length.map(|_| 25.0)
    }
}

/// #985 phase 3: the subject's matte lands where the clip's picture
/// does: a portrait source pillarboxed in the frame, and a clip shrunk to
/// 60% and moved right. Strips leave round the subject; while it stays,
/// the disc's place in the frame shows the outgoing picture and the rest
/// of the middle the incoming, on both routes.
#[test]
fn a_transitions_subject_matte_is_placed_as_its_picture() {
    use klipp_core::piece_transition::{PieceEase, PieceOrder, PieceParams, SubjectCut};
    use klipp_nodes::matte_sequence;
    let (a_col, b_col) = ([0.8f32, 0.2, 0.1, 1.0], [0.1f32, 0.3, 0.8, 1.0]);
    let shown = |c: [f32; 4]| display(&c.repeat(4))[..3].to_vec();
    let (want_a, want_b) = (shown(a_col), shown(b_col));
    let kind = klipp_core::TransitionKind::SlideStrips;
    let user_params = PieceParams { count: 4, stagger: 0.0, order: PieceOrder::AllAtOnce, ease: PieceEase::Linear, alternate: true, subject: Some(SubjectCut::default()), ..Default::default() }.to_user(kind);
    // (source size, matte size, disc radius in matte pixels, the clip's
    // effects, where the disc is in the frame, a point of the middle
    // outside it).
    let shrink_right = klipp_effects::BuiltinEffect::Transform { position_x: 60.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let cases: [(&str, (u32, u32), (u32, u32), f32, Vec<klipp_effects::BuiltinEffect>, (u32, u32), Vec<(u32, u32)>); 2] = [
        // 90 x 160 fitted to 101 x 180, centred: the disc's 28 px round
        // the middle; 80 px right is a bar, where a matte stretched to the
        // frame would have put the subject.
        ("portrait", (90, 160), (18, 32), 5.0, Vec::new(), (W / 2, H / 2), vec![(W / 2 + 50, H / 2), (W / 2 + 80, H / 2)]),
        // Shrunk and moved: the disc 24 px round 60 px right of the
        // middle; the middle itself is not the subject.
        ("picture in picture", (W, H), (32, 18), 4.0, vec![shrink_right], (W / 2 + 60, H / 2), vec![(W / 2, H / 2), (W / 2 - 40, H / 2)]),
    ];
    for (what, (sw, sh), (mw, mh), radius, effects, subject_at, elsewhere) in cases {
        let dir = std::env::temp_dir().join(format!("klipp-transition-placed-{}-{}", std::process::id(), ClipId::new().0));
        let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        matte_sequence::create(&dir, &manifest).unwrap();
        let disc: Vec<u8> = (0..mw * mh).map(|i| { let (x, y) = ((i % mw) as f32 + 0.5 - mw as f32 * 0.5, (i / mw) as f32 + 0.5 - mh as f32 * 0.5); if x * x + y * y < radius * radius { 255 } else { 0 } }).collect();
        for f in 0..=100i64 {
            matte_sequence::put(&dir, &manifest, f * 40_000, &disc).unwrap();
        }
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut v = Track::new("V1", TrackKind::Video);
        let a = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0)).effects(effects).build();
        let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
        let mut frames = HashMap::new();
        frames.insert(a.id, (sw, sh, Arc::new(a_col.repeat((sw * sh) as usize))));
        frames.insert(b.id, (W, H, Arc::new(b_col.repeat((W * H) as usize))));
        v.transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), clip_a_id: a.id, clip_b_id: b.id, kind, duration: tc(2.0), alignment: Default::default(), user_params });
        v.clips = vec![a, b];
        seq.tracks = vec![v];
        let mut src = Shaped { frames, dir: dir.clone(), length: None };
        // 2.17 s: 58% through; the strips are most of the way out, the
        // subject not yet moving.
        let at = tc(2.2);
        let mut r = renderer();
        let (cpu, found) = r.cpu_render_inspected(&seq, at, W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
        assert!(found.declines.iter().all(|d| !d.fails()), "{what}: {:?}", found.declines);
        let mut routes = vec![("CPU", display(&cpu))];
        match r.render_display_u8_on_gpu(&seq, at, W, H, &mut src, RenderOptions::default()) {
            Some((gpu, _)) => routes.push(("GPU", gpu)),
            None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "{what}: the GPU stack declined: {:?}", r.last_decline()),
        }
        for (route, frame) in routes {
            let px = |(x, y): (u32, u32)| frame[((y * W + x) * 4) as usize..((y * W + x) * 4) as usize + 3].to_vec();
            let near = |got: Vec<u8>, want: &[u8]| got.iter().zip(want).all(|(g, w)| (*g as i32 - *w as i32).abs() <= 1);
            assert!(near(px(subject_at), &want_a), "{what} {route}: the subject at {subject_at:?}: {:?}", px(subject_at));
            for p in &elsewhere {
                assert!(near(px(*p), &want_b), "{what} {route}: not the subject at {p:?}: {:?}", px(*p));
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// #985 phase 3: an outgoing clip whose handle runs 0.2 s past its
/// media's end is read there at its last frame, picture and matte alike:
/// a matte made to the end covers the transition, on both routes, with
/// nothing declined; a matte short of the end is still said.
#[test]
fn a_handle_past_the_media_end_reads_its_last_matte() {
    use klipp_core::piece_transition::{PieceParams, SubjectCut};
    use klipp_nodes::matte_sequence;
    let dir = std::env::temp_dir().join(format!("klipp-transition-end-{}-{}", std::process::id(), ClipId::new().0));
    let (mw, mh) = (32u32, 18u32);
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    matte_sequence::create(&dir, &manifest).unwrap();
    // A 2 s media: its frames, and the matte's, 0 to 1.96 s.
    for f in 0..50i64 {
        matte_sequence::put(&dir, &manifest, f * 40_000, &[255; 32 * 18]).unwrap();
    }
    let kind = klipp_core::TransitionKind::SlideStrips;
    let user_params = PieceParams { subject: Some(SubjectCut::default()), ..PieceParams::read(kind, &klipp_core::spec_for_kind(kind).default_params) }.to_user(kind);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    // The cut at 2 s, the media's end: the transition, 1.6 s to 2.4 s,
    // reads the outgoing clip to 2.4 s.
    let a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    let mut frames = HashMap::new();
    frames.insert(a.id, (W, H, picture(0.0)));
    frames.insert(b.id, (W, H, picture(0.5)));
    v.transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), clip_a_id: a.id, clip_b_id: b.id, kind, duration: tc(0.8), alignment: Default::default(), user_params });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    let mut src = Shaped { frames, dir: dir.clone(), length: Some(2.0) };
    for t in [2.1, 2.3] {
        let mut r = renderer();
        let (_, found) = r.render_inspected(&seq, tc(t), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
        assert!(found.declines.iter().all(|d| !d.fails()), "at {t} s: {:?}", found.declines);
        match r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) {
            Some(_) => assert!(r.frame_declines().iter().all(|d| !d.fails()), "GPU at {t} s: {:?}", r.frame_declines()),
            None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined: {:?}", r.last_decline()),
        }
    }
    // The last frame not made: said, not stood in for silently.
    std::fs::remove_file(matte_sequence::frame_path(&dir, 49 * 40_000)).unwrap();
    let mut r = renderer();
    let (_, found) = r.render_inspected(&seq, tc(2.3), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    assert!(found.declines.iter().any(|d| d.fails()), "{:?}", found.declines);
    let _ = std::fs::remove_dir_all(dir);
}

/// #1881: a grade's tracked subject is bound by the clip's media and its
/// model, its folder found at render from the frame source (the media as
/// linked now), not a path kept in the grade: the same grade reads one
/// folder, then another once the media is relinked, on both routes, and
/// declines when there is none, so the viewer says it and an export
/// refuses.
#[test]
fn a_subject_bound_grade_follows_its_media() {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, matte_sequence, params::NodeParams};
    let (mw, mh) = (32u32, 18u32);
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    // Two files' mattes: all subject, and none.
    let folder = |value: u8| {
        let dir = std::env::temp_dir().join(format!("klipp-subject-bound-{}-{}", std::process::id(), ClipId::new().0));
        matte_sequence::create(&dir, &manifest).unwrap();
        for f in 0..=100i64 {
            matte_sequence::put(&dir, &manifest, f * 40_000, &vec![value; (mw * mh) as usize]).unwrap();
        }
        dir
    };
    let (all, none) = (folder(255), folder(0));
    let mut g = NodeGraph::default();
    let corr = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.saturation = Some(0.0);
    let matte = NodeId::new_v4();
    let params = NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() };
    g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
    g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    a.set_grade(g);
    let mut frames = HashMap::new();
    frames.insert(a.id, (W, H, picture(0.2)));
    v.clips = vec![a];
    seq.tracks = vec![v];
    let grey = |f: &[u8]| f.chunks_exact(4).all(|p| (p[0] as i32 - p[1] as i32).abs() <= 2 && (p[1] as i32 - p[2] as i32).abs() <= 2);
    let original = display(&picture(0.2));
    for (dir, desaturated) in [(&all, true), (&none, false)] {
        let mut src = Shaped { frames: frames.clone(), dir: dir.clone(), length: None };
        let mut r = renderer();
        let (cpu, found) = r.cpu_render_inspected(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
        let cpu = display(&cpu);
        assert!(found.declines.iter().all(|d| !d.fails()), "{:?}", found.declines);
        assert_eq!(grey(&cpu), desaturated, "{}: the grade reads the folder its media has", dir.display());
        if !desaturated {
            assert!(worst(&cpu, &original) <= 1, "no subject: the picture as it was");
        }
        match r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) {
            Some((gpu, _)) => assert!(worst(&cpu, &gpu) <= 1, "the stack reads the same folder"),
            None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined: {:?}", r.last_decline()),
        }
    }
    // No folder for the media (relinked, not tracked yet): said.
    struct NoMatte(Shaped);
    impl FrameSource for NoMatte {
        fn frame(&mut self, clip: &Clip, s: f64, w: u32, h: u32) -> Option<SourceFrame> {
            self.0.frame(clip, s, w, h)
        }
    }
    let mut src = NoMatte(Shaped { frames, dir: all.clone(), length: None });
    let mut r = renderer();
    let (_, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    let missing: Vec<_> = found.declines.iter().filter(|d| d.outcome == klipp_nodes::DeclineOutcome::FileMissing).collect();
    assert_eq!(missing.len(), 1, "{:?}", found.declines);
    assert!(missing[0].fails() && missing[0].reason.contains("track its subject"), "{}", missing[0].reason);
    // The export's check counts it the same way: by the folder found.
    let short = klipp_compose::subject_matte::uncovered_with(&seq, |_, _| None, |_| None);
    assert_eq!(short.len(), 1);
    assert_eq!((short[0].subject, short[0].covered), (Some(0), 0));
    assert!(klipp_compose::subject_matte::uncovered_with(&seq, |_, m| (m == 0).then(|| all.clone()), |_| None).is_empty());
    let _ = std::fs::remove_dir_all(all);
    let _ = std::fs::remove_dir_all(none);
}

/// A grade bound to a tracked subject, on two 2 s clips meeting in a
/// dissolve: the outgoing one is read 0.2 s past its media's end, the
/// incoming 0.2 s before its start. Each matte is read at the media's
/// last or first frame there, as the picture is, on both routes, with
/// nothing declined; the export's check counts those frames there too.
#[test]
fn a_grade_matte_read_past_the_media_ends_is_held_there() {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, matte_sequence, params::NodeParams};
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 32, height: 18, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    let dir = std::env::temp_dir().join(format!("klipp-grade-matte-ends-{}-{}", std::process::id(), ClipId::new().0));
    matte_sequence::create(&dir, &manifest).unwrap();
    // The media's frames, 0 to 1.96 s.
    for f in 0..50i64 {
        matte_sequence::put(&dir, &manifest, f * 40_000, &[255; 32 * 18]).unwrap();
    }
    let graded = |c: &mut Clip| {
        let mut g = NodeGraph::default();
        let corr = g.ensure_corrector();
        g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.saturation = Some(0.0);
        let matte = NodeId::new_v4();
        let params = NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() };
        g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        g.edges.push(Edge { from_node: matte, from_port: 0, to_node: corr, to_port: 1 });
        c.set_grade(g);
    };
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    let mut a = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let mut b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 2.0, 4.0);
    graded(&mut a);
    graded(&mut b);
    let mut frames = HashMap::new();
    frames.insert(a.id, (W, H, picture(0.0)));
    frames.insert(b.id, (W, H, picture(0.5)));
    v.transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), clip_a_id: a.id, clip_b_id: b.id, kind: klipp_core::TransitionKind::CrossDissolve, duration: tc(0.4), alignment: Default::default(), user_params: Default::default() });
    v.clips = vec![a, b];
    seq.tracks = vec![v];
    let mut src = Shaped { frames, dir: dir.clone(), length: Some(2.0) };
    // 1.8 s: the incoming clip at -0.2 s; 2.2 s: the outgoing at 2.2 s.
    for t in [1.8, 2.2] {
        let mut r = renderer();
        let (_, found) = r.render_inspected(&seq, tc(t), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
        assert!(found.declines.iter().all(|d| !d.fails()), "CPU at {t} s: {:?}", found.declines);
        match r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) {
            Some(_) => assert!(r.frame_declines().iter().all(|d| !d.fails()), "GPU at {t} s: {:?}", r.frame_declines()),
            None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined: {:?}", r.last_decline()),
        }
    }
    let last = klipp_compose::subject_matte::last_frame_secs(2.0, 25.0);
    assert!(klipp_compose::subject_matte::uncovered_with(&seq, |_, _| Some(dir.clone()), |_| Some(last)).is_empty(), "the check holds them at the ends too");
    assert!(!klipp_compose::subject_matte::uncovered_with(&seq, |_, _| Some(dir.clone()), |_| None).is_empty(), "unheld, the outgoing handle could never be covered");
    let _ = std::fs::remove_dir_all(dir);
}

/// #1922: with the camera's shutter open, an animated clip's hold is the
/// GPU stack's, and since #1923 so is its entrance, blurred there: the
/// stack's frames match the CPU's, held and moving. Where the entrance
/// gives way to the hold, the last blurred frame and the first held one
/// put the card in the same place: no jump where the blur stops.
#[test]
fn an_animated_clip_s_hold_stays_on_the_stack_without_a_jump() {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let card = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0))
        .lane("scale", 0.0, 0.3).lane("pos_x", 0.0, -120.0).lane("pos_x", 1.0, 0.0).lane("pos_x", 3.0, 0.0).lane("pos_x", 4.0, 120.0)
        .build();
    let mut src = Flat::default();
    src.0.insert(card.id, picture(0.3));
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![card];
    seq.tracks = vec![v];
    seq.camera.shutter_deg = 180.0;
    seq.camera.shutter_samples = 8;
    let mut r = renderer();
    let Some((held, _)) = r.render_display_u8_on_gpu(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()) else {
        assert_ne!(r.last_decline(), Some("motion blur"), "the hold is not turned away for blur");
        eprintln!("UNVERIFIED: the GPU stack declined the hold ({:?})", r.last_decline());
        return;
    };
    let cpu = display(&r.cpu_render(&seq, tc(2.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &held);
    assert!(d <= 1, "the held card on the stack differs from the CPU's by {d} levels");
    for t in [0.5, 1.0] {
        let Some((moving, _)) = r.render_display_u8_on_gpu(&seq, tc(t), W, H, &mut src, RenderOptions::default()) else {
            panic!("the entrance at {t} s is blurred on the stack: declined for {:?}", r.last_decline());
        };
        let d = worst(&display(&r.cpu_render(&seq, tc(t), W, H, &mut src, RenderOptions::default())), &moving);
        assert!(d <= 1, "the blurred card at {t} s differs from the CPU's by {d} levels");
    }
    // Frame 24 (1.000 s) still moves at the start of its shutter; frame 25
    // (1.042 s) is held.
    let (last_blurred, _) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()).expect("frame 24 on the stack");
    let Some((first_held, _)) = r.render_display_u8_on_gpu(&seq, tc(25.0 / 24.0), W, H, &mut src, RenderOptions::default()) else {
        panic!("frame 25 is held and the stack's: declined for {:?}", r.last_decline());
    };
    let centre = |img: &[u8]| {
        let (mut sx, mut n) = (0.0f64, 0.0f64);
        for y in 0..H {
            for x in 0..W {
                let p = &img[((y * W + x) * 4) as usize..][..3];
                if p.iter().any(|&c| c > 8) {
                    sx += x as f64;
                    n += 1.0;
                }
            }
        }
        sx / n.max(1.0)
    };
    let (a, b) = (centre(&last_blurred), centre(&first_held));
    assert!((a - b).abs() < 1.0, "the card jumps {:.2} px where the stack takes over: {a:.2} then {b:.2}", b - a);
    let cpu_first_held = display(&r.cpu_render(&seq, tc(25.0 / 24.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu_first_held, &first_held);
    assert!(d <= 1, "the first held frame differs between the paths by {d} levels");
}

/// #1923: a clip that moves under the camera's open shutter is smeared on
/// the stack as the CPU smears it: its effects before the move once, the
/// picture placed at every sample and averaged premultiplied, the
/// layer's opacity and blend once on the average. A fast card leaving
/// its own place, one carried by the camera, one following another, one
/// tilting, one cropped before the move, one with a second Transform
/// after its first, one cut out by its grade's Output alpha, and one at
/// half opacity in Screen, at a 180 degree
/// shutter of 8 samples and a 90 degree one of 12: the stack takes each
/// and matches the CPU's frame. (A picture is placed once, by its first
/// Transform; a second draws nothing, shutter open or shut, on either
/// path.)
#[test]
fn a_moving_clip_is_smeared_on_the_stack_as_the_cpu_smears_it() {
    let media = || clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
    let fast = |c: Clip| TestClip::of(c).lane("scale", 0.0, 0.3).lane("pos_x", 0.0, -150.0).lane("pos_x", 2.0, 150.0);
    let still = || TestClip::of(media()).lane("scale", 0.0, 0.3).build();
    let cases: Vec<(&str, Box<dyn Fn() -> (Vec<Clip>, bool)>)> = vec![
        ("fast", Box::new(move || (vec![fast(media()).build()], false))),
        ("camera", Box::new(move || (vec![still()], true))),
        ("linked", Box::new(move || {
            let parent = fast(media()).lane("opacity", 0.0, 0.0).build();
            let mut card = still();
            card.transform_link = Some(klipp_timeline::TransformLink::new(parent.id));
            (vec![card, parent], false)
        })),
        ("tilting", Box::new(move || (vec![TestClip::of(media()).lane("scale", 0.0, 0.4).lane("tilt_y", 0.0, 0.0).lane("tilt_y", 2.0, 60.0).build()], false))),
        ("cropped", Box::new(move || (vec![fast(media()).effect(klipp_effects::BuiltinEffect::Crop { left: 0.2, right: 0.1, top: 0.15, bottom: 0.0, feather: 0.0 }).build()], false))),
        ("two transforms", Box::new(move || {
            let turn = |deg: f32| klipp_effects::BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: deg,
                anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
            };
            (vec![fast(media()).effect(turn(10.0)).effect(turn(-25.0)).build()], false)
        })),
        ("cut out", Box::new(move || {
            // Its grade's Output takes a hard circle as its alpha (#898):
            // the alpha travels with the picture into every sample.
            let mut c = fast(media()).build();
            let mut g = klipp_nodes::NodeGraph::default();
            g.ensure_corrector();
            let source = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).expect("the Source").id;
            let circle = klipp_core::MaskStack { masks: vec![klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.0, ..klipp_core::Mask::default() }] };
            let id = klipp_nodes::NodeId::new_v4();
            g.nodes.push(klipp_nodes::Node {
                id,
                kind: klipp_nodes::NodeKind::Mask,
                label: "Mask".into(),
                enabled: true,
                params: klipp_nodes::NodeParams { masks: Some(circle), ..Default::default() },
                keyframes: Default::default(),
                output_link: None,
                input_links: Vec::new(),
            });
            g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: id, to_port: 0 });
            g.edges.push(klipp_nodes::Edge { from_node: id, from_port: 0, to_node: g.output_node_id, to_port: 1 });
            c.set_grade(g);
            (vec![c], false)
        })),
        ("half in screen", Box::new(move || {
            let mut c = fast(media()).lane("opacity", 0.0, 0.5).build();
            c.blend_mode = klipp_core::BlendMode::Screen;
            (vec![c], false)
        })),
    ];
    for (shutter, samples) in [(180.0, 8u8), (90.0, 12)] {
        for (name, make) in &cases {
            let (clips, camera) = make();
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: W, height: H };
            let mut src = Flat::default();
            let mut tracks = Vec::new();
            for (k, c) in clips.into_iter().enumerate() {
                src.0.insert(c.id, picture(0.3 + k as f32 * 0.2));
                let mut t = Track::new(&format!("V{}", k + 2), TrackKind::Video);
                t.clips = vec![c];
                tracks.push(t);
            }
            let base = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
            src.0.insert(base.id, picture(0.05));
            let mut b = Track::new("V1", TrackKind::Video);
            b.clips = vec![base];
            tracks.push(b);
            seq.tracks = tracks;
            seq.camera.shutter_deg = shutter;
            seq.camera.shutter_samples = samples;
            if camera {
                klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 0.0, 0.2);
                klipp_core::keyframe::upsert_keyframe(&mut seq.camera.x_keyframes, 2.0, 0.8);
            }
            let mut r = renderer();
            let Some((gpu, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
                let why = r.last_decline();
                assert_ne!(why, Some("motion blur"), "{name}: a moving clip is the stack's now");
                eprintln!("UNVERIFIED: {name}: the GPU stack declined ({why:?})");
                return;
            };
            let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
            let d = worst(&cpu, &gpu);
            assert!(d <= 1, "{name} at {shutter} degrees, {samples} samples: the stack's smear differs from the CPU's by {d} levels");
        }
    }
}

/// #2090: a moving clip with effects after its move (rounded corners, an
/// outer glow and a drop shadow, cast once from the smeared picture) stays
/// on the stack, the effects run on the average there, and matches the
/// CPU within a level at a 180 degree shutter of 8 samples. The control:
/// the same card without them is another picture, so they ran.
#[test]
fn a_moving_clip_with_effects_after_its_move_stays_on_the_stack() {
    use klipp_effects::BuiltinEffect as E;
    let scene = |after: bool| {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut card = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0))
            .lane("scale", 0.0, 0.3).lane("pos_x", 0.0, -150.0).lane("pos_x", 2.0, 150.0);
        if after {
            card = card
                .effect(E::RoundedCorners { radius: 0.25, feather: 0.002 })
                .effect(E::OuterGlow { radius: 0.02, intensity: 0.3, color: [140, 150, 255] })
                .effect(E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity: 0.6 });
        }
        let card = card.build();
        let base = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 2.0);
        let mut src = Flat::default();
        src.0.insert(card.id, picture(0.6));
        src.0.insert(base.id, picture(0.25));
        let (mut v2, mut v1) = (Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video));
        v2.clips = vec![card];
        v1.clips = vec![base];
        seq.tracks = vec![v2, v1];
        seq.camera.shutter_deg = 180.0;
        seq.camera.shutter_samples = 8;
        (seq, src)
    };
    let (seq, mut src) = scene(true);
    let mut r = renderer();
    let Some((gpu, stats)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
        let why = r.last_decline();
        assert_ne!(why, Some("motion blur with effects after the move"), "the stack takes it now");
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required: {why:?}");
        eprintln!("UNVERIFIED: the GPU stack declined ({why:?})");
        return;
    };
    assert_eq!(stats.cpu_layers, 0, "no layer rendered on the CPU");
    let cpu = display(&r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &gpu);
    assert!(d <= 1, "the stack's smear with its effects differs from the CPU's by {d} levels");
    let (plain, mut plain_src) = scene(false);
    let without = display(&r.cpu_render(&plain, tc(1.0), W, H, &mut plain_src, RenderOptions::default()));
    assert!(worst(&without, &gpu) > 8, "the control: without the effects it is another picture");
}

/// #2090's case at 4K, a frame of each route: before, the stack declined
/// it and the frame was the CPU's; now the stack's. Run with
/// `--ignored --nocapture`.
#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_moving_clip_with_effects_after_its_move() {
    use klipp_effects::BuiltinEffect as E;
    struct Even(HashMap<(u32, u32, u32), Arc<Vec<f32>>>, HashMap<ClipId, f32>);
    impl FrameSource for Even {
        fn frame(&mut self, clip: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
            let v = *self.1.get(&clip.id)?;
            let px = self.0.entry((width, height, v.to_bits())).or_insert_with(|| Arc::new([v, v * 0.8, v * 0.6, 1.0].repeat((width * height) as usize)));
            Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(px)), width, height, pts: None })
        }
    }
    let (w, h) = (3840u32, 2160u32);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    let card = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0))
        .lane("scale", 0.0, 0.4).lane("pos_x", 0.0, -1200.0).lane("pos_x", 4.0, 1200.0)
        .effect(E::RoundedCorners { radius: 0.25, feather: 0.002 })
        .effect(E::OuterGlow { radius: 0.02, intensity: 0.3, color: [140, 150, 255] })
        .effect(E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity: 0.6 })
        .build();
    let base = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    let mut src = Even(HashMap::new(), HashMap::from([(card.id, 0.6), (base.id, 0.2)]));
    let (mut v2, mut v1) = (Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video));
    v2.clips = vec![card];
    v1.clips = vec![base];
    seq.tracks = vec![v2, v1];
    seq.camera.shutter_deg = 180.0;
    seq.camera.shutter_samples = 8;
    let mut r = renderer();
    let median = |v: &mut Vec<f64>| { v.sort_by(f64::total_cmp); v[v.len() / 2] };
    let (mut gpu, mut cpu) = (Vec::new(), Vec::new());
    for n in 0..13 {
        let at = tc(1.0 + n as f64 / 24.0);
        let t = std::time::Instant::now();
        assert!(r.render_display_u8_on_gpu(&seq, at, w, h, &mut src, RenderOptions::default()).is_some(), "{:?}", r.last_decline());
        if n >= 3 { gpu.push(t.elapsed().as_secs_f64() * 1000.0); }
    }
    for n in 0..5 {
        let at = tc(1.0 + n as f64 / 24.0);
        let t = std::time::Instant::now();
        std::hint::black_box(r.render(&seq, at, w, h, &mut src, RenderOptions::default()));
        if n >= 1 { cpu.push(t.elapsed().as_secs_f64() * 1000.0); }
    }
    eprintln!("MEASURE 4K moving card with corners, glow and shadow, 180/8: GPU stack {:.1} ms; CPU frame (the old route) {:.1} ms", median(&mut gpu), median(&mut cpu));
}

/// #73: a tracked subject's matte wired into the grade's Output alpha
/// cuts the clip out over the track below, on both routes: inside the
/// disc the clip, outside it the lower track.
#[test]
fn a_subject_matte_cuts_the_clip_out() {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, matte_sequence, params::NodeParams};
    let (mw, mh) = (32u32, 18u32);
    let manifest = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: mw, height: mh, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
    let dir = std::env::temp_dir().join(format!("klipp-cut-out-{}-{}", std::process::id(), ClipId::new().0));
    matte_sequence::create(&dir, &manifest).unwrap();
    let disc: Vec<u8> = (0..mw * mh).map(|i| { let (x, y) = ((i % mw) as f32 + 0.5 - 16.0, (i / mw) as f32 + 0.5 - 9.0); if x * x + y * y < 36.0 { 255 } else { 0 } }).collect();
    for f in 0..=100i64 {
        matte_sequence::put(&dir, &manifest, f * 40_000, &disc).unwrap();
    }
    // Unplaced, and shrunk to 60% and moved 60 px right: the cut follows
    // the picture where it lands (#1982 takes the alpha before the
    // clip's placement).
    let shrink_right = klipp_effects::BuiltinEffect::Transform { position_x: 60.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let cases: [(&str, Vec<klipp_effects::BuiltinEffect>, (u32, u32), Vec<(u32, u32)>); 2] = [
        ("in place", Vec::new(), (W / 2, H / 2), vec![(4, 4)]),
        // The disc's 36 px round 60 px right of the middle; the middle is
        // inside the moved picture but outside the subject.
        ("moved", vec![shrink_right], (W / 2 + 60, H / 2), vec![(W / 2, H / 2), (4, 4)]),
    ];
    for (what, effects, subject_at, outside) in cases {
        let mut g = NodeGraph::default();
        let matte = NodeId::new_v4();
        g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params: NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        g.edges.push(Edge { from_node: matte, from_port: 0, to_node: g.output_node_id, to_port: 1 });
        let (top_col, below_col) = ([0.8f32, 0.2, 0.1, 1.0], [0.1f32, 0.3, 0.8, 1.0]);
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let below = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
        let mut top = TestClip::of(clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0)).effects(effects.clone()).build();
        top.set_grade(g);
        let mut frames = HashMap::new();
        frames.insert(top.id, (W, H, Arc::new(top_col.repeat((W * H) as usize))));
        frames.insert(below.id, (W, H, Arc::new(below_col.repeat((W * H) as usize))));
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![below];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![top];
        // The first track is the top layer.
        seq.tracks = vec![v2, v1];
        let mut src = Shaped { frames, dir: dir.clone(), length: None };
        let shown = |c: [f32; 4]| display(&c.repeat(4))[..3].to_vec();
        let (want_top, want_below) = (shown(top_col), shown(below_col));
        let mut r = renderer();
        let (cpu, found) = r.cpu_render_inspected(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
        assert!(found.declines.iter().all(|d| !d.fails()), "{what}: {:?}", found.declines);
        let mut routes = vec![("CPU", display(&cpu))];
        match r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) {
            Some((gpu, _)) => routes.push(("GPU", gpu)),
            None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "{what}: the GPU stack declined: {:?}", r.last_decline()),
        }
        // The viewer's stack draws what the export's CPU route does.
        if let [(_, cpu), (_, gpu)] = &routes[..] {
            let d = worst(cpu, gpu);
            assert!(d <= 2, "{what}: the stack differs from the CPU by {d} levels");
        }
        for (route, frame) in routes {
            let px = |x: u32, y: u32| frame[((y * W + x) * 4) as usize..((y * W + x) * 4) as usize + 3].to_vec();
            let near = |got: Vec<u8>, want: &[u8]| got.iter().zip(want).all(|(g, w)| (*g as i32 - *w as i32).abs() <= 1);
            assert!(near(px(subject_at.0, subject_at.1), &want_top), "{what} {route}: the clip inside the subject at {subject_at:?}: {:?}", px(subject_at.0, subject_at.1));
            for (x, y) in &outside {
                assert!(near(px(*x, *y), &want_below), "{what} {route}: the track below outside it at {:?}: {:?}", (x, y), px(*x, *y));
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// #73: a cut-out grade pasted onto a clip whose subject was never
/// tracked (its media has no matte folder) is never shown whole and
/// silent: the frame declines, failing, on both routes, and the export's
/// check counts the clip as not covered.
#[test]
fn an_untracked_cut_out_fails_the_frame_and_the_check() {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, params::NodeParams};
    let mut g = NodeGraph::default();
    let matte = NodeId::new_v4();
    g.nodes.push(Node { id: matte, kind: NodeKind::ExternalMatte, label: "Subject".into(), enabled: true, params: NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
    g.edges.push(Edge { from_node: matte, from_port: 0, to_node: g.output_node_id, to_port: 1 });
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut top = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    top.set_grade(g);
    // A source with no matte folders: `subject_matte_dir`'s default.
    let mut src = Flat::default();
    src.0.insert(top.id, picture(0.2));
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![top];
    seq.tracks = vec![v];
    let mut r = renderer();
    let (_, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), klipp_compose::Inspect::default());
    let mut routes = vec![("CPU", found.declines)];
    match r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) {
        Some(_) => routes.push(("GPU", r.frame_declines().to_vec())),
        None => assert!(klipp_effects::gpu::GpuEffectsPipeline::new().is_none(), "the GPU stack declined: {:?}", r.last_decline()),
    }
    for (route, declines) in routes {
        assert!(declines.iter().any(|d| d.fails() && d.reason.contains("track its subject")), "{route}: {declines:?}");
    }
    let short = klipp_compose::subject_matte::uncovered_with(&seq, |_, _| None, |_| None);
    assert_eq!(short.len(), 1, "the export's check counts it: {short:?}");
    assert_eq!((short[0].subject, short[0].covered), (Some(0), 0));
}

/// #2488: an export whose encoder takes no planes made on the GPU, or
/// whose picture is letterboxed, takes the stack's frame brought back
/// once, and it is the CPU renderer's frame: the title, the blended
/// generator and layers, and footage under vignette, sharpen, denoise and
/// fill bars, in each working space.
#[test]
fn the_stacks_frame_brought_back_is_the_renderers_frame() {
    use klipp_core::WorkingColorspace as Ws;
    use klipp_effects::BuiltinEffect as E;
    let (mut seq, mut src) = stack();
    let base = seq.tracks.last_mut().unwrap();
    let c = base.clips[0].clone();
    base.clips[0] = TestClip::of(c).effects([
        E::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.7 },
        E::Sharpen { amount: 0.4 },
        E::Denoise { spatial_sigma: 2.0, color_sigma: 0.05, iterations: 1 },
        E::FillBars { blur: 0.07, zoom: 1.0, darken: 0.25 },
    ]).build();
    for ws in [Ws::Rec709Linear, Ws::AcesCG, Ws::Bt2020Linear] {
        let shown = |linear: &[f32]| klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
        let mut r = renderer();
        r.set_gpu_enabled(true);
        r.set_working_colorspace(ws);
        let Some(gpu) = r.render_linear_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default()) else {
            if r.last_decline().is_none() {
                no_gpu("no usable GPU");
                return;
            }
            panic!("{ws:?}: the GPU stack declined it: {:?}", r.last_decline());
        };
        assert_eq!(gpu.len(), (W * H * 4) as usize);
        let cpu = r.cpu_render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
        let d = worst(&shown(&cpu), &shown(&gpu));
        assert!(d <= 1, "{ws:?}: the stack's frame differs from the CPU renderer by {d} levels");
    }
}
