//! A frozen frame's key changes with everything its picture depends on,
//! for exactly the frames it changes, and with nothing that is applied
//! live over it (#3207): so a stale frame is never shown, and undo back to
//! a frozen state finds its frames current again. Also the whole frame's
//! key, which the background render (#3212) caches by.

use klipp_compose::freeze::{frame_key, live_reason, slot_key, FreezeEnv};
use klipp_core::{BlendMode, RationalTime, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, TextBox, TitleData, Track, TrackKind};

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn media(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) }
}

fn title(text: &str, from: f64, to: f64) -> Clip {
    let td = TitleData::single_text(TextBox { text: text.into(), font_size: 28.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) }
}

/// Source → Saturation → Output.
fn saturation(amount: f32) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Saturation, NodeParams { saturation: Some(amount), ..Default::default() }), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

/// A frozen track (V2, index 0) of two clips, 0–2 s and 2–4 s, over V1.
fn project() -> Sequence {
    let a = TestClip::of(media(0.0, 2.0)).effects(vec![BuiltinEffect::GaussianBlur { radius: 2.0 }]).build();
    let b = TestClip::of(media(2.0, 4.0)).effects(vec![BuiltinEffect::Invert { mix: 0.5 }]).build();
    let mut seq = Sequence::new_default();
    seq.tracks = vec![track("V2", TrackKind::Video, vec![a, b]), track("V1", TrackKind::Video, vec![media(0.0, 4.0)])];
    seq
}

/// The frames sampled: four in each clip.
const TIMES: [f64; 8] = [0.1, 0.6, 1.2, 1.9, 2.1, 2.6, 3.2, 3.9];

struct Env {
    media: u64,
    plugin: u64,
    environment: u64,
}

impl Default for Env {
    fn default() -> Self {
        Env { media: 1, plugin: 1, environment: 7 }
    }
}

/// Track 0's slot keys at [`TIMES`].
fn keys(seq: &Sequence, others: &[Sequence], e: &Env) -> Vec<Option<u64>> {
    let media = |_: &str| Some(e.media);
    let plugin = |_: &str| Some(e.plugin);
    let env = FreezeEnv { sequences: others, media: &media, plugin: &plugin, environment: e.environment };
    TIMES.iter().map(|t| slot_key(seq, 0, tc(*t), &env)).collect()
}

fn frame_keys(seq: &Sequence, e: &Env) -> Vec<u64> {
    let media = |_: &str| Some(e.media);
    let plugin = |_: &str| Some(e.plugin);
    let env = FreezeEnv { sequences: &[], media: &media, plugin: &plugin, environment: e.environment };
    TIMES.iter().map(|t| frame_key(seq, tc(*t), &env)).collect()
}

/// Which of [`TIMES`] differ between two key lists.
fn changed(a: &[Option<u64>], b: &[Option<u64>]) -> Vec<f64> {
    TIMES.iter().zip(a.iter().zip(b)).filter(|(_, (x, y))| x != y).map(|(t, _)| *t).collect()
}

const FIRST: [f64; 4] = [0.1, 0.6, 1.2, 1.9];
const SECOND: [f64; 4] = [2.1, 2.6, 3.2, 3.9];

#[test]
fn a_key_is_the_same_for_the_same_picture_and_each_frame_its_own() {
    let seq = project();
    let k = keys(&seq, &[], &Env::default());
    assert!(k.iter().all(Option::is_some), "every frame of the track can be frozen");
    assert_eq!(k, keys(&seq.clone(), &[], &Env::default()), "the same for the same picture");
    let mut distinct = k.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), TIMES.len(), "each frame its own key");
}

/// Every edit that changes what the track draws changes the keys of the
/// frames it changes, and only those.
#[test]
fn an_edit_on_the_track_changes_the_keys_of_the_frames_it_changes() {
    let base = project();
    let before = keys(&base, &[], &Env::default());
    let edited = |f: &dyn Fn(&mut Sequence)| {
        let mut s = project_like(&base);
        f(&mut s);
        changed(&before, &keys(&s, &[], &Env::default()))
    };
    // An effect's value on the first clip.
    assert_eq!(edited(&|s| s.tracks[0].clips[0] = TestClip::of(s.tracks[0].clips[0].clone()).effects(vec![BuiltinEffect::GaussianBlur { radius: 3.0 }]).build()), FIRST, "an effect's value");
    // An effect added to the second.
    assert_eq!(edited(&|s| s.tracks[0].clips[1] = TestClip::of(s.tracks[0].clips[1].clone()).effect(BuiltinEffect::Vignette { amount: 0.4, size: 0.5, softness: 0.5, roundness: 0.5 }).build()), SECOND, "an effect added");
    // A keyframe on the first clip's blur.
    assert_eq!(edited(&|s| s.tracks[0].clips[0] = TestClip::of(s.tracks[0].clips[0].clone()).key(0, "radius", 0.0, 1.0).key(0, "radius", 1.0, 4.0).build()), FIRST, "a keyframe");
    // A grade on the second.
    assert_eq!(edited(&|s| s.tracks[0].clips[1].set_grade(saturation(0.4))), SECOND, "a grade");
    // The second clip trimmed to end at 3 s: its frames after are gone,
    // and its in-frames are drawn from a clip that changed.
    let trimmed = edited(&|s| s.tracks[0].clips[1].timeline_out = tc(3.0));
    assert!(SECOND.iter().all(|t| trimmed.contains(t)) && FIRST.iter().all(|t| !trimmed.contains(t)), "a trim: {trimmed:?}");
    // The first clip removed.
    assert_eq!(edited(&|s| { s.tracks[0].clips.remove(0); }), FIRST, "a clip removed");
}

/// Nothing applied live over a frozen layer changes its key: the track's
/// opacity, the clip's blend mode, the tracks around it. The whole frame's
/// key changes with each.
#[test]
fn what_is_applied_live_leaves_a_frozen_layer_current() {
    let base = project();
    let (slot_before, frame_before) = (keys(&base, &[], &Env::default()), frame_keys(&base, &Env::default()));
    let mut s = project_like(&base);
    s.tracks[0].opacity = 0.5;
    s.tracks[0].clips[0].blend_mode = BlendMode::Screen;
    s.tracks[1].clips[0] = TestClip::of(s.tracks[1].clips[0].clone()).effect(BuiltinEffect::Invert { mix: 1.0 }).build();
    assert_eq!(keys(&s, &[], &Env::default()), slot_before, "the frozen layer's keys hold");
    let frame_after = frame_keys(&s, &Env::default());
    assert!(frame_before.iter().zip(&frame_after).all(|(a, b)| a != b), "the whole frame's change");
}

/// A media file, a plug-in's version, the render's environment and a
/// nested sequence each change the keys of the frames that read them.
#[test]
fn a_file_a_plug_in_the_environment_or_a_nested_sequence_changes_the_keys() {
    let base = project();
    let before = keys(&base, &[], &Env::default());
    assert_eq!(changed(&before, &keys(&base, &[], &Env { media: 2, ..Env::default() })), TIMES, "a media file changed on disk");
    assert_eq!(changed(&before, &keys(&base, &[], &Env { environment: 8, ..Env::default() })), TIMES, "the working space or the size");

    // A frei0r plug-in updated: the frames of the clip that runs it.
    let mut with_plugin = project_like(&base);
    with_plugin.tracks[0].clips[1] = TestClip::of(with_plugin.tracks[0].clips[1].clone()).effect(BuiltinEffect::Frei0r { type_id: "frei0r:/usr/lib/frei0r-1/x.so:x".into(), params: Vec::new() }).build();
    let plugged = keys(&with_plugin, &[], &Env::default());
    assert_eq!(changed(&plugged, &keys(&with_plugin, &[], &Env { plugin: 2, ..Env::default() })), SECOND, "a plug-in's version");

    // A precomposition on the track: its scene's title edited.
    let scene = {
        let mut s = Sequence::new_default();
        s.tracks = vec![track("T1", TrackKind::Text, vec![title("Pre", 0.0, 4.0)])];
        s
    };
    let mut nested = project_like(&base);
    nested.tracks[0].clips[0] = Clip::precomposition(scene.id, tc(0.0), tc(2.0));
    let nest_before = keys(&nested, std::slice::from_ref(&scene), &Env::default());
    let mut scene_after = scene.clone();
    scene_after.tracks[0].clips[0] = title("Changed", 0.0, 4.0);
    scene_after.tracks[0].clips[0].id = scene.tracks[0].clips[0].id;
    assert_eq!(changed(&nest_before, &keys(&nested, std::slice::from_ref(&scene_after), &Env::default())), FIRST, "a nested sequence's title");
}

/// Undo back to a frozen state: the same keys, so its frames are current
/// again.
#[test]
fn undo_back_to_a_frozen_state_finds_its_keys_again() {
    let base = project();
    let before = keys(&base, &[], &Env::default());
    let mut s = project_like(&base);
    let undone = s.tracks[0].clips[0].clone();
    s.tracks[0].clips[0] = TestClip::of(undone.clone()).effects(vec![BuiltinEffect::GaussianBlur { radius: 6.0 }]).build();
    assert_ne!(keys(&s, &[], &Env::default()), before);
    s.tracks[0].clips[0] = undone;
    assert_eq!(keys(&s, &[], &Env::default()), before);
}

/// Layers that read or write other tracks' pictures stay live: no key, and
/// a reason.
#[test]
fn a_layer_that_reads_or_writes_other_tracks_stays_live() {
    let mut cases: Vec<(&str, Sequence)> = Vec::new();
    let with_top = |c: Clip| {
        let mut s = project();
        s.tracks[0].clips = vec![c];
        s
    };
    cases.push(("adjustment layer", with_top(Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(4.0)) })));
    let mut matted = media(0.0, 4.0);
    matted.matte_source = Some(klipp_timeline::MatteBinding::new(klipp_core::ClipId::new()));
    cases.push(("matted by another track", with_top(matted)));
    let mut stencil = media(0.0, 4.0);
    stencil.consumed_as_matte = true;
    cases.push(("another clip's matte", with_top(stencil)));
    let mut captions = project();
    captions.tracks[0].kind = TrackKind::Subtitle;
    cases.push(("caption track", captions));
    let media_fn = |_: &str| Some(1u64);
    let env = FreezeEnv { sequences: &[], media: &media_fn, plugin: &media_fn, environment: 0 };
    for (name, seq) in cases {
        assert!(live_reason(&seq, 0, tc(1.0)).is_some(), "{name}: a reason");
        assert_eq!(slot_key(&seq, 0, tc(1.0), &env), None, "{name}: no key");
    }
    assert_eq!(live_reason(&project(), 0, tc(1.0)), None, "a plain clip freezes");
}

/// `base` itself: the tests edit a copy with the same ids.
fn project_like(base: &Sequence) -> Sequence {
    base.clone()
}

/// A picture-truth clip, whose picture its own serde doesn't carry: an
/// edit of its picture changes the keys of its frames.
#[test]
fn an_edit_of_a_picture_truth_clips_picture_changes_its_keys() {
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_lift::ops::GraphOp;
    let mut half = project();
    let c = &mut half.tracks[0].clips[0];
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    for n in &mut g.nodes {
        if n.id == corrector {
            n.params.saturation = Some(0.0);
        }
    }
    c.set_grade(g);
    let group = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).unwrap().id;
    let path = NodePath::new(Root::Clip { clip: c.id.0 }, [group, corrector]);
    c.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.5) }]).unwrap();
    assert!(c.is_picture_truth());
    let before = keys(&half, &[], &Env::default());
    let mut most = half.clone();
    most.tracks[0].clips[0].apply_picture_ops(&[GraphOp::SetMix { path, mix: Some(0.8) }]).unwrap();
    assert_eq!(changed(&before, &keys(&most, &[], &Env::default())), FIRST, "the picture-truth clip's mix");
}
