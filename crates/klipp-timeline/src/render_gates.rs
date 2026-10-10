//! Render-path decisions over today's clip storage. Keep their individual
//! meanings when the storage changes; a raw entry is not an active effect.
use crate::{Clip, TransformLinkTarget};
use klipp_core::{ClipId, Keyframe};
use klipp_effects::{BuiltinEffect, FocalRef};

impl Clip {
    /// Any raw built-in entry, including disabled and identity entries.
    pub fn has_effect_entries(&self) -> bool {
        !self.builtin_effects().is_empty()
    }

    /// Any raw OFX entry. Render gates that ignore disabled plug-ins must
    /// continue to use [`Self::has_enabled_plugins`] instead.
    pub fn has_plugin_entries(&self) -> bool {
        !self.ofx_effects().is_empty()
    }

    /// Whether the viewer's GPU display stack can draw the raw list,
    /// disabled entries included: not with a frei0r entry, which runs as a
    /// node on the CPU timeline render (M4 step 5).
    pub fn effect_chain_gpu_capable(&self) -> bool {
        !klipp_effects::has_frei0r(self.builtin_effects())
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
            && !self.builtin_effects().iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::Frei0r { type_id, .. } if klipp_effects::frei0r_effect::keeps_history(type_id)))
    }

    /// A modifier entry counts even when its stack is empty or targets audio.
    pub fn has_param_modifiers(&self) -> bool {
        !self.param_modifiers().is_empty()
    }

    /// The seven clip tracks used by direct-present and title-overlay gates.
    /// A single constant key counts; audio, effect, speed and matte keys do not.
    pub fn has_render_keyframes(&self) -> bool {
        !self.opacity_keyframes().is_empty()
            || self
                .transform_keyframe_tracks()
                .iter()
                .any(|k| !k.is_empty())
    }

    /// Existing picture-present animation decision: any modifier or one of
    /// the seven render keyframe tracks, without evaluating their values.
    pub fn has_animation(&self) -> bool {
        self.has_param_modifiers() || self.has_render_keyframes()
    }

    pub fn has_tilt_keyframes(&self) -> bool {
        !self.transform_tilt_x_keyframes().is_empty() || !self.transform_tilt_y_keyframes().is_empty()
    }

    /// Playback's speed-ramp gate; separate from the picture-present tracks.
    pub fn has_speed_keyframes(&self) -> bool {
        !self.speed_keyframes.is_empty()
    }

    /// The shutter gate needs changing neighbouring values, not merely keys.
    pub fn has_changing_transform_keys(&self) -> bool {
        self.transform_keyframe_tracks()
            .iter()
            .any(|k| k.windows(2).any(|w| (w[0].value - w[1].value).abs() > 1e-6))
    }

    fn transform_keyframe_tracks(&self) -> [&[Keyframe]; 6] {
        [
            self.transform_pos_x_keyframes(),
            self.transform_pos_y_keyframes(),
            self.transform_scale_keyframes(),
            self.transform_rotation_keyframes(),
            self.transform_tilt_x_keyframes(),
            self.transform_tilt_y_keyframes(),
        ]
    }

    /// `sits_unmoved` considers raw placement entries, even disabled ones.
    pub fn has_nonidentity_placement_entries(&self) -> bool {
        self.builtin_effects().iter().any(|e| {
            matches!(
                e,
                BuiltinEffect::Transform { .. } | BuiltinEffect::CornerPin { .. }
            ) && !klipp_effects::is_identity_effect(e)
        })
    }

    /// An enabled SpeedRamp requesting frame blending. The caller
    /// separately checks playback direction and speed at the current time.
    pub fn has_frame_blend_entry(&self) -> bool {
        self.builtin_effects().iter().enumerate().any(|(i, e)| {
            self.builtin_effects_enabled().get(i).copied().unwrap_or(true)
                && matches!(
                    e,
                    BuiltinEffect::SpeedRamp { blend_mode: 1, .. }
                )
        })
    }

    /// Whether an enabled plug-in matches a render-path requirement.
    pub fn has_enabled_plugin_matching(&self, predicate: impl Fn(&str) -> bool) -> bool {
        self.ofx_effects()
            .iter()
            .any(|e| e.enabled && predicate(&e.plugin_name))
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
        for e in self.builtin_effects() {
            if let BuiltinEffect::Transform {
                focal_ref: Some(f), ..
            } = e
            {
                match f {
                    FocalRef::Glyph { title_clip_id, .. } => refs.push(ClipId(*title_clip_id)),
                    FocalRef::Tracker { clip_id, .. } => refs.push(ClipId(*clip_id)),
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
        let tracks: [(&str, Track, bool, bool, bool); 13] = [
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
            let mut c = clip();
            assert!(!c.has_param_modifiers());
            c.edit_param_modifiers(|v| v.insert(target, ModifierStack::default()));
            assert!(c.has_param_modifiers(), "an empty stack is still an entry");
            assert!(c.has_animation());
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
