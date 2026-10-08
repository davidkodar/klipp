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
    let problems = presentation::problems(&types(), presentation::PARAMS, presentation::TYPES);
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

fn encode(v: f32) -> f32 {
    v.max(0.0).powf(1.0 / 2.4)
}
fn decode(v: f32) -> f32 {
    v.max(0.0).powf(2.4)
}
fn halve(v: f32) -> f32 {
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
    pres.widget = Widget::Toggle;
    assert!(presentation::param_problems(ct.desc.id, cp, &pres).iter().any(|e| e.contains("a toggle")), "a choice is never a toggle");
}

#[test]
fn an_entry_naming_no_param_is_refused() {
    let p = effect_types().next().unwrap().desc.params[0];
    let stray = [presentation::ParamEntry { type_id: "klipp.fx.nothing", key: "nothing", pres: Presentation::derived(&p) }];
    assert_eq!(presentation::problems(&types(), &stray, &[]).len(), 1);
    let twice = [presentation::ParamEntry { type_id: effect_types().next().unwrap().desc.id, key: p.key, pres: Presentation::derived(&p) }; 2];
    assert!(presentation::problems(&types(), &twice, &[]).iter().any(|e| e.contains("two entries")));
}
