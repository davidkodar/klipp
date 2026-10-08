//! The Inspector with several clips selected (#843): what is set on the
//! clip it shows is set on every other selected clip that has the same
//! thing, live while a slider is dragged and recorded as one undo step
//! when it is let go. The Inspector draws the primary clip; this copies
//! each change it makes to the rest.

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::Clip;

/// Where in an edit gesture a command falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A drag opens: the other clips are noted as they are.
    Begin,
    /// A value while dragging, or a change made in one go.
    Update,
    /// The drag is let go: the others take the final value, recorded.
    End,
    /// The drag is taken back: the others go back to how they were.
    Abandon,
    /// A change that reshapes the clip (its speed changes its length):
    /// made on each clip by its own handler rather than copied.
    Redo,
    /// A change its own handler records nowhere (an OFX parameter, set
    /// every frame of a drag): copied live, and recorded nowhere either.
    Live,
}

/// The clip a command edits and where it falls in a gesture, for the
/// commands the Inspector edits a clip's settings with.
pub fn classify(cmd: &AppCommand) -> Option<(ClipId, Phase)> {
    Some(match cmd {
        AppCommand::BeginAudioDrag { clip_id, .. } | AppCommand::BeginEffectDrag { clip_id, .. } | AppCommand::BeginAudioChainDrag { clip_id, .. } => (*clip_id, Phase::Begin),
        AppCommand::EndAudioDrag { clip_id, .. } | AppCommand::EndEffectDrag { clip_id, .. } | AppCommand::EndAudioChainDrag { clip_id, .. } => (*clip_id, Phase::End),
        AppCommand::SetEffectParam { clip_id, .. } => (*clip_id, Phase::Live),
        AppCommand::AbandonEffectDrag { clip_id, .. } => (*clip_id, Phase::Abandon),
        AppCommand::UpdateClipAudio { clip_id, .. }
        | AppCommand::UpdateBuiltinEffect { clip_id, .. }
        | AppCommand::UpdateAudioEffectParam { clip_id, .. }
        | AppCommand::SetClipRole { clip_id, .. }
        | AppCommand::SetClipBlendMode { clip_id, .. } => (*clip_id, Phase::Update),
        AppCommand::SetClipSpeed { clip_id, .. } => (*clip_id, Phase::Redo),
        _ => return None,
    })
}

/// `cmd`, made on the clip it names, made on `other` too, from what it
/// left on `primary`. False when `other` has nothing it applies to: an
/// effect `primary` has and `other` does not, an audio plugin it lacks.
/// An effect's type, as its picture node names it: the saved tag
/// (`"opacity"`, `"gaussian_blur"`), whichever node type the lift gives the
/// entry (a placement, an extra Transform, an effect).
fn effect_kind(effect: &klipp_effects::BuiltinEffect) -> Option<String> {
    serde_json::to_value(effect).ok()?.get("type")?.as_str().map(str::to_owned)
}

/// The first of `clip`'s effect entries of type `kind` along its picture's
/// spine, from the source to the output: the order the picture runs them
/// in, which a picture-truth clip may hold apart from its list's.
pub(crate) fn first_on_the_spine(clip: &mut Clip, kind: &str) -> Option<klipp_timeline::EffectId> {
    let lifted = clip.picture_v2().ok()?;
    let graph = &lifted.picture.graph;
    // Back from the output along each node's primary input.
    let mut spine = Vec::new();
    let mut at = Some(graph.output);
    while let Some(id) = at.filter(|id| !spine.contains(id)) {
        spine.push(id);
        at = graph.edges.iter().find(|e| e.to == (id, "in".into())).map(|e| e.from.0);
    }
    spine.reverse();
    let of_kind = |id: &klipp_graph_format::NodeId| {
        graph.nodes.iter().find(|n| n.id == *id).and_then(klipp_graph_lift::effect_params::tag) == Some(kind)
    };
    let ids: Vec<_> = spine.into_iter().filter(of_kind).collect();
    ids.into_iter().map(klipp_timeline::EffectId::of_node).find(|id| clip.index_of(*id).is_some())
}

pub fn copy_to(cmd: &AppCommand, primary: &Clip, other: &mut Clip) -> bool {
    match cmd {
        AppCommand::UpdateClipAudio { .. } | AppCommand::BeginAudioDrag { .. } | AppCommand::EndAudioDrag { .. } => {
            other.gain = primary.gain;
            other.volume = primary.volume;
            other.pan = primary.pan;
            other.fade_in_secs = primary.fade_in_secs;
            other.fade_out_secs = primary.fade_out_secs;
            other.fade_in_curve = primary.fade_in_curve;
            other.fade_out_curve = primary.fade_out_curve;
            other.channel_config = primary.channel_config;
            true
        }
        AppCommand::UpdateBuiltinEffect { index, .. } | AppCommand::EndEffectDrag { effect_index: index, .. } => {
            let Some(effect) = primary.builtin_effects().get(*index) else { return false };
            // The same type of effect on the other clip, the first on its
            // spine (M3 step 4), wherever its list holds it.
            let Some(kind) = effect_kind(effect) else { return false };
            match first_on_the_spine(other, &kind) {
                Some(id) => other.replace_effect(id, effect.clone()),
                None => false,
            }
        }
        AppCommand::SetEffectParam { effect_index, param_name, value, .. } => {
            let Some(plugin) = primary.ofx_effects().get(*effect_index).map(|e| e.plugin_name.clone()) else { return false };
            other.edit_ofx_effects(|effects| match effects.iter_mut().find(|e| e.plugin_name == plugin) {
                Some(e) => {
                    e.param_values.insert(param_name.clone(), value.clone());
                    true
                }
                None => false,
            })
        }
        AppCommand::UpdateAudioEffectParam { effect_index, param_name, value, .. } => {
            let Some(kind) = primary.audio_effects.get(*effect_index).map(|e| e.effect_type.clone()) else { return false };
            let Some(fx) = other.audio_effects.iter_mut().find(|e| e.effect_type == kind) else { return false };
            match fx.params.iter_mut().find(|(n, _)| n == param_name) {
                Some(p) => p.1 = *value,
                None => fx.params.push((param_name.clone(), *value)),
            }
            true
        }
        // An audio plugin drag's values were copied as they came; its end
        // only records them.
        AppCommand::BeginAudioChainDrag { .. } | AppCommand::EndAudioChainDrag { .. } => true,
        AppCommand::SetClipRole { .. } => {
            other.role = primary.role;
            true
        }
        AppCommand::SetClipBlendMode { .. } => {
            other.blend_mode = primary.blend_mode;
            true
        }
        _ => false,
    }
}

/// `cmd` with its clip changed to `clip`, for the commands that reshape a
/// clip ([`Phase::Redo`]) and are made on each clip by its own handler.
pub fn retarget(cmd: &AppCommand, clip: ClipId) -> Option<AppCommand> {
    match cmd {
        AppCommand::SetClipSpeed { speed, ripple, .. } => Some(AppCommand::SetClipSpeed { clip_id: clip, speed: *speed, ripple: *ripple }),
        _ => None,
    }
}

/// Which of the settings the Inspector shows differ across `clips`, by
/// name, for its "mixed" line.
pub fn mixed(clips: &[&Clip]) -> Vec<&'static str> {
    let Some(first) = clips.first() else { return Vec::new() };
    let differs = |f: &dyn Fn(&Clip) -> f64| clips.iter().any(|c| (f(c) - f(first)).abs() > 1e-6);
    let mut out = Vec::new();
    if differs(&|c| f64::from(c.volume)) || differs(&|c| f64::from(c.gain)) {
        out.push("volume");
    }
    if differs(&|c| f64::from(c.pan)) {
        out.push("pan");
    }
    if differs(&|c| c.speed) {
        out.push("speed");
    }
    if differs(&|c| f64::from(c.fade_in_secs)) || differs(&|c| f64::from(c.fade_out_secs)) {
        out.push("fades");
    }
    if clips.iter().any(|c| c.blend_mode != first.blend_mode) {
        out.push("blend mode");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_effects::BuiltinEffect;

    fn clip() -> Clip {
        Clip::media(MediaId::new(), RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(1.0, 24), RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(1.0, 24))
    }

    #[test]
    fn a_volume_set_on_one_clip_lands_on_the_others() {
        let mut primary = clip();
        primary.volume = 0.5;
        primary.fade_in_secs = 1.0;
        let mut other = clip();
        let cmd = AppCommand::UpdateClipAudio {
            clip_id: primary.id, gain: primary.gain, volume: 0.5, pan: 0.0, fade_in_secs: 1.0, fade_out_secs: 0.0,
            fade_in_curve: primary.fade_in_curve, fade_out_curve: primary.fade_out_curve, channel_config: primary.channel_config,
        };
        assert_eq!(classify(&cmd), Some((primary.id, Phase::Update)));
        assert!(copy_to(&cmd, &primary, &mut other));
        assert_eq!((other.volume, other.fade_in_secs), (0.5, 1.0));
    }

    #[test]
    fn an_effect_lands_on_the_same_kind_of_effect_wherever_it_sits() {
        let mut primary = clip();
        primary.set_effect_list(vec![BuiltinEffect::Opacity { value: 0.4 }], primary.builtin_effects_enabled().to_vec());
        let mut other = clip();
        other.set_effect_list(vec![BuiltinEffect::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5 }, BuiltinEffect::Opacity { value: 1.0 }], other.builtin_effects_enabled().to_vec());
        let cmd = AppCommand::UpdateBuiltinEffect { clip_id: primary.id, index: 0, effect: BuiltinEffect::Opacity { value: 0.4 } };
        assert!(copy_to(&cmd, &primary, &mut other));
        assert!(matches!(other.builtin_effects()[1], BuiltinEffect::Opacity { value } if (value - 0.4).abs() < 1e-6));
        assert!(matches!(other.builtin_effects()[0], BuiltinEffect::Vignette { .. }), "the rest of its chain untouched");
        // A clip without that effect is left alone and says so.
        let mut bare = clip();
        assert!(!copy_to(&cmd, &primary, &mut bare));
        assert!(bare.builtin_effects().is_empty());
    }

    /// M3 step 4: the match is by the effect's type as its picture node
    /// names it, the first on the other clip's spine: a Transform the lift
    /// made a placement is still a Transform, a different type in front of
    /// it is passed over, and of two of a type the first the picture runs.
    #[test]
    fn multi_edit_matches_by_type_id() {
        let transform = |x: f32| BuiltinEffect::Transform { position_x: x, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let mut primary = clip();
        primary.set_effect_list(vec![transform(40.0)], vec![true]);
        let mut other = clip();
        other.set_effect_list(vec![BuiltinEffect::Gamma { gamma: 1.2 }, transform(0.0)], vec![true, true]);
        let cmd = AppCommand::UpdateBuiltinEffect { clip_id: primary.id, index: 0, effect: transform(40.0) };
        assert!(copy_to(&cmd, &primary, &mut other));
        assert!(matches!(other.builtin_effects()[1], BuiltinEffect::Transform { position_x, .. } if position_x == 40.0), "the placement's Transform: {:?}", other.builtin_effects());
        assert!(matches!(other.builtin_effects()[0], BuiltinEffect::Gamma { gamma } if gamma == 1.2));

        let mut primary = clip();
        primary.set_effect_list(vec![BuiltinEffect::Gamma { gamma: 0.7 }], vec![true]);
        let mut other = clip();
        other.set_effect_list(vec![BuiltinEffect::Vibrance { amount: 0.2 }, BuiltinEffect::Gamma { gamma: 1.0 }, BuiltinEffect::Gamma { gamma: 2.0 }], vec![true; 3]);
        let cmd = AppCommand::UpdateBuiltinEffect { clip_id: primary.id, index: 0, effect: BuiltinEffect::Gamma { gamma: 0.7 } };
        assert!(copy_to(&cmd, &primary, &mut other));
        let gammas: Vec<f32> = other.builtin_effects().iter().filter_map(|e| match e { BuiltinEffect::Gamma { gamma } => Some(*gamma), _ => None }).collect();
        assert_eq!(gammas, vec![0.7, 2.0], "the first Gamma the picture runs");
    }

    /// The undo step the app records for a change on three clips: the
    /// edited clip's own entry and each other clip whole, in one batch,
    /// taken back by one undo.
    #[test]
    fn one_undo_takes_the_change_off_every_clip() {
        use klipp_document::history::{CommandHistory, EditCommand, HistoryEntry};
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut track = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        track.clips = vec![clip(), clip(), clip()];
        seq.tracks = vec![track];
        let ids: Vec<ClipId> = seq.tracks[0].clips.iter().map(|c| c.id).collect();
        let before: Vec<Clip> = seq.tracks[0].clips.clone();
        // The Inspector sets the first clip's volume; the others copy it.
        seq.tracks[0].clips[0].volume = 0.25;
        let primary = seq.tracks[0].clips[0].clone();
        let cmd = AppCommand::UpdateClipAudio {
            clip_id: ids[0], gain: primary.gain, volume: 0.25, pan: primary.pan, fade_in_secs: primary.fade_in_secs, fade_out_secs: primary.fade_out_secs,
            fade_in_curve: primary.fade_in_curve, fade_out_curve: primary.fade_out_curve, channel_config: primary.channel_config,
        };
        for c in seq.tracks[0].clips.iter_mut().skip(1) {
            assert!(copy_to(&cmd, &primary, c));
        }
        let mut history = CommandHistory::new();
        history.begin_batch();
        history.push(HistoryEntry { command: EditCommand::ReplaceClip { description: "Edit", old: Box::new(before[0].clone()), new: Box::new(primary.clone()) } });
        for (old, now) in before.iter().zip(&seq.tracks[0].clips).skip(1) {
            history.push(HistoryEntry { command: EditCommand::ReplaceClip { description: "Edit Selection", old: Box::new(old.clone()), new: Box::new(now.clone()) } });
        }
        history.end_batch_as("Edit 3 Clips");
        assert!(seq.tracks[0].clips.iter().all(|c| c.volume == 0.25));
        let entry = history.undo().expect("one step");
        let mut seqs = vec![seq];
        let mut pool = klipp_media::MediaPool::default();
        let mut fps = seqs[0].fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_inverse(&entry.command);
        assert!(view.sequences[0].tracks[0].clips.iter().all(|c| c.volume == 1.0), "every clip back as it was");
        assert!(history.undo().is_none(), "it was one step");
    }

    /// #843 review: an audio plugin's drag opens and closes a session like
    /// any other drag, and an OFX parameter, which its own handler records
    /// nowhere, is copied live and recorded nowhere either.
    #[test]
    fn plugin_drags_are_gestures_and_ofx_parameters_are_live() {
        let c = clip();
        let begin = AppCommand::BeginAudioChainDrag { drag_key: "audio_chain:x:0:gain".into(), clip_id: c.id };
        let end = AppCommand::EndAudioChainDrag { drag_key: "audio_chain:x:0:gain".into(), clip_id: c.id };
        assert_eq!(classify(&begin), Some((c.id, Phase::Begin)));
        assert_eq!(classify(&end), Some((c.id, Phase::End)));
        let mut other = clip();
        assert!(copy_to(&end, &c, &mut other), "its values were copied as they came");
        let ofx = AppCommand::SetEffectParam { clip_id: c.id, effect_index: 0, param_name: "amount".into(), value: klipp_core::OfxValue::Double(0.5) };
        assert_eq!(classify(&ofx), Some((c.id, Phase::Live)));
    }

    #[test]
    fn a_speed_is_made_on_each_clip_and_mixed_settings_are_named() {
        let (a, mut b) = (clip(), clip());
        let cmd = AppCommand::SetClipSpeed { clip_id: a.id, speed: 2.0, ripple: false };
        assert_eq!(classify(&cmd), Some((a.id, Phase::Redo)));
        assert!(matches!(retarget(&cmd, b.id), Some(AppCommand::SetClipSpeed { clip_id, speed, .. }) if clip_id == b.id && speed == 2.0));
        b.volume = 0.2;
        b.speed = 2.0;
        assert_eq!(mixed(&[&a, &b]), vec!["volume", "speed"]);
        assert!(mixed(&[&a, &a]).is_empty());
    }
}
