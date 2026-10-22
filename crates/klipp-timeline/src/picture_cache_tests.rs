//! Every public way to change a clip's picture leaves no stale lifted
//! picture behind: after the change, `picture_v2()` shows it (#1089, M1b
//! 2b; the checklist on #1162).

use crate::{Clip, GradeGroup, GradeVersion, Sequence};
use klipp_core::{Interpolation, Keyframe, MediaId, ModifierStack, RationalTime};
use klipp_effects::BuiltinEffect;
use std::sync::Arc;

fn t(n: i64) -> RationalTime {
    RationalTime::from_frames(n, 24)
}

fn clip() -> Clip {
    Clip::media(MediaId::new(), t(0), t(48), t(0), t(48))
}

fn key(value: f32) -> Keyframe {
    Keyframe { time_secs: 0.0, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// A grade that does something: one stop up on the Corrector.
fn brighter() -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    g.nodes[1].params.exposure = Some(1.0);
    g
}

fn oscillate() -> klipp_core::Modifier {
    klipp_core::Modifier::Oscillate {
        amplitude: 1.0,
        frequency: 2.0,
        phase: 0.0,
        waveform: Default::default(),
        blend: Default::default(),
        enabled: true,
    }
}

/// Lift `clip`, change it with `change`, and require the change to show:
/// the lifted picture was dropped, and lifting again differs.
fn shows(name: &str, mut clip: Clip, change: impl FnOnce(&mut Clip)) {
    let before = clip.picture_v2().unwrap();
    change(&mut clip);
    assert!(!clip.has_lifted_picture(), "{name}: the lifted picture is dropped");
    assert_ne!(*before, *clip.picture_v2().unwrap(), "{name}: the next read shows the change");
}

#[test]
fn a_whole_picture_replaced_never_reads_a_picture_lifted_for_another_clip() {
    let mut a = clip();
    let backup = a.picture.clone();
    let own = a.picture_v2().unwrap();
    a.edit_opacity_keyframes(|v| v.push(key(0.5)));
    // Its own earlier picture put back (as undo does) brings its own lift.
    a.picture = backup.clone();
    let restored = a.picture_v2().unwrap();
    assert!(Arc::ptr_eq(&restored, &own), "the backup shares the cell its clip filled");
    let mut clean = a.clone();
    clean.edit_opacity_keyframes(|_| ()); // empties the cache
    assert_eq!(*restored, *clean.picture_v2().unwrap());

    // Another clip's picture, lifted for that clip's id: never read as this
    // clip's; the next read lifts this clip as it now is.
    let mut b = clip();
    b.edit_opacity_keyframes(|v| v.push(key(0.25)));
    let theirs = b.picture_v2().unwrap();
    let mut replaced = a.clone();
    replaced.picture = b.picture.clone();
    assert!(!replaced.has_lifted_picture());
    let ours = replaced.picture_v2().unwrap();
    assert!(!Arc::ptr_eq(&ours, &theirs));
    assert_eq!(ours.picture.opacity_anim, theirs.picture.opacity_anim, "with that picture's content");
    assert_ne!(*ours, *theirs, "under this clip's own ids");

    std::mem::swap(&mut a.picture, &mut b.picture);
    assert_eq!(a.picture_v2().unwrap().picture.opacity_anim, theirs.picture.opacity_anim, "a swap moves the content");

    let mut taken = b.clone();
    let _ = std::mem::take(&mut taken.picture);
    assert!(!taken.has_lifted_picture(), "a taken picture leaves an empty one");

    let read: Clip = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
    assert!(!read.has_lifted_picture(), "a clip read from JSON starts with none");
    assert_eq!(*read.picture_v2().unwrap(), *b.picture_v2().unwrap(), "and lifts to the same picture");
}

#[test]
fn grade_versions_edited_show() {
    shows("edit_grade_versions", clip(), |c| {
        let v = GradeVersion::new("Warm", brighter());
        c.edit_grade_versions(|vs| vs.push(v));
    });
}

#[test]
fn every_grade_edited_through_the_sequence_shows() {
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(clip());
    let before = seq.tracks[0].clips[0].picture_v2().unwrap();
    seq.edit_each_grade(|g| g.nodes[1].params.exposure = Some(0.5));
    let c = &seq.tracks[0].clips[0];
    assert!(!c.has_lifted_picture());
    assert_ne!(*before, *c.picture_v2().unwrap());
}

#[test]
fn baking_and_unbaking_a_render_copy_show() {
    let mut group = GradeGroup::new("Look");
    group.pre = Some(crate::Grade::new(brighter()));
    let mut seq = Sequence::new_default();
    let mut c = clip();
    c.grade_group = Some(group.id);
    seq.grade_groups.push(group);
    seq.tracks[0].effects.set_video_effect_list(vec![BuiltinEffect::Invert { mix: 1.0 }], vec![]);
    seq.tracks[0].clips.push(c);
    let document = seq.tracks[0].clips[0].picture_v2().unwrap();

    seq.bake_grade_groups();
    let c = &seq.tracks[0].clips[0];
    assert!(!c.has_lifted_picture(), "bake_grade_groups");
    let grouped = c.picture_v2().unwrap();
    assert_ne!(*document, *grouped);

    seq.bake_track_effects();
    let c = &seq.tracks[0].clips[0];
    assert!(!c.has_lifted_picture(), "bake_track_effects");
    assert_ne!(*grouped, *c.picture_v2().unwrap());

    let own = seq.with_own_grades();
    let c = &own.tracks[0].clips[0];
    assert!(!c.has_lifted_picture(), "with_own_grades");
    assert_eq!(*document, *c.picture_v2().unwrap(), "unbaked, it is the document's picture again");
}

#[test]
fn every_effect_list_change_shows() {
    let base = || {
        let mut c = clip();
        c.push_effect(BuiltinEffect::GaussianBlur { radius: 2.0 });
        c.push_effect(BuiltinEffect::Invert { mix: 0.5 });
        c
    };
    let first = |c: &mut Clip| c.effect_id_at(0).unwrap();
    shows("push_effect", base(), |c| {
        c.push_effect(BuiltinEffect::Invert { mix: 0.2 });
    });
    shows("insert_effect", base(), |c| {
        c.insert_effect(0, BuiltinEffect::Invert { mix: 0.2 });
    });
    shows("remove_effect", base(), |c| {
        let id = first(c);
        c.remove_effect(id);
    });
    shows("move_effect", base(), |c| {
        let id = first(c);
        c.move_effect(id, 1);
    });
    shows("set_effect_enabled", base(), |c| {
        let id = first(c);
        c.set_effect_enabled(id, false);
    });
    shows("replace_effect", base(), |c| {
        let id = first(c);
        c.replace_effect(id, BuiltinEffect::Invert { mix: 0.9 });
    });
    shows("set_effects", base(), |c| c.set_effects(vec![BuiltinEffect::Invert { mix: 0.1 }]));
    shows("set_effect_list", base(), |c| c.set_effect_list(vec![], vec![]));
    shows("remove_builtin_effect", base(), |c| {
        c.remove_builtin_effect(1);
    });
    shows("move_builtin_effect", base(), |c| {
        c.move_builtin_effect(0, 1);
    });
    shows("edit_effect", base(), |c| {
        let id = first(c);
        c.edit_effect(id, |e| *e = BuiltinEffect::GaussianBlur { radius: 9.0 });
    });
    shows("edit_effect_keyframes_of", base(), |c| {
        let id = first(c);
        c.edit_effect_keyframes_of(id, "radius", |v| v.push(key(4.0)));
    });
    shows("edit_effect_modifiers", base(), |c| {
        let id = first(c);
        c.edit_effect_modifiers(id, "radius", |m| m.modifiers.push(oscillate()));
    });
    shows("restore_effect_keyframes", base(), |c| {
        c.restore_effect_keyframes([("1:mix".to_string(), vec![key(0.3)])].into());
    });
    shows("restore_effects_state", base(), |c| {
        let mut other = clip();
        let state = other.effects_state();
        c.restore_effects_state(state);
    });
    shows("edit_param_modifiers", base(), |c| {
        c.edit_param_modifiers(|m| m.insert(klipp_core::ParamId::Opacity, ModifierStack::default()));
    });
}

/// A clone given a new id (a split's half, a duplicate) never reads the
/// picture lifted for the clip it came from: the lift derives node ids
/// from the clip's id, which isn't in the view (#1172 review, B2).
#[test]
fn a_clone_given_a_new_id_lifts_its_own_picture() {
    let original = clip();
    let theirs = original.picture_v2().unwrap();
    let mut half = original.clone();
    half.id = klipp_core::ClipId::new();
    assert!(!half.has_lifted_picture(), "the copied cache was lifted for another clip");
    let ours = half.picture_v2().unwrap();
    assert!(!Arc::ptr_eq(&theirs, &ours));
    assert_ne!(*theirs, *ours, "its node ids are its own");
    let mut fresh = half.clone();
    fresh.edit_opacity_keyframes(|_| ()); // empties the cache
    assert_eq!(*ours, *fresh.picture_v2().unwrap(), "the same as a clean lift of the new clip");
    assert!(Arc::ptr_eq(&theirs, &original.picture_v2().unwrap()), "the original keeps its own");
}

/// Effect ids are this session's names for the effects, not picture data:
/// looking one up, or making them anew, keeps the lifted picture.
#[test]
fn an_effect_id_lookup_keeps_the_lifted_picture() {
    let mut c = clip();
    c.push_effect(BuiltinEffect::GaussianBlur { radius: 2.0 });
    c.push_effect(BuiltinEffect::Invert { mix: 0.5 });
    let before = c.picture_v2().unwrap();
    let id = c.effect_id_at(1).unwrap();
    assert_eq!(c.index_of(id), Some(1));
    assert_eq!(c.effect_entries().count(), 2);
    let _ = c.effects_state();
    c.refresh_effect_ids();
    assert!(Arc::ptr_eq(&before, &c.picture_v2().unwrap()), "the same lifted picture, not a new lift");
    // A copy that becomes a new clip names its effects anew (and, being
    // another clip, lifts its own picture: see the test above).
    let mut copy = c.clone();
    copy.id = klipp_core::ClipId::new();
    assert_ne!(copy.effect_id_at(0), c.effect_id_at(0));
}
