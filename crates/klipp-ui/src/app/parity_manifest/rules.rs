//! Interaction obligations are independent of the family that supplies
//! fixtures and assertions. Unknown specialist editors fail closed.

use super::{
    CatalogEntry, CatalogValue, CurveAction, Interaction, KeyInterpolation, Surface, WheelAxis,
};
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::ParamKind;
use std::collections::BTreeSet;

/// A track's grade is graded nowhere in the app, so it has no generated or
/// old route to compare: the `NonClipGrade` surface's track variant.
pub(super) const TRACK_GRADE_EXCLUSION: &str = "#1408 (a track's grade has no inspector surface yet; nothing to compare or delete)";

/// A RETAINED old route, not a dead one: nested grade compounds keep the
/// old rows until M4 step 1b (#1391), so the surface earns nothing and
/// blocks nothing, and step 19's deletion keeps the code nested editing
/// uses.
pub(super) const NESTED_GRADE_EXCLUSION: &str = "#1391: RETAINED old route: nested grade compounds keep the old rows until M4 step 1b (none in the 348-project corpus); step 19 keeps the code nested grade editing uses";

/// A grade row's track in the curve editor: grade keyframing isn't M3's.
pub(super) const GRADE_CURVE_EXCLUSION: &str = "#891 (grade keyframing: phase 4; the curve editor lists effect tracks only)";

pub(super) const GRAPH_INSPECTOR_EXCLUSION: &str =
    "#1347: picture-graph selection does not reveal an effect inspector in either view";

/// Obligations no catalog parameter carries, named here so no family can
/// define them away: a representative per kind, not one per row.
///
/// A masked effect's row (`MaskedPixels`): one effect, a Gaussian Blur,
/// for each kind of mask an effect can read, a window inside the grade
/// that the grade group publishes and a window node beside the clip's
/// chain. A masked effect renders through its own path
/// (`effects_mask_unavailable`), and its row moves with the flip.
pub(super) const REPRESENTATIVE: &[(&str, &str, Surface, &str, Interaction)] = &[
    ("klipp.fx.gaussian_blur", "@mask/in-the-grade", Surface::EffectsStack, "masked", Interaction::MaskedPixels),
    ("klipp.fx.gaussian_blur", "@mask/beside", Surface::EffectsStack, "masked", Interaction::MaskedPixels),
];

/// [`REPRESENTATIVE`] as keys.
pub(super) fn representative_keys() -> Vec<super::CoverageKey> {
    REPRESENTATIVE.iter().map(|(ty, parameter, surface, variant, interaction)| super::CoverageKey::new(ty, parameter, *surface, variant, *interaction)).collect()
}

/// These catalog capabilities have no inspector control in either view.
/// Keep the list narrow: a new port must receive an explicit decision.
pub(super) fn entry_exclusion(entry: &CatalogEntry) -> Option<&'static str> {
    match &entry.value {
        CatalogValue::Port { port: "mask", desc } if desc.key == "channel" =>
            Some("#1348: mask.channel is command-tested but neither inspector exposes it"),
        CatalogValue::Mix if entry.grade =>
            Some("#1348: implicit grade-node mix is command-tested but neither inspector exposes it"),
        CatalogValue::Parameter { .. } if entry.type_id == "klipp.grade.primaries_matrix" =>
            Some("#1351: PrimariesMatrix parameter controls have no reachable inspector route in either view"),
        CatalogValue::Parameter { .. } if entry.type_id == "klipp.grade.color_match" =>
            Some("#1421: Colour Match's values are readouts in both views; the catalog presents them as sliders"),
        CatalogValue::Enabled if entry.type_id == "klipp.grade.input_transform" =>
            Some("#1351: InputTransform selection reaches camera encoding before the bypass header in both views"),
        _ => None,
    }
}

pub(super) const NARROW_WHEEL_EXCLUSION: &str =
    "#1351: WheelSatBri parameter controls are exposed in wide bottom wheels, not the narrow inspector";

/// Require each location actually exposed by the production layouts.
/// Families select fixture variants, but cannot silently omit a panel.
pub(super) fn surfaces(entry: &CatalogEntry) -> Result<BTreeSet<Surface>, String> {
    use Surface::*;
    if entry_exclusion(entry).is_some() {
        return Ok(BTreeSet::new());
    }
    let mut required = BTreeSet::new();
    if !entry.grade {
        let ty = klipp_effects::registry::effect_types()
            .find(|t| t.desc.id == entry.type_id)
            .ok_or_else(|| format!("{}: unclassified effect surfaces", entry.type_id))?;
        // Placed kinds still expose their own cards. In particular a
        // non-anchor Opacity remains a card; only the final enabled one
        // becomes the clip property, so its value needs both fixtures.
        required.insert(EffectsStack);
        if matches!(&entry.value, CatalogValue::Parameter { desc, .. }
            if (ty.tag == "opacity" && desc.key == "value")
                || (ty.tag == "transform" && matches!(desc.key,
                    "position_x" | "position_y" | "scale_x" | "scale_y"
                    | "rotation" | "rotation_x" | "rotation_y" | "perspective")))
        {
            required.insert(VideoTab);
        }
    } else {
        use klipp_nodes::NodeKind::*;
        let kind = klipp_nodes::registry::node_types()
            .find(|t| t.desc.id == entry.type_id)
            .ok_or_else(|| format!("{}: unclassified grade surfaces", entry.type_id))?
            .kind;
        // Use the same predicate that routes the production inspector.
        if crate::app::main_layout::per_node_kind(kind) {
            required.insert(GradePerNode);
        } else {
            match kind {
                Corrector | Exposure | Contrast | Saturation | ToneControls | ColorBalance
                | ColorWheels | RGBCurves | HueSatCurves | InputTransform => {
                    required.extend([GradeTabbedRight, GradeBottomTools]);
                }
                WheelSatBri => {
                    required.insert(GradeBottomTools);
                }
                // Only their bypass entry remains after the parameter exclusions.
                PrimariesMatrix | MaskCutout | Reroute => {
                    if matches!(entry.value, CatalogValue::Enabled) {
                        required.extend([GradeTabbedRight, GradeBottomTools]);
                    } else {
                        return Err(format!(
                            "{} / {}: unclassified grade surfaces",
                            entry.type_id, entry.parameter
                        ));
                    }
                }
                _ => return Err(format!("{}: unclassified grade surfaces", entry.type_id)),
            }
        }
        // These controls are also reached through tabs, independently of
        // the selected-node layout: Masks, Noise Reduction and Warper.
        if matches!(kind, Mask | Denoise | ColorWarper)
            && !matches!(entry.value, CatalogValue::Enabled)
        {
            required.extend([GradeTabbedRight, GradeBottomTools]);
        }
        // The bottom Adjustments bypass strip targets any selected node
        // when a Corrector is present. The right strip is available only
        // when selection has not switched that panel to per-node mode.
        if matches!(entry.value, CatalogValue::Enabled) {
            required.insert(GradeBottomTools);
            if !required.contains(&GradePerNode) {
                required.insert(GradeTabbedRight);
            }
        }
        // Nested grades keep the old rows, the boundary step 19 keeps
        // (NESTED_GRADE_EXCLUSION), so they're no surface to earn. The
        // timeline grade and a group's stages draw generated at their own
        // roots (M3 step 19); the grade families earn their keys by
        // running on them (`parity_controls::on_roots`). A track's grade
        // has no surface (TRACK_GRADE_EXCLUSION).
        required.insert(NonClipGrade);
    }
    // A grade row's track isn't in the curve editor (GRADE_CURVE_EXCLUSION).
    if !entry.grade && matches!(entry.value, CatalogValue::Parameter { desc, .. } if desc.keyframable) {
        required.insert(CurveEditor);
    }
    Ok(required)
}

/// Keyframing a grade track, a colour wheel's parts among them: no view
/// does it today (no diamond on a wheel part, and the curve editor lists
/// effect tracks only), and it's phase 4's "animation everywhere".
const GRADE_KEYFRAMING: &str = "#891 (grade keyframing: phase 4)";

/// Reviewed absent affordances stay obligations with explicit exclusions,
/// not silently omitted interactions. One decision covers every fixture.
pub(super) fn key_exclusion(entry: &CatalogEntry, surface: Surface) -> Option<&'static str> {
    // The curve editor lists effect tracks only; a grade track, a wheel's
    // parts among them, is keyed in neither view today.
    if surface == Surface::CurveEditor {
        return entry.grade.then_some(GRADE_KEYFRAMING);
    }
    match &entry.value {
        CatalogValue::Parameter { presentation, .. }
            if matches!(presentation.widget, Widget::Toggle | Widget::Combo) =>
        {
            Some("#1318 (IC1: switch/choice diamonds follow-up)")
        }
        CatalogValue::Parameter { presentation, .. }
            if matches!(presentation.widget, Widget::WheelPart(_)) =>
        {
            Some(GRADE_KEYFRAMING)
        }
        _ => None,
    }
}

/// Previous and Next key on the curve editor: Alt+←/→ walks the clip's own
/// video or audio track's keys, never the curve editor's selected track,
/// in either view, so it's a feature to add after M3, not parity.
pub(super) fn navigation_exclusion(surface: Surface, interaction: Interaction) -> Option<&'static str> {
    (surface == Surface::CurveEditor && matches!(interaction, Interaction::PreviousKey | Interaction::NextKey))
        .then_some("#1401 (Previous/Next key doesn't follow the curve editor's track)")
}

pub(super) fn is_key(interaction: Interaction) -> bool {
    matches!(
        interaction,
        Interaction::AddKey
            | Interaction::EditKey
            | Interaction::RemoveKey
            | Interaction::PreviousKey
            | Interaction::NextKey
            | Interaction::Interpolation(_)
    )
}

pub(super) fn interactions(
    entry: &CatalogEntry,
    surface: Surface,
) -> Result<BTreeSet<Interaction>, String> {
    use Interaction::*;
    let mut required = BTreeSet::from([OpenWithoutWrite]);
    if surface == Surface::CurveEditor {
        let kind = match entry.value {
            CatalogValue::Parameter { desc, .. } if desc.keyframable => desc.kind,
            CatalogValue::Enabled => ParamKind::Bool,
            _ => {
                return Err(format!(
                    "{} / {}: no curve-editor key track",
                    entry.type_id, entry.parameter
                ))
            }
        };
        required.extend([
            AddKey,
            EditKey,
            RemoveKey,
            PreviousKey,
            NextKey,
            UndoRedo,
            Pixels,
        ]);
        required.insert(Interpolation(KeyInterpolation::Step));
        if matches!(kind, ParamKind::Float | ParamKind::Colour) {
            required.extend([
                Interpolation(KeyInterpolation::Linear),
                Interpolation(KeyInterpolation::Bezier),
            ]);
        }
        return Ok(required);
    }
    match &entry.value {
        CatalogValue::Parameter { desc, presentation } => {
            match presentation.widget {
                Widget::Scrub
                | Widget::Slider
                | Widget::Gradient(_)
                | Widget::HueRainbow
                | Widget::Vec2 => {
                    required.extend([TypeNumber, Drag, CancelDrag, Reset, UndoRedo, Pixels])
                }
                Widget::Toggle => required.extend([Toggle(false), Toggle(true), UndoRedo, Pixels]),
                Widget::Combo => {
                    let choices = presentation
                        .labels
                        .or(match desc.kind {
                            ParamKind::Choice(choices) => Some(choices),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            format!(
                                "{} / {}: combo has no choices",
                                entry.type_id, entry.parameter
                            )
                        })?;
                    if choices.is_empty() {
                        return Err(format!(
                            "{} / {}: combo has no choices",
                            entry.type_id, entry.parameter
                        ));
                    }
                    required.extend((0..choices.len()).map(ComboSelect));
                    required.extend([UndoRedo, Pixels]);
                }
                Widget::ColourRgb => required.extend([ColourEdit, UndoRedo, Pixels]),
                Widget::ColourRgba => required.extend([ColourEdit, AlphaEdit, UndoRedo, Pixels]),
                Widget::WheelPart(_) => {
                    let axis = match entry.parameter.rsplit('_').next() {
                        Some("x") => WheelAxis::X,
                        Some("y") => WheelAxis::Y,
                        Some("master") => WheelAxis::Master,
                        Some("sat") => WheelAxis::Saturation,
                        Some("bri") => WheelAxis::Brightness,
                        _ => {
                            return Err(format!(
                                "{} / {}: unclassified wheel axis",
                                entry.type_id, entry.parameter
                            ))
                        }
                    };
                    required.extend([WheelDrag(axis), CancelDrag, Reset, UndoRedo, Pixels]);
                }
                Widget::Hidden => {
                    required.insert(PreserveHidden);
                }
                Widget::Custom(id) => required.extend(specialist(id)?),
            }
            if desc.keyframable && presentation.widget != Widget::Hidden {
                required.extend([AddKey, EditKey, RemoveKey, PreviousKey, NextKey]);
                // A numeric effect row's lane renders through the effect
                // node's animation, apart from its static value. Grade rows
                // are left out: grade keyframing is #891.
                if surface == Surface::EffectsStack && !entry.grade && matches!(presentation.widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::Vec2) {
                    required.insert(KeyedPixels);
                }
                match desc.kind {
                    ParamKind::Float | ParamKind::Colour => required.extend([
                        Interpolation(KeyInterpolation::Linear),
                        Interpolation(KeyInterpolation::Step),
                        Interpolation(KeyInterpolation::Bezier),
                    ]),
                    ParamKind::Whole | ParamKind::Bool | ParamKind::Choice(_) => {
                        required.insert(Interpolation(KeyInterpolation::Step));
                    }
                    ParamKind::Other => {}
                }
            }
            if presentation.visible_if.is_some() {
                required.insert(ConditionalVisibility);
            }
            if presentation.enabled_if.is_some() {
                required.insert(ConditionalEnabled);
            }
        }
        CatalogValue::Port { desc, .. } => {
            required.extend((0..desc.options.len()).map(ComboSelect));
            required.extend([UndoRedo, Pixels]);
        }
        CatalogValue::Mix => {
            required.extend([TypeNumber, Drag, CancelDrag, Reset, UndoRedo, Pixels])
        }
        CatalogValue::Enabled => {
            required.extend([Toggle(false), Toggle(true), UndoRedo, Pixels]);
            if entry.grade {
                required.extend([
                    AddKey,
                    EditKey,
                    RemoveKey,
                    PreviousKey,
                    NextKey,
                    Interpolation(KeyInterpolation::Step),
                ]);
            }
        }
        CatalogValue::Workflow(id) => required.extend(specialist(id)?),
    }
    Ok(required)
}

fn specialist(id: &str) -> Result<Vec<Interaction>, String> {
    use Interaction::*;
    Ok(match id {
        "curves" => vec![
            CurvePoint(CurveAction::Add),
            CurvePoint(CurveAction::Drag),
            CurvePoint(CurveAction::Remove),
            CancelDrag,
            Reset,
            UndoRedo,
            Pixels,
        ],
        "huesat_curves" => vec![
            CurvePoint(CurveAction::Add),
            CurvePoint(CurveAction::Drag),
            CurvePoint(CurveAction::Remove),
            CancelDrag,
            Reset,
            Eyedropper,
            UndoRedo,
            Pixels,
        ],
        "colour_warper" => vec![Drag, CancelDrag, Reset, UndoRedo, Pixels],
        "frei0r_plugin" => vec![
            PluginSelect,
            PluginParameter,
            MissingAsset,
            UndoRedo,
            Pixels,
        ],
        "lut_file" | "matte_file" => vec![
            FileSelect,
            FileClear,
            FileCancel,
            MissingAsset,
            UndoRedo,
            Pixels,
        ],
        "plate_file" => vec![
            FileSelect,
            FileClear,
            FileCancel,
            MissingAsset,
            CapturePlate,
            UndoRedo,
            Pixels,
        ],
        "camera_encoding" => vec![SelectEncoding, UndoRedo, Pixels],
        "mask_stack" => vec![AddMask, EditMask, RemoveMask, CancelDrag, UndoRedo, Pixels],
        "layer_mixer_inputs" => vec![AddLayer, EditLayer, RemoveLayer, UndoRedo, Pixels],
        "compound_body" => vec![
            AddNode,
            RemoveNode,
            ReorderNode,
            SharedBranchRefusal,
            Reveal,
            UndoRedo,
            Pixels,
        ],
        "deflicker" | "video_stabilization" => vec![
            StartAnalysis,
            CancelAnalysis,
            ResultDuringDrag,
            UndoRedo,
            Pixels,
        ],
        "white_balance" => vec![
            AutoWhiteBalance,
            Eyedropper,
            ResultDuringDrag,
            UndoRedo,
            Pixels,
        ],
        "qualifier_picker" => vec![Eyedropper, ResultDuringDrag, UndoRedo, Pixels],
        "colour_match" => vec![CaptureReference, ColourMatch, Reset, UndoRedo, Pixels],
        // These adapters add notices or nothing; parameter rows carry the edits.
        "temporal_denoise" | "speed_ramp" | "focus_blur" | "color_fade" | "corner_pin" => vec![],
        _ => return Err(format!("unclassified specialist: {id:?}")),
    })
}
