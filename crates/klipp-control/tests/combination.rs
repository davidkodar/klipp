//! The combination test project (#3071): the parts that are tested on
//! their own, put together in one synthetic project, because bugs keep
//! appearing where they meet.
//!
//! The project, made here from synthetic pictures:
//! - a composition template nested two levels deep (a composition that
//!   holds a precomposition), with a media slot and a macro control, the
//!   control overridden by hand after it was placed;
//! - a recipe inside the inner precomposition, and another on a plain
//!   clip;
//! - a grade with a window and a qualifier, a transition whose outgoing
//!   side is seen through a title's matte, transform keys with an ease;
//! - a title in a style the brief names, and the plain clip's sound
//!   linked under it with fades.
//!
//! Each step names itself and the frame time when it fails. A missing
//! GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it
//! fails.

use std::collections::BTreeMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_document::ops::{resolve, Anchor, Op, PlacedComposition, TrackRef};
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::composition_template::{add_controls, builtin_recipes, CompositionTemplate, ControlSpec, SlotKind, TemplateBinding, TemplateSlot};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// The frame times every step draws: the placed template, the matted
/// clip, the transition, the graded clip under the styled title, the
/// graded clip late in its keys.
const TIMES: [f64; 5] = [1.0, 3.2, 4.5, 5.4, 6.6];

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Every media clip's picture: a grey field with a green square that
/// drifts right over time, and a tint of its own so two clips differ.
struct Pictures;

fn picture(media: &str, secs: f64, w: u32, h: u32) -> Vec<f32> {
    let tint = media.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 5;
    let base = 0.18 + 0.04 * tint as f32;
    let x0 = 0.2 + 0.05 * secs as f32;
    (0..w * h)
        .flat_map(|i| {
            let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            if (x0..x0 + 0.3).contains(&fx) && (0.3..0.7).contains(&fy) {
                [0.05, 0.6, 0.08, 1.0]
            } else {
                [base, base * 0.9, base * 1.1, 1.0]
            }
        })
        .collect()
}

impl FrameSource for Pictures {
    fn frame(&mut self, clip: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let media = match &clip.kind {
            ClipKind::Media { media_id } => media_id.0.to_string(),
            _ => clip.id.0.to_string(),
        };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(picture(&media, secs, w, h))), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        Ok(picture(media, secs, w, h))
    }
}

fn blank(name: &str) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.name = name.into();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks.clear();
    seq
}

fn title(text: &str, from: f64, to: f64) -> Clip {
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 120.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) }
}

/// `recipe` (a built-in with no slots) built into `clip` of `seq`.
fn with_recipe(seq: &Sequence, clip: &mut Clip, recipe: &str) {
    let r = builtin_recipes().into_iter().find(|r| r.name == recipe).unwrap_or_else(|| panic!("no recipe {recipe}"));
    let (ops, _) = klipp_control::recipes::on_clip(seq, clip.id, &r, &BTreeMap::new()).unwrap_or_else(|e| panic!("{recipe}: {e}"));
    clip.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{recipe}: {e}"));
}

/// Applies the first step of `ops` resolved against the project's first
/// sequence.
fn apply(seqs: &mut Vec<Sequence>, pool: &mut klipp_media::MediaPool, ops: &[Op]) -> klipp_document::history::EditCommand {
    let plan = resolve(&seqs[0], pool, ops).unwrap_or_else(|r| panic!("{}", r.why));
    let edit = plan.steps[0].edit.clone();
    let mut fps = seqs[0].fps;
    let mut view = klipp_document::apply::DocumentView { sequences: seqs, active: 0, media_pool: pool, fps: &mut fps };
    let _ = view.apply_forward(&edit);
    edit
}

/// The project's parts, each left out in turn to show it draws.
const PARTS: [(&str, f64); 9] = [
    ("slot", 1.0),
    ("override", 1.0),
    ("inner recipe", 1.0),
    ("matte", 3.2),
    ("transition", 4.5),
    ("grade", 5.4),
    ("recipe", 5.4),
    ("title", 5.4),
    ("keys", 6.6),
];

/// The project: its sequences (the timeline first) and its media pool.
fn project() -> (Vec<Sequence>, klipp_media::MediaPool) {
    project_without(None)
}

/// The project with part `omit` left out.
fn project_without(omit: Option<&str>) -> (Vec<Sequence>, klipp_media::MediaPool) {
    let has = |part: &str| omit != Some(part);
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/combination-photo.png".into(), None);
    item.id = MediaId(uuid::Uuid::from_bytes([9; 16]));
    let photo = pool.add(item);
    // Fixed ids: a picture's tint follows its media id, so every build
    // of the project draws the same pictures.
    let id = |n: u8| MediaId(uuid::Uuid::from_bytes([n; 16]));
    let (inner_media, a_media, b_media, slot_media) = (id(1), id(2), id(3), id(4));

    // The inner precomposition: a shot with a recipe built in.
    let mut inner = blank("Inner");
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::media(inner_media, tc(0.0), tc(3.0), tc(0.0), tc(3.0))];
    inner.tracks = vec![v];
    if has("inner recipe") {
        let mut shot = inner.tracks[0].clips[0].clone();
        with_recipe(&inner, &mut shot, "Gradient map");
        inner.tracks[0].clips[0] = shot;
    }

    // The composition: a headline over the inner precomposition, half
    // size, over a media slot.
    let mut comp = blank("Card");
    let mut t = Track::new("T1", TrackKind::Text);
    t.clips = vec![title("Launch", 0.0, 3.0)];
    let mut over = Track::new("V2", TrackKind::Video);
    let mut pre = Clip::precomposition(inner.id, tc(0.0), tc(3.0));
    let mut k = klipp_document::history::KeyframeSnapshot::from_clip(&pre);
    klipp_core::keyframe::upsert_keyframe(&mut k.transform_scale_keyframes, 0.0, 0.5);
    k.apply_to_clip(&mut pre);
    over.clips = vec![pre];
    let mut under = Track::new("V1", TrackKind::Video);
    under.clips = vec![Clip::media(slot_media, tc(0.0), tc(3.0), tc(0.0), tc(3.0))];
    comp.tracks = vec![t, over, under];
    let slot_clip = comp.tracks.iter().flat_map(|t| &t.clips).position(|c| matches!(c.kind, ClipKind::Media { .. })).unwrap();

    let mut template = CompositionTemplate::new("Card", comp.id.0);
    template.slots.insert("slot_1".into(), TemplateSlot { label: "Photo".into(), kind: SlotKind::Media });
    template.bindings.push(TemplateBinding { parameter: "slot_1".into(), property: format!("clip:{slot_clip}.media"), link: None, take: None });
    let controls: Vec<ControlSpec> = serde_json::from_value(serde_json::json!([{ "label": "Grow", "default": 0.25, "drives": [{ "target": "Text 1 size", "to": [60, 140] }] }])).unwrap();
    add_controls(&mut template, &comp, &controls).unwrap();
    template.sequences = vec![comp.clone(), inner.clone()];

    // The timeline: the template, a clip seen through a title's matte, a
    // transition into a graded clip with keys and a recipe, a styled title
    // over it, and its sound under it.
    let mut seq = blank("Main");
    seq.brief = Some(serde_json::from_value(serde_json::json!({ "type_styles": [{ "name": "kicker", "size": 90.0, "color": "#ffcc33" }] })).unwrap());
    let mut titles = Track::new("Titles", TrackKind::Text);
    titles.clips.clear();
    let mut matte = Track::new("Matte", TrackKind::Text);
    let mut shape = title("KLIPP", 2.5, 4.5);
    shape.consumed_as_matte = true;
    // A and B meet at 4.5 s, each with source to spare for the
    // transition across the cut.
    let mut a = Clip::media(a_media, tc(2.5), tc(4.5), tc(0.0), tc(2.0));
    if has("matte") {
        a.matte_source = Some(klipp_timeline::MatteBinding { source_clip_id: shape.id, channel: klipp_nodes::params::MatteChannel::Alpha, invert: false, feather_px: 2.0, choke_px: 0.0 });
        matte.clips = vec![shape];
    }
    let mut b = Clip::media(b_media, tc(4.5), tc(7.0), tc(0.5), tc(3.0));
    b.fade_in_secs = 0.0;
    if has("grade") { b.edit_grade(|g| {
        let (qual, mask, corr) = g.ensure_windowed_secondary(klipp_nodes::NodeKind::Corrector);
        g.set_window_track(mask, &[(0.0, [0.15, 0.15, 0.7, 0.7])], H as f32 / W as f32, 0.2);
        let q = g.nodes.iter_mut().find(|n| n.id == qual).unwrap();
        for (name, v) in [("qual_enabled", 1.0), ("qual_hue_center", 125.0), ("qual_hue_width", 60.0)] {
            assert!(q.params.apply_override(name, v), "{name}");
        }
        let c = g.nodes.iter_mut().find(|n| n.id == corr).unwrap();
        assert!(c.params.apply_override("exposure", 1.0));
    }); }
    if has("keys") {
        let mut k = klipp_document::history::KeyframeSnapshot::from_clip(&b);
        klipp_core::keyframe::upsert_keyframe(&mut k.transform_scale_keyframes, 0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut k.transform_scale_keyframes, 2.5, 0.7);
        klipp_core::keyframe::ease_into(&mut k.transform_scale_keyframes, 1, klipp_core::keyframe::Ease::InOut);
        k.apply_to_clip(&mut b);
    }
    let mut sound = Clip::media(b_media, tc(4.5), tc(7.0), tc(0.5), tc(3.0));
    sound.fade_in_secs = 0.5;
    sound.fade_out_secs = 0.75;
    sound.linked_clip_id = Some(b.id);
    b.linked_clip_id = Some(sound.id);
    let mut v1 = Track::new("V1", TrackKind::Video);
    if has("transition") { v1.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(0.5),
        alignment: Default::default(),
        user_params: Default::default(),
    }); }
    v1.clips = vec![a, b];
    let mut a1 = Track::new("A1", TrackKind::Audio);
    a1.clips = vec![sound];
    seq.tracks = vec![titles, matte, v1, a1];
    let b_id = seq.tracks[2].clips[1].id;
    if has("recipe") {
        let mut placed = seq.tracks[2].clips[1].clone();
        with_recipe(&seq, &mut placed, "Twirl");
        seq.tracks[2].clips[1] = placed;
    }

    let mut seqs = vec![seq];
    // The template placed by the document, its slot filled and its control
    // set like a field; then the control's target set by hand.
    let missing = klipp_timeline::composition_template::missing_sequences(&template, &seqs);
    apply(&mut seqs, &mut pool, &[Op::Template {
        name: "Card".into(),
        at: Anchor::Secs(0.0),
        secs: Some(2.5),
        track: Some(TrackRef::Index(2)),
        fields: serde_json::from_value(if has("slot") { serde_json::json!({ "Photo": photo.0.to_string(), "Grow": 0.5 }) } else { serde_json::json!({ "Grow": 0.5 }) }).unwrap(),
        source: None,
        kind: None,
        data: None,
        composition: Some(Box::new(PlacedComposition { template, missing })),
    }]);
    let placed = template_clip(&seqs[0]).id;
    if has("override") {
        apply(&mut seqs, &mut pool, &[serde_json::from_value(serde_json::json!({ "op": "template_link", "clip_id": placed.0.to_string(), "target": "object:0.size", "value": 110.0 })).unwrap()]);
    }
    // The title in the brief's style, over the graded clip.
    if has("title") { apply(&mut seqs, &mut pool, &[serde_json::from_value(serde_json::json!({ "op": "title", "text": "Hello", "at": { "secs": 5.0 }, "secs": 1.0, "style": "kicker", "track": 0 })).unwrap()]); }
    assert!(seqs[0].all_clips().any(|c| c.id == b_id), "the graded clip is there");
    (seqs, pool)
}

/// The placed template's clip.
fn template_clip(seq: &Sequence) -> &Clip {
    seq.all_clips().find(|c| matches!(c.kind, ClipKind::TemplatePrecomposition { .. })).expect("the template is placed")
}

fn flat(seqs: &[Sequence]) -> Sequence {
    klipp_timeline::flatten::flatten_sequence(&seqs[0], seqs)
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn cpu_frame(seq: &Sequence, at: f64) -> Vec<u8> {
    let px = renderer(false).render(seq, tc(at), W, H, &mut Pictures, RenderOptions::default());
    klipp_color::display::display_transform_ws_to(&px, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

/// The frame at `at` on the GPU display stack, nothing fallen back;
/// `None` when the stack declined and no GPU is required.
fn gpu_frame(seq: &Sequence, at: f64, step: &str) -> Option<Vec<u8>> {
    let mut r = renderer(true);
    let frame = klipp_effects::residency::frame("combination", at);
    let drawn = r.render_display_u8_on_gpu(seq, tc(at), W, H, &mut Pictures, RenderOptions::default());
    drop(frame);
    let Some((px, _)) = drawn else {
        assert!(!required(), "{step} at {at} s: the GPU stack declined: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: {step} at {at} s: the GPU stack declined ({:?})", r.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "{step} at {at} s: nothing falls back: {:?}", fell.events);
    Some(px)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The timeline's text as the control socket's `timeline` reads it.
fn timeline_text(seq: &Sequence, pool: &klipp_media::MediaPool) -> String {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let ctx = klipp_control::ControlContext {
        commands: tx,
        shared: Arc::new(klipp_core::ControlShared::default()),
        wake_ui: Arc::new(|| {}),
        sequence: Arc::new(klipp_timeline::SharedSequence::new(seq.clone())),
        others: Default::default(),
        media_pool: Arc::new(std::sync::RwLock::new(pool.clone())),
        frame_buffer: Arc::new(std::sync::Mutex::new(klipp_engine::FrameBuffer::new(4, 4))),
        perf_ring: klipp_engine::perf::shared(),
        log_path: std::env::temp_dir().join("klipp-combination.log"),
    };
    let reply = klipp_control::respond_local(&ctx, serde_json::json!({ "method": "timeline" })).expect("the timeline reads");
    reply["text"].as_str().expect("its text").to_string()
}

/// Steps 1 and 2: every frame drawn on the CPU and on the GPU agrees
/// within the parity bar; saved, closed and opened again, the project
/// draws the same frames and reads as the same timeline.
#[test]
fn the_project_draws_alike_on_both_routes_and_after_reopening() {
    let (seqs, pool) = project();
    let seq = flat(&seqs);
    let mut drawn = Vec::new();
    for at in TIMES {
        let cpu = cpu_frame(&seq, at);
        let Some(gpu) = gpu_frame(&seq, at, "step 1 (CPU and GPU)") else { return };
        let d = worst(&cpu, &gpu);
        assert!(d <= 3, "step 1 (CPU and GPU) at {at} s: {d} levels apart");
        drawn.push((at, gpu));
    }
    // Built again, the project draws the same frames: what follows
    // compares builds.
    let again = flat(&project().0);
    for (at, px) in &drawn {
        let d = worst(px, &gpu_frame(&again, *at, "step 1 (built again)").expect("drawn already"));
        assert_eq!(d, 0, "step 1 (built again) at {at} s: {d} levels");
    }
    // Every part draws: left out, its frame is another picture.
    for (part, at) in PARTS {
        let (without, _) = project_without(Some(part));
        let full = &drawn.iter().find(|d| d.0 == at).unwrap().1;
        let other = gpu_frame(&flat(&without), at, "step 1 (each part draws)").expect("drawn already");
        let d = worst(full, &other);
        eprintln!("{part} at {at} s: {d} levels");
        assert!(d > 10, "step 1 (each part draws): {part} at {at} s changes the frame by only {d} levels");
    }

    let dir = std::env::temp_dir().join(format!("klipp-combination-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    klipp_project::save_project(&dir, &seqs, &pool).unwrap_or_else(|e| panic!("step 2 (save): {e}"));
    let loaded = klipp_project::load_project_strict(&dir).unwrap_or_else(|e| panic!("step 2 (reopen): {e}"));
    let (back, back_pool) = (loaded.0, loaded.1);
    assert_eq!(timeline_text(&back[0], &back_pool), timeline_text(&seqs[0], &pool), "step 2 (reopen): the timeline reads the same");
    let reopened = flat(&back);
    for (at, before) in &drawn {
        let after = gpu_frame(&reopened, *at, "step 2 (reopen)").expect("drawn before");
        let d = worst(before, &after);
        assert!(d == 0, "step 2 (reopen) at {at} s: {d} levels from before saving");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
