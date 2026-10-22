//! A clip's effects read from its lifted picture are the effects its list
//! gives, bit for bit (#1089, phase 2b M2, step 10c-1): the renderer reads
//! the picture (step 12a), and the list stands in when it can't be read,
//! until the lists go (step 12f).
//!
//! Each case builds a clip, bakes it as a render copy, and holds
//! `resolve_effects` over the copy's `picture_v2()` to `list_effects` on
//! the copy, at several times: keys and modifier stacks, a disabled
//! effect, the fixed and the automatic placement and an extra Transform,
//! a shadow drawn outside the picture, the film seeds, a colour fade, the
//! deflicker gain, and a track's effects baked after the clip's own.

use klipp_compose::resolve::{resolve_effects, resolve_effects_at};
use klipp_core::{Keyframe, ModifierStack, ParamId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

fn tc(s: f64) -> Timecode {
    RationalTime::from_secs_f64(s, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn media(effects: Vec<E>, enabled: Vec<bool>) -> Clip {
    let mut b = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) });
    for (i, e) in effects.into_iter().enumerate() {
        b = b.effect(e);
        if !enabled.get(i).copied().unwrap_or(true) {
            b = b.off();
        }
    }
    b.build()
}

/// `clip` on a track carrying `track_fx`, baked as a render copy.
fn baked(clip: Clip, track_fx: Vec<E>) -> Clip {
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    let n = track_fx.len();
    track.effects.edit_video_effects(|fx, on| {
        *fx = track_fx;
        *on = vec![true; n];
    });
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let seq = seq.baked_for_render().into_owned();
    seq.tracks[0].clips[0].clone()
}

fn check(name: &str, clip: &Clip) {
    let picture = clip.picture_v2().unwrap_or_else(|e| panic!("{name}: lift: {e}"));
    for t in [0.0, 0.4, 1.0, 1.7, 2.5, 3.9] {
        let want = klipp_compose::render::list_effects(clip, tc(t), 24.0, None, None).0;
        let got = resolve_effects(clip, &picture.picture, tc(t), 24.0, None, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format!("{got:?}"), format!("{want:?}"), "{name} at {t}");
    }
}

#[test]
fn keys_and_modifier_stacks_resolve_as_today() {
    let mut c = media(vec![E::Gamma { gamma: 1.2 }, E::Vibrance { amount: 0.3 }, E::Posterize { levels: 8.0 }], vec![true, true, true]);
    c = TestClip::of(c).key(0, "gamma", 0.0, 0.8).key(0, "gamma", 3.0, 1.6).build();
    c.edit_param_modifiers(|m| {
        let osc = klipp_core::Modifier::Oscillate { amplitude: 0.2, frequency: 1.5, phase: 0.1, waveform: Default::default(), blend: Default::default(), enabled: true };
        m.insert(ParamId::Effect { effect_index: 1, param_name: "amount".into() }, ModifierStack { modifiers: vec![osc] });
    });
    check("keys and modifiers", &baked(c, vec![]));
}

#[test]
fn a_disabled_effect_stays_out() {
    let c = media(vec![E::Gamma { gamma: 1.2 }, E::Invert { mix: 0.5 }, E::Vibrance { amount: 0.3 }], vec![true, false, true]);
    check("disabled", &baked(c, vec![]));
}

#[test]
fn the_transform_is_placed_as_today() {
    let transform = || E::Transform { position_x: 30.0, position_y: -10.0, scale_x: 0.8, scale_y: 0.8, rotation: 5.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let shadow = E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity: 0.7 };
    // A fixed placement after a shadow (where the automatic one would
    // never go), tilted, with an extra Transform after it; the clip moves.
    let tilted = || match transform() {
        E::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, .. } => {
            E::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x: 12.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
        }
        _ => unreachable!(),
    };
    let mut c = media(vec![E::Gamma { gamma: 1.1 }, shadow.clone(), tilted(), transform()], vec![true; 4]);
    c.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, 0.0), key(4.0, 120.0)]);
    check("fixed placement", &baked(c, vec![]));
    // No Transform effect: the clip's own move goes before the shadow.
    let mut c = media(vec![E::Gamma { gamma: 1.1 }, shadow.clone(), E::Vibrance { amount: 0.2 }], vec![true; 3]);
    c.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, 0.0), key(4.0, 120.0)]);
    check("automatic placement", &baked(c, vec![]));
}

#[test]
fn what_the_frame_gives_resolves_as_today() {
    let c = media(
        vec![
            E::FilmGrain { amount: 0.3, size: 1.0, color: 0, shadows: 0.5, seed: 0 },
            E::ColorFade { duration_secs: 1.0, mode: klipp_effects::ColorFadeMode::Out, color: [0, 0, 0, 255], progress: 0.0 },
            E::Deflicker { strength: 0.8, window_secs: 1.0, gain: 0.0 },
        ],
        vec![true; 3],
    );
    check("frame inputs", &baked(c, vec![]));
}

#[test]
fn a_tracks_effects_follow_the_clips_own() {
    let mut c = media(vec![E::Gamma { gamma: 1.2 }, E::Opacity { value: 0.8 }], vec![true, true]);
    c = TestClip::of(c).key(0, "gamma", 0.0, 0.8).key(0, "gamma", 3.0, 1.6).build();
    check("track effects", &baked(c, vec![E::Vibrance { amount: 0.4 }, E::Opacity { value: 0.5 }, E::Invert { mix: 0.2 }]));
}

#[test]
fn an_aliased_key_animates_the_field_it_always_did() {
    // A ChromaKey's "spill" keys drive its soft edge: the lift files them
    // under `soft_edge`, and the node reads them back by "spill".
    let key_fx = E::ChromaKey { key_color: [20, 200, 40], tolerance: 0.3, soft_edge: 0.1, spill_method: klipp_effects::SpillMethod::None, spill_strength: 0.5 };
    let mut c = media(vec![E::Gamma { gamma: 1.1 }, key_fx], vec![true, true]);
    c.edit_effect_keyframes(|k| {
        k.insert("1:spill".into(), vec![key(0.0, 0.05), key(3.0, 0.4)]);
        k.insert("1:tolerance".into(), vec![key(0.0, 0.2), key(2.0, 0.5)]);
    });
    check("aliased key", &baked(c, vec![]));
}

#[test]
fn the_automatic_placement_ignores_what_is_switched_off_or_not_a_pixel_effect() {
    // A switched-off shadow first, and an Opacity, a speed ramp and a
    // temporal denoise among the effects: the move goes before the first
    // *enabled* effect drawn outside the picture, and the others are no
    // pixel effects at all.
    let shadow = E::DropShadow { offset_x: 0.02, offset_y: 0.02, blur: 0.01, color: [0, 0, 0], opacity: 0.7 };
    let glow = E::OuterGlow { radius: 0.02, intensity: 0.5, color: [255, 200, 0] };
    let mut c = media(
        vec![
            shadow,
            E::Gamma { gamma: 1.1 },
            E::Opacity { value: 0.7 },
            E::SpeedRamp { speed: 2.0, blend_mode: 0 },
            E::TemporalDenoise { strength: 0.5, threshold: 0.1 },
            glow,
            E::Vibrance { amount: 0.2 },
        ],
        vec![false, true, true, true, true, true, true],
    );
    c.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| *k = vec![key(0.0, 0.0), key(4.0, 60.0)]);
    check("placement among switched-off and non-pixel effects", &baked(c, vec![]));
}

/// A frame rendered smaller resolves each effect scaled as the renderer
/// scales it after resolving today (node-graph M2 step 13): keyed pixel
/// sizes, a whole radius, a block, and an effect with nothing to scale.
#[test]
fn a_frame_at_render_scale_resolves_each_effect_scaled() {
    let mut c = media(
        vec![E::GaussianBlur { radius: 6.0 }, E::Dilate { radius: 3 }, E::Mosaic { block_size: 2.0 }, E::Gamma { gamma: 1.2 }],
        vec![true, true, true, true],
    );
    c = TestClip::of(c).key(0, "radius", 0.0, 4.0).key(0, "radius", 3.0, 12.0).build();
    let clip = baked(c, vec![E::Bloom { threshold: 0.6, radius: 8.0, intensity: 0.5 }]);
    let picture = clip.picture_v2().unwrap();
    for k in [1.0, 0.5, 0.25] {
        for t in [0.0, 1.0, 2.5] {
            let want: Vec<E> = klipp_compose::render::list_effects(&clip, tc(t), 24.0, None, None).0.into_iter().map(|e| e.at_render_scale(k)).collect();
            let got = resolve_effects_at(&clip, &picture.picture, tc(t), 24.0, None, None, k).unwrap();
            assert_eq!(format!("{got:?}"), format!("{want:?}"), "at {k}, {t} s");
        }
    }
    // At scale 1 it is resolve_effects.
    assert_eq!(
        format!("{:?}", resolve_effects_at(&clip, &picture.picture, tc(1.0), 24.0, None, None, 1.0).unwrap()),
        format!("{:?}", resolve_effects(&clip, &picture.picture, tc(1.0), 24.0, None, None).unwrap()),
    );
}
