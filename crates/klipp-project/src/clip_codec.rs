//! A clip in the project file, both ways: what a save writes for a clip
//! (its `ClipJson`, in `format.rs` with the rest of the file's form) and how
//! a load reads it back, each beside the other, so a field added to one
//! side is seen missing from the other. `tests/every_clip_field_survives.rs`
//! checks every field of a clip comes back.

use crate::format::*;
use crate::load::{migrate_graph, parse_uuid};
use crate::path_util::to_project_relative;
use crate::save::{one_rate, timecode_to_json};
use klipp_core::*;
use klipp_timeline::picture_v1::ClipPictureV1;
use klipp_timeline::{Clip, ClipKind, FontStyle, GradeAccess, TextAlignment, TitleBackground, TitlePosition, TitleShadow};
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;
use uuid::Uuid;

// ---- Saving ----

/// A clip as the project file holds it, its audition's takes (#3209)
/// saved the same way.
pub(crate) fn clip_to_json(c: &klipp_timeline::Clip, project_dir: &Path) -> ClipJson {
    // The picture fields come from the one mapping the
    // picture store lifts too (#1089, M1b).
    let ClipPictureV1 {
        node_graph,
        color_grade,
        effects,
        builtin_effects,
        builtin_effects_enabled,
        opacity_keyframes,
        transform_pos_x_keyframes,
        transform_pos_y_keyframes,
        transform_scale_keyframes,
        transform_scale_y_keyframes,
        transform_rotation_keyframes,
        transform_tilt_x_keyframes,
        transform_tilt_y_keyframes,
        effect_keyframes,
        param_modifiers,
        grade_versions,
        active_grade_version,
    } = ClipPictureV1::of(c);
    let ([timeline_in, timeline_out, source_in, source_out], rate) =
        one_rate([c.timeline_in, c.timeline_out, c.source_in, c.source_out]);
    ClipJson {
        id: c.id.to_string(),
        media_id: c.media_id().map(|mid| mid.to_string()),
        kind: Some(clip_kind_to_json(&c.kind, project_dir)),
        timeline_in,
        timeline_out,
        source_in,
        source_out,
        rate,
        node_graph,
        color_grade,
        effects,
        builtin_effects,
        builtin_effects_enabled,
        effects_switched_off: c.effects_switched_off.clone(),
        magnetic_anchor: c.magnetic_anchor.clone(),
        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
        linked_title_asset_id: c.linked_title_asset_id.map(|id| id.to_string()),
        group_id: c.group_id.map(|id| id.to_string()),
        grade_group: c.grade_group.map(|id| id.to_string()),
        gain: if (c.gain - 1.0).abs() > f32::EPSILON { Some(c.gain) } else { None },
        volume: if (c.volume - 1.0).abs() > f32::EPSILON { Some(c.volume) } else { None },
        pan: if c.pan.abs() > f32::EPSILON { Some(c.pan) } else { None },
        fade_in_secs: if c.fade_in_secs.abs() > f32::EPSILON { Some(c.fade_in_secs) } else { None },
        fade_out_secs: if c.fade_out_secs.abs() > f32::EPSILON { Some(c.fade_out_secs) } else { None },
        fade_in_curve: if c.fade_in_curve != klipp_timeline::FadeCurve::Linear { Some(c.fade_in_curve) } else { None },
        fade_out_curve: if c.fade_out_curve != klipp_timeline::FadeCurve::Linear { Some(c.fade_out_curve) } else { None },
        channel_config: if c.channel_config != klipp_timeline::ChannelConfig::Default { Some(c.channel_config) } else { None },
        volume_keyframes: if c.volume_keyframes.is_empty() { None } else { Some(c.volume_keyframes.clone()) },
        pan_keyframes: if c.pan_keyframes.is_empty() { None } else { Some(c.pan_keyframes.clone()) },
        gain_keyframes: if c.gain_keyframes.is_empty() { None } else { Some(c.gain_keyframes.clone()) },
        opacity_keyframes,
        transform_pos_x_keyframes,
        transform_pos_y_keyframes,
        transform_scale_keyframes,
        transform_scale_y_keyframes,
        transform_rotation_keyframes,
        transform_tilt_x_keyframes,
        transform_tilt_y_keyframes,
        frame_glide: c.frame_glide,
        matte_feather_keyframes: if c.matte_feather_keyframes.is_empty() { None } else { Some(c.matte_feather_keyframes.clone()) },
        matte_choke_keyframes: if c.matte_choke_keyframes.is_empty() { None } else { Some(c.matte_choke_keyframes.clone()) },
        clip_markers: if c.clip_markers.is_empty() { None } else {
            Some(c.clip_markers.iter().map(|m| crate::format::ClipMarkerJson {
                time_secs: m.time_secs,
                name: m.name.clone(),
                color: format!("{:?}", m.color),
                comment: m.comment.clone(),
            }).collect())
        },
        effect_keyframes,
        role: None,
        role_id: (c.role.is_some() || !c.role_source.is_unset()).then(|| c.role.clone()),
        role_source: c.role_source,
        matched_to: c.matched_to,
        param_modifiers,
        speed: if (c.speed - 1.0).abs() < f64::EPSILON { None } else { Some(c.speed) },
        speed_keyframes: if c.speed_keyframes.is_empty() { None } else { Some(c.speed_keyframes.clone()) },
        keep_pitch: c.keep_pitch.then_some(true),
        sound_apart: c.sound_apart.then_some(true),
        composition: c.composition,
        composition_versions: c.composition_versions.clone(),
        hold: c.hold.as_ref().map(timecode_to_json),
        source_rotation: if c.source_rotation == 0 { None } else { Some(c.source_rotation) },
        enabled: if c.enabled { None } else { Some(false) },
        audio_effects: if c.audio_effects.is_empty() { None } else { Some(c.audio_effects.clone()) },
        audio_effect_keyframes: c.audio_effect_keyframes.clone(),
        multicam_group: c.multicam_group.map(|g| g.to_string()),
        multicam_angle: if c.multicam_angle == 0 { None } else { Some(c.multicam_angle) },
        multicam_cuts: c.multicam_cuts.clone(),
        multicam_sound: Some(c.multicam_sound).filter(|s| !s.is_default()),
        solo: if c.solo { Some(true) } else { None },
        blend_mode: if matches!(c.blend_mode, klipp_core::BlendMode::Normal) {
            None
        } else {
            Some(c.blend_mode)
        },
        depth: ((c.depth - 1.0).abs() > 1e-6).then_some(c.depth),
        matte_source: c.matte_source.as_ref().map(|m| crate::format::MatteBindingJson {
            source_clip_id: m.source_clip_id.0.to_string(),
            channel: m.channel,
            invert: m.invert,
            feather_px: m.feather_px,
            choke_px: m.choke_px,
        }),
        matte_stencil: c.matte_stencil.as_ref().map(|s| crate::format::StencilSettingsJson {
            channel: s.channel,
            invert: s.invert,
            feather_px: s.feather_px,
            choke_px: s.choke_px,
        }),
        consumed_as_matte: if c.consumed_as_matte { Some(true) } else { None },
        transform_link: c.transform_link.as_ref().map(|tl| crate::format::TransformLinkJson {
            target: tl.target,
            mode: tl.mode,
            inherit_scale: tl.inherit_scale,
            inherit_position: tl.inherit_position,
            inherit_rotation: tl.inherit_rotation,
            inherit_anchor: tl.inherit_anchor,
        }),
        grade_versions,
        active_grade_version,
        track_points: c.track_points.clone(),
        surfaces: c.surfaces.clone(),
        surface_pin: c.surface_pin,
        name: c.name.clone(),
        colour_label: c.colour_label,
        grade_flag: c.grade_flag,
        audition: c.audition.as_ref().map(|a| crate::format::AuditionJson {
            active: a.active.to_string(),
            takes: a.takes.iter().map(|t| crate::format::TakeJson {
                id: t.id.to_string(),
                name: t.name.clone(),
                clip: t.clip.as_deref().map(|c| Box::new(clip_to_json(c, project_dir))),
                sound: t.sound.iter().map(|(track, c)| crate::format::TakeSoundJson { track: track.to_string(), clip: clip_to_json(c, project_dir) }).collect(),
            }).collect(),
        }),
    }
}

#[allow(clippy::needless_borrows_for_generic_args)]
fn clip_kind_to_json(kind: &ClipKind, project_dir: &Path) -> ClipKindJson {
    match kind {
        ClipKind::Media { media_id } => ClipKindJson::Media {
            media_id: media_id.to_string(),
        },
        // A layer with a stack of its own, a blend or a mask (#1513) is
        // the newest thing a title can carry, and takes the newest tag; a
        // path without one keeps its tag; a title with neither keeps the
        // oldest.
        ClipKind::Title(data) if data.objects.iter().any(|o| !o.fx().is_default() || o.blend != klipp_core::BlendMode::Normal || o.mask.is_some()) => {
            ClipKindJson::TitleV2 { title_data: title_data_for_file(data, project_dir) }
        }
        ClipKind::Title(data) if data.objects.iter().any(|o| matches!(&o.content,klipp_timeline::TitleObjectContent::Shape(shape) if matches!(shape.kind,klipp_timeline::ShapeKind::Path(_)))) => {
            ClipKindJson::VectorTitle {title_data:title_data_for_file(data,project_dir)}
        }
        ClipKind::Title(data) => {
            // Serialise via the legacy single-text-flat shape for
            // commit 1: read the primary TextBox + first object's
            // transform centre. Multi-object titles will get a
            // proper `objects` array in the JSON in a follow-up
            // commit when the loader can read them.
            let tb = data.primary_text_or_default();
            // Still and bare by the arm above, so these are the empty
            // stacks an older reader expects to find here.
            let fx = data.primary_fx();
            let pos = data.position();
            ClipKindJson::Title {
                title_data: serde_json::to_value(title_data_for_file(data, project_dir)).ok(),
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: format!("{:?}", tb.font_style),
                color: tb.color,
                alignment: format!("{:?}", tb.alignment),
                position_x: pos.x,
                position_y: pos.y,
                background: serde_json::to_value(&data.background).unwrap_or_default(),
                // Legacy single-shadow JSON field — emit shadows[0]
                // for back-compat with older Klipp readers; the full
                // stack lives under `shadows`.
                shadow: serde_json::to_value(fx.shadows.first()).unwrap_or_default(),
                shadows: serde_json::to_value(&fx.shadows).unwrap_or_default(),
                shadow_stack_enabled: fx.shadow_stack_enabled,
                // Single-outline JSON field is legacy; the canonical
                // list lives under `outlines`. Emit the first outline
                // as `outline` for compat with older readers, and the
                // full list under `outlines`.
                outline: serde_json::to_value(fx.outlines.first()).unwrap_or_default(),
                outlines: serde_json::to_value(&fx.outlines).unwrap_or_default(),
                outline_stack_enabled: fx.outline_stack_enabled,
                // Legacy single-glow field — emit glows[0] for
                // back-compat. Full stack lives under `glows`.
                glow: serde_json::to_value(fx.glows.first()).unwrap_or_default(),
                glows: serde_json::to_value(&fx.glows).unwrap_or_default(),
                glow_stack_enabled: fx.glow_stack_enabled,
                gradient: serde_json::to_value(&tb.gradient).unwrap_or_default(),
                // Legacy single-inner-shadow field — emit inner_shadows[0]
                // for back-compat with older readers; the full stack
                // lives under `inner_shadows`.
                inner_shadow: serde_json::to_value(fx.inner_shadows.first()).unwrap_or_default(),
                inner_shadows: serde_json::to_value(&fx.inner_shadows).unwrap_or_default(),
                inner_shadow_stack_enabled: fx.inner_shadow_stack_enabled,
                pattern_fill: serde_json::to_value(&tb.pattern_fill).unwrap_or_default(),
                tracking: tb.tracking,
                line_spacing: tb.line_spacing,
                write_on: tb.write_on,
                extrusion: serde_json::to_value(&tb.extrusion).unwrap_or_default(),
                text_path: serde_json::to_value(&tb.text_path).unwrap_or_default(),
                char_animation: serde_json::to_value(&tb.char_animation).unwrap_or_default(),
                // Legacy path field — written when the source is a
                // File so older Klipp versions can still render the
                // texture. Procedural-preset sources have no path
                // representation, so this is None for those.
                texture_fill_path: tb.texture_fill.as_ref()
                    .and_then(|t| t.file_path())
                    .map(|p| {
                        crate::path_util::to_project_relative(project_dir, Path::new(p))
                            .to_string_lossy()
                            .to_string()
                    }),
                // New canonical field — full TextureFill struct.
                // Project-relative path is rewritten in the same
                // way before serialisation so the project stays
                // portable.
                texture_fill: tb.texture_fill.as_ref().map(|t| {
                    let mut copy = t.clone();
                    if let Some(p) = copy.file_path_mut() {
                        *p = crate::path_util::to_project_relative(project_dir, Path::new(&p))
                            .to_string_lossy()
                            .to_string();
                    }
                    serde_json::to_value(&copy).unwrap_or_default()
                }).unwrap_or_default(),
                motion_blur: serde_json::to_value(&fx.motion_blur).unwrap_or_default(),
                style_runs: serde_json::to_value(&tb.style_runs).unwrap_or_default(),
            }
        }
        ClipKind::Gap => ClipKindJson::Gap,
        ClipKind::Sequence { sequence_id } => ClipKindJson::Sequence {
            sequence_id: sequence_id.to_string(),
        },
        ClipKind::Generator(data) => ClipKindJson::Generator {
            data: serde_json::to_value(data).unwrap_or_default(),
        },
        ClipKind::AdjustmentLayer => ClipKindJson::AdjustmentLayer,
        ClipKind::Precomposition { sequence_id } => ClipKindJson::Precomposition {
            sequence_id: sequence_id.to_string(),
        },
        ClipKind::TemplatePrecomposition { sequence_id, template_instance } => ClipKindJson::TemplatePrecomposition {
            sequence_id: sequence_id.to_string(),
            template_instance: template_instance.clone(),
        },
    }
}

/// The title as it goes into the file: every texture path made relative
/// to the project directory, the way the flat fields have always stored
/// it, so a project that is moved still finds its textures. The
/// in-memory data keeps its absolute paths.
fn title_data_for_file(data: &klipp_timeline::TitleData, project_dir: &Path) -> klipp_timeline::TitleData {
    let mut copy = data.clone();
    for obj in &mut copy.objects {
        match &mut obj.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                if let Some(p) = tb.texture_fill.as_mut().and_then(|t| t.file_path_mut()) {
                    *p = to_project_relative(project_dir, Path::new(p)).to_string_lossy().to_string();
                }
            }
            klipp_timeline::TitleObjectContent::Image(img) => {
                img.path = to_project_relative(project_dir, Path::new(&img.path)).to_string_lossy().to_string();
            }
            klipp_timeline::TitleObjectContent::Shape(_) => {}
        }
    }
    copy
}

// ---- Loading ----

/// The most effects a clip read from a file remembers its All effects
/// switch turning off.
const MAX_SWITCHED_OFF: usize = 1024;

/// The most takes an audition read from a file keeps (#3209).
const MAX_TAKES: usize = 100;

/// A saved clip as the timeline holds it, its audition's takes (#3209)
/// read the same way.
pub(crate) fn load_clip(c: &ClipJson, project_dir: &Path, effects_by_clip: &HashMap<&str, Vec<klipp_effects::BuiltinEffect>>, rewritten: &mut std::collections::HashSet<String>) -> Clip {
    let clip_kind = load_clip_kind(c, project_dir);
    let mut clip = Clip {
        id: ClipId(parse_uuid(&c.id).unwrap_or_else(|_| Uuid::new_v4())),
        kind: clip_kind,
        timeline_in: RationalTime { value: c.timeline_in, rate: c.rate },
        timeline_out: RationalTime { value: c.timeline_out, rate: c.rate },
        source_in: RationalTime { value: c.source_in, rate: c.rate },
        source_out: RationalTime { value: c.source_out, rate: c.rate },
        picture: klipp_timeline::ClipPictureParts {
            grade: {
                let mut g = c.node_graph.clone().unwrap_or_default();
                migrate_graph(&mut g);
                g
            },
            builtin_effects: effects_by_clip.get(c.id.as_str()).cloned().unwrap_or_default(),
            builtin_effects_enabled: c.builtin_effects_enabled.clone(),
            ofx_effects: c.effects.iter().map(|e| e.to_effect()).collect(),
            opacity_keyframes: c.opacity_keyframes.clone().unwrap_or_default(),
            transform_pos_x_keyframes: c.transform_pos_x_keyframes.clone().unwrap_or_default(),
            transform_pos_y_keyframes: c.transform_pos_y_keyframes.clone().unwrap_or_default(),
            transform_scale_keyframes: c.transform_scale_keyframes.clone().unwrap_or_default(),
            transform_scale_y_keyframes: c.transform_scale_y_keyframes.clone().unwrap_or_default(),
            transform_rotation_keyframes: c.transform_rotation_keyframes.clone().unwrap_or_default(),
            transform_tilt_x_keyframes: c.transform_tilt_x_keyframes.clone().unwrap_or_default(),
            transform_tilt_y_keyframes: c.transform_tilt_y_keyframes.clone().unwrap_or_default(),
            effect_keyframes: c.effect_keyframes.clone().unwrap_or_default(),
            param_modifiers: c.param_modifiers.as_ref().map(|m| {
                m.iter().filter_map(|(k, v)| {
                    klipp_core::ParamId::from_key(k).map(|pid| (pid, v.clone()))
                }).collect::<std::collections::HashMap<_, _>>()
            }).unwrap_or_default(),
            grade_versions: c.grade_versions.iter().map(|gv| {
                let mut g = gv.node_graph.clone();
                migrate_graph(&mut g);
                klipp_timeline::GradeVersion {
                    name: gv.name.clone(),
                    node_graph: g.into(),
                }
            }).collect(),
            active_grade_version: c.active_grade_version,
        }
        .into(),
        // Read as untrusted: no more than a clip has effects to switch.
        effects_switched_off: c.effects_switched_off.iter().take(MAX_SWITCHED_OFF).copied().collect(),
        linked_clip_id: c.linked_clip_id.as_ref().and_then(|s| {
            parse_uuid(s).ok().map(ClipId)
        }),
        linked_title_asset_id: c.linked_title_asset_id.as_ref().and_then(|s| {
            parse_uuid(s).ok().map(klipp_core::MediaId)
        }),
        group_id: c.group_id.as_ref().and_then(|s| {
            parse_uuid(s).ok().map(GroupId)
        }),
        grade_group: c.grade_group.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::GradeGroupId)),
        magnetic_anchor: c.magnetic_anchor.clone(),
        gain: c.gain.unwrap_or(1.0),
        volume: c.volume.unwrap_or(1.0),
        pan: c.pan.unwrap_or(0.0),
        fade_in_secs: c.fade_in_secs.unwrap_or(0.0),
        fade_out_secs: c.fade_out_secs.unwrap_or(0.0),
        fade_in_curve: c.fade_in_curve.unwrap_or_default(),
        fade_out_curve: c.fade_out_curve.unwrap_or_default(),
        channel_config: c.channel_config.unwrap_or_default(),
        volume_keyframes: c.volume_keyframes.clone().unwrap_or_default(),
        pan_keyframes: c.pan_keyframes.clone().unwrap_or_default(),
        gain_keyframes: c.gain_keyframes.clone().unwrap_or_default(),
        matte_feather_keyframes: c.matte_feather_keyframes.clone().unwrap_or_default(),
        matte_choke_keyframes: c.matte_choke_keyframes.clone().unwrap_or_default(),
        frame_glide: c.frame_glide,
        clip_markers: c.clip_markers.as_ref().map(|markers| {
            markers.iter().map(|m| klipp_timeline::ClipMarker {
                time_secs: m.time_secs,
                name: m.name.clone(),
                color: match m.color.as_str() {
                    "Red" => klipp_core::MarkerColor::Red,
                    "Orange" => klipp_core::MarkerColor::Orange,
                    "Yellow" => klipp_core::MarkerColor::Yellow,
                    "Green" => klipp_core::MarkerColor::Green,
                    "Purple" => klipp_core::MarkerColor::Purple,
                    _ => klipp_core::MarkerColor::Blue,
                },
                comment: m.comment.clone(),
            }).collect()
        }).unwrap_or_default(),
        role: clip_role(c),
        role_source: c.role_source,
        matched_to: c.matched_to,
        speed: c.speed.unwrap_or(1.0),
        speed_keyframes: c.speed_keyframes.clone().unwrap_or_default(),
        keep_pitch: c.keep_pitch.unwrap_or(false),
        sound_apart: c.sound_apart.unwrap_or(false),
        composition: c.composition,
        composition_versions: c.composition_versions.clone(),
        hold: c.hold.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
        source_rotation: c.source_rotation.unwrap_or(0),
        enabled: c.enabled.unwrap_or(true),
        solo: c.solo.unwrap_or(false),
        blend_mode: c.blend_mode.unwrap_or(klipp_core::BlendMode::Normal),
        depth: c.depth.filter(|d| d.is_finite() && *d >= 0.0).unwrap_or(1.0),
        matte_source: c.matte_source.as_ref().and_then(load_matte_binding),
        matte_stencil: c.matte_stencil.as_ref().map(|s| klipp_timeline::StencilSettings {
            channel: s.channel,
            invert: s.invert,
            feather_px: s.feather_px,
            choke_px: s.choke_px,
        }),
        consumed_as_matte: c.consumed_as_matte.unwrap_or(false),
        transform_link: c.transform_link.as_ref().and_then(load_transform_link),
        audio_effects: c.audio_effects.clone().unwrap_or_default(),
        audio_effect_keyframes: c.audio_effect_keyframes.clone(),
        multicam_group: c.multicam_group.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::GroupId)),
        multicam_angle: c.multicam_angle.unwrap_or(0),
        multicam_cuts: c.multicam_cuts.clone(),
        multicam_sound: c.multicam_sound.unwrap_or_default(),
        track_points: c.track_points.clone(),
        surfaces: c.surfaces.clone(),
        surface_pin: c.surface_pin,
        name: c.name.clone(),
        colour_label: c.colour_label,
        grade_flag: c.grade_flag,
        audition: None,
        precomp: None,
        baked_own: None,
    };
    // Titles saved with the old whole-title or
    // per-character animation get the layer model; the
    // keyframes the old kinds baked onto the clip go
    // with them, since the loops now do the moving.
    if let ClipKind::Title(td) = &mut clip.kind {
        // The layer stacks first: an object from before they
        // existed takes its text box's old fields; a text box
        // with old fields beside an explicit stack loses them
        // to it, and the log says so.
        let fx = td.migrate_fx();
        if fx.dropped_legacy > 0 {
            warn!("title {}: {} text box(es) had old effect fields beside a layer stack; the stack wins", clip.id, fx.dropped_legacy);
        }
        let m = td.migrate_legacy_animation();
        if m.clear_clip_keyframes {
            rewritten.insert(clip.id.0.to_string());
            clip.edit_opacity_keyframes(|v| v.clear());
            clip.edit_transform_pos_x_keyframes(|v| v.clear());
            clip.edit_transform_pos_y_keyframes(|v| v.clear());
            clip.edit_transform_scale_keyframes(|v| v.clear());
            clip.edit_transform_rotation_keyframes(|v| v.clear());
        }
    }
    // Self-heal orphaned mask trackers from older
    // projects that were saved before the inspector's
    // delete-mask button routed through the centralized
    // cleanup. A `TrackPoint` whose `linked_mask` points
    // at a Mask node that no longer exists in any grade
    // version (or at a `mask_index` past the end of the
    // mask stack on whichever Mask node it does match)
    // would otherwise stay attached forever and there's
    // no UI to remove an orphaned tracker because the
    // Untrack button lives inside the missing mask's
    // inspector row. Drop them at load time.
    let mask_counts: std::collections::HashMap<uuid::Uuid, usize> = clip
        .grade_versions().iter()
        .flat_map(|gv| gv.grade().nodes.iter())
        .filter(|n| n.kind == klipp_nodes::NodeKind::Mask)
        .map(|n| {
            let count = n.params.masks.as_ref()
                .map(|ms| ms.masks.len())
                .unwrap_or(0);
            (n.id, count)
        })
        .collect();
    let pre = clip.track_points.len();
    clip.track_points.retain(|tp| match tp.linked_mask {
        None => true,
        Some((nid, mi)) => mask_counts.get(&nid).is_some_and(|&n| mi < n),
    });
    let dropped = pre - clip.track_points.len();
    if dropped > 0 {
        tracing::info!(
            "load: pruned {dropped} orphaned mask tracker(s) from clip {:?}",
            clip.id,
        );
    }
    clip.audition = c.audition.as_ref().and_then(|a| load_audition(a, &c.id, project_dir, effects_by_clip, rewritten));
    clip
}

/// Load ClipKind from JSON — backward-compatible with old `media_id`-only format.
fn load_clip_kind(c: &ClipJson, project_dir: &Path) -> ClipKind {
    // New format: `kind` field is present
    if let Some(kind_json) = &c.kind {
        return match kind_json {
            ClipKindJson::VectorTitle {title_data} => { let mut td=title_data.clone();resolve_title_paths(&mut td,project_dir);ClipKind::Title(td) },
            ClipKindJson::TitleV2 { title_data } => {
                let mut td = title_data.clone();
                resolve_title_paths(&mut td, project_dir);
                ClipKind::Title(td)
            }
            ClipKindJson::Media { media_id } => {
                let mid = parse_uuid(media_id)
                    .map(MediaId)
                    .unwrap_or_else(|_| MediaId::new());
                ClipKind::Media { media_id: mid }
            }
            ClipKindJson::Title {
                title_data,
                text,
                font_size,
                font_name,
                font_style,
                color,
                alignment,
                position_x,
                position_y,
                background,
                shadow,
                shadows,
                shadow_stack_enabled,
                outline,
                outlines,
                outline_stack_enabled,
                glow,
                glows,
                glow_stack_enabled,
                gradient,
                inner_shadow,
                inner_shadows,
                inner_shadow_stack_enabled,
                pattern_fill,
                tracking,
                line_spacing,
                write_on,
                extrusion,
                text_path,
                char_animation,
                texture_fill_path,
                texture_fill,
                motion_blur,
                style_runs,
            } => {
                if let Some(v) = title_data {
                    match serde_json::from_value::<klipp_timeline::TitleData>(v.clone()) {
                        Ok(mut td) => {
                            resolve_title_paths(&mut td, project_dir);
                            return ClipKind::Title(td);
                        }
                        Err(e) => warn!("title_data could not be read, using the flat title fields: {e}"),
                    }
                }
                let align = match alignment.as_str() {
                    "Left" => TextAlignment::Left,
                    "Right" => TextAlignment::Right,
                    "Justify" => TextAlignment::Justify,
                    _ => TextAlignment::Center,
                };
                let fs = match font_style.as_str() {
                    "Bold" => FontStyle::Bold,
                    "Italic" => FontStyle::Italic,
                    "BoldItalic" => FontStyle::BoldItalic,
                    _ => FontStyle::Regular,
                };
                let bg: TitleBackground = serde_json::from_value(background.clone())
                    .unwrap_or(TitleBackground::None);
                // Drop-shadow stack migration: prefer the new
                // `shadows` array; fall back to wrapping the legacy
                // single `shadow` field as a one-element vec when
                // `shadows` is absent or empty.
                let shadow_vec: Vec<TitleShadow> = {
                    let from_new: Vec<TitleShadow> =
                        serde_json::from_value(shadows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<TitleShadow> =
                            serde_json::from_value(shadow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let ol: Option<klipp_timeline::TextOutline> = serde_json::from_value(outline.clone()).ok().flatten();
                let ols: Vec<klipp_timeline::TextOutline> = serde_json::from_value(outlines.clone()).unwrap_or_default();
                // Glow stack migration: prefer `glows` array; fall
                // back to wrapping legacy `glow` as a one-element vec.
                let glow_vec: Vec<klipp_timeline::TextGlow> = {
                    let from_new: Vec<klipp_timeline::TextGlow> =
                        serde_json::from_value(glows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<klipp_timeline::TextGlow> =
                            serde_json::from_value(glow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let gr: Option<klipp_timeline::TextGradient> = serde_json::from_value(gradient.clone()).ok().flatten();
                // Inner shadow stack migration: prefer `inner_shadows`
                // array; fall back to wrapping legacy `inner_shadow`.
                let inner_shadow_vec: Vec<klipp_timeline::InnerShadow> = {
                    let from_new: Vec<klipp_timeline::InnerShadow> =
                        serde_json::from_value(inner_shadows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<klipp_timeline::InnerShadow> =
                            serde_json::from_value(inner_shadow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let pf: Option<klipp_timeline::TextPattern> = serde_json::from_value(pattern_fill.clone()).ok().flatten();
                let ext: Option<klipp_timeline::TextExtrusion> = serde_json::from_value(extrusion.clone()).ok().flatten();
                let tp: Option<klipp_timeline::TextPath> = serde_json::from_value(text_path.clone()).ok().flatten();
                let ca: Option<klipp_timeline::CharAnimation> = serde_json::from_value(char_animation.clone()).ok().flatten();
                let mb: Option<klipp_timeline::MotionBlur> = serde_json::from_value(motion_blur.clone()).ok().flatten();
                let sr: Vec<klipp_timeline::StyleRun> = serde_json::from_value(style_runs.clone()).unwrap_or_default();
                // Build a TextBox from the legacy flat JSON fields,
                // then wrap it in a TitleData with one TextBox object.
                // The legacy single-outline `outline` field is folded
                // into the `outlines` vec (rendered widest-first).
                let mut outlines_vec = ols;
                if let Some(o) = ol {
                    if !outlines_vec.iter().any(|existing| existing == &o) {
                        outlines_vec.insert(0, o);
                    }
                }
                // The flat fields are the old text-box shape; they go in
                // as that shape and the object takes them when the text
                // box is wrapped (`single_text` → `add_object`).
                let tb = klipp_timeline::TextBox {
                    text: text.clone(),
                    font_size: *font_size,
                    font_name: font_name.clone(),
                    font_style: fs,
                    color: *color,
                    alignment: align,
                    legacy_fx: klipp_timeline::LegacyTextFx {
                        shadows: shadow_vec,
                        shadow_stack_enabled: *shadow_stack_enabled,
                        outlines: outlines_vec,
                        outline_stack_enabled: *outline_stack_enabled,
                        glows: glow_vec,
                        glow_stack_enabled: *glow_stack_enabled,
                        inner_shadows: inner_shadow_vec,
                        inner_shadow_stack_enabled: *inner_shadow_stack_enabled,
                        motion: Default::default(),
                        loops: Vec::new(),
                        motion_blur: mb,
                    },
                    gradient: gr,
                    pattern_fill: pf,
                    pattern_fill_enabled: true,
                    tracking: *tracking,
                    line_spacing: *line_spacing,
                    font_size_keyframes: Vec::new(),
                    tracking_keyframes: Vec::new(),
                    line_spacing_keyframes: Vec::new(),
                    color_keyframes: Default::default(),
                    write_on: *write_on,
                    extrusion: ext,
                    three_d: None,
                    material: None,
                    text_path: tp,
                    char_animation: ca,
                    // Prefer the new TextureFill struct when present
                    // (post-migration saves). Fall back to the legacy
                    // bare path for old projects, synthesizing a
                    // TextureFill with default transform/blend params.
                    // The path inside the new struct is also resolved
                    // through resolve_media_path so a project that
                    // moved still finds its texture file.
                    texture_fill: {
                        let from_new: Option<klipp_timeline::TextureFill> =
                            serde_json::from_value(texture_fill.clone()).ok().flatten();
                        if let Some(mut tf) = from_new {
                            if let Some(p) = tf.file_path_mut() {
                                *p = crate::path_util::resolve_media_path(project_dir, Path::new(&p))
                                    .to_string_lossy()
                                    .to_string();
                            }
                            Some(tf)
                        } else {
                            texture_fill_path.as_ref().map(|p| {
                                let resolved = crate::path_util::resolve_media_path(project_dir, Path::new(p))
                                    .to_string_lossy()
                                    .to_string();
                                klipp_timeline::TextureFill::from_legacy_path(resolved, true)
                            })
                        }
                    },
                    style_runs: sr,
                    ass_style_name: None,
                    underline: false,
                    strikethrough: false,
                    case_transform: klipp_timeline::TextCase::None,
                    baseline_shift: 0.0,
                    drop_cap_scale: 0.0,
                    outline_write_on: 1.0,
                    chromatic_aberration_px: 0.0,
                    glitch_intensity: 0.0,
                    columns: 1,
                    vertical_layout: false,
                    vertical_columns_ltr: false,
                    text_direction: klipp_timeline::TextDirection::Ltr,
                    last_line: Default::default(),
                    vertical_align: Default::default(),
                    fit: Default::default(),
                    paragraph: Default::default(),
                    features: Vec::new(),
                    font_weight: None,
                    font_axes: Vec::new(),
                    language: None,
                };
                let mut td = klipp_timeline::TitleData::single_text(tb);
                if let Some(obj) = td.objects.first_mut() {
                    obj.transform.center = TitlePosition {
                        x: *position_x,
                        y: *position_y,
                    };
                }
                td.background = bg;
                ClipKind::Title(td)
            }
            ClipKindJson::Gap => ClipKind::Gap,
            ClipKindJson::Sequence { sequence_id } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::Sequence { sequence_id: sid }
            }
            ClipKindJson::Generator { data } => {
                let gen_data = serde_json::from_value(data.clone())
                    .unwrap_or_default();
                ClipKind::Generator(gen_data)
            }
            ClipKindJson::AdjustmentLayer => ClipKind::AdjustmentLayer,
            ClipKindJson::Precomposition { sequence_id } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::Precomposition { sequence_id: sid }
            }
            ClipKindJson::TemplatePrecomposition { sequence_id, template_instance } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::TemplatePrecomposition { sequence_id: sid, template_instance: template_instance.clone() }
            }
        };
    }

    // Old format: `media_id` field only (Phase 6/7 backward compat)
    if let Some(media_id_str) = &c.media_id {
        let mid = parse_uuid(media_id_str)
            .map(MediaId)
            .unwrap_or_else(|_| MediaId::new());
        return ClipKind::Media { media_id: mid };
    }

    // Fallback — should not happen
    warn!("Clip {} has neither 'kind' nor 'media_id' — using default", c.id);
    ClipKind::default()
}

fn resolve_title_paths(td: &mut klipp_timeline::TitleData, project_dir: &Path) {
    // Texture paths are stored project-relative, like the flat fields.
    for obj in &mut td.objects {
        match &mut obj.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                if let Some(p) = tb.texture_fill.as_mut().and_then(|t| t.file_path_mut()) {
                    *p = crate::path_util::resolve_media_path(project_dir, Path::new(p))
                        .to_string_lossy().to_string();
                }
            }
            klipp_timeline::TitleObjectContent::Image(img) => {
                img.path = crate::path_util::resolve_media_path(project_dir, Path::new(&img.path))
                    .to_string_lossy().to_string();
            }
            klipp_timeline::TitleObjectContent::Shape(_) => {}
        }
    }
}

/// A saved audition, read as untrusted (#3209): at most [`MAX_TAKES`]
/// takes; a take's own clips never hold an audition of their own; and
/// exactly one take, the active one, has no clip, being the slot. One
/// that isn't so is dropped with a warning, the slot kept as a plain clip.
fn load_audition(a: &crate::format::AuditionJson, owner: &str, project_dir: &Path, effects: &HashMap<&str, Vec<klipp_effects::BuiltinEffect>>, rewritten: &mut std::collections::HashSet<String>) -> Option<Box<klipp_timeline::audition::Audition>> {
    use klipp_timeline::audition::{Audition, Take};
    if a.takes.len() > MAX_TAKES {
        warn!("clip {owner}: {} takes, more than {MAX_TAKES}; the rest are left out", a.takes.len());
    }
    let id = |s: &str| parse_uuid(s).ok().map(klipp_core::TakeId);
    let Some(active) = id(&a.active) else {
        warn!("clip {owner}: its audition's active take has no id; the takes are left out");
        return None;
    };
    let mut plain = |c: &ClipJson| {
        let mut clip = load_clip(c, project_dir, effects, rewritten);
        clip.audition = None;
        clip
    };
    let mut takes = Vec::new();
    for t in a.takes.iter().take(MAX_TAKES) {
        let Some(tid) = id(&t.id) else {
            warn!("clip {owner}: a take has no id; it is left out");
            continue;
        };
        let sound = t.sound.iter().filter_map(|s| Some((klipp_core::TrackId(parse_uuid(&s.track).ok()?), plain(&s.clip)))).collect();
        takes.push(Take { id: tid, name: t.name.clone(), clip: t.clip.as_deref().map(|c| Box::new(plain(c))), sound });
    }
    let slots = takes.iter().filter(|t| t.clip.is_none()).count();
    if slots != 1 || takes.iter().find(|t| t.clip.is_none()).is_none_or(|t| t.id != active) {
        warn!("clip {owner}: its audition doesn't name the take in the slot; the takes are left out");
        return None;
    }
    Some(Box::new(Audition { takes, active }))
}

/// A saved clip and the clips its audition's takes hold (#3209): one
/// level, as [`load_audition`] reads them.
pub(crate) fn with_takes(c: &ClipJson) -> Vec<&ClipJson> {
    let takes = c.audition.iter().flat_map(|a| a.takes.iter().take(MAX_TAKES));
    std::iter::once(c).chain(takes.flat_map(|t| t.clip.as_deref().into_iter().chain(t.sound.iter().map(|s| &s.clip)))).collect()
}

/// Decode a wire-format `MatteBindingJson` into a runtime `MatteBinding`.
/// Returns `None` if the source-clip UUID is malformed — the consumer clip
/// then loads as if it had no matte (the alternative would be to drop the
/// whole project load, which is far worse than silently losing one binding
/// that the user can re-attach in the inspector). The orphan-clip-id case
/// (well-formed UUID that doesn't match any clip) is allowed through here
/// and cleaned up by the post-load pass that runs across the whole sequence.
fn load_matte_binding(j: &crate::format::MatteBindingJson) -> Option<klipp_timeline::MatteBinding> {
    let id = parse_uuid(&j.source_clip_id).ok()?;
    Some(klipp_timeline::MatteBinding {
        source_clip_id: ClipId(id),
        channel: j.channel,
        invert: j.invert,
        feather_px: j.feather_px,
        choke_px: j.choke_px,
    })
}

/// Resolve a `TransformLinkJson` back to its in-memory form. Same
/// dangling-id policy as `load_matte_binding`: a well-formed-but-
/// missing target survives this function and gets cleaned up by
/// the post-load orphan sweep. The malformed-uuid path is now
/// handled by `TransformLinkTarget`'s own deserialization (a
/// non-uuid string fails the `Clip(ClipId)` arm of the untagged
/// enum and the file load surfaces the error there).
fn load_transform_link(j: &crate::format::TransformLinkJson) -> Option<klipp_timeline::TransformLink> {
    Some(klipp_timeline::TransformLink {
        target: j.target,
        mode: j.mode,
        inherit_scale: j.inherit_scale,
        inherit_position: j.inherit_position,
        inherit_rotation: j.inherit_rotation,
        inherit_anchor: j.inherit_anchor,
    })
}

/// `c`'s role (#3211): as written now; else as a file before roles were
/// data wrote it, where nothing written was Dialogue, set or proposed,
/// and nothing said for a clip whose role was never set.
fn clip_role(c: &ClipJson) -> Option<klipp_core::roles::RoleRef> {
    match (&c.role_id, c.role) {
        (Some(now), _) => now.clone(),
        (None, Some(old)) => klipp_core::roles::RoleRef::from_legacy(old),
        (None, None) if !c.role_source.is_unset() => Some(klipp_core::roles::RoleRef::of("dialogue")),
        (None, None) => None,
    }
}
