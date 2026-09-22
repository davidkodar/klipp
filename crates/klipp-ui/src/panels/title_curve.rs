//! Title layer channels shared by curve selection, menus and keyboard deletion.
use klipp_core::{AppCommand, ClipId, Interpolation, Keyframe, TitlePositionAxis};
use klipp_timeline::{Clip, ClipKind, TitleObjectContent, TitleObjectId};

#[derive(Debug, Clone, PartialEq)]
pub enum TitleCurveParam {
    Position(TitlePositionAxis),
    Effect(String),
}

impl TitleCurveParam {
    pub fn keyframes<'a>(&self, clip: &'a Clip, object_id: u32) -> &'a [Keyframe] {
        let ClipKind::Title(title) = &clip.kind else {
            return &[];
        };
        let Some(object) = title.object(TitleObjectId(object_id)) else {
            return &[];
        };
        match self {
            Self::Position(TitlePositionAxis::X) => &object.transform.center_x_keyframes,
            Self::Position(TitlePositionAxis::Y) => &object.transform.center_y_keyframes,
            Self::Effect(key) => match &object.content {
                TitleObjectContent::TextBox(tb) => tb.effect_keyframes_for(key).unwrap_or(&[]),
                _ => &[],
            },
        }
    }

    /// Apply handles without changing the opposite handle, time, or value.
    pub fn set_tangent(
        &self,
        title: &mut klipp_timeline::TitleData,
        object_id: u32,
        index: usize,
        tangent_in: Option<(f64, f32)>,
        tangent_out: Option<(f64, f32)>,
    ) {
        let Some(object) = title.object_mut(TitleObjectId(object_id)) else {
            return;
        };
        let keyframes = match self {
            Self::Position(axis) => object.transform.position_keyframes_mut(*axis),
            Self::Effect(key) => {
                let TitleObjectContent::TextBox(tb) = &mut object.content else {
                    return;
                };
                let Some(keyframes) = tb.effect_keyframes_mut_for(key) else {
                    return;
                };
                keyframes
            }
        };
        if let Some(kf) = keyframes.get_mut(index) {
            if let Some(tangent) = tangent_in {
                kf.tangent_in = tangent;
            }
            if let Some(tangent) = tangent_out {
                kf.tangent_out = tangent;
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Position(TitlePositionAxis::X) => "Layer X".into(),
            Self::Position(TitlePositionAxis::Y) => "Layer Y".into(),
            Self::Effect(key) => {
                let parts: Vec<_> = key.split('.').collect();
                let effect = match parts[0] {
                    "glow" => "Glow",
                    "shadow" => "Shadow",
                    "inner_shadow" => "Inner Shadow",
                    "outline" => "Outline",
                    "extrusion" => "Extrusion",
                    "motion_blur" => "Motion Blur",
                    "char_anim" => "Character Animation",
                    other => other,
                };
                let field = parts.last().unwrap_or(&"").replace('_', " ");
                if parts.len() == 3 {
                    if let Ok(index) = parts[1].parse::<usize>() {
                        return format!("{effect} {} · {field}", index + 1);
                    }
                }
                format!("{effect} · {field}")
            }
        }
    }

    pub fn range(&self) -> (f32, f32, f32) {
        match self {
            Self::Position(_) => (-1.0, 2.0, 0.5),
            Self::Effect(key) => match key.rsplit('.').next().unwrap_or("") {
                "intensity" => (0.0, 5.0, 1.0),
                "lighting" => (0.0, 1.0, 0.5),
                "angle" => (-180.0, 180.0, 0.0),
                "shutter_angle" => (0.0, 360.0, 180.0),
                "offset_x" | "offset_y" => (-200.0, 200.0, 0.0),
                "stagger" => (0.0, 1.0, 0.05),
                "wave_speed" => (0.0, 20.0, 1.0),
                _ => (0.0, 200.0, 0.0),
            },
        }
    }

    pub fn add(&self, clip_id: ClipId, object_id: u32, time_secs: f64, value: f32) -> AppCommand {
        match self {
            Self::Position(axis) => AppCommand::AddTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis: *axis,
                time_secs,
                value,
            },
            Self::Effect(key) => AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: key.clone(),
                time_secs,
                value,
            },
        }
    }
    pub fn remove(&self, clip_id: ClipId, object_id: u32, time_secs: f64) -> AppCommand {
        match self {
            Self::Position(axis) => AppCommand::RemoveTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis: *axis,
                time_secs,
            },
            Self::Effect(key) => AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: key.clone(),
                time_secs,
            },
        }
    }
    pub fn move_keyframe(
        &self,
        clip_id: ClipId,
        object_id: u32,
        index: usize,
        new_time_secs: f64,
        new_value: f32,
    ) -> AppCommand {
        match self {
            Self::Position(axis) => AppCommand::MoveTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis: *axis,
                index,
                new_time_secs,
                new_value,
            },
            Self::Effect(key) => AppCommand::MoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: key.clone(),
                index,
                new_time_secs,
                new_value,
            },
        }
    }
    pub fn interpolation(
        &self,
        clip_id: ClipId,
        object_id: u32,
        keyframe_index: usize,
        interpolation: Interpolation,
    ) -> AppCommand {
        match self {
            Self::Position(axis) => AppCommand::SetTitleObjectPositionKeyframeInterpolation {
                clip_id,
                object_id,
                axis: *axis,
                keyframe_index,
                interpolation,
            },
            Self::Effect(key) => AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id,
                object_id,
                param_key: key.clone(),
                keyframe_index,
                interpolation,
            },
        }
    }
    pub fn tangent(
        &self,
        clip_id: ClipId,
        object_id: u32,
        keyframe_index: usize,
        tangent_in: Option<(f64, f32)>,
        tangent_out: Option<(f64, f32)>,
    ) -> AppCommand {
        match self {
            Self::Position(axis) => AppCommand::SetTitleObjectPositionKeyframeTangent {
                clip_id,
                object_id,
                axis: *axis,
                keyframe_index,
                tangent_in,
                tangent_out,
            },
            Self::Effect(key) => AppCommand::SetTitleObjectEffectKeyframeTangent {
                clip_id,
                object_id,
                param_key: key.clone(),
                keyframe_index,
                tangent_in,
                tangent_out,
            },
        }
    }
}

/// Include every available channel, even before its first keyframe is added.
pub fn parameters(object: &klipp_timeline::TitleObject) -> Vec<TitleCurveParam> {
    use klipp_core::title_effect_keys as k;
    let mut params = vec![
        TitleCurveParam::Position(TitlePositionAxis::X),
        TitleCurveParam::Position(TitlePositionAxis::Y),
    ];
    if let TitleObjectContent::TextBox(tb) = &object.content {
        let mut keys = vec![
            k::EXTRUSION_DEPTH.into(),
            k::EXTRUSION_ANGLE.into(),
            k::EXTRUSION_LIGHTING.into(),
            k::MOTION_BLUR_SHUTTER_ANGLE.into(),
            k::CHAR_ANIM_STAGGER.into(),
            k::CHAR_ANIM_WAVE_AMPLITUDE.into(),
            k::CHAR_ANIM_WAVE_SPEED.into(),
        ];
        for i in 0..tb.glows.len() {
            keys.extend([k::glow_radius_key(i), k::glow_intensity_key(i)]);
        }
        for i in 0..tb.shadows.len() {
            keys.extend([
                k::shadow_offset_x_key(i),
                k::shadow_offset_y_key(i),
                k::shadow_blur_key(i),
            ]);
        }
        for i in 0..tb.inner_shadows.len() {
            keys.extend([
                k::inner_shadow_offset_x_key(i),
                k::inner_shadow_offset_y_key(i),
                k::inner_shadow_blur_key(i),
            ]);
        }
        for i in 0..tb.outlines.len() {
            keys.push(k::outline_width_key(i));
        }
        params.extend(
            keys.into_iter()
                .filter(|key| tb.effect_keyframes_for(key).is_some())
                .map(TitleCurveParam::Effect),
        );
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, Timecode};
    use klipp_document::{apply::DocumentView, history::EditCommand};
    use klipp_timeline::{Sequence, TextBox, TextGlow, TitleData};

    fn animated_title() -> (TitleData, u32) {
        let mut title = TitleData::single_text(TextBox {
            glows: vec![
                TextGlow {
                    enabled: true,
                    color: [255; 4],
                    radius: 10.0,
                    intensity: 0.8,
                    radius_keyframes: Vec::new(),
                    intensity_keyframes: Vec::new()
                };
                2
            ],
            ..Default::default()
        });
        let object = &mut title.objects[0];
        let id = object.id.0;
        let keys = vec![
            Keyframe {
                time_secs: 0.0,
                value: 0.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.1, 0.2),
                tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0,
                value: 1.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
            },
        ];
        object.transform.center_x_keyframes = keys.clone();
        if let TitleObjectContent::TextBox(tb) = &mut object.content {
            *tb.effect_keyframes_mut_for("glow.1.radius").unwrap() = keys;
        }
        (title, id)
    }

    #[test]
    fn title_tangent_edit_changes_curve_and_undo_redo_restores_exact_handles() {
        for param in [
            TitleCurveParam::Position(TitlePositionAxis::X),
            TitleCurveParam::Effect("glow.1.radius".into()),
        ] {
            let (old, id) = animated_title();
            let mut new = old.clone();
            param.set_tangent(&mut new, id, 0, None, Some((0.6, 0.0)));
            let mut clip = Clip::gap(
                Timecode::from_secs_f64(0.0, 24),
                Timecode::from_secs_f64(1.0, 24),
            );
            clip.kind = ClipKind::Title(new.clone());
            let kfs = param.keyframes(&clip, id);
            assert_eq!(kfs[0].tangent_in, (-0.1, 0.2));
            assert!(klipp_core::keyframe::evaluate(kfs, 0.5, 0.0) < 0.4);
            let edit = EditCommand::EditTitle {
                clip_id: clip.id,
                description: "Set Title Tangent",
                old_data: old.clone(),
                new_data: new.clone(),
            };
            let mut sequences = vec![Sequence::new_default()];
            sequences[0].tracks[0].clips.push(clip);
            let mut media_pool = klipp_media::MediaPool::new();
            let mut fps = Fps::film_24();
            let mut doc = DocumentView {
                sequences: &mut sequences,
                active: 0,
                media_pool: &mut media_pool,
                fps: &mut fps,
            };
            doc.apply_inverse(&edit);
            assert_eq!(doc.sequences[0].tracks[0].clips[0].title_data(), Some(&old));
            doc.apply_forward(&edit);
            assert_eq!(doc.sequences[0].tracks[0].clips[0].title_data(), Some(&new));
        }
    }

    #[test]
    fn title_channels_include_empty_stacked_effects_without_crossing_layers() {
        let (mut title, id) = animated_title();
        let channels = parameters(&title.objects[0]);
        assert!(channels.contains(&TitleCurveParam::Effect("glow.0.radius".into())));
        assert!(channels.contains(&TitleCurveParam::Effect("glow.1.intensity".into())));
        assert!(!channels.contains(&TitleCurveParam::Effect("shadow.0.blur".into())));
        let old = title.clone();
        TitleCurveParam::Effect("glow.1.radius".into()).set_tangent(
            &mut title,
            id + 100,
            0,
            None,
            Some((0.3, 3.0)),
        );
        TitleCurveParam::Effect("glow.99.radius".into()).set_tangent(
            &mut title,
            id,
            0,
            None,
            Some((0.3, 3.0)),
        );
        TitleCurveParam::Position(TitlePositionAxis::X).set_tangent(
            &mut title,
            id,
            99,
            None,
            Some((0.3, 3.0)),
        );
        assert_eq!(title, old);
        TitleCurveParam::Effect("glow.1.radius".into()).set_tangent(
            &mut title,
            id,
            0,
            None,
            Some((0.3, 3.0)),
        );
        assert_eq!(title.objects[0].transform, old.objects[0].transform);
        let TitleObjectContent::TextBox(tb) = &title.objects[0].content else {
            panic!()
        };
        assert!(tb.effect_keyframes_for("glow.0.radius").unwrap().is_empty());
    }
}
