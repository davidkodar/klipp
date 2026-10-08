//! Interaction obligations are independent of the family that supplies
//! fixtures and assertions. Unknown specialist editors fail closed.

use super::{
    CatalogEntry, CatalogValue, CurveAction, Interaction, KeyInterpolation, Surface, WheelAxis,
};
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::ParamKind;
use std::collections::BTreeSet;

/// Reviewed absent affordances stay obligations with explicit exclusions,
/// not silently omitted interactions. One decision covers every fixture.
pub(super) fn key_exclusion(entry: &CatalogEntry, surface: Surface) -> Option<&'static str> {
    if surface == Surface::CurveEditor {
        return None;
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
            Some("#1318 (wheel axes have no diamonds; keys come from the curve editor and canvas)")
        }
        _ => None,
    }
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
            CatalogValue::Mix => ParamKind::Float,
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
        if kind == ParamKind::Float {
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
                match desc.kind {
                    ParamKind::Float => required.extend([
                        Interpolation(KeyInterpolation::Linear),
                        Interpolation(KeyInterpolation::Step),
                        Interpolation(KeyInterpolation::Bezier),
                    ]),
                    ParamKind::Whole | ParamKind::Bool | ParamKind::Choice(_) => {
                        required.insert(Interpolation(KeyInterpolation::Step));
                    }
                    ParamKind::Colour | ParamKind::Other => {}
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
        CatalogValue::Mix => required.extend([
            TypeNumber,
            Drag,
            CancelDrag,
            Reset,
            AddKey,
            EditKey,
            RemoveKey,
            PreviousKey,
            NextKey,
            UndoRedo,
            Pixels,
        ]),
        CatalogValue::Enabled => required.extend([Toggle(false), Toggle(true), UndoRedo, Pixels]),
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
