//! The grade nodes' presentation (M3 step 0a), held to the effects'
//! schema: every parameter of every grade node type is shown somehow, and
//! the table holds together.

use klipp_effects::registry::presentation::{self, Presentation, Widget};
use klipp_effects::registry::{ParamDesc, ParamKind};
use klipp_nodes::registry::{node_types, presentation as grade};

fn types() -> Vec<(&'static str, &'static [ParamDesc])> {
    node_types().map(|t| (t.desc.id, t.desc.params)).collect()
}

#[test]
fn every_param_has_presentation() {
    let mut params = 0;
    for t in node_types() {
        for p in t.desc.params {
            let pres = t.presentation(p);
            let problems = presentation::param_problems(t.desc.id, p, &pres);
            assert!(problems.is_empty(), "{problems:?}");
            if p.kind == ParamKind::Other {
                assert!(matches!(pres.widget, Widget::Custom(_)), "{}/{}: an Other param is drawn by an editor", t.desc.id, p.key);
            }
            params += 1;
        }
    }
    // Counted from the catalog, not taken from a list.
    let types = node_types().count();
    assert!(types >= 35 && params > types, "{types} types, {params} parameters");
}

#[test]
fn the_table_names_real_params_and_holds_together() {
    let problems = presentation::problems(&types(), &grade::params().copied().collect::<Vec<_>>(), grade::TYPES);
    assert!(problems.is_empty(), "{problems:#?}");
    // Every entry is one this catalog's lookup returns.
    for e in grade::params() {
        let t = node_types().find(|t| t.desc.id == e.type_id).unwrap();
        let p = t.desc.params.iter().find(|p| p.key == e.key).unwrap();
        assert_eq!(t.presentation(p), e.pres);
    }
}

#[test]
fn a_param_the_table_names_nothing_for_is_derived() {
    for t in node_types() {
        for p in t.desc.params.iter().filter(|p| !grade::params().any(|e| e.type_id == t.desc.id && e.key == p.key)) {
            assert_eq!(t.presentation(p), Presentation::derived(p), "{}/{}", t.desc.id, p.key);
        }
        assert_eq!(t.type_presentation(), Default::default(), "{}", t.desc.id);
    }
}

/// The catalog key a recorded row edits: the recorder names some rows by
/// their struct path or their editor's channel, not by the key.
fn key_of(kind: &str, recorded: &str) -> String {
    let base = recorded.split('/').next().unwrap();
    if let Some(rest) = base.strip_prefix("qualifier.") {
        return format!("qual_{rest}");
    }
    if let Some(rest) = base.strip_prefix("luma_key.") {
        return format!("luma_key_{rest}");
    }
    match base {
        // A wheel's row is its group; its parts are x, y and master.
        "lift" | "gamma" | "gain" | "offset" | "log_low" | "log_mid" | "log_high" => format!("{base}_x"),
        "warper_params" => "warper_hue_0".into(),
        _ if kind.ends_with("color_warper") && base == "warper_params" => "warper_hue_0".into(),
        _ => base.into(),
    }
}

/// Recorded rows this table doesn't present, each with why.
const NOT_PRESENTED: &[(&str, &str, &str)] = &[
    // The colour wheels node draws S and B sliders under each wheel, but
    // the node has no such parameters (they're the Corrector's and the
    // Wheel Saturation/Brightness node's): the old row edits nothing the
    // node stores (#1295).
    ("klipp.grade.color_wheels", "lift_sat", "not this kind's"),
    ("klipp.grade.color_wheels", "lift_bri", "not this kind's"),
    ("klipp.grade.color_wheels", "gamma_sat", "not this kind's"),
    ("klipp.grade.color_wheels", "gamma_bri", "not this kind's"),
    ("klipp.grade.color_wheels", "gain_sat", "not this kind's"),
    ("klipp.grade.color_wheels", "gain_bri", "not this kind's"),
    ("klipp.grade.color_wheels", "offset_sat", "not this kind's"),
    ("klipp.grade.color_wheels", "offset_bri", "not this kind's"),
    // The layer mixer's per-input rows are its editor's own.
    ("klipp.grade.layer_mixer", "layer_mixer.opacity", "the editor's"),
    ("klipp.grade.layer_mixer", "layer_mixer.blend_mode", "the editor's"),
    // The Corrector's Adjustments tab draws the denoise rows of the
    // grade's Denoise node beside its own.
    ("klipp.grade.corrector", "denoise_luma", "not this kind's"),
    ("klipp.grade.corrector", "denoise_chroma", "not this kind's"),
    ("klipp.grade.corrector", "denoise_radius", "not this kind's"),
];

/// Meant differences from the recorded rows, by (type, key, what).
const MEANT: &[(&str, &str, &str)] = &[
    // The Corrector's page settles each control once: its Global Mix is a
    // plain slider (today a gradient one, with nothing to grade over).
    ("klipp.grade.corrector", "global_mix", "widget"),
];

/// Rows shown where the recorded panel didn't show them, each with why.
const SHOWN_NOW: &[(&str, &str, &str)] = &[
    // The qualifier's mode (#895), new, and its HSL rows, which are the
    // HSL mode's rather than the switched-on key's: set before the key
    // is on, an edit turns it on.
    ("klipp.grade.qualifier", "qual_mode", "the qualifier's mode, #895"),
    ("klipp.grade.qualifier", "qual_hue_center", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_hue_width", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_hue_soft", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_sat_low", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_sat_high", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_sat_soft", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_lum_low", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_lum_high", "the HSL mode's, #895"),
    ("klipp.grade.qualifier", "qual_lum_soft", "the HSL mode's, #895"),
    // The matte finesse's denoise (#901), new.
    ("klipp.grade.qualifier", "qual_denoise", "the matte's denoise, #901"),
    // A tracked subject an external matte reads (#2758), new.
    ("klipp.grade.external_matte", "matte_subject", "the tracked subject, #2758"),
];

/// What the variant's stored values switch on.
fn switched_on(kind: &str, variant: &str) -> Vec<&'static str> {
    match (kind.trim_start_matches("klipp.grade."), variant) {
        ("qualifier", "enabled") => vec!["qual_enabled"],
        ("luma_key", "enabled") => vec!["luma_key_enabled"],
        ("soft_clip", "enabled") => vec!["soft_clip.enabled"],
        ("difference_key", "enabled") => vec!["difference_key.enabled"],
        ("film_grain", "enabled") => vec!["grain.enabled"],
        (_, "log" | "tab/Wheels/log") => vec!["wheels_log_mode"],
        _ => vec![],
    }
}

fn visible(pres: &Presentation, on: &[&str]) -> bool {
    match pres.visible_if {
        None => true,
        Some(klipp_effects::registry::presentation::Cond::ParamIs { key, value }) => (value != 0.0) == on.contains(&key),
    }
}

/// Every grade kind's rows are today's, as the old node inspector
/// recorded them (M3 step 1b's fixture), in each recorded state (a key
/// switched on, log wheels, a curve channel, a Corrector tab): the same
/// rows shown, in today's order, each with today's label, widget,
/// decimals, drag step, suffix and reset (a pivot's in its shown
/// encoding), but for the rows listed as not presented and the meant
/// differences.
#[test]
fn presentation_matches_the_old_rows() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../../klipp-ui/tests/fixtures/old_rows.json")).unwrap();
    let mut differ = Vec::new();
    let mut compared = 0;
    for case in fixture["cases"].as_array().unwrap().iter().filter(|c| c["kind"].as_str().unwrap().starts_with("klipp.grade.")) {
        let (kind, variant) = (case["kind"].as_str().unwrap(), case["variant"].as_str().unwrap());
        let t = node_types().find(|t| t.desc.id == kind).unwrap_or_else(|| panic!("{kind}: no such type"));
        let on = switched_on(kind, variant);
        let mut keys = Vec::new();
        for r in case["rows"].as_array().unwrap() {
            let recorded = r["parameter"].as_str().unwrap();
            if NOT_PRESENTED.iter().any(|(k, p, _)| *k == kind && *p == recorded) {
                continue;
            }
            let key = key_of(kind, recorded);
            let Some(p) = t.desc.params.iter().find(|p| p.key == key) else {
                differ.push(format!("{kind}/{recorded} [{variant}]: no parameter {key}"));
                continue;
            };
            let pres = t.presentation(p);
            if !visible(&pres, &on) {
                differ.push(format!("{kind}/{key} [{variant}]: shown today, hidden by its condition"));
            }
            keys.push(key.clone());
            let mut check = |what: &str, ours: String, today: String| {
                if ours != today && !MEANT.contains(&(kind, key.as_str(), what)) {
                    differ.push(format!("{kind}/{key} [{variant}] {what}: {ours}, today {today}"));
                }
            };
            let editor = matches!(pres.widget, Widget::Custom(_));
            // An editor draws its own labels and suffixes; a wheel's row is
            // its group.
            let label = match pres.widget {
                Widget::WheelPart(g) if r["widget"] == "colour_wheel" => g.to_owned(),
                _ => pres.label.unwrap_or(p.label).to_owned(),
            };
            if !editor {
                check("label", label, r["label"].as_str().unwrap().into());
            }
            let widget = match (r["widget"].as_str().unwrap(), pres.widget) {
                (_, Widget::Custom(_)) => format!("{:?}", pres.widget),
                ("grade_slider" | "inspector_slider" | "slider", _) => "Slider".into(),
                ("gradient_slider", Widget::Gradient(g)) => format!("Gradient({g:?})"),
                ("gradient_slider", _) => "Gradient".into(),
                ("hue_slider", _) => "HueRainbow".into(),
                ("colour_wheel" | "wheel_slider", Widget::WheelPart(g)) => format!("WheelPart({g:?})"),
                ("colour_wheel" | "wheel_slider", _) => "WheelPart".into(),
                ("checkbox", _) => "Toggle".into(),
                ("choice", _) => "Combo".into(),
                (other, _) => other.into(),
            };
            check("widget", format!("{:?}", pres.widget), widget);
            if let Some(dec) = r["decimals"].as_u64() {
                check("decimals", pres.decimals.to_string(), dec.to_string());
            }
            if let Some(speed) = r["speed"].as_f64() {
                check("step", pres.step.to_string(), (speed as f32).to_string());
            }
            if !editor {
                check("suffix", pres.suffix.text(p.unit).into(), r["suffix"].as_str().unwrap().trim().into());
            }
            if let Some(reset) = r["reset"].as_f64() {
                let ours = pres.reset.unwrap_or(p.default);
                let shown = pres.display.map_or(ours, |m| (m.to_shown)(ours, klipp_color::grade_model::Encoding::Bt1886));
                check("reset", shown.to_string(), (reset as f32).to_string());
            }
            compared += 1;
        }
        // Today's order: each row at or after the one before it.
        let rank = |k: &str| t.desc.params.iter().position(|p| p.key == k).map(|i| (t.presentation(&t.desc.params[i]).order, i));
        for w in keys.windows(2) {
            if w[0] != w[1] && rank(&w[0]) > rank(&w[1]) && !(kind.ends_with("corrector") && w[1] == "contrast") {
                differ.push(format!("{kind} [{variant}]: {} shown after {}, today before", w[1], w[0]));
            }
        }
        // And nothing shown that isn't today, but on a Corrector tab (each
        // shows a part of the page) and for kinds without rows today.
        if !variant.starts_with("tab/") && !keys.is_empty() {
            for p in t.desc.params {
                let pres = t.presentation(p);
                let ours = pres.widget != Widget::Hidden && visible(&pres, &on) && grade::params().any(|e| e.type_id == kind && e.key == p.key);
                let today = keys.iter().any(|k| *k == p.key) || matches!(pres.widget, Widget::WheelPart(_) | Widget::Custom(_)) && keys.iter().any(|k| t.desc.params.iter().find(|q| q.key == *k).map(|q| t.presentation(q).group == pres.group && t.presentation(q).widget == pres.widget).unwrap_or(false));
                if ours && !today && !SHOWN_NOW.iter().any(|(k, q, _)| *k == kind && *q == p.key) {
                    differ.push(format!("{kind}/{} [{variant}]: shown, not today", p.key));
                }
            }
        }
    }
    assert!(differ.is_empty(), "{} differences:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 100, "{compared} rows compared");
}

/// Every grade parameter the catalog can't describe (`Other`: curves, a
/// mask stack, a file) is drawn by a named editor (M3 step 0d).
#[test]
fn every_other_param_names_a_custom_editor() {
    let mut named = std::collections::BTreeSet::new();
    for t in node_types() {
        for p in t.desc.params.iter().filter(|p| p.kind == ParamKind::Other) {
            match t.presentation(p).widget {
                Widget::Custom(id) if !id.is_empty() => {
                    named.insert(id);
                }
                w => panic!("{}/{}: {w:?}, not a named editor", t.desc.id, p.key),
            }
        }
    }
    assert_eq!(named.into_iter().collect::<Vec<_>>(), ["camera_encoding", "compound_body", "curves", "huesat_curves", "key_mixer_inputs", "layer_mixer_inputs", "lut_file", "mask_stack", "matte_file", "plate_file"]);
}
