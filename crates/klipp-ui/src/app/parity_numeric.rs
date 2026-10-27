//! Numeric effect controls of the inspector driven through real pointer
//! and key events. The oracle is the document command with just the
//! addressed field set to what the edit must store (the typed value, the
//! drag's rule, the reset value); comparison includes the complete
//! picture and exact undo/redo.
//! Keyframe evidence is a separate obligation. Pixels are a case of their
//! own (`pixel_case`): the typed edit drawn by the real renderer on the
//! CPU and the native GPU stack (`parity_pixels`). A type whose
//! production route is the CPU by design is drawn on the CPU alone,
//! declared in [`CPU_BY_DESIGN`] with the M4 step that moves it. One that
//! should be native and leaves the GPU route is BLOCKED with its reason,
//! open in the report and pinned in [`BLOCKED`], so neither set can change
//! unseen.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{
    catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface,
};
use crate::inspector::generated::{effect_value, row};
use klipp_core::AppCommand;
use klipp_effects::registry::presentation::{Cond, Presentation, Widget};
use klipp_effects::registry::{effect_types, EffectType, ParamDesc, ParamKind};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip;
use serde_json::Value;

const VARIANT: &str = "editable";

pub(super) fn numeric(entry: &CatalogEntry) -> bool {
    !entry.grade
        && matches!(entry.value, CatalogValue::Parameter { presentation, .. }
        if matches!(presentation.widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::Vec2))
}

fn contexts(ty: &EffectType) -> Vec<(Surface, &'static str)> {
    // A lone opacity node is the clip anchor, hidden from the stack.
    // Other placed kinds still draw their own rows in Effects/Placement.
    if ty.tag == "opacity" {
        vec![
            (Surface::VideoTab, "anchor"),
            (Surface::EffectsStack, "non-anchor"),
        ]
    } else {
        vec![(Surface::EffectsStack, VARIANT)]
    }
}

/// Rows whose control is shown but doesn't move the picture, or whose
/// picture the CPU and the GPU stack draw differently: known defects, each
/// excluded from Pixels with its issue until it is fixed (type, parameter,
/// surface, variant, reason). Not a route, and not a
/// design: the deletion report reads them as open gaps.
const KNOWN_DEFECTS: &[(&str, &str, Surface, &str, &str)] = &[];

pub(super) fn known_defect(entry: &CatalogEntry, surface: Surface, variant: &str) -> Option<&'static str> {
    KNOWN_DEFECTS.iter().find(|(ty, param, s, v, _)| *ty == entry.type_id && *param == entry.parameter && *s == surface && *v == variant).map(|d| d.4)
}

pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog().into_iter().filter(numeric) {
        let ty = effect_types()
            .find(|ty| ty.desc.id == entry.type_id)
            .unwrap();
        for (surface, variant) in contexts(ty) {
            manifest.require_fixture(&entry, surface, variant);
            if let Some(reason) = known_defect(&entry, surface, variant) {
                manifest.exclude_key(CoverageKey::new(entry.type_id, &entry.parameter, surface, variant, Interaction::Pixels), reason);
            }
        }
    }
}

pub(super) fn with_field(
    effect: &BuiltinEffect,
    desc: &ParamDesc,
    value: Value,
) -> Result<BuiltinEffect, String> {
    let mut entry = serde_json::to_value(effect).map_err(|e| e.to_string())?;
    match desc.index {
        Some(index) => entry[desc.field][index] = value,
        None => entry[desc.field] = value,
    }
    serde_json::from_value(entry).map_err(|e| format!("{}: {e}", desc.key))
}

pub(super) fn stored(effect: &BuiltinEffect, desc: &ParamDesc) -> Value {
    effect_value(&serde_json::to_value(effect).unwrap(), desc)
}

pub(super) fn number(desc: &ParamDesc, value: f32) -> Value {
    if desc.kind == ParamKind::Whole {
        Value::from(value.round().max(0.0) as u32)
    } else {
        Value::from(value)
    }
}

/// `ty`'s sample, set to act where the sample alone can't (#1746): a 3D
/// LUT reads a file, a two-point one whose corners aren't a straight map
/// (so its interpolations differ too); a colour-space transform converts;
/// a picture transform is shrunk and turned; a media source reads the
/// harness's item.
pub(super) fn acting_sample(ty: &EffectType) -> BuiltinEffect {
    match ty.tag {
        "lut_3d" => {
            static FILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let path = FILE.get_or_init(|| {
                let path = std::env::temp_dir().join(format!("klipp-parity-lut-{}.cube", std::process::id()));
                let mut text = String::from("LUT_3D_SIZE 2\n");
                for b in 0..2u8 {
                    for g in 0..2u8 {
                        for r in 0..2u8 {
                            text.push_str(&format!("{} {} {}\n", r & g, g & b, b | r));
                        }
                    }
                }
                std::fs::write(&path, text).expect("the parity LUT written");
                path.to_string_lossy().into_owned()
            });
            BuiltinEffect::Lut3D { path: path.clone(), opacity: 1.0, tetrahedral: true, prepared: None }
        }
        "colour_space_transform" => BuiltinEffect::ColourSpaceTransform { input_gamut: 2, input_curve: 1, output_gamut: 1, output_curve: 0, prepared: None },
        // Shrunk and turned, so its anchor moves the picture too.
        "picture_transform" => BuiltinEffect::PictureTransform { position_x: 0.0, position_y: 0.0, scale_x: 0.8, scale_y: 0.8, rotation: 10.0, anchor_x: 0.5, anchor_y: 0.5 },
        // A gradient, part clear, so its placing and its colours' alpha
        // show (a solid is a flat frame).
        "generator" => BuiltinEffect::Generator { shape: 1, color_a: [240, 200, 80, 255], color_b: [30, 60, 160, 160], angle: 20.0, center_x: 0.45, center_y: 0.55, radius: 0.4 },
        // Over its gradient at half, so the mode and the opacity both show.
        "merge" => BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Screen, opacity: 0.5 },
        // The item the harness's sources hold, a little on.
        "media_source" => BuiltinEffect::MediaSource { media: super::parity_pixels::MEDIA_ITEM.into(), offset: 0.25 },
        // On the screen its fixture's clip carries (`fixture_on`).
        "surface_pin" => BuiltinEffect::SurfacePin { surface: PARITY_SCREEN.into(), inset: 0.0, on_source: [[0.0; 2]; 4], source: [0; 2] },
        _ => (ty.sample)(),
    }
}

pub(super) fn editable(ty: &EffectType, pres: &Presentation) -> Result<BuiltinEffect, String> {
    let mut effect = acting_sample(ty);
    for Cond::ParamIs { key, value } in [pres.visible_if, pres.enabled_if].into_iter().flatten() {
        let desc = ty
            .desc
            .params
            .iter()
            .find(|p| p.key == key)
            .ok_or_else(|| format!("unknown condition {key}"))?;
        let value = match (desc.kind, stored(&effect, desc)) {
            (ParamKind::Choice(names), Value::String(_)) => Value::from(
                *names
                    .get(value as usize)
                    .ok_or("condition index outside choices")?,
            ),
            (_, Value::Bool(_)) => Value::Bool(value != 0.0),
            (_, Value::Number(n)) if n.is_f64() => Value::from(value),
            _ => Value::from(value as u32),
        };
        effect = with_field(&effect, desc, value)?;
    }
    Ok(effect)
}

/// The surface a surface pin's fixture clip carries, by name.
const PARITY_SCREEN: &str = "Parity screen";

pub(super) fn fixture_on(effect: &BuiltinEffect, surface: Surface) -> klipp_timeline::Clip {
    let mut clip = fixture(effect);
    if matches!(effect, BuiltinEffect::SurfacePin { .. }) {
        // A screen leaning in the shot, still, on a picture of any size
        // (no size: the frame's), so the pin and its inset show.
        let screen = [[0.2, 0.15], [0.8, 0.2], [0.75, 0.85], [0.25, 0.8]];
        let key = |secs| klipp_timeline::SurfaceKey::at_corners(secs, &screen, &screen).expect("a quad");
        clip.surfaces.push(klipp_timeline::Surface {
            id: uuid::Uuid::new_v4(),
            name: PARITY_SCREEN.into(),
            corners: screen,
            reference_secs: 0.0,
            keys: vec![key(0.0), key(3600.0)],
            source_size: [0, 0],
            media_fps: 0.0,
            track_area: None,
            exclude: Vec::new(),
        });
    }
    if matches!(effect, BuiltinEffect::Opacity { .. }) && surface == Surface::EffectsStack {
        // The final enabled Opacity becomes the clip anchor. The first
        // remains a card and must be exercised independently.
        clip.push_effect(BuiltinEffect::Opacity { value: 1.0 });
    }
    clip
}

fn run_for(surface: Surface, clip: klipp_timeline::Clip) -> Run {
    let mut run = Run::new(clip);
    run.property = surface == Surface::VideoTab;
    run
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RowLabel {
    pub(super) text: &'static str,
    pub(super) group: Option<&'static str>,
}

pub(super) fn ready(run: &mut Run, label: RowLabel) -> Result<(egui::Pos2, egui::Pos2), String> {
    run.frame(Vec::new(), egui::Modifiers::NONE);
    let Some(group) = label.group else {
        return run
            .row(label.text)
            .ok_or_else(|| format!("numeric row {label:?} is not drawn"));
    };
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let group_y = drawn
        .iter()
        .find(|(s, _)| s == group)
        .map(|(_, p)| p.y)
        .ok_or_else(|| format!("group {group:?} is not drawn"))?;
    let at = drawn
        .iter()
        .filter(|(s, p)| s == label.text && p.y > group_y)
        .min_by(|a, b| a.1.y.total_cmp(&b.1.y))
        .map(|(_, p)| *p)
        .ok_or_else(|| format!("numeric row {label:?} is not drawn"))?;
    drawn
        .iter()
        .filter(|(s, p)| {
            (p.y - at.y).abs() < 6.0
                && p.x > at.x
                && s.trim_start_matches('-')
                    .starts_with(|c: char| c.is_ascii_digit())
        })
        .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map(|(_, p)| (at, *p))
        .ok_or_else(|| format!("numeric value {label:?} is not drawn"))
}

pub(super) fn type_value(run: &mut Run, label: RowLabel, text: &str) -> Result<(), String> {
    let (_, at) = ready(run, label)?;
    for _ in 0..2 {
        run.button(at, egui::PointerButton::Primary, true);
        run.button(at, egui::PointerButton::Primary, false);
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    run.key(egui::Key::A, egui::Modifiers::COMMAND);
    run.frame(vec![egui::Event::Text(text.into())], egui::Modifiers::NONE);
    run.key(egui::Key::Enter, egui::Modifiers::NONE);
    Ok(())
}

/// How far a row is dragged, in one move: 64 pixels, or for a whole-number
/// row as far as moves it a unit and a half (within the harness's screen),
/// since a shorter drag rounds back to where it was.
fn drag_px(desc: &ParamDesc, pres: &Presentation) -> f32 {
    match desc.kind {
        ParamKind::Whole => (1.5 / row::drag_speed(pres.step)).ceil().clamp(64.0, 400.0) as f32,
        _ => 64.0,
    }
}

/// A drag of the row by `px` in one move, held: a scrubby row's by its
/// label, its own drag, whose one move is one step of its rule
/// (`value + delta · speed`); a slider row's by its number, as its label
/// only resets. Returns where the pointer is.
fn start_drag(run: &mut Run, label: RowLabel, px: f32, slider: bool) -> Result<egui::Pos2, String> {
    let (on_label, on_number) = ready(run, label)?;
    let at = if slider { on_number } else { on_label };
    run.button(at, egui::PointerButton::Primary, true);
    run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(px, 0.0))], egui::Modifiers::NONE);
    Ok(at + egui::vec2(px, 0.0))
}

fn commands_applied(run: &Run) -> Result<(), String> {
    if run.unapplied.is_empty() {
        Ok(())
    } else {
        Err(format!("unapplied: {:?}", run.unapplied))
    }
}

fn edit_case(
    surface: Surface,
    effect: &BuiltinEffect,
    desc: &ParamDesc,
    want: Option<Value>,
    drive: impl Fn(&mut Run) -> Result<(), String>,
) -> Result<(), String> {
    let clip = fixture_on(effect, surface);
    let before = clip.picture_v2().unwrap();
    // The view driven, and the oracle: the document command alone,
    // applied to the same clip, never a view.
    let (mut new, mut oracle) = (
        run_for(surface, clip.clone()),
        run_for(surface, clip),
    );
    drive(&mut new)?;
    commands_applied(&new)?;
    let after = new.clip().picture_v2().unwrap();
    if after == before {
        return Err("the control changed nothing".into());
    }
    let actual = test_clip::effects(new.clip())
        .first()
        .map(|effect| stored(effect, desc))
        .ok_or("effect disappeared")?;
    // A slider row places its value where the pointer is, by its own
    // layout: what it must store there is held to the drag's direction (to
    // the right, a rise) and, below, to the document command's picture.
    if want.is_none() {
        let start = stored(effect, desc).as_f64();
        if !actual.as_f64().zip(start).is_some_and(|(to, from)| to > from) {
            return Err(format!("a drag to the right stored {actual}, from {}", stored(effect, desc)));
        }
    }
    let expected = want.unwrap_or_else(|| actual.clone());
    oracle.apply(AppCommand::UpdateBuiltinEffect {
        clip_id: oracle.clip,
        index: 0,
        effect: with_field(effect, desc, expected.clone())?,
    });
    commands_applied(&oracle)?;
    if after != oracle.clip().picture_v2().unwrap() {
        return Err(format!(
            "not the document command's picture: value {actual}, expected {expected}"
        ));
    }
    if new.side != oracle.side {
        return Err(format!(
            "side commands differ: {:?} / {:?}",
            new.side, oracle.side
        ));
    }
    if new.state.any_drag_in_progress() || new.state.active_history().undo_count() != 1 {
        return Err(format!(
            "edit has {} undo steps; drag open {}",
            new.state.active_history().undo_count(),
            new.state.any_drag_in_progress()
        ));
    }
    if !new.step(false) || new.clip().picture_v2().unwrap() != before {
        return Err("undo did not restore the exact picture".into());
    }
    if !new.step(true) || new.clip().picture_v2().unwrap() != after {
        return Err("redo did not restore the exact edit".into());
    }
    Ok(())
}

/// The effect types whose Pixels cases are BLOCKED, each with why: their
/// frame can't stay on the native GPU route, so each needs its own route
/// handling before it earns pixel evidence. The family test holds the
/// blocked set to this list exactly.
const BLOCKED: &[(&str, &str)] = &[];

/// The effect types whose production route is the CPU by design, each
/// with the M4 step that moves it: their Pixels are drawn on the CPU route
/// alone, the GPU half declared not applicable (never BLOCKED).
pub(super) const CPU_BY_DESIGN: &[(&str, &str)] = &[
    ("klipp.fx.temporal_denoise", "M4 step 6 (temporal denoise as a source-boundary node): the GPU stack declines a frame with temporal noise reduction"),
    ("klipp.fx.video_stabilization", "M4 step 9 (analysis effects as nodes) deletes the stack's temporal gate (klipp-compose render.rs), which draws a stabilised layer on the CPU"),
    ("klipp.fx.rolling_shutter_correction", "M4 step 9 (analysis effects as nodes) deletes the stack's temporal gate (klipp-compose render.rs), which draws a corrected layer on the CPU"),
    ("klipp.fx.frei0r", "M4 step 5 (frei0r as a node, with a local CPU segment): a frei0r plug-in vetoes the chain's GPU route, so its layer is drawn on the CPU"),
];

/// What a row's pixel case starts from beyond the effect's sample, by
/// effect tag, so its edit can show in a frame: values of other fields
/// the edited one acts through, as JSON (a crop for its feather, a tilt
/// for the perspective, a visible shadow for its offset), and whether the
/// clip is placed shrunk first, for an effect drawn in the margin a
/// smaller picture leaves (a glow, a shadow, fill bars).
const PIXEL_CONTEXT: &[(&str, &[(&str, &str)], bool)] = &[
    ("crop", &[("left", "0.1"), ("right", "0.1"), ("top", "0.1"), ("bottom", "0.1")], false),
    ("rounded_corners", &[("radius", "0.2")], false),
    ("transform", &[("rotation", "20.0"), ("scale_x", "0.8"), ("scale_y", "0.8"), ("rotation_x", "20.0")], false),
    ("outer_glow", &[("radius", "0.05"), ("intensity", "0.6")], true),
    // White, so the shadow shows over the black the frame is flattened on.
    ("drop_shadow", &[("offset_x", "0.04"), ("offset_y", "0.04"), ("blur", "0.01"), ("opacity", "0.8"), ("color", "[255, 255, 255]")], true),
    ("fill_bars", &[], true),
];

/// The effect types drawn at the delivery size: their strength is scaled
/// to the frame, so the oracle's small frame rounds it away (a gate weave
/// of a few pixels at 1920 wide is under one at 320).
pub(super) const AT_FULL_SIZE: &[&str] = &["film_grain", "old_film"];

/// The routes a type's Pixels are drawn on, and the declared reason when
/// it is the CPU alone ([`CPU_BY_DESIGN`]).
pub(super) fn routes_for(type_id: &str) -> (super::parity_pixels::Routes, Option<&'static str>) {
    match CPU_BY_DESIGN.iter().find(|(ty, _)| *ty == type_id) {
        Some((_, why)) => (super::parity_pixels::Routes::CpuByDesign, Some(why)),
        None => (super::parity_pixels::Routes::Both, None),
    }
}

/// The frame size a type's Pixels are drawn at ([`AT_FULL_SIZE`]).
pub(super) fn size_for(tag: &str) -> (u32, u32) {
    if AT_FULL_SIZE.contains(&tag) { super::parity_pixels::FULL } else { super::parity_pixels::SMALL }
}

/// The clip a row's pixel case edits, and the place of the edited effect
/// in its list: `effect` with the [`PIXEL_CONTEXT`] for its tag, after a
/// shrinking Transform where the context asks for one.
pub(super) fn pixel_fixture(ty: &EffectType, effect: &BuiltinEffect, surface: Surface) -> Result<(klipp_timeline::Clip, usize), String> {
    let context = PIXEL_CONTEXT.iter().find(|(tag, _, _)| *tag == ty.tag);
    let mut entry = serde_json::to_value(effect).map_err(|e| e.to_string())?;
    for (field, value) in context.map_or(&[][..], |(_, fields, _)| *fields) {
        entry[*field] = serde_json::from_str(value).map_err(|e| format!("{}.{field}: {e}", ty.tag))?;
    }
    let effect: BuiltinEffect = serde_json::from_value(entry).map_err(|e| format!("{}: {e}", ty.tag))?;
    // An analysis effect draws by its media's analysis, which the
    // oracle's renderers hold for every media clip
    // (`parity_pixels::analyses`), derived with the effect's settings as
    // it reads them (M4 step 9).
    let mut clip = fixture_on(&effect, surface);
    // A merge lays a picture over: a gradient beside the chain wired into
    // its foreground, as the Picture view's Sources menu makes one (#1746).
    if matches!(effect, BuiltinEffect::Merge { .. }) {
        let graph = clip.picture_v2()?.picture.graph.clone();
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).ok_or("the merge")?.id;
        let level = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip.id.0 }, []);
        let ops = crate::picture_mattes::source_ops(&graph, &level, &crate::picture_mattes::SOURCES[1], klipp_graph_format::NodeId::new_v4(), (0.0, 0.0), Some(merge)).ok_or("the source's ops")?;
        clip.apply_picture_ops(&ops)?;
    }
    if context.is_some_and(|(_, _, shrunk)| *shrunk) {
        let shrink = BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 0.6, scale_y: 0.6, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        clip.insert_effect(0, shrink).ok_or("the shrinking Transform")?;
        return Ok((clip, 1));
    }
    Ok((clip, 0))
}

/// A retiming fixture's positive control: the clip's own speed changed
/// must pick a different source frame at the sample time, so a ramp row
/// that moves nothing is the row's, not the fixture's.
fn retime_control(clip: &klipp_timeline::Clip, size: (u32, u32)) -> Result<(), String> {
    let mut faster = clip.clone();
    faster.speed = 1.5;
    if !super::parity_pixels::cpu_frames_differ(clip, &faster, size)? {
        return Err("positive control: the clip's speed doesn't pick another frame, so the fixture can't show a retime".into());
    }
    Ok(())
}

/// The typed edit drawn by the renderer: on the [`pixel_fixture`], the
/// generated view's row typed to the first of a few values near the
/// middle of its range whose edit (by the document command) changes the
/// CPU frame (a modest move, which keeps a moved picture in the frame),
/// drawn against the unedited clip: it changes the frame, and the CPU
/// draws it as the GPU does ([`super::parity_pixels::clip_edit_drawn_on`]).
/// A row no such value moves is an error naming it.
#[allow(clippy::too_many_arguments)]
fn pixel_case(routes: super::parity_pixels::Routes, ty: &EffectType, surface: Surface, effect: &BuiltinEffect, desc: &ParamDesc, label: RowLabel, value_at: impl Fn(f32) -> (String, Value)) -> Result<(), String> {
    use super::parity_pixels::cpu_frames_differ;
    let size = size_for(ty.tag);
    let (clip, index) = pixel_fixture(ty, effect, surface)?;
    if ty.tag == "speed_ramp" {
        retime_control(&clip, size)?;
    }
    let current = stored(&test_clip::effects(&clip)[index], desc);
    let edit = |target: &Value| -> Result<klipp_timeline::Clip, String> {
        let mut c = clip.clone();
        let id = test_clip::effect_id(&c, index);
        c.replace_effect(id, with_field(&test_clip::effects(&clip)[index], desc, target.clone())?);
        // A Speed Ramp's speed retimes the clip, as the effect's update
        // does (`inspector_edit_host`).
        if let BuiltinEffect::SpeedRamp { speed, .. } = test_clip::effects(&c)[index] {
            c.speed = f64::from(speed).clamp(0.1, 16.0);
            let length = klipp_core::RationalTime::from_secs_f64(c.source_duration().to_secs_f64() / c.speed, c.timeline_in.rate);
            c.timeline_out = c.timeline_in + length;
        }
        Ok(c)
    };
    let mut chosen = None;
    for (typed, target) in [0.55, 0.45, 0.65, 0.35, 0.8, 0.2, 0.95, 0.05].into_iter().map(&value_at) {
        // A value whose frame can't be drawn (a move that takes the
        // picture out of the frame) is passed over for the next.
        if target != current && matches!(cpu_frames_differ(&clip, &edit(&target)?, size), Ok(true)) {
            chosen = Some((typed, target));
            break;
        }
    }
    let (typed, target) = chosen.ok_or("the edit doesn't change the CPU frame at any value tried")?;
    let start = clip.clone();
    let mut new = run_for(surface, clip);
    type_value(&mut new, label, &typed)?;
    commands_applied(&new)?;
    let _ = target;
    // The analysis follows the edit where it is read: nothing to redo.
    super::parity_pixels::clip_edit_drawn_on(routes, size, &start, new.clip())
}

fn run_entry(report: &mut Report, entry: &CatalogEntry, surface: Surface, variant: &str) {
    let CatalogValue::Parameter {
        desc,
        presentation: pres,
    } = &entry.value
    else {
        unreachable!()
    };
    let ty = effect_types()
        .find(|ty| ty.desc.id == entry.type_id)
        .unwrap();
    let key = |interaction| {
        CoverageKey::new(
            entry.type_id,
            &entry.parameter,
            surface,
            variant,
            interaction,
        )
    };
    let effect = match editable(ty, pres) {
        Ok(effect) => effect,
        Err(error) => {
            report.case(entry.type_id, [], || Err(error));
            return;
        }
    };
    let label = RowLabel {
        text: pres.label.unwrap_or(desc.label),
        group: pres.group,
    };
    let encoding = klipp_grade_model::Encoding::from_u8(
        klipp_core::grade_encoding()
            .effective(klipp_engine::video_decoder::working_colorspace())
            .as_u8(),
    );
    let (lo, hi) = row::shown_range(desc, pres, encoding);
    let to_stored = |value| {
        pres.display
            .map_or(value, |map| (map.to_stored)(value, encoding))
    };
    let value_at = |part: f32| {
        let shown = lo + (hi - lo) * part;
        let text = format!("{:.*}", usize::from(pres.decimals), shown);
        let value = number(desc, to_stored(text.parse().unwrap()));
        (text, value)
    };
    // Stay apart after integer rounding, including a small 1..=3 range.
    let (_, start) = value_at(0.1);
    let (typed, target) = value_at(0.9);
    let editing = with_field(&effect, desc, start).unwrap();
    let name = |action| {
        format!(
            "{} / {} / {surface:?} / {variant} / {action}",
            entry.type_id, entry.parameter
        )
    };

    report.case(&name("open"), [key(Interaction::OpenWithoutWrite)], || {
        let mut run = run_for(surface, fixture_on(&effect, surface));
        let before = run.clip().picture_v2().unwrap();
        let dirty = run.state.dirty;
        ready(&mut run, label)?;
        commands_applied(&run)?;
        if run.clip().picture_v2().unwrap() != before
            || run.state.dirty != dirty
            || run.state.active_history().undo_count() != 0
        {
            return Err("opening the row wrote document state".into());
        }
        Ok(())
    });
    // Shown under a condition (Speed Ramp's Freeze At, with Freeze on):
    // the row comes and goes with it, the condition's stored field set the
    // other way on the effect straight, no widget involved; drawing either
    // writes nothing.
    if let Some(Cond::ParamIs { key: cond_key, value: met }) = pres.visible_if {
        report.case(&name("shown with its condition"), [key(Interaction::ConditionalVisibility)], || {
            let cond = ty.desc.params.iter().find(|p| p.key == cond_key).ok_or_else(|| format!("unknown condition {cond_key}"))?;
            let drawn = |effect: &BuiltinEffect| -> Result<usize, String> {
                let mut run = run_for(surface, fixture_on(effect, surface));
                let before = run.clip().picture_v2().unwrap();
                run.frame(Vec::new(), egui::Modifiers::NONE);
                let n = run.frame(Vec::new(), egui::Modifiers::NONE).iter().filter(|(t, _)| t == label.text).count();
                commands_applied(&run)?;
                if run.clip().picture_v2().unwrap() != before || run.state.active_history().undo_count() != 0 {
                    return Err("drawing the card wrote".into());
                }
                Ok(n)
            };
            let shown = drawn(&effect)?;
            if shown == 0 {
                return Err(format!("{:?} isn't drawn with {cond_key} met", label.text));
            }
            let unmet = match stored(&effect, cond) {
                Value::Bool(_) => Value::Bool(met == 0.0),
                _ => Value::from(u32::from(met == 0.0)),
            };
            let hidden = drawn(&with_field(&effect, cond, unmet)?)?;
            if hidden >= shown {
                return Err(format!("{:?} is drawn {hidden} times with {cond_key} unmet, {shown} with it met", label.text));
            }
            Ok(())
        });
    }
    report.case(
        &name("type"),
        [key(Interaction::TypeNumber), key(Interaction::UndoRedo)],
        || {
            edit_case(surface, &editing, desc, Some(target.clone()), |run| {
                type_value(run, label, &typed)
            })
        },
    );
    use super::parity_pixels::Routes;
    match CPU_BY_DESIGN.iter().find(|(ty, _)| *ty == entry.type_id).filter(|_| known_defect(entry, surface, variant).is_none()) {
        Some((_, reason)) => report.case_cpu_by_design(&name("pixels"), [key(Interaction::Pixels)], reason, || {
            pixel_case(Routes::CpuByDesign, ty, surface, &effect, desc, label, value_at)
        }),
        // A known CPU/GPU defect: its key excluded with the issue, the edit
        // still changes the frame.
        None if known_defect(entry, surface, variant).is_some_and(|why| why.starts_with("known CPU/GPU")) => {
            report.case(&name("pixels, routes apart (pinned)"), [], || super::parity_pixels::with_routes_apart(|| pixel_case(Routes::Both, ty, surface, &effect, desc, label, value_at)))
        }
        None if known_defect(entry, surface, variant).is_some() => {}
        None => report.case_or_blocked(&name("pixels"), [key(Interaction::Pixels)], || {
            pixel_case(Routes::Both, ty, surface, &effect, desc, label, value_at)
        }),
    }
    // What the drag must store: a scrubby row's rule from where it starts,
    // over the range it draws (its shown range, widened to hold the value
    // and the reset), whole for a whole-number row. A slider row places
    // its value where the pointer is, held to having moved instead.
    let dragged_to = match pres.widget {
        Widget::Slider => None,
        _ => stored(&editing, desc).as_f64().map(|v| {
            let (v, reset) = (v as f32, row::reset_value(desc, pres));
            let (lo, hi) = (lo.min(v).min(reset), hi.max(v).max(reset));
            number(desc, (v + (f64::from(drag_px(desc, pres)) * row::drag_speed(pres.step)) as f32).clamp(lo, hi))
        }),
    };
    report.case(
        &name("drag"),
        [key(Interaction::Drag), key(Interaction::UndoRedo)],
        || {
            edit_case(surface, &editing, desc, dragged_to.clone(), |run| {
                let at = start_drag(run, label, drag_px(desc, pres), matches!(pres.widget, Widget::Slider))?;
                run.button(at, egui::PointerButton::Primary, false);
                Ok(())
            })
        },
    );
    let reset = number(desc, row::reset_value(desc, pres));
    let away = if stored(&editing, desc) == reset {
        with_field(&effect, desc, value_at(0.9).1).unwrap()
    } else {
        editing.clone()
    };
    report.case(
        &name("reset"),
        [key(Interaction::Reset), key(Interaction::UndoRedo)],
        || {
            edit_case(surface, &away, desc, Some(reset.clone()), |run| {
                let (at, _) = ready(run, label)?;
                run.button(at, egui::PointerButton::Primary, true);
                run.button(at, egui::PointerButton::Primary, false);
                Ok(())
            })
        },
    );
    report.case(&name("cancel"), [key(Interaction::CancelDrag)], || {
        let mut run = run_for(surface, fixture_on(&editing, surface));
        let before = run.clip().picture_v2().unwrap();
        let at = start_drag(&mut run, label, drag_px(desc, pres), matches!(pres.widget, Widget::Slider))?;
        if run.clip().picture_v2().unwrap() == before {
            return Err("cancel setup did not change the value".into());
        }
        run.key(egui::Key::Escape, egui::Modifiers::NONE);
        run.frame(
            vec![egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))],
            egui::Modifiers::NONE,
        );
        run.button(
            at + egui::vec2(10.0, 0.0),
            egui::PointerButton::Primary,
            false,
        );
        commands_applied(&run)?;
        if run.clip().picture_v2().unwrap() != before
            || run.state.active_history().undo_count() != 0
            || run.state.any_drag_in_progress()
        {
            return Err("Escape did not restore the picture without an undo entry".into());
        }
        Ok(())
    });
}

pub(super) fn run(report: &mut Report) {
    for entry in catalog().into_iter().filter(numeric) {
        let ty = effect_types()
            .find(|ty| ty.desc.id == entry.type_id)
            .unwrap();
        for (surface, variant) in contexts(ty) {
            let name = format!(
                "{} / {} / {surface:?} / {variant}",
                entry.type_id, entry.parameter
            );
            report.family(&name, |report| run_entry(report, &entry, surface, variant));
        }
    }
}

#[test]
fn all_effect_numeric_rows_edit_and_cancel_through_real_controls() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    eprintln!(
        "numeric catalog rows: {}; earned keys: {}; failures: {}",
        catalog().iter().filter(|e| numeric(e)).count(),
        report.passed.len(),
        report.failures.len()
    );
    for (key, why) in report.blocked.iter().chain(&report.cpu_by_design) {
        eprintln!("{key:?}: {why}");
    }
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report
        .passed
        .iter()
        .all(|key| manifest.required.contains(key)));
    // The blocked Pixels cases are exactly the pinned types: a new one is
    // a route regression to look at, and one that passes now comes off
    // the list.
    let blocked: std::collections::BTreeSet<&str> = report.blocked.keys().map(|k| k.type_id.as_str()).collect();
    let pinned: std::collections::BTreeSet<&str> = BLOCKED.iter().map(|(ty, _)| *ty).collect();
    assert_eq!(blocked, pinned, "blocked Pixels types differ from the pinned list");
}

/// The Speed Ramp card's Speed row retimes its clip (#1409's question, M4
/// step F): typed to 2, the clip plays at twice its speed, and the source
/// is read at twice the elapsed time, as one undo step. A row stays only
/// if it changes the picture.
#[test]
fn the_speed_ramp_speed_row_retimes_its_clip() {
    let ty = effect_types().find(|t| t.tag == "speed_ramp").expect("Speed Ramp");
    let effect = (ty.sample)();
    let mut run = Run::new(fixture_on(&effect, Surface::EffectsStack));
    let before = run.clip().source_secs_at(0.5, 24.0);
    type_value(&mut run, RowLabel { text: "Speed", group: None }, "2.00").expect("the Speed row is drawn and typed");
    commands_applied(&run).expect("applied");
    assert_eq!(run.clip().speed, 2.0, "the clip's speed");
    let after = run.clip().source_secs_at(0.5, 24.0);
    assert!((after - 2.0 * before).abs() < 1e-6, "the source is read at {after} s, not twice {before} s");
    assert_eq!(run.state.active_history().undo_count(), 1, "one step");
}

