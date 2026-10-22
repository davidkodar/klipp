//! `ui_placement_is_the_renderer_placement` (node-graph M3 step 8): the
//! Placement row reads where the clip's move goes from
//! `resolved_placement`, which runs the renderer's one rule
//! (`placement_cut`) over the renderer's walk of the picture. Each case
//! holds the read to the frame's effects as both routes build them (the
//! picture route, `resolve_effects`, and the list route, `clip_effects`):
//! the Transform sits right before the effect the read names, or last
//! when it says the end, or nowhere when there is nothing to move.
//!
//! The cases are the placement rules' list: automatic with an enabled, a
//! disabled and an identity effect drawn outside the picture, a track's
//! effects baked after the clip's own, a fixed Transform, an identity
//! move, and an adjustment layer.

use klipp_compose::resolve::{placement_in, resolve_effects, resolved_placement};
use klipp_core::{Keyframe, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::Origin;
use klipp_graph_format::path::{Root, TrackPart};
use klipp_graph_lift::ids::{entry_id, RootKind};
use klipp_graph_lift::stack_view::{PlacementBoundary, ResolvedPlacement};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::{self, TestClip};

fn tc(s: f64) -> Timecode {
    RationalTime::from_secs_f64(s, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn shadow(opacity: f32) -> E {
    E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity }
}

fn transform() -> E {
    E::Transform { position_x: 30.0, position_y: -10.0, scale_x: 0.8, scale_y: 0.8, rotation: 5.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

/// A media clip with `effects`, moving across its four seconds unless
/// `still`.
fn clip(kind: ClipKind, effects: Vec<E>, enabled: Vec<bool>, still: bool) -> Clip {
    let mut b = TestClip::of(Clip { kind, ..Clip::gap(tc(0.0), tc(4.0)) });
    for (i, e) in effects.into_iter().enumerate() {
        b = b.effect(e);
        if !enabled.get(i).copied().unwrap_or(true) {
            b = b.off();
        }
    }
    let mut c = b.build();
    if !still {
        c.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, 0.0), key(4.0, 120.0)]);
    }
    c
}

fn media(effects: Vec<E>, enabled: Vec<bool>, still: bool) -> Clip {
    clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, effects, enabled, still)
}

/// `clip` on a track carrying `track_fx`: the sequence, and the clip's
/// render copy with its track.
struct Placed {
    seq: Sequence,
    clip: Clip,
    track: klipp_core::TrackId,
}

fn baked(clip: Clip, track_fx: Vec<E>) -> Placed {
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    let n = track_fx.len();
    track.effects.edit_video_effects(|fx, on| {
        *fx = track_fx;
        *on = vec![true; n];
    });
    track.clips = vec![clip];
    let id = track.id;
    seq.tracks = vec![track];
    let copy = seq.baked_for_render().into_owned().tracks[0].clips[0].clone();
    Placed { seq, clip: copy, track: id }
}

/// What the frame's effects say about the move, from both routes (which
/// must agree): the effect right after the Transform, or the end, or
/// `None` when no Transform is drawn.
fn rendered(name: &str, clip: &Clip, t: f64) -> Option<Option<String>> {
    let lifted = clip.picture_v2().unwrap_or_else(|e| panic!("{name}: lift: {e}"));
    let picture = &lifted.picture;
    let list = klipp_compose::render::list_effects(clip, tc(t), 24.0, None, None).0;
    let fx = resolve_effects(clip, picture, tc(t), 24.0, None, None).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(format!("{fx:?}"), format!("{list:?}"), "{name}: the routes disagree at {t}");
    let at = fx.iter().position(|e| matches!(e, E::Transform { .. }))?;
    Some(fx.get(at + 1).map(kind))
}

fn kind(e: &E) -> String {
    serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_owned()
}

/// The read at `t`, held to the frame: its boundary names the effect the
/// Transform is drawn before, or the end, and nothing is placed only when
/// it says so.
fn read(name: &str, placed: &Placed, t: f64) -> ResolvedPlacement {
    let clip = &placed.clip;
    let lifted = clip.picture_v2().unwrap_or_else(|e| panic!("{name}: lift: {e}"));
    let picture = &lifted.picture;
    let read = resolved_placement(clip, picture, tc(t), 24.0, None, Some(placed.track)).unwrap_or_else(|e| panic!("{name}: {e}")).unwrap_or_else(|| panic!("{name}: no placement"));
    let from_seq = placement_in(&placed.seq, clip.id, tc(t), 24.0, None).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(from_seq.as_ref(), Some(&read), "{name} at {t}: the sequence's read is the copy's");
    let named = match &read.boundary {
        PlacementBoundary::Before(path) => {
            let id = *path.nodes.last().unwrap();
            match path.root {
                Root::Clip { clip: c } => {
                    assert_eq!(c, clip.id.0, "{name}");
                    let node = picture.graph.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{name}: {id} isn't in the picture"));
                    Some(node.legacy.as_ref().unwrap().payload["type"].as_str().unwrap().to_owned())
                }
                // A track's effect by its slot in the track's list.
                Root::Track { track, part: TrackPart::Effects } => {
                    assert_eq!(track, placed.track.0, "{name}");
                    let list = placed.seq.tracks[0].effects.video_effects();
                    let k = (0..list.len()).find(|k| entry_id(RootKind::Track, track, "effects", *k) == id).unwrap_or_else(|| panic!("{name}: {id} is no effect of the track"));
                    Some(kind(&list[k]))
                }
                other => panic!("{name}: a boundary under {other:?}"),
            }
        }
        PlacementBoundary::End => None,
    };
    let frame = rendered(name, clip, t);
    if read.reason == "Auto · nothing to move" {
        assert_eq!(frame, None, "{name} at {t}: the read says nothing moves");
    } else {
        // The frame drops an identity effect after cutting before it, so an
        // identity boundary is held to the effect drawn after it.
        let effects: Vec<E> = test_clip::effects(clip);
        let drawn = named.clone().filter(|k| !effects.iter().any(|e| kind(e) == *k && klipp_effects::is_identity_effect(e)));
        if named.is_none() || drawn.is_some() {
            assert_eq!(frame, Some(drawn), "{name} at {t}: {}", read.reason);
        } else {
            assert!(frame.is_some(), "{name} at {t}: the read places the move, the frame doesn't");
        }
    }
    read
}

fn reads(name: &str, placed: &Placed) -> ResolvedPlacement {
    let first = read(name, placed, 0.4);
    for t in [1.0, 2.5, 3.9] {
        let r = read(name, placed, t);
        // The move animates; where it goes, and the revision, don't.
        assert_eq!(r, first, "{name} at {t}");
    }
    first
}

#[test]
fn ui_placement_is_the_renderer_placement() {
    // Automatic, before an enabled shadow.
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7), E::Vibrance { amount: 0.2 }], vec![true; 3], false), vec![]);
    let r = reads("auto, shadow", &c);
    assert_eq!(r.reason, "Auto · before Drop Shadow, which draws outside the picture");
    assert_eq!(r.provenance, None);

    // A disabled shadow isn't drawn: the move goes last.
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7), E::Vibrance { amount: 0.2 }], vec![true, false, true], false), vec![]);
    let r = reads("auto, disabled shadow", &c);
    assert_eq!((r.boundary, r.reason.as_str()), (PlacementBoundary::End, "Auto · end"));

    // An identity shadow still cuts (the renderer cuts before dropping it).
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.0), E::Vibrance { amount: 0.2 }], vec![true; 3], false), vec![]);
    let r = reads("auto, identity shadow", &c);
    assert_eq!(r.reason, "Auto · before Drop Shadow, which draws outside the picture");

    // The track's shadow, baked after the clip's own: the boundary is the
    // track's node, and the read says so.
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }], vec![true], false), vec![shadow(0.7), E::Invert { mix: 0.2 }]);
    let r = reads("auto, track shadow", &c);
    assert_eq!(r.reason, "Auto · before Drop Shadow, which draws outside the picture");
    assert_eq!(r.provenance, Some(Origin::Track { track: c.track.0 }));

    // A fixed Transform after a shadow: where the automatic one would
    // never go.
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7), transform(), E::Vibrance { amount: 0.2 }], vec![true; 4], false), vec![]);
    let r = reads("fixed", &c);
    assert_eq!(r.reason, "Fixed · before Vibrance");
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, transform()], vec![true; 2], true), vec![]);
    let r = reads("fixed, last", &c);
    assert_eq!((r.boundary, r.reason.as_str()), (PlacementBoundary::End, "Fixed · end"));

    // An identity move and no Transform: nothing is placed.
    let c = baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true; 2], true), vec![]);
    let r = reads("identity", &c);
    assert_eq!((r.boundary, r.reason.as_str()), (PlacementBoundary::End, "Auto · nothing to move"));

    // An adjustment layer: its move is read by the same rule.
    let c = baked(clip(ClipKind::AdjustmentLayer, vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true; 2], false), vec![shadow(0.5)]);
    let r = reads("adjustment layer", &c);
    assert_eq!(r.reason, "Auto · before Drop Shadow, which draws outside the picture");
    assert_eq!(r.provenance, None, "an adjustment layer takes nothing from its track");
}

#[test]
fn the_revision_moves_with_the_picture() {
    // The same clip, its shadow then switched off.
    let one = media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true; 2], false);
    let mut other = one.clone();
    other.set_effect_list(vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true, false]);
    let (a, b) = (baked(one, vec![]), baked(other, vec![]));
    let (ra, rb) = (reads("a", &a), reads("b", &b));
    assert_eq!(ra.plan_revision, read("a again", &a, 0.4).plan_revision, "a read is stable");
    assert_ne!(ra.plan_revision, rb.plan_revision);
}

/// The move as the renderer resolves it (a parent's, the camera's, a
/// shutter sample's) decides whether anything is placed, both ways: a
/// still clip resolved away from identity is placed, a moving clip
/// resolved to identity isn't. Each read held to the frame given the same
/// resolved move.
#[test]
fn the_resolved_move_decides_identity() {
    let identity = (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5);
    let moved = (40.0, -12.0, 0.9, 0.9, 3.0, 0.5, 0.5);
    let cases = [
        ("still, resolved moving", baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true; 2], true), vec![]), moved, Some("Drop Shadow")),
        ("moving, resolved still", baked(media(vec![E::Gamma { gamma: 1.1 }, shadow(0.7)], vec![true; 2], false), vec![]), identity, None),
    ];
    for (name, placed, xf, before) in cases {
        let clip = &placed.clip;
        let lifted = clip.picture_v2().unwrap();
        for t in [0.4, 2.5] {
            let read = resolved_placement(clip, &lifted.picture, tc(t), 24.0, Some(xf), Some(placed.track)).unwrap().unwrap();
            assert_eq!(placement_in(&placed.seq, clip.id, tc(t), 24.0, Some(xf)).unwrap().as_ref(), Some(&read), "{name}");
            let fx = resolve_effects(clip, &lifted.picture, tc(t), 24.0, Some(xf), None).unwrap();
            let at = fx.iter().position(|e| matches!(e, E::Transform { .. }));
            match before {
                Some(effect) => {
                    assert_eq!(read.reason, format!("Auto · before {effect}, which draws outside the picture"), "{name} at {t}");
                    assert_eq!(at.and_then(|i| fx.get(i + 1)).map(kind).as_deref(), Some("drop_shadow"), "{name} at {t}: {fx:?}");
                }
                None => {
                    assert_eq!((read.boundary, read.reason.as_str()), (PlacementBoundary::End, "Auto · nothing to move"), "{name} at {t}");
                    assert_eq!(at, None, "{name} at {t}: {fx:?}");
                }
            }
        }
    }
}
