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

/// A grade row's keyframe controls (its diamond, its carets, a typed edit
/// at a key, its interpolation menu) are the generated rows' one code
/// path, whatever the type: so they're owed on a representative row of
/// each grade page, the clip's grade and each of the sequence's grades,
/// not on every type × page × grade (flip readiness ruling, 10-09). That
/// every keyframable grade row takes a key at all is owed per type, by
/// its diamond on its own page ([`primary_grade_page`], `AddKey` alone).
/// The rows: a per-node type's on the per-node layout, the Corrector's on
/// the tabbed page and the bottom tools.
pub(super) const GRADE_KEY_REPRESENTATIVE: [(Surface, &str, &str); 3] = [
    (Surface::GradePerNode, "klipp.grade.denoise", "denoise_luma"),
    (Surface::GradeTabbedRight, "klipp.grade.corrector", "exposure"),
    (Surface::GradeBottomTools, "klipp.grade.corrector", "exposure"),
];

/// The fixture variant of [`GRADE_KEY_REPRESENTATIVE`]'s cases: the row
/// with keys on it.
pub(super) const GRADE_KEY_VARIANT: &str = "keyed";

/// A curve-editor lane's edit drawn (`Pixels`): the lane is one widget
/// writing the key it edits, whatever the effect, and the key reaches the
/// frame through the effect node's animation, which the keyed-pixels family
/// draws per type on the stack. So it's owed on one lane of each kind
/// (flip readiness ruling, 10-09): a number on an effect node, a whole
/// number (stepped), and a placed kind's lane, the clip's opacity (its
/// Video track). No colour lane exists: no effect colour is keyframable.
pub(super) const CURVE_PIXEL_REPRESENTATIVE: [(&str, &str); 3] = [
    ("klipp.fx.gaussian_blur", "radius"),
    ("klipp.fx.posterize", "levels"),
    ("klipp.fx.opacity", "value"),
];

/// A keyframable number's key controls.
pub(super) const KEY_INTERACTIONS: [Interaction; 8] = [
    Interaction::AddKey,
    Interaction::EditKey,
    Interaction::RemoveKey,
    Interaction::PreviousKey,
    Interaction::NextKey,
    Interaction::Interpolation(KeyInterpolation::Linear),
    Interaction::Interpolation(KeyInterpolation::Step),
    Interaction::Interpolation(KeyInterpolation::Bezier),
];

/// A sequence grade (the timeline grade, a group's pre-clip and post-clip
/// stages) is drawn by the clip grade's pages at its own root, so what's
/// owed there is that each root draws and edits through each page: a
/// representative row's whole numeric set ([`GRADE_KEY_REPRESENTATIVE`]'s
/// rows, as the grade numeric family runs them), not every type on every
/// root (flip readiness ruling, 10-09). A family that runs more on the
/// sequence's grades still owes all it requires.
pub(super) const ROOT_NUMERIC: [Interaction; 7] = [
    Interaction::OpenWithoutWrite,
    Interaction::TypeNumber,
    Interaction::Drag,
    Interaction::CancelDrag,
    Interaction::Reset,
    Interaction::UndoRedo,
    Interaction::Pixels,
];

/// The variant the grade numeric family runs a row with no conditions
/// under (`parity_controls::grade_variant`).
pub(super) const ROOT_NUMERIC_VARIANT: &str = "sample";

/// A grade node's bypass (the per-node layout's header, the tabbed page's
/// and the bottom tools' strip) is one generated switch whatever the
/// node, so its switching, its frame and its keys are owed on a
/// representative node of each page, on the clip's grade and each
/// sequence grade (flip readiness ruling, 10-09); per type, only that its
/// page opens without a write. The rows: a per-node type's on the per-node
/// layout, the Corrector's on the tabbed page and the bottom tools.
pub(super) const BYPASS_REPRESENTATIVE: [(Surface, &str); 3] = [
    (Surface::GradePerNode, "klipp.grade.denoise"),
    (Surface::GradeTabbedRight, "klipp.grade.corrector"),
    (Surface::GradeBottomTools, "klipp.grade.corrector"),
];

/// The variant of [`BYPASS_REPRESENTATIVE`]'s cases.
pub(super) const BYPASS_VARIANT: &str = "bypass";

/// What the bypass switch does: on and off, its frame, and its keys (its
/// diamond adds and removes one; once keyed, the switch edits the key at
/// the playhead; its carets go to the keys either side, and its menu holds
/// a key).
pub(super) const BYPASS_INTERACTIONS: [Interaction; 10] = [
    Interaction::Toggle(false),
    Interaction::Toggle(true),
    Interaction::UndoRedo,
    Interaction::Pixels,
    Interaction::AddKey,
    Interaction::RemoveKey,
    Interaction::EditKey,
    Interaction::PreviousKey,
    Interaction::NextKey,
    Interaction::Interpolation(KeyInterpolation::Step),
];

/// The bypass representatives' keys on `page`'s grade (`variant` names it),
/// each interaction of `of`.
fn bypass_keys(of: &[Interaction]) -> Vec<super::CoverageKey> {
    let mut keys = Vec::new();
    for (page, ty) in BYPASS_REPRESENTATIVE {
        for interaction in of {
            keys.push(super::CoverageKey::new(ty, "@enabled", page, BYPASS_VARIANT, *interaction));
            for root in crate::app::parity_harness::GradeRoot::SEQUENCE {
                keys.push(super::CoverageKey::new(ty, "@enabled", Surface::NonClipGrade, &format!("{}: {page:?} {BYPASS_VARIANT}", root.variant()), *interaction));
            }
        }
    }
    keys
}

/// [`REPRESENTATIVE`] as keys, with the grade representatives.
pub(super) fn representative_keys() -> Vec<super::CoverageKey> {
    use super::CoverageKey;
    let mut keys: Vec<CoverageKey> = REPRESENTATIVE.iter().map(|(ty, parameter, surface, variant, interaction)| CoverageKey::new(ty, parameter, *surface, variant, *interaction)).collect();
    for (ty, parameter) in CURVE_PIXEL_REPRESENTATIVE {
        keys.push(CoverageKey::new(ty, parameter, Surface::CurveEditor, "keyed", Interaction::Pixels));
    }
    for (page, ty, parameter) in GRADE_KEY_REPRESENTATIVE {
        for interaction in KEY_INTERACTIONS {
            keys.push(CoverageKey::new(ty, parameter, page, GRADE_KEY_VARIANT, interaction));
        }
        for root in crate::app::parity_harness::GradeRoot::SEQUENCE {
            for interaction in KEY_INTERACTIONS {
                keys.push(CoverageKey::new(ty, parameter, Surface::NonClipGrade, &format!("{}: {page:?} {GRADE_KEY_VARIANT}", root.variant()), interaction));
            }
            for interaction in ROOT_NUMERIC {
                keys.push(CoverageKey::new(ty, parameter, Surface::NonClipGrade, &format!("{}: {page:?} {ROOT_NUMERIC_VARIANT}", root.variant()), interaction));
            }
        }
    }
    keys.extend(bypass_keys(&BYPASS_INTERACTIONS));
    keys
}

/// The clip grade page a grade row is first drawn on: the per-node layout
/// for a per-node type, else the tabbed page, else the bottom tools. Where
/// its per-type `AddKey` is owed ([`GRADE_KEY_REPRESENTATIVE`]).
pub(super) fn primary_grade_page(entry: &CatalogEntry) -> Option<Surface> {
    let surfaces = surfaces(entry).ok()?;
    [Surface::GradePerNode, Surface::GradeTabbedRight, Surface::GradeBottomTools].into_iter().find(|s| surfaces.contains(s))
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
        CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, Widget::Custom("compound_body")) =>
            Some("#1391: a grade compound's body is edited on the retained nested route until M4 step F lifts compounds as groups"),
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
        // A lean specialist editor, owed once where its node is first drawn.
        if matches!(&entry.value, CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, Widget::Custom(id) if LEAN_EDITORS.contains(&id))) {
            let page = if crate::app::main_layout::per_node_kind(kind) { GradePerNode } else { GradeTabbedRight };
            return Ok(BTreeSet::from([page]));
        }
        // A node's bypass is one switch on every page (`BYPASS_REPRESENTATIVE`):
        // per type, owed where its node is first drawn.
        if matches!(entry.value, CatalogValue::Enabled) {
            let page = if crate::app::main_layout::per_node_kind(kind) { GradePerNode } else { GradeTabbedRight };
            return Ok(BTreeSet::from([page]));
        }
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
        // the selected-node layout: Noise Reduction's rows (the Adjustments
        // tab's section) and the Warper. The Adjustments tab's MASKS section
        // is the Mask node's own editor (`render_masks_body`), owed where
        // the node's page draws it.
        if matches!(kind, Denoise | ColorWarper)
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
        // A grade inside a compound draws the same generated rows by its
        // path through the compound's group (#1391), so it is no separate
        // surface. The timeline grade and a group's stages draw generated
        // at their own roots (M3 step 19); the grade families earn their keys by
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
            if matches!(presentation.widget, Widget::WheelPart(_)) || sat_bri_cell(entry) =>
        {
            Some(GRADE_KEYFRAMING)
        }
        // A grade specialist editor (curves, the warper) draws no diamond.
        CatalogValue::Parameter { presentation, .. }
            if entry.grade && matches!(presentation.widget, Widget::Custom(_)) =>
        {
            Some(GRADE_KEYFRAMING)
        }
        _ => None,
    }
}

/// The specialist editors owed once, on the page their node is first drawn
/// on (`specialist`): the same editor on another page or a sequence grade
/// is the same widget writing the same command.
pub(super) const LEAN_EDITORS: [&str; 6] = ["curves", "huesat_curves", "camera_encoding", "layer_mixer_inputs", "plate_file", "matte_file"];

/// A Wheel Sat/Bri node's saturation or brightness cell.
fn sat_bri_cell(entry: &CatalogEntry) -> bool {
    entry.type_id == "klipp.grade.wheel_sat_bri" && (entry.parameter.ends_with("_sat") || entry.parameter.ends_with("_bri"))
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
        // A lane's edit drawn is owed per kind of lane, not per type
        // (`CURVE_PIXEL_REPRESENTATIVE`).
        required.extend([
            AddKey,
            EditKey,
            RemoveKey,
            PreviousKey,
            NextKey,
            UndoRedo,
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
            // A Wheel Sat/Bri node's cells are presented as sliders and
            // drawn as the cells under the bottom tools' wide wheels: a
            // wheel part's (`NARROW_WHEEL_EXCLUSION`).
            let widget = if sat_bri_cell(entry) { Widget::WheelPart("sat_bri") } else { presentation.widget };
            match widget {
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
                // A measured value: shown, written by its node's tool, so
                // it owes its frame, not editing (#1421).
                Widget::Readout => {
                    required.insert(Pixels);
                }
                Widget::Custom(id) => required.extend(specialist(id)?),
            }
            // A grade row's key controls are owed on the representatives
            // (`GRADE_KEY_REPRESENTATIVE`); per type, only that its diamond
            // takes a key, on its own page.
            if entry.grade && desc.keyframable && presentation.widget != Widget::Hidden {
                if primary_grade_page(entry) == Some(surface) {
                    required.insert(AddKey);
                }
            } else if desc.keyframable && presentation.widget != Widget::Hidden {
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
        // A grade node's bypass: per type, its page opened; what the switch
        // does is the representatives' (`BYPASS_REPRESENTATIVE`).
        CatalogValue::Enabled if entry.grade => {}
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
    // A sampling tool's frame (white balance, the qualifier's picker,
    // Colour Match) on a sequence grade is drawn through the stage the
    // representative row's frame is (`ROOT_NUMERIC`'s Pixels): owed on the
    // clip's grade, not again on every root.
    let specialist = matches!(entry.value, CatalogValue::Workflow(_));
    if surface == Surface::NonClipGrade && specialist {
        required.remove(&Pixels);
    }
    Ok(required)
}

fn specialist(id: &str) -> Result<Vec<Interaction>, String> {
    use Interaction::*;
    Ok(match id {
        // A specialist editor owes what shows it does the right thing
        // (flip readiness ruling, 10-09): one real edit through the drawn
        // widget storing what it must, one undo step undone exactly, its
        // cancel where it has one, and its frame, on the page it's drawn
        // on ([`LEAN_EDITORS`]). A curve's other point edits and its reset
        // write the same curve through the same command as the edit does.
        "curves" => vec![CurvePoint(CurveAction::Add), CancelDrag, UndoRedo, Pixels],
        // The eyedropper is a request the app runs on the viewer's frame.
        "huesat_curves" => vec![CurvePoint(CurveAction::Add), CancelDrag, Eyedropper, UndoRedo, Pixels],
        "colour_warper" => vec![Drag, CancelDrag, Reset, UndoRedo, Pixels],
        "frei0r_plugin" => vec![
            PluginSelect,
            PluginParameter,
            MissingAsset,
            UndoRedo,
            Pixels,
        ],
        // An image picked, cleared, or the picker cancelled; a missing file
        // is the LUT's case, which reads it the same way.
        "matte_file" | "plate_file" => vec![FileSelect, FileClear, FileCancel, UndoRedo, Pixels],
        "lut_file" => vec![
            FileSelect,
            FileClear,
            FileCancel,
            MissingAsset,
            UndoRedo,
            Pixels,
        ],
        // The file's encoding converts its frames as they're decoded; the
        // Input Transform node passes its input on, so no frame the grade
        // draws shows the pick. Its frame is owed by the decode tests:
        // klipp-color's `camera_log_parity`
        // (`every_camera_log_curve_matches_its_reference_at_every_code`,
        // `the_decoder_uses_the_same_curve_clamped_at_zero`) and
        // klipp-compose's `golden_corpus` case "scene/S-Log3 graded with glow".
        "camera_encoding" => vec![SelectEncoding, UndoRedo],
        "mask_stack" => vec![AddMask, EditMask, RemoveMask, CancelDrag, UndoRedo, Pixels],
        // The inspector edits an input's settings; inputs are added and
        // removed by wiring the node on the canvas.
        "layer_mixer_inputs" => vec![EditLayer, UndoRedo, Pixels],
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
        // A sampling tool asks the app to measure the viewer's frame and
        // writes what it measured into rows whose own frames are owed (the
        // balance's temperature and tint, the qualifier's ranges, Colour
        // Match's readouts): its frame is theirs, not owed again here.
        "white_balance" => vec![
            AutoWhiteBalance,
            Eyedropper,
            ResultDuringDrag,
            UndoRedo,
        ],
        "qualifier_picker" => vec![Eyedropper, ResultDuringDrag, UndoRedo],
        // A match landing while a grade slider's drag is held, as the
        // balance and the qualifier's key can (a socket write, a missed
        // release): the drag its own step first.
        "colour_match" => vec![CaptureReference, ColourMatch, Reset, ResultDuringDrag, UndoRedo],
        // These adapters add notices or nothing; parameter rows carry the edits.
        "temporal_denoise" | "speed_ramp" | "focus_blur" | "color_fade" | "corner_pin" => vec![],
        _ => return Err(format!("unclassified specialist: {id:?}")),
    })
}
