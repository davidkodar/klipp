//! Render-path decisions over the clip's picture (M4 step 12f). Keep their
//! individual meanings; a raw entry is not an active effect.
use crate::{Clip, TransformLinkTarget};
use klipp_core::{ClipId, Keyframe, ParamId};
use klipp_effects::{BuiltinEffect, FocalRef};

impl Clip {
    /// The clip's built-in entries in list order as its picture's nodes
    /// hold them, each with whether it's on (M4 step 12f): a plain effect
    /// from its map, a kind the clip places itself from its stored entry.
    fn picture_entries(&self) -> Vec<(BuiltinEffect, bool)> {
        let Ok(lifted) = self.picture_v2() else { return Vec::new() };
        (0..)
            .map_while(|i| self.effect_node_at(false, i))
            .filter_map(|id| {
                let node = lifted.picture.graph.nodes.iter().find(|n| n.id == id)?;
                let effect = serde_json::from_value(klipp_graph_lift::effect_params::entry_of(node).ok()?).ok()?;
                Some((effect, node.enabled))
            })
            .collect()
    }

    /// Any raw built-in entry, including disabled and identity entries.
    pub fn has_effect_entries(&self) -> bool {
        self.effect_node_at(false, 0).is_some()
    }

    /// Any raw OFX entry. Render gates that ignore disabled plug-ins must
    /// continue to use [`Self::has_enabled_plugins`] instead.
    pub fn has_plugin_entries(&self) -> bool {
        self.effect_node_at(true, 0).is_some()
    }

    /// Whether the viewer's GPU display stack can draw the raw list,
    /// disabled entries included: not with a frei0r entry, which runs as a
    /// node on the CPU timeline render (M4 step 5).
    pub fn effect_chain_gpu_capable(&self) -> bool {
        let effects: Vec<BuiltinEffect> = self.picture_entries().into_iter().map(|(e, _)| e).collect();
        !klipp_effects::has_frei0r(&effects)
    }

    pub fn grade_is_identity(&self) -> bool {
        self.picture.view().node_graph.0.is_identity()
    }

    /// The render-cache/grade-job gate. Plug-ins and animation are deliberately
    /// not part of this existing decision.
    pub fn needs_grade_or_builtin_processing(&self) -> bool {
        !(self.grade_is_identity() && !self.has_effect_entries())
    }

    /// Whether the clip's rendered frames can be kept and shown again by
    /// frame (the render cache, its background jobs, prerendering): only
    /// when the grade or an effect makes them worth keeping, and no effect
    /// keeps history. A frei0r delay or feedback draws a frame from the
    /// frames rendered before it, so a kept one would be shown after a
    /// seek, and rendering frames out of order to keep them would start
    /// its history again (M4 step 5).
    pub fn frames_cacheable(&self) -> bool {
        self.needs_grade_or_builtin_processing()
            && !self.picture_entries().iter().any(|(e, _)| matches!(e, klipp_effects::BuiltinEffect::Frei0r { type_id, .. } if klipp_effects::frei0r_effect::keeps_history(type_id)))
    }

    /// Any modifier stack the clip's picture holds: on its placement and
    /// opacity, on an effect's parameter, or on its sound.
    pub fn has_param_modifiers(&self) -> bool {
        let lanes = [ParamId::Opacity, ParamId::PositionX, ParamId::PositionY, ParamId::Scale, ParamId::Rotation].iter().any(|p| self.truth_lane(p).is_some_and(|(_, m)| !m.is_empty()));
        // A stack with a modifier in it: an empty one changes nothing.
        let stacked = |m: &Option<serde_json::Value>| m.as_ref().and_then(|m| m.get("modifiers")).and_then(serde_json::Value::as_array).is_some_and(|m| !m.is_empty());
        let effects = self.picture_v2().is_ok_and(|l| l.picture.graph.nodes.iter().any(|n| n.anim.values().any(|a| stacked(&a.modifiers))));
        lanes || effects || !self.sound_modifiers().is_empty()
    }

    /// The eight clip tracks used by direct-present and title-overlay gates.
    /// A single constant key counts; audio, effect, speed and matte keys do not.
    pub fn has_render_keyframes(&self) -> bool {
        self.render_tracks().is_some_and(|tracks| tracks.iter().any(|k| !k.is_empty()))
    }

    /// The clip's eight render tracks as its picture holds them: opacity,
    /// then the seven transform tracks (the height's own, #1471, among them).
    fn render_tracks(&self) -> Option<Vec<Vec<Keyframe>>> {
        let lane = |p: ParamId| self.truth_lane(&p).map(|(k, _)| k).unwrap_or_default();
        let [tilt_x, tilt_y] = self.truth_tilt_keys()?;
        Some(vec![lane(ParamId::Opacity), lane(ParamId::PositionX), lane(ParamId::PositionY), lane(ParamId::Scale), lane(ParamId::ScaleY), lane(ParamId::Rotation), tilt_x, tilt_y])
    }

    /// Existing picture-present animation decision: any modifier or one of
    /// the eight render keyframe tracks, without evaluating their values.
    pub fn has_animation(&self) -> bool {
        self.has_param_modifiers() || self.has_render_keyframes()
    }

    pub fn has_tilt_keyframes(&self) -> bool {
        self.truth_tilt_keys().is_some_and(|tilt| tilt.iter().any(|k| !k.is_empty()))
    }

    /// Playback's speed-ramp gate; separate from the picture-present tracks.
    pub fn has_speed_keyframes(&self) -> bool {
        !self.speed_keyframes.is_empty()
    }

    /// The shutter gate needs changing neighbouring values, not merely keys.
    pub fn has_changing_transform_keys(&self) -> bool {
        self.render_tracks().is_some_and(|tracks| tracks[1..].iter().any(|k| k.windows(2).any(|w| (w[0].value - w[1].value).abs() > 1e-6)))
    }

    /// `sits_unmoved` considers raw placement entries, even disabled ones.
    pub fn has_nonidentity_placement_entries(&self) -> bool {
        self.picture_entries().iter().any(|(e, _)| {
            matches!(
                e,
                BuiltinEffect::Transform { .. } | BuiltinEffect::CornerPin { .. }
            ) && !klipp_effects::is_identity_effect(e)
        })
    }

    /// An enabled SpeedRamp requesting frame blending, by cross-fade or
    /// by optical flow. The caller separately checks playback direction
    /// and speed at the current time.
    pub fn has_frame_blend_entry(&self) -> bool {
        self.frame_blend_mode().is_some()
    }

    /// The blend mode of the first enabled SpeedRamp that blends frames:
    /// 1 cross-fades, 2 follows the motion (#2215). `None` when none does.
    pub fn frame_blend_mode(&self) -> Option<u32> {
        self.picture_entries().iter().find_map(|(e, on)| match e {
            BuiltinEffect::SpeedRamp { blend_mode: mode @ (1 | 2), .. } if *on => Some(*mode),
            _ => None,
        })
    }

    /// Whether the clip has a SpeedRamp, on or off, blending or not.
    pub fn has_speed_ramp(&self) -> bool {
        self.picture_entries().iter().any(|(e, _)| matches!(e, BuiltinEffect::SpeedRamp { .. }))
    }

    /// Whether an enabled plug-in matches a render-path requirement.
    pub fn has_enabled_plugin_matching(&self, predicate: impl Fn(&str) -> bool) -> bool {
        self.picture_ofx().is_ok_and(|ofx| ofx.iter().any(|p| p.entry.enabled && predicate(&p.entry.plugin_name)))
    }

    /// Clips needed by this picture's transform link or focal references.
    /// Raw Transform entries count, even when disabled; duplicates and order
    /// are retained just as the frame-snapshot walk previously returned them.
    pub fn picture_references(&self) -> Vec<ClipId> {
        let mut refs = Vec::new();
        if let Some(link) = &self.transform_link {
            match &link.target {
                TransformLinkTarget::Clip(id) => refs.push(*id),
                TransformLinkTarget::Tracker { clip_id, .. } => refs.push(*clip_id),
            }
        }
        for (e, _) in self.picture_entries() {
            if let BuiltinEffect::Transform { focal_ref: Some(f), .. } = e {
                match f {
                    FocalRef::Glyph { title_clip_id, .. } => refs.push(ClipId(title_clip_id)),
                    FocalRef::Tracker { clip_id, .. } => refs.push(ClipId(clip_id)),
                }
            }
        }
        refs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, MediaId, ModifierStack, ParamId, RationalTime};
    use crate::test_clip::TestClip;

    fn clip() -> Clip {
        let t = |n| RationalTime::from_frames(n, 24);
        Clip::media(MediaId::new(), t(0), t(48), t(0), t(48))
    }
    fn key(value: f32) -> Keyframe {
        Keyframe {
            time_secs: 0.0,
            value,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
        }
    }
    fn transform(x: f32) -> BuiltinEffect {
        BuiltinEffect::Transform {
            position_x: x,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
            rotation_x: 0.0,
            rotation_y: 0.0,
            perspective: 0.0,
            focal_ref: None,
        }
    }

    #[test]
    fn raw_effect_gates_keep_disabled_identity_and_placement_entries() {
        for (name, entry, enabled, gpu, placement) in [
            ("empty", None, true, true, false),
            (
                "identity",
                Some(BuiltinEffect::GaussianBlur { radius: 0.0 }),
                true,
                true,
                false,
            ),
            (
                "disabled",
                Some(BuiltinEffect::GaussianBlur { radius: 2.0 }),
                false,
                true,
                false,
            ),
            (
                "opacity",
                Some(BuiltinEffect::Opacity { value: 0.5 }),
                true,
                true,
                false,
            ),
            (
                "identity transform",
                Some(transform(0.0)),
                true,
                true,
                false,
            ),
            (
                "disabled moved transform",
                Some(transform(0.2)),
                false,
                true,
                true,
            ),
            (
                "frei0r",
                Some(BuiltinEffect::Frei0r {
                    type_id: "test".into(),
                    params: vec![],
                }),
                true,
                false,
                false,
            ),
            (
                "disabled frei0r",
                Some(BuiltinEffect::Frei0r {
                    type_id: "test".into(),
                    params: vec![],
                }),
                false,
                false,
                false,
            ),
            (
                "corner pin",
                Some(BuiltinEffect::CornerPin {
                    tl: [0.1, 0.0],
                    tr: [1.0, 0.0],
                    br: [1.0, 1.0],
                    bl: [0.0, 1.0],
                }),
                true,
                true,
                true,
            ),
        ] {
            let present = entry.is_some();
            let mut b = TestClip::of(clip());
            if let Some(e) = entry {
                b = b.effect(e);
                if !enabled {
                    b = b.off();
                }
            }
            let c = b.build();
            assert_eq!(c.has_effect_entries(), present, "{name}");
            assert_eq!(c.effect_chain_gpu_capable(), gpu, "{name}");
            assert_eq!(c.has_nonidentity_placement_entries(), placement, "{name}");
            assert_eq!(c.needs_grade_or_builtin_processing(), present, "{name}");
        }
    }

    /// A frei0r plug-in that keeps history makes the clip's frames
    /// uncacheable (M4 step 5): one that doesn't leaves them so; one not
    /// installed is taken to keep it.
    #[test]
    fn a_frei0r_that_keeps_history_makes_frames_uncacheable() {
        let info = |stem: &str| klipp_frei0r::Frei0rPluginInfo {
            path: format!("/usr/lib/frei0r-1/{stem}.so").into(),
            name: stem.into(),
            author: String::new(),
            plugin_type: klipp_frei0r::PluginType::Filter,
            color_model: klipp_frei0r::ColorModel::Rgba8888,
            frei0r_version: 1,
            major_version: 0,
            minor_version: 1,
            explanation: String::new(),
            params: Vec::new(),
        };
        let (delay, invert) = (info("delay0r"), info("invert0r"));
        klipp_effects::frei0r_effect::_register_for_test(delay.clone());
        klipp_effects::frei0r_effect::_register_for_test(invert.clone());
        let with = |type_id: String| {
            let mut c = clip();
            c.set_effect_list(vec![BuiltinEffect::Frei0r { type_id, params: vec![] }], vec![true]);
            c.frames_cacheable()
        };
        assert!(!with(delay.type_id()), "a delay keeps history");
        assert!(with(invert.type_id()), "an invert doesn't");
        assert!(!with("frei0r:/nowhere/x.so:x".into()), "an unknown plug-in is taken to");
        assert!(!clip().frames_cacheable(), "nothing to keep without a grade or an effect");
    }

    #[test]
    fn plugin_entry_and_enabled_gates_remain_distinct() {
        for (present, enabled) in [(false, false), (true, false), (true, true)] {
            let mut c = clip();
            if present {
                c.edit_ofx_effects(|effects| effects.push(klipp_plugin::AppliedEffect {
                    plugin_name: "temporal".into(),
                    enabled,
                    param_values: Default::default(),
                    context: klipp_plugin::OfxCategory::Filter,
                    transition_input: None,
                    transition_amount: 0.0,
                    id: None,
                }));
            }
            assert_eq!(c.has_plugin_entries(), present);
            assert_eq!(c.has_enabled_plugins(), present && enabled);
            assert_eq!(
                c.has_enabled_plugin_matching(|n| n == "temporal"),
                present && enabled
            );
            assert!(!c.has_enabled_plugin_matching(|n| n == "other"));
            assert!(!c.needs_grade_or_builtin_processing());
        }
    }

    #[test]
    fn animation_gates_pin_each_included_and_excluded_track() {
        type Track = fn(&mut Clip) -> &mut Vec<Keyframe>;
        let tracks: [(&str, Track, bool, bool, bool); 14] = [
            ("opacity", |c| &mut c.picture.view_mut().opacity_keyframes.0, true, false, false),
            ("x", |c| &mut c.picture.view_mut().transform_pos_x_keyframes.0, true, false, true),
            ("y", |c| &mut c.picture.view_mut().transform_pos_y_keyframes.0, true, false, true),
            (
                "scale",
                |c| &mut c.picture.view_mut().transform_scale_keyframes.0,
                true,
                false,
                true,
            ),
            (
                "scale y",
                |c| &mut c.picture.view_mut().transform_scale_y_keyframes.0,
                true,
                false,
                true,
            ),
            (
                "rotation",
                |c| &mut c.picture.view_mut().transform_rotation_keyframes.0,
                true,
                false,
                true,
            ),
            (
                "tilt x",
                |c| &mut c.picture.view_mut().transform_tilt_x_keyframes.0,
                true,
                true,
                true,
            ),
            (
                "tilt y",
                |c| &mut c.picture.view_mut().transform_tilt_y_keyframes.0,
                true,
                true,
                true,
            ),
            ("volume", |c| &mut c.volume_keyframes, false, false, false),
            ("pan", |c| &mut c.pan_keyframes, false, false, false),
            ("gain", |c| &mut c.gain_keyframes, false, false, false),
            ("speed", |c| &mut c.speed_keyframes, false, false, false),
            (
                "matte feather",
                |c| &mut c.matte_feather_keyframes,
                false,
                false,
                false,
            ),
            (
                "matte choke",
                |c| &mut c.matte_choke_keyframes,
                false,
                false,
                false,
            ),
        ];
        for (name, track, render, tilt, transform) in tracks {
            let mut c = clip();
            assert!(!c.has_animation());
            track(&mut c).push(key(0.5));
            assert_eq!(c.has_render_keyframes(), render, "{name}");
            assert_eq!(c.has_animation(), render, "{name}");
            assert_eq!(c.has_tilt_keyframes(), tilt, "{name}");
            assert_eq!(c.has_speed_keyframes(), name == "speed", "{name}");
            assert!(!c.has_changing_transform_keys(), "one constant key: {name}");
            track(&mut c).push(key(0.5));
            assert!(!c.has_changing_transform_keys(), "same values: {name}");
            track(&mut c)[1].value = 0.6;
            assert_eq!(c.has_changing_transform_keys(), transform, "{name}");
        }
        let mut c = clip();
        c.edit_effect_keyframes(|v| v.insert("0:radius".into(), vec![key(1.0)]));
        assert!(
            !c.has_animation(),
            "effect keys were never part of this gate"
        );
        for target in [
            ParamId::Opacity,
            ParamId::Volume,
            ParamId::Effect {
                effect_index: 0,
                param_name: "radius".into(),
            },
        ] {
            // An empty stack changes nothing, and the picture keeps none, so
            // on a picture parameter it leaves the clip static (M4 step 12f);
            // a sound modifier, held beside the picture, is still an entry.
            let sound = matches!(target, ParamId::Volume | ParamId::Pan | ParamId::Gain);
            let real = ModifierStack { modifiers: vec![klipp_core::Modifier::Oscillate { amplitude: 1.0, frequency: 1.0, phase: 0.0, waveform: Default::default(), blend: Default::default(), enabled: true }] };
            // A blur for an effect's stack to be on: one on no effect
            // animates nothing. The blur itself animates nothing.
            let with_target = || TestClip::of(clip()).effect(BuiltinEffect::GaussianBlur { radius: 2.0 }).build();
            for (stack, counts) in [(ModifierStack::default(), sound), (real, true)] {
                let mut c = with_target();
                assert!(!c.has_param_modifiers());
                c.edit_param_modifiers(|v| v.insert(target.clone(), stack));
                assert_eq!(c.has_param_modifiers(), counts, "{target:?}, {} stack", if counts { "a counted" } else { "an empty" });
                assert_eq!(c.has_animation(), counts);
            }
        }
    }

    #[test]
    fn grade_gate_keeps_identity_and_nonidentity_separate() {
        for exposure in [None, Some(0.0), Some(1.0)] {
            let mut c = clip();
            c.view_mut().node_graph
                .0
                .nodes
                .iter_mut()
                .find(|n| n.kind == klipp_nodes::NodeKind::Corrector)
                .unwrap()
                .params
                .exposure = exposure;
            let changed = exposure == Some(1.0);
            assert_eq!(c.grade_is_identity(), !changed);
            assert_eq!(c.needs_grade_or_builtin_processing(), changed);
        }
    }

    #[test]
    fn frame_blend_entry_keeps_mode_and_enabled_rules() {
        for (mode, enabled, expected) in [
            (1, None, true),
            (1, Some(false), false),
            (0, Some(true), false),
        ] {
            let mut b = TestClip::of(clip()).effect(BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: mode });
            if enabled == Some(false) {
                b = b.off();
            }
            let c = b.build();
            assert_eq!(c.has_frame_blend_entry(), expected);
        }
    }

    #[test]
    fn picture_references_keep_disabled_entries_order_and_duplicates() {
        let host = ClipId::new();
        for focal in [
            FocalRef::Glyph {
                title_clip_id: host.0,
                char_index: 0,
                fallback_xy: (0.0, 0.0),
            },
            FocalRef::Tracker {
                clip_id: host.0,
                point_index: 0,
                fallback_xy: (0.0, 0.0),
            },
        ] {
            let mut c = clip();
            assert!(c.picture_references().is_empty());
            c.transform_link = Some(crate::TransformLink::new(host));
            let mut effect = transform(0.0);
            if let BuiltinEffect::Transform { focal_ref, .. } = &mut effect {
                *focal_ref = Some(focal);
            }
            let c = TestClip::of(c).effect(effect).off().build();
            assert_eq!(c.picture_references(), vec![host, host]);
        }
    }
}
