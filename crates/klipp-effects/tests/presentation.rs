//! The effects' presentation (M3 step 0a): every parameter of every
//! effect type is shown somehow, the table holds together, and nothing in
//! it can change a stored value.

use klipp_effects::registry::presentation::{self, DisplayMap, Presentation, Suffix, Widget};
use klipp_effects::registry::{effect_types, ParamDesc, ParamKind, Unit};

fn types() -> Vec<(&'static str, &'static [ParamDesc])> {
    effect_types().map(|t| (t.desc.id, t.desc.params)).collect()
}

#[test]
fn every_param_has_presentation() {
    let mut params = 0;
    for t in effect_types() {
        for p in t.desc.params {
            let pres = t.presentation(p);
            let problems = presentation::param_problems(t.desc.id, p, &pres);
            assert!(problems.is_empty(), "{problems:?}");
            params += 1;
        }
    }
    // Counted from the catalog, not taken from a list.
    let types = effect_types().count();
    assert!(types >= 54 && params > types, "{types} types, {params} parameters");
}

#[test]
fn the_table_names_real_params_and_holds_together() {
    let problems = presentation::problems(&types(), &presentation::params().copied().collect::<Vec<_>>(), &presentation::types().copied().collect::<Vec<_>>());
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_derived_presentation_follows_the_kind() {
    for t in effect_types() {
        for p in t.desc.params {
            let pres = Presentation::derived(p);
            let want = match p.kind {
                ParamKind::Float | ParamKind::Whole => Widget::Scrub,
                ParamKind::Bool => Widget::Toggle,
                // Two choices never imply a toggle.
                ParamKind::Choice(_) => Widget::Combo,
                ParamKind::Colour => Widget::ColourRgb,
                ParamKind::Other => Widget::Custom(""),
            };
            assert_eq!(pres.widget, want, "{}/{}", t.desc.id, p.key);
            // Derived, nothing is hidden: a fixed parameter is still shown.
            assert_ne!(pres.widget, Widget::Hidden);
            assert_eq!((pres.reset, pres.display, pres.labels), (None, None, None), "a derived row changes nothing about the value");
        }
    }
}

#[test]
fn a_suffix_comes_from_the_unit_unless_given() {
    assert_eq!(Suffix::FromUnit.text(Unit::CanvasPx), "px");
    assert_eq!(Suffix::FromUnit.text(Unit::Per1080), "px");
    assert_eq!(Suffix::FromUnit.text(Unit::Degrees), "°");
    assert_eq!(Suffix::FromUnit.text(Unit::Seconds), "s");
    assert_eq!(Suffix::FromUnit.text(Unit::Ratio), "");
    assert_eq!(Suffix::Text("×").text(Unit::Ratio), "×");
    assert_eq!(Suffix::None.text(Unit::Degrees), "");
}

fn encode(v: f32, _: klipp_grade_model::Encoding) -> f32 {
    v.max(0.0).powf(1.0 / 2.4)
}
fn decode(v: f32, _: klipp_grade_model::Encoding) -> f32 {
    v.max(0.0).powf(2.4)
}
fn halve(v: f32, _: klipp_grade_model::Encoding) -> f32 {
    v * 0.5
}

/// A display map shows a stored value and writes back exactly what was
/// stored (within one ulp) over the slider's range; one that doesn't is
/// found, so the table check refuses it.
#[test]
fn presentation_never_changes_stored_values() {
    let pivot = DisplayMap { to_shown: encode, to_stored: decode };
    assert_eq!(pivot.round_trip_fault((0.01, 1.0), 1001), None);
    let broken = DisplayMap { to_shown: encode, to_stored: halve };
    assert!(broken.round_trip_fault((0.01, 1.0), 1001).is_some());
    // The table check holds an entry's map to that over the parameter's
    // slider range, and holds labels to the choice count.
    let t = effect_types().find(|t| t.desc.params.iter().any(|p| p.kind == ParamKind::Float && p.soft.is_some())).unwrap();
    let p = t.desc.params.iter().find(|p| p.kind == ParamKind::Float && p.soft.is_some()).unwrap();
    let mut pres = Presentation::derived(p);
    pres.display = Some(broken);
    assert!(presentation::param_problems(t.desc.id, p, &pres).iter().any(|e| e.contains("comes back")));
    pres.display = Some(pivot);
    let soft = p.soft.unwrap();
    if soft.0 >= 0.0 {
        assert!(presentation::param_problems(t.desc.id, p, &pres).is_empty());
    }
    let (ct, cp, n) = effect_types().find_map(|t| t.desc.params.iter().find_map(|p| match p.kind { ParamKind::Choice(c) => Some((t, p, c.len())), _ => None })).unwrap();
    let mut pres = Presentation::derived(cp);
    pres.labels = Some(&["only one"]);
    assert_eq!(n == 1, presentation::param_problems(ct.desc.id, cp, &pres).is_empty());
    pres.labels = None;
    pres.widget = Widget::Toggle;
    // Said on a choice of two (off and on) it's allowed; on more, refused.
    assert_eq!(presentation::param_problems(ct.desc.id, cp, &pres).iter().any(|e| e.contains("a toggle")), n != 2, "{}/{}", ct.desc.id, cp.key);
    let (mt, mp) = effect_types().find_map(|t| t.desc.params.iter().find(|p| matches!(p.kind, ParamKind::Choice(c) if c.len() > 2)).map(|p| (t, p))).unwrap();
    assert!(presentation::param_problems(mt.desc.id, mp, &pres).iter().any(|e| e.contains("a toggle")), "a choice of more than two is never a toggle");
}

#[test]
fn an_entry_naming_no_param_is_refused() {
    let p = effect_types().next().unwrap().desc.params[0];
    let stray = [presentation::ParamEntry { type_id: "klipp.fx.nothing", key: "nothing", pres: Presentation::derived(&p) }];
    assert_eq!(presentation::problems(&types(), &stray, &[]).len(), 1);
    let twice = [presentation::ParamEntry { type_id: effect_types().next().unwrap().desc.id, key: p.key, pres: Presentation::derived(&p) }; 2];
    assert!(presentation::problems(&types(), &twice, &[]).iter().any(|e| e.contains("two entries")));
}

/// The types the tables give rows for so far (M3 step 0b lands family by
/// family): each gives every one of its parameters an entry.
fn covered() -> Vec<&'static klipp_effects::registry::EffectType> {
    effect_types().filter(|t| presentation::params().any(|e| e.type_id == t.desc.id)).collect()
}

/// Types made after the old inspector was recorded (#1746, #989, #1638): their rows
/// are the tables' alone, with no old row to match.
const AFTER_THE_RECORDING: &[&str] = &["klipp.fx.lut_3d", "klipp.fx.colour_space_transform", "klipp.fx.picture_transform", "klipp.fx.flicker", "klipp.fx.binocular_vignette", "klipp.fx.merge", "klipp.fx.generator", "klipp.fx.luma_key", "klipp.fx.particles", "klipp.fx.media_source", "klipp.fx.title_source", "klipp.fx.surface_pin", "klipp.fx.clip_below", "klipp.fx.grade_compound", "klipp.fx.displace", "klipp.fx.reflection", "klipp.fx.old_film"];

#[test]
fn a_covered_type_names_every_param() {
    for t in covered() {
        for p in t.desc.params {
            assert!(presentation::params().any(|e| e.type_id == t.desc.id && e.key == p.key), "{}/{} has no entry", t.desc.id, p.key);
        }
    }
}

/// Differences from today's rows that are meant, by (type, parameter,
/// what): each is a reviewed change (m3-work-breakdown's IC list).
const MEANT: &[(&str, &str, &str)] = &[
    // #2763: Glow's threshold, where its light starts, is a row of its own.
    ("klipp.fx.glow", "threshold", "row"),
    // IC2: Vibrance resets to its catalog default, 0, not 0.5.
    ("klipp.fx.vibrance", "amount", "reset"),
    // IC12: a Transform effect card shows its own tilt and perspective
    // rows (today drawn in the video tab, which shows only the clip's
    // placement then).
    ("klipp.fx.transform", "rotation_x", "row"),
    ("klipp.fx.transform", "rotation_y", "row"),
    ("klipp.fx.transform", "perspective", "row"),
    // IC5: Letterbox's bar colour is a choice of two colours, not a 0-to-1
    // slider whose label explains the numbers.
    ("klipp.fx.letterbox", "bar_color", "widget"),
    ("klipp.fx.letterbox", "bar_color", "label"),
    // IC2: Sharpen resets to its catalog default, 0, not 0.5.
    ("klipp.fx.sharpen", "amount", "reset"),
    // IC5: Noise's monochrome is a switch, not a 0-to-1 slider.
    ("klipp.fx.noise_generator", "monochrome", "widget"),
];

/// `p`'s value as the recorded effect stores it: a switch as 0 or 1, a
/// choice by its index.
fn stored(case: &serde_json::Value, p: &ParamDesc) -> Option<f32> {
    let v = &case["stored"][p.field];
    match (p.kind, v) {
        (_, serde_json::Value::Bool(b)) => Some(f32::from(u8::from(*b))),
        (ParamKind::Choice(names), serde_json::Value::String(s)) => names.iter().position(|n| n == s).map(|i| i as f32),
        (_, serde_json::Value::Number(n)) => n.as_f64().map(|x| x as f32),
        _ => None,
    }
}

/// Whether `pres`'s row shows for the recorded `case`.
fn shows(t: &klipp_effects::registry::EffectType, case: &serde_json::Value, pres: &Presentation) -> bool {
    match pres.visible_if {
        None => true,
        Some(presentation::Cond::ParamIs { key, value }) => {
            let p = t.desc.params.iter().find(|p| p.key == key).unwrap_or_else(|| panic!("{}: a condition on {key}, no such parameter", t.desc.id));
            stored(case, p) == Some(value)
        }
    }
}

/// Every covered type's rows are today's: the same parameters in the same
/// order, each with today's label, widget, decimals, drag step, suffix and
/// reset, as the old inspector recorded them (M3 step 1b's fixture), but
/// for the meant differences above.
#[test]
fn presentation_matches_the_old_rows() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../../klipp-ui/tests/fixtures/old_rows.json")).unwrap();
    let mut differ = Vec::new();
    let mut compared = 0;
    for t in covered().into_iter().filter(|t| !AFTER_THE_RECORDING.contains(&t.desc.id)) {
        let cases: Vec<&serde_json::Value> = fixture["cases"].as_array().unwrap().iter().filter(|c| c["kind"] == t.desc.id).collect();
        assert!(cases.iter().any(|c| c["variant"] == "sample"), "{}: not recorded", t.desc.id);
        for case in cases {
        let rows = case["rows"].as_array().unwrap();
        // The order: by `order`, then the catalog's, hidden rows and rows
        // whose condition doesn't hold left out.
        let mut shown: Vec<(u16, usize, &ParamDesc)> = t.desc.params.iter().enumerate().map(|(i, p)| (t.presentation(p).order, i, p)).filter(|(_, _, p)| t.presentation(p).widget != Widget::Hidden && shows(t, case, &t.presentation(p))).collect();
        shown.sort_by_key(|(o, i, _)| (*o, *i));
        // A named editor draws its own (frei0r's plug-in picker drew none
        // when recorded, with no plug-in installed): it isn't a row.
        let order: Vec<&str> = shown
            .iter()
            .filter(|(_, _, p)| !matches!(t.presentation(p).widget, Widget::Custom(_)))
            .map(|(_, _, p)| p.key)
            .filter(|k| !MEANT.contains(&(t.desc.id, k, "row")))
            .collect();
        let recorded: Vec<&str> = rows.iter().map(|r| r["parameter"].as_str().unwrap()).collect();
        if order != recorded {
            differ.push(format!("{} [{}]: rows {order:?}, today {recorded:?}", t.desc.id, case["variant"]));
        }
        for r in rows {
            let key = r["parameter"].as_str().unwrap();
            let Some(p) = t.desc.params.iter().find(|p| p.key == key) else {
                differ.push(format!("{}/{key}: recorded, not in the catalog", t.desc.id));
                continue;
            };
            let pres = t.presentation(p);
            let mut check = |what: &str, ours: String, today: String| {
                if ours != today && !MEANT.contains(&(t.desc.id, key, what)) {
                    differ.push(format!("{}/{key} {what}: {ours}, today {today}", t.desc.id));
                }
            };
            check("label", pres.label.unwrap_or(p.label).into(), r["label"].as_str().unwrap().into());
            let widget = match r["widget"].as_str().unwrap() {
                "scrubby" | "drag_value" => "Scrub",
                "choice" => "Combo",
                // RGB or RGBA by what today's picker holds, never by ours.
                "colour" => match r["value"].as_array().map(Vec::len) {
                    Some(4) => "ColourRgba",
                    Some(3) => "ColourRgb",
                    n => panic!("{}/{key}: a colour of {n:?} channels", t.desc.id),
                },
                "checkbox" => "Toggle",
                "slider" => "Slider",
                other => other,
            };
            check("widget", format!("{:?}", pres.widget), widget.into());
            if let Some(dec) = r["decimals"].as_u64() {
                check("decimals", pres.decimals.to_string(), dec.to_string());
            }
            if let Some(speed) = r["speed"].as_f64() {
                check("step", pres.step.to_string(), (speed as f32).to_string());
            }
            check("suffix", pres.suffix.text(p.unit).into(), r["suffix"].as_str().unwrap().trim().into());
            if let Some(reset) = r["reset"].as_f64() {
                check("reset", pres.reset.unwrap_or(p.default).to_string(), (reset as f32).to_string());
            }
            compared += 1;
        }
        }
    }
    assert!(differ.is_empty(), "{} differences:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 0 || covered().is_empty());
}

/// System values kept as they are, hidden by name: never because they're
/// fixed (fixed means not keyframable, and a fixed parameter is still
/// edited by hand).
const HIDDEN: &[(&str, &str)] = &[
    // A title source's title (#2758): made and changed by the title op.
    ("klipp.fx.title_source", "title"),
    ("klipp.fx.surface_pin", "surface"),
    ("klipp.fx.transform", "focal_ref"),
    ("klipp.fx.film_grain", "seed"),
    ("klipp.fx.old_film", "seed"),
    ("klipp.fx.vhs", "seed"),
    ("klipp.fx.deflicker", "gain"),
    // The clip's time, set each frame (#989).
    ("klipp.fx.flicker", "time"),
    // The clip's time, set each frame (#1141).
    ("klipp.fx.particles", "time"),
    // The clip's time, set each frame (#2702).
    ("klipp.fx.displace", "time"),
    // The clip's time, set each frame (#2707).
    ("klipp.fx.reflection", "time"),
    // One source so far; the picture below is to come (#2707).
    ("klipp.fx.reflection", "ripple_from"),
    // The water as measured under it, and for what (#2795): Re-analyze
    // in the clip's menu measures it.
    ("klipp.fx.reflection", "water"),
    ("klipp.fx.reflection", "water_key"),
    // Set by a title's roll from its focus band, in the Title tab.
    ("klipp.fx.focus_blur", "radius"),
    ("klipp.fx.focus_blur", "vertical"),
    ("klipp.fx.focus_blur", "band"),
];

#[test]
fn hidden_is_not_fixed() {
    for e in presentation::params().filter(|e| e.pres.widget == Widget::Hidden) {
        let t = effect_types().find(|t| t.desc.id == e.type_id).unwrap();
        let p = t.desc.params.iter().find(|p| p.key == e.key).unwrap();
        assert!(p.derived || HIDDEN.contains(&(e.type_id, e.key)), "{}/{}: hidden, but neither derived nor a listed system value", e.type_id, e.key);
    }
    for (id, key) in HIDDEN {
        let t = effect_types().find(|t| t.desc.id == *id).unwrap();
        let p = t.desc.params.iter().find(|p| p.key == *key).unwrap();
        assert_eq!(t.presentation(p).widget, Widget::Hidden, "{id}/{key}");
    }
}

/// Every effect type has today's rows in its family's table (M3 step 0b
/// complete).
#[test]
fn every_effect_type_is_covered() {
    let missing: Vec<&str> = effect_types().filter(|t| !presentation::params().any(|e| e.type_id == t.desc.id) && !t.desc.params.is_empty()).map(|t| t.desc.id).collect();
    assert!(missing.is_empty(), "no rows for {missing:?}");
}

/// Every effect parameter the catalog can't describe (`Other`) is drawn by
/// a named editor, or is a listed system value (M3 step 0d).
#[test]
fn every_other_param_names_a_custom_editor() {
    let mut named = std::collections::BTreeSet::new();
    for t in effect_types() {
        for p in t.desc.params.iter().filter(|p| p.kind == ParamKind::Other) {
            match t.presentation(p).widget {
                Widget::Custom(id) if !id.is_empty() => {
                    named.insert(id);
                }
                // Worked out by the renderer each run, never edited.
                Widget::Hidden if p.derived || HIDDEN.contains(&(t.desc.id, p.key)) => {}
                w => panic!("{}/{}: {w:?}, not a named editor", t.desc.id, p.key),
            }
        }
    }
    assert_eq!(named.into_iter().collect::<Vec<_>>(), ["effect_lut_file", "effect_media", "frei0r_plugin"]);
}

/// The adapters drawing what rows can't, by type (M3 step 6): exactly the
/// breakdown's list.
#[test]
fn the_adapters_are_named() {
    let mut adapters: Vec<(&str, &str)> = presentation::types().filter_map(|e| e.pres.extra_ui.map(|a| (e.type_id, a))).collect();
    adapters.sort();
    assert_eq!(
        adapters,
        [
            ("klipp.fx.color_fade", "color_fade"),
            ("klipp.fx.corner_pin", "corner_pin"),
            ("klipp.fx.deflicker", "deflicker"),
            ("klipp.fx.focus_blur", "focus_blur"),
            ("klipp.fx.speed_ramp", "speed_ramp"),
            ("klipp.fx.temporal_denoise", "temporal_denoise"),
            ("klipp.fx.video_stabilization", "video_stabilization"),
        ]
    );
}

/// Today's choice labels, from the effects tab's combos
/// (`inspector/effects_tab.rs`), and IC5's new one: what each choice's
/// presentation must name, independently of the tables.
const CHOICE_LABELS: &[(&str, &str, &[&str])] = &[
    ("klipp.fx.color_fade", "mode", &["Fade From", "Fade To"]),
    ("klipp.fx.letterbox", "bar_color", &["Black", "White"]),
    ("klipp.fx.chroma_key", "spill_method", &["None", "Channel Clamp", "Hawk"]),
    ("klipp.fx.speed_ramp", "blend_mode", &["Nearest", "Linear", "Optical Flow"]),
    ("klipp.fx.radial_blur", "mode", &["Zoom", "Spin"]),
    ("klipp.fx.flicker", "target", &["Opacity", "Exposure", "Gamma"]),
    ("klipp.fx.displace", "direction", &["Both", "Across", "Up and Down"]),
    ("klipp.fx.reflection", "blend", &["Normal", "Screen", "Soft Light", "Overlay"]),
    ("klipp.fx.particles", "style", &["Dust", "Snow", "Rain", "Sparks", "Embers"]),
    ("klipp.fx.merge", "blend_mode", &["Normal", "Add", "Multiply", "Screen", "Overlay", "Soft Light", "Hard Light", "Difference", "Exclusion", "Colour Dodge", "Colour Burn", "Darken", "Lighten"]),
];

/// Today's sub-headers (the channel mixer's per-output groups).
const GROUPS: &[(&str, &str, &str)] = &[
    ("klipp.fx.channel_mixer", "red_red", "Red Output"),
    ("klipp.fx.channel_mixer", "red_green", "Red Output"),
    ("klipp.fx.channel_mixer", "red_blue", "Red Output"),
    ("klipp.fx.channel_mixer", "green_red", "Green Output"),
    ("klipp.fx.channel_mixer", "green_green", "Green Output"),
    ("klipp.fx.channel_mixer", "green_blue", "Green Output"),
    ("klipp.fx.channel_mixer", "blue_red", "Blue Output"),
    ("klipp.fx.channel_mixer", "blue_green", "Blue Output"),
    ("klipp.fx.channel_mixer", "blue_blue", "Blue Output"),
    ("klipp.fx.frei0r", "type_id", "Plug-in"),
    ("klipp.fx.frei0r", "params", "Plug-in"),
    ("klipp.fx.colour_space_transform", "input_gamut", "Input"),
    ("klipp.fx.colour_space_transform", "input_curve", "Input"),
    ("klipp.fx.colour_space_transform", "output_gamut", "Output"),
    ("klipp.fx.colour_space_transform", "output_curve", "Output"),
];

/// Every choice drawn as a combo names today's labels, and every group is
/// today's sub-header: held to the lists above, not to the tables.
#[test]
fn choice_labels_and_groups_are_todays() {
    for t in effect_types() {
        for p in t.desc.params {
            let pres = t.presentation(p);
            let want = CHOICE_LABELS.iter().find(|(id, k, _)| *id == t.desc.id && *k == p.key).map(|(_, _, l)| *l);
            if pres.widget == Widget::Combo || want.is_some() {
                assert_eq!(pres.labels, want, "{}/{}: choice labels", t.desc.id, p.key);
            }
            let group = GROUPS.iter().find(|(id, k, _)| *id == t.desc.id && *k == p.key).map(|(_, _, g)| *g);
            assert_eq!(pres.group, group, "{}/{}: group", t.desc.id, p.key);
        }
    }
}
