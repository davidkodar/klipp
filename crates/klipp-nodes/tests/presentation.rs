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
    let problems = presentation::problems(&types(), grade::PARAMS, grade::TYPES);
    assert!(problems.is_empty(), "{problems:#?}");
    // Every entry is one this catalog's lookup returns.
    for e in grade::PARAMS {
        let t = node_types().find(|t| t.desc.id == e.type_id).unwrap();
        let p = t.desc.params.iter().find(|p| p.key == e.key).unwrap();
        assert_eq!(t.presentation(p), e.pres);
    }
}

#[test]
fn a_param_the_table_names_nothing_for_is_derived() {
    for t in node_types() {
        for p in t.desc.params.iter().filter(|p| !grade::PARAMS.iter().any(|e| e.type_id == t.desc.id && e.key == p.key)) {
            assert_eq!(t.presentation(p), Presentation::derived(p), "{}/{}", t.desc.id, p.key);
        }
        assert_eq!(t.type_presentation(), Default::default(), "{}", t.desc.id);
    }
}
