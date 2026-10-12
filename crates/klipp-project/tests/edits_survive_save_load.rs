//! Edits that used to vanish on save/load, each pinned through the real
//! `save_project_with_color` → `load_project` pipeline.
//!
//! None of these was an error: the project file simply had no place for
//! the value, the loader filled in a default, and the next save made the
//! loss permanent. A transition's alignment and settings, a disabled
//! built-in effect, a title's animation and its objects after the first,
//! and the tracks' magnetic roles.

use klipp_core::{RationalTime, TransitionAlignment, TransitionId, TransitionKind, TransitionUserParams};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::{Clip, ClipKind, MagneticAnchor, Sequence, TextBox, TitleAnimationType, TitleData, TitleObjectContent, Track, TrackKind, TrackRole, Transition};

fn project_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_edits_survive_{}_{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tc(f: i64) -> RationalTime {
    RationalTime::from_frames(f, 24)
}

fn media_clip(from: i64, to: i64) -> Clip {
    Clip::media(klipp_core::MediaId::new(), tc(from), tc(to), tc(0), tc(to - from))
}

fn round_trip(label: &str, seq: Sequence) -> Sequence {
    let dir = project_dir(label);
    save_project_with_color(&dir, &[seq], &MediaPool::new(), &ColorSettings::default(), None, None, "").unwrap();
    let (seqs, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    seqs.into_iter().next().expect("one sequence")
}

#[test]
fn a_transitions_alignment_and_settings_come_back() {
    let mut seq = Sequence::new_default();
    let (a, b) = (media_clip(0, 48), media_clip(48, 96));
    let (a_id, b_id) = (a.id, b.id);
    seq.tracks[0].clips = vec![a, b];
    let user_params = TransitionUserParams { param0: 0.37, ..TransitionUserParams::default() };
    seq.tracks[0].transitions.push(Transition {
        id: TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(12),
        clip_a_id: a_id,
        clip_b_id: b_id,
        user_params,
        alignment: TransitionAlignment::StartAtCut,
    });

    let back = round_trip("transition", seq);
    let t = &back.tracks[0].transitions[0];
    assert_eq!(t.alignment, TransitionAlignment::StartAtCut, "the alignment reverted");
    assert_eq!(t.user_params, user_params, "the per-kind settings reverted");
}

#[test]
fn a_disabled_builtin_effect_stays_disabled() {
    let mut seq = Sequence::new_default();
    let mut c = media_clip(0, 48);
    c.set_effect_list(c.builtin_effects().iter().cloned().chain(std::iter::once(klipp_effects::BuiltinEffect::Opacity { value: 0.5 })).collect(), vec![false]);
    seq.tracks[0].clips = vec![c];

    let back = round_trip("effect-enabled", seq);
    let c = &back.tracks[0].clips[0];
    assert_eq!(c.builtin_effects().len(), 1);
    assert_eq!(c.builtin_effects_enabled(), vec![false], "the effect came back enabled");
}

#[test]
fn a_title_keeps_its_animation_and_every_object() {
    let mut seq = Sequence::new_default();
    let mut td = TitleData::single_text_fx(
        TextBox { text: "Headline".into(), ..TextBox::default() },
        klipp_timeline::LayerFx { loops: vec![klipp_timeline::TitleLoop::ColorCycle { rate_hz: 2.0 }], ..Default::default() },
    );
    td.add_object(
        TitleObjectContent::TextBox(TextBox { text: "Subtitle".into(), font_size: 40.0, ..TextBox::default() }),
        klipp_timeline::default_text_transform(),
    );
    let mut c = Clip::gap(tc(0), tc(48));
    c.kind = ClipKind::Title(td.clone());
    seq.tracks[0].clips = vec![c];

    let back = round_trip("title", seq);
    let ClipKind::Title(loaded) = &back.tracks[0].clips[0].kind else { panic!("not a title") };
    assert_eq!(loaded.objects.len(), 2, "the second object was dropped");
    assert_eq!(loaded.primary_fx().loops, vec![klipp_timeline::TitleLoop::ColorCycle { rate_hz: 2.0 }], "the loop was dropped");
    assert_eq!(loaded, &td, "the title did not round-trip whole");
}

#[test]
fn a_title_saved_with_the_old_animation_opens_with_loops() {
    let mut seq = Sequence::new_default();
    let mut td = TitleData::single_text(TextBox { text: "Roll".into(), ..TextBox::default() });
    td.animation_type = TitleAnimationType::ScrollUp { speed_px_per_sec: 40.0 };
    let mut c = Clip::gap(tc(0), tc(48));
    c.kind = ClipKind::Title(td);
    c.edit_transform_pos_y_keyframes(|v| v.push(klipp_core::Keyframe {
        time_secs: 0.0, value: 1080.0, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
    }));
    seq.tracks[0].clips = vec![c];

    let back = round_trip("title-legacy", seq);
    let clip = &back.tracks[0].clips[0];
    let ClipKind::Title(loaded) = &clip.kind else { panic!("not a title") };
    assert_eq!(loaded.animation_type, TitleAnimationType::Static, "the old kind should be folded away");
    assert_eq!(
        loaded.primary_fx().loops,
        vec![klipp_timeline::TitleLoop::Scroll { direction: klipp_timeline::ScrollDirection::Up, speed_px_per_sec: 40.0, shape: Default::default() }]
    );
    assert!(clip.transform_pos_y_keyframes().is_empty(), "the baked keyframes should go with it");
}

/// #1506: a justified text box's last-line choice and a text box's
/// vertical alignment come back through a real save and a strict load;
/// a box at the defaults writes neither, so it saves as it did before.
#[test]
fn a_text_boxs_justify_and_vertical_alignment_come_back() {
    let mut seq = Sequence::new_default();
    let set = TextBox {
        text: "Set".into(),
        alignment: klipp_timeline::TextAlignment::Justify,
        last_line: klipp_timeline::LastLineAlign::Center,
        vertical_align: klipp_timeline::TextVerticalAlign::Bottom,
        ..TextBox::default()
    };
    let mut td = TitleData::single_text(set);
    td.add_object(TitleObjectContent::TextBox(TextBox { text: "Plain".into(), ..TextBox::default() }), klipp_timeline::default_text_transform());
    let mut c = Clip::gap(tc(0), tc(48));
    c.kind = ClipKind::Title(td.clone());
    seq.tracks[0].clips = vec![c];

    let dir = project_dir("justify");
    klipp_project::save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let saved = std::fs::read_dir(&dir).unwrap().filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok()).collect::<String>();
    let (seqs, ..) = klipp_project::load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(saved.matches("\"vertical_align\"").count(), 1, "only the set box writes its vertical alignment");
    assert_eq!(saved.matches("\"last_line\"").count(), 1, "only the set box writes its last line");
    let ClipKind::Title(loaded) = &seqs[0].tracks[0].clips[0].kind else { panic!("not a title") };
    assert_eq!(loaded, &td, "the title did not round-trip whole");
}

/// A style run's font, style, tracking and baseline offset (#1505), and
/// its own underline (#1515), come back through a real save and a strict
/// load, beside its size and colour.
#[test]
fn a_style_runs_font_style_tracking_and_offset_come_back() {
    let mut seq = Sequence::new_default();
    let styled = klipp_timeline::StyleRun {
        font_name: Some("Libre Baskerville".into()),
        font_style: Some(klipp_timeline::FontStyle::BoldItalic),
        tracking: Some(6.5),
        baseline_offset: Some(-12.0),
        font_size: Some(90.0),
        color: Some([200, 40, 10, 255]),
        ..klipp_timeline::StyleRun::default_for_range(4, 9)
    };
    let raised = klipp_timeline::StyleRun { baseline_offset: Some(20.0), underline: Some(true), ..klipp_timeline::StyleRun::default_for_range(10, 14) };
    let td = TitleData::single_text(TextBox { text: "the quick brown fox".into(), style_runs: vec![styled, raised], ..TextBox::default() });
    let mut c = Clip::gap(tc(0), tc(48));
    c.kind = ClipKind::Title(td.clone());
    seq.tracks[0].clips = vec![c];

    let dir = project_dir("style-runs");
    klipp_project::save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (seqs, ..) = klipp_project::load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let ClipKind::Title(loaded) = &seqs[0].tracks[0].clips[0].kind else { panic!("not a title") };
    assert_eq!(loaded.primary_text().unwrap().style_runs, td.primary_text().unwrap().style_runs, "the style runs did not come back whole");
    assert_eq!(loaded, &td, "the title did not round-trip whole");
}

/// A text box's OpenType features and a style run's (#1501) come back
/// through a real save and a strict load; a box and a run without any
/// write neither.
#[test]
fn a_text_boxs_and_a_runs_features_come_back() {
    let mut seq = Sequence::new_default();
    let run = klipp_timeline::StyleRun { features: vec!["smcp".into(), "-liga".into()], ..klipp_timeline::StyleRun::default_for_range(0, 6) };
    let plain_run = klipp_timeline::StyleRun { color: Some([1, 2, 3, 255]), ..klipp_timeline::StyleRun::default_for_range(7, 9) };
    let set = TextBox { text: "office to".into(), features: vec!["onum".into(), "ss01=1".into()], style_runs: vec![run, plain_run], ..TextBox::default() };
    let mut td = TitleData::single_text(set);
    td.add_object(TitleObjectContent::TextBox(TextBox { text: "Plain".into(), ..TextBox::default() }), klipp_timeline::default_text_transform());
    let mut c = Clip::gap(tc(0), tc(48));
    c.kind = ClipKind::Title(td.clone());
    seq.tracks[0].clips = vec![c];

    let dir = project_dir("features");
    klipp_project::save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let saved = std::fs::read_dir(&dir).unwrap().filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok()).collect::<String>();
    let (seqs, ..) = klipp_project::load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(saved.contains("\"onum\"") && saved.contains("\"smcp\""), "the features were not written");
    assert!(!saved.replace(char::is_whitespace, "").contains("\"features\":[]"), "a box or run without features wrote them");
    let ClipKind::Title(loaded) = &seqs[0].tracks[0].clips[0].kind else { panic!("not a title") };
    assert_eq!(loaded.primary_text().unwrap().features, ["onum", "ss01=1"]);
    assert_eq!(loaded.primary_text().unwrap().style_runs[0].features, ["smcp", "-liga"]);
    assert_eq!(loaded, &td, "the title did not round-trip whole");
}

#[test]
fn magnetic_roles_and_anchors_come_back() {
    let mut seq = Sequence::new_default();
    // V1 Primary, A1 Connected by construction; add a Free video track and
    // an anchored clip on the connected one.
    let mut v2 = Track::new("V2", TrackKind::Video);
    v2.magnetic_role = TrackRole::Free;
    let primary = media_clip(0, 48);
    let primary_id = primary.id;
    seq.tracks[0].clips = vec![primary];
    let mut connected = media_clip(10, 30);
    connected.magnetic_anchor = Some(MagneticAnchor { anchor_clip_id: primary_id, offset: tc(10), alignment: klipp_timeline::AnchorAlignment::StartAligned });
    seq.tracks[1].clips = vec![connected];
    seq.tracks.push(v2);

    let back = round_trip("roles", seq);
    assert_eq!(back.tracks[0].magnetic_role, TrackRole::Primary);
    assert_eq!(back.tracks[1].magnetic_role, TrackRole::Connected);
    assert_eq!(back.tracks[2].magnetic_role, TrackRole::Free, "an explicitly Free track must stay Free");
    let anchor = back.tracks[1].clips[0].magnetic_anchor.as_ref().expect("the anchor was dropped");
    assert_eq!(anchor.anchor_clip_id, primary_id);
    assert_eq!(anchor.offset, tc(10));
}

/// A file written before roles were saved carries none. Such a sequence
/// gets what a new one gets — first video track Primary, first audio
/// track Connected — instead of every track silently becoming Free.
#[test]
fn a_file_without_roles_gets_a_new_sequences_roles() {
    let dir = project_dir("legacy-roles");
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![media_clip(0, 48)];
    save_project_with_color(&dir, &[seq], &MediaPool::new(), &ColorSettings::default(), None, None, "").unwrap();
    // Strip the role fields, as an older Klipp would have written it.
    let p = dir.join("project.json");
    let json = std::fs::read_to_string(&p).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    for t in v["sequences"][0]["tracks"].as_array_mut().unwrap() {
        t.as_object_mut().unwrap().remove("magnetic_role");
        t.as_object_mut().unwrap().remove("storyline_anchor");
    }
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let (seqs, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = &seqs[0];
    assert_eq!(back.tracks[0].magnetic_role, TrackRole::Primary);
    assert_eq!(back.tracks[1].magnetic_role, TrackRole::Connected);
}

#[test]
fn a_timeline_grade_comes_back() {
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(media_clip(0, 48));
    let mut graph = klipp_nodes::NodeGraph::default();
    let corrector = graph.ensure_corrector();
    if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == corrector) {
        n.label = "Show look".into();
    }
    seq.timeline_grade_slot().set(Some(graph.clone()));
    let back = round_trip("timeline-grade", seq);
    let got = back.timeline_grade().cloned().expect("the timeline grade");
    let ids = |g: &klipp_nodes::NodeGraph| g.nodes.iter().map(|n| (n.id, n.label.clone())).collect::<Vec<_>>();
    assert_eq!(ids(&got), ids(&graph));

    let none = round_trip("no-timeline-grade", Sequence::new_default());
    assert!(none.timeline_grade().is_none(), "a sequence with none reads back with none");
}

/// #237: a grade group, its two grades and its members come back.
#[test]
fn a_grade_group_comes_back() {
    let mut seq = Sequence::new_default();
    let mut group = klipp_timeline::GradeGroup::new("Day exterior");
    let mut post = klipp_nodes::NodeGraph::default();
    post.ensure_lut3d();
    group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(post));
    group.stage_slot(klipp_timeline::GradeStage::Pre).set(Some(klipp_nodes::NodeGraph::default()));
    let mut member = media_clip(0, 48);
    member.grade_group = Some(group.id);
    let outsider = media_clip(48, 96);
    seq.tracks[0].clips = vec![member, outsider];
    seq.grade_groups = vec![group.clone()];
    let back = round_trip("grade-group", seq);
    assert_eq!(back.grade_groups, vec![group.clone()]);
    let groups: Vec<_> = back.tracks[0].clips.iter().map(|c| c.grade_group).collect();
    assert_eq!(groups, vec![Some(group.id), None]);

    let none = round_trip("no-grade-group", Sequence::new_default());
    assert!(none.grade_groups.is_empty());
}
