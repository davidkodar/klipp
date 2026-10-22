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
            Self::Effect(key) => object.effect_keyframes_for(key).unwrap_or(&[]),
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
                let Some(keyframes) = object.effect_keyframes_mut_for(key) else {
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
            Self::Effect(key) if key.starts_with("text.color.") || key.starts_with("gradient.stop.") => {
                let channel = match key.rsplit('.').next() {
                    Some("r") => "red",
                    Some("g") => "green",
                    Some("b") => "blue",
                    _ => "alpha",
                };
                match klipp_core::title_effect_keys::parse_gradient_stop_key(key) {
                    Some((i, _)) => format!("Gradient stop {} · {channel}", i + 1),
                    None => format!("Text colour · {channel}"),
                }
            }
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
                    "text" => "Text",
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
            // Linear light, and alpha, 0..1.
            Self::Effect(key) if key.starts_with("text.color.") || key.starts_with("gradient.stop.") => (0.0, 1.0, 1.0),
            Self::Effect(key) => match key.rsplit('.').next().unwrap_or("") {
                "intensity" => (0.0, 5.0, 1.0),
                "lighting" => (0.0, 1.0, 0.5),
                "angle" => (-180.0, 180.0, 0.0),
                "shutter_angle" => (0.0, 360.0, 180.0),
                "offset_x" | "offset_y" => (-200.0, 200.0, 0.0),
                "stagger" => (0.0, 1.0, 0.05),
                "wave_speed" => (0.0, 20.0, 1.0),
                "size" => (1.0, 300.0, 72.0),
                "tracking" => (-10.0, 200.0, 0.0),
                "line_spacing" => (0.5, 4.0, 1.0),
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
            k::TEXT_SIZE.into(),
            k::TEXT_TRACKING.into(),
            k::TEXT_LINE_SPACING.into(),
            k::TEXT_SPACE_BEFORE.into(),
            k::TEXT_SPACE_AFTER.into(),
            k::TEXT_INDENT_START.into(),
            k::TEXT_INDENT_END.into(),
            k::TEXT_INDENT_FIRST.into(),
            k::EXTRUSION_DEPTH.into(),
            k::EXTRUSION_ANGLE.into(),
            k::EXTRUSION_LIGHTING.into(),
            k::MOTION_BLUR_SHUTTER_ANGLE.into(),
            k::CHAR_ANIM_STAGGER.into(),
            k::CHAR_ANIM_WAVE_AMPLITUDE.into(),
            k::CHAR_ANIM_WAVE_SPEED.into(),
        ];
        // A colour's channels once it is keyed: it is keyed whole, in
        // the inspector, and shaped channel by channel here (#1511).
        if !tb.color_keyframes.is_empty() {
            keys.extend([k::TEXT_COLOR_R, k::TEXT_COLOR_G, k::TEXT_COLOR_B, k::TEXT_COLOR_A].map(String::from));
        }
        // Where a text on a drawn path sits along it (#1508).
        if matches!(tb.text_path, Some(klipp_timeline::TextPath::Layer { .. })) {
            keys.push(k::TEXT_PATH_OFFSET.into());
        }
        // And a gradient stop's, once its colour is keyed.
        if let Some(g) = &tb.gradient {
            for (i, _) in g.stop_keyframes.iter().enumerate().filter(|(_, s)| !s.is_empty()) {
                keys.extend(["r", "g", "b", "a"].map(|c| format!("{}.{c}", k::gradient_stop_key(i))));
            }
        }
        for i in 0..object.fx().glows.len() {
            keys.extend([k::glow_radius_key(i), k::glow_intensity_key(i)]);
        }
        for i in 0..object.fx().shadows.len() {
            keys.extend([
                k::shadow_offset_x_key(i),
                k::shadow_offset_y_key(i),
                k::shadow_blur_key(i),
            ]);
        }
        for i in 0..object.fx().inner_shadows.len() {
            keys.extend([
                k::inner_shadow_offset_x_key(i),
                k::inner_shadow_offset_y_key(i),
                k::inner_shadow_blur_key(i),
            ]);
        }
        for i in 0..object.fx().outlines.len() {
            keys.push(k::outline_width_key(i));
        }
        params.extend(
            keys.into_iter()
                .filter(|key| object.effect_keyframes_for(key).is_some())
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
        let mut title = TitleData::single_text_fx(
            TextBox::default(),
            klipp_timeline::LayerFx {
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
            },
        );
        let object = &mut title.objects[0];
        let id = object.id.0;
        let keys = vec![
            Keyframe {
                time_secs: 0.0,
                value: 0.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.1, 0.2),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0,
                value: 1.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
            },
        ];
        object.transform.center_x_keyframes = keys.clone();
        *object.effect_keyframes_mut_for("glow.1.radius").unwrap() = keys;
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
        assert!(title.objects[0].effect_keyframes_for("glow.0.radius").unwrap().is_empty());
    }

    /// A text layer's size, tracking and line spacing are channels of
    /// their own, named as the text's (#1511).
    #[test]
    fn a_text_layers_type_is_a_channel() {
        let (title, _) = animated_title();
        let channels = parameters(&title.objects[0]);
        for (key, label) in [("text.size", "Text · size"), ("text.tracking", "Text · tracking"), ("text.line_spacing", "Text · line spacing")] {
            let param = TitleCurveParam::Effect(key.into());
            assert!(channels.contains(&param), "{key}");
            assert_eq!(param.label(), label);
        }
        assert_eq!(TitleCurveParam::Effect("text.size".into()).range(), (1.0, 300.0, 72.0));
    }

    /// A text's colour channels are listed once it is keyed, named as
    /// the colour's, in linear light 0..1 (#1511).
    #[test]
    fn a_keyed_colour_lists_its_channels() {
        let (mut title, _) = animated_title();
        let colour = TitleCurveParam::Effect("text.color.g".into());
        assert!(!parameters(&title.objects[0]).contains(&colour), "not before it is keyed");
        let TitleObjectContent::TextBox(tb) = &mut title.objects[0].content else { panic!("text") };
        tb.color_keyframes.set(0.5, [10, 20, 30, 255]);
        assert!(parameters(&title.objects[0]).contains(&colour));
        assert_eq!(colour.label(), "Text colour · green");
        assert_eq!(colour.range(), (0.0, 1.0, 1.0));
        assert_eq!(title.objects[0].effect_keyframes_for("text.color.g").map(<[_]>::len), Some(1));
    }

    /// A keyed gradient stop's channels are listed, named by the stop.
    #[test]
    fn a_keyed_gradient_stop_lists_its_channels() {
        let (mut title, _) = animated_title();
        let TitleObjectContent::TextBox(tb) = &mut title.objects[0].content else { panic!("text") };
        let mut g = klipp_timeline::TextGradient::pair([255; 4], [0, 0, 0, 255], true);
        let mut keys = klipp_timeline::ColourKeys::default();
        keys.set(0.0, [1, 2, 3, 255]);
        g.stop_keyframes = vec![Default::default(), keys];
        tb.gradient = Some(g);
        let channels = parameters(&title.objects[0]);
        let blue = TitleCurveParam::Effect("gradient.stop.1.b".into());
        assert!(channels.contains(&blue) && !channels.contains(&TitleCurveParam::Effect("gradient.stop.0.b".into())));
        assert_eq!(blue.label(), "Gradient stop 2 · blue");
    }
}
