//! The grade node registry describes every node kind, and describes it
//! the way the executor behaves (#1003, filled in by #1009).
//!
//! Every variant of `NodeKind`, read from its source, is either described
//! or on [`NOT_YET_DESCRIBED`]. A description is checked against the code:
//! its parameters against `NodeParams`' fields and their defaults, its
//! keyframable parameters against the names `NodeParams::apply_override`
//! takes (both ways), and its ports against the kind's port counts and
//! roles and against which corrections a matte can limit.

use std::collections::BTreeSet;

use klipp_effects::registry::{ParamKind, PortType};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::registry::node_types;
use klipp_nodes::{NodeKind, NodeParams, PortRole};
use serde_json::Value;

/// Node kinds with no description yet: none since #1009. A new kind goes
/// here until it is described.
const NOT_YET_DESCRIBED: &[&str] = &[];

/// The variant names of `pub enum NodeKind`, without the catch-all
/// `Unknown`, read from the source so a new kind cannot go undescribed.
fn declared_kinds() -> BTreeSet<String> {
    let src = include_str!("../src/kinds.rs");
    let start = src.find("pub enum NodeKind {").expect("the enum");
    let body = &src[start..];
    let end = body.find("\n}").expect("the enum's end");
    body[..end]
        .lines()
        .skip(1)
        .filter_map(|l| {
            let rest = l.strip_prefix("    ")?;
            if !rest.chars().next()?.is_ascii_uppercase() {
                return None;
            }
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            Some(name)
        })
        .filter(|n| n != "Unknown")
        .collect()
}

#[test]
fn every_kind_is_described_or_listed_as_not_yet() {
    let declared = declared_kinds();
    assert_eq!(declared.len(), 35, "the kind scan found {declared:?}");
    let described: BTreeSet<String> = node_types().map(|t| format!("{:?}", t.kind)).collect();
    let pending: BTreeSet<String> = NOT_YET_DESCRIBED.iter().map(|s| s.to_string()).collect();

    let both: Vec<_> = described.intersection(&pending).collect();
    assert!(both.is_empty(), "described, and still on NOT_YET_DESCRIBED (take them off): {both:?}");
    let known: BTreeSet<String> = described.union(&pending).cloned().collect();
    let missing: Vec<_> = declared.difference(&known).collect();
    assert!(missing.is_empty(), "kinds neither described nor listed as not yet: {missing:?}");
    let stale: Vec<_> = known.difference(&declared).collect();
    assert!(stale.is_empty(), "names that are not node kinds: {stale:?}");
}

#[test]
fn keyframable_parameters_are_ones_a_keyframe_track_can_drive() {
    for t in node_types() {
        for p in t.desc.params.iter().filter(|p| p.keyframable) {
            assert!(NodeParams::accepts(p.key), "{}: {} is keyframable but NodeParams::apply_override does not take it", t.desc.id, p.key);
        }
    }
}

#[test]
fn ports_agree_with_the_kinds_port_counts() {
    for t in node_types() {
        assert_eq!(t.desc.outputs.len(), t.kind.output_port_count() as usize, "{}: outputs", t.desc.id);
        // A correction's mask input (port 1, `with_gates_applied`) is not
        // in its port count today, so one extra input named "mask" is
        // allowed.
        let mask = t.desc.inputs.iter().any(|p| p.key == "mask") as usize;
        let counted = t.kind.input_port_count() as usize;
        assert!(
            t.desc.inputs.len() == counted || (mask == 1 && t.desc.inputs.len() == counted + 1),
            "{}: {} inputs described, the kind counts {counted}",
            t.desc.id,
            t.desc.inputs.len()
        );
    }
}

#[test]
fn ids_are_unique_grade_ids() {
    let mut ids = BTreeSet::new();
    for t in node_types() {
        assert!(t.desc.id.starts_with("klipp.grade."), "{}: a grade node's id starts with klipp.grade.", t.desc.id);
        assert!(ids.insert(t.desc.id), "{} is described twice", t.desc.id);
    }
}

/// Names `apply_override` takes that the executor never applies, so no
/// description carries them (#1040).
const TAKEN_BUT_UNUSED: &[&str] = &["qual_exposure", "qual_saturation", "qual_hue_shift", "qual_temperature"];

/// Every name `apply_override` takes, read from its match arms: the plain
/// names, the qualifier's and luma key's prefixed ones, and the warper's
/// indexed ones. Each is confirmed by `NodeParams::accepts`, so a misread
/// arm cannot pass.
fn override_names() -> BTreeSet<String> {
    let src = include_str!("../src/params.rs");
    let start = src.find("pub fn apply_override").expect("apply_override");
    let end = start + src[start..].find("pub fn accepts").expect("accepts");
    let mut prefix = "";
    let mut names = BTreeSet::new();
    for line in src[start..end].lines() {
        let t = line.trim_start();
        if let Some(i) = t.find("p if p.starts_with(\"") {
            let rest = &t[i + "p if p.starts_with(\"".len()..];
            prefix = if rest.starts_with("qual_") { "qual_" } else if rest.starts_with("luma_key_") { "luma_key_" } else { "" };
            continue;
        }
        let Some(rest) = t.strip_prefix('"') else { continue };
        let Some(close) = rest.find('"') else { continue };
        if !rest[close + 1..].trim_start().starts_with("=>") {
            continue;
        }
        let indent = line.len() - t.len();
        let name = if indent > 12 { format!("{prefix}{}", &rest[..close]) } else { rest[..close].to_string() };
        names.insert(name);
    }
    for i in 0..12 {
        names.insert(format!("warper_hue_{i}"));
        names.insert(format!("warper_sat_{i}"));
    }
    for n in &names {
        assert!(NodeParams::accepts(n), "{n} was read as a name apply_override takes, and it does not");
    }
    assert!(names.len() > 100, "the name scan found only {names:?}");
    names
}

#[test]
fn every_name_a_keyframe_track_can_drive_is_a_described_parameter() {
    let described: BTreeSet<&str> = node_types().flat_map(|t| t.desc.params.iter()).filter(|p| p.keyframable).map(|p| p.key).collect();
    for name in override_names() {
        if TAKEN_BUT_UNUSED.contains(&name.as_str()) {
            assert!(!described.contains(name.as_str()), "{name} is described: take it off TAKEN_BUT_UNUSED");
            continue;
        }
        assert!(described.contains(name.as_str()), "apply_override takes {name}, and no description makes it keyframable");
    }
}

/// `NodeParams` with every struct-valued field at its own default, as
/// saved: what a described `struct.field` and its default are read from.
fn saved_with_structs() -> serde_json::Map<String, Value> {
    let p = NodeParams {
        qualifier: Some(Default::default()),
        luma_key: Some(Default::default()),
        difference_key: Some(Default::default()),
        soft_clip: Some(Default::default()),
        grain: Some(Default::default()),
        warper_params: Some(Default::default()),
        color_match: Some(Default::default()),
        ..Default::default()
    };
    match serde_json::to_value(p).unwrap() {
        Value::Object(m) => m,
        other => panic!("NodeParams saved as {other}"),
    }
}

/// The field names of `pub struct NodeParams`, read from its source
/// (unset fields are not saved, so serde alone cannot list them).
fn node_params_fields() -> BTreeSet<String> {
    let src = include_str!("../src/params.rs");
    let start = src.find("pub struct NodeParams {").expect("NodeParams");
    let body = &src[start..start + src[start..].find("\n}").expect("its end")];
    body.lines()
        .filter_map(|l| l.trim_start().strip_prefix("pub ")?.split(':').next().map(str::to_string))
        .filter(|n| !n.contains(' '))
        .collect()
}

#[test]
fn parameters_are_node_params_fields_with_their_defaults() {
    let fields = node_params_fields();
    assert!(fields.len() > 60, "the field scan found only {fields:?}");
    let saved = saved_with_structs();
    for t in node_types() {
        for p in t.desc.params {
            let at = format!("{}.{}", t.desc.id, p.key);
            let (top, sub) = match p.field.split_once('.') {
                Some((a, b)) => (a, Some(b)),
                None => (p.field, None),
            };
            assert!(fields.contains(top), "{at}: {top} is not a field of NodeParams");
            let Some(sub) = sub else { continue };
            // An unset optional field inside the struct (a plate's path)
            // is not saved; only `Other` parameters are optional.
            let Some(value) = saved.get(top).and_then(|v| v.get(sub)) else {
                assert_eq!(p.kind, ParamKind::Other, "{at}: {top} has no field {sub}");
                continue;
            };
            let value = match (p.index, value) {
                (Some(i), Value::Array(a)) => &a[i],
                (Some(_), v) => panic!("{at}: indexed, but {v} is not an array"),
                (None, v) => v,
            };
            match (p.kind, value) {
                (ParamKind::Bool, Value::Bool(b)) => assert_eq!(p.default, *b as u8 as f32, "{at}: default"),
                (ParamKind::Float | ParamKind::Whole, Value::Number(n)) => {
                    assert!((p.default as f64 - n.as_f64().unwrap()).abs() < 1e-6, "{at}: default {} but the struct's is {n}", p.default)
                }
                (ParamKind::Choice(names), Value::String(s)) => {
                    let i = p.default as usize;
                    assert!(names.len() > i, "{at}: default {i} is past its choices");
                    assert_eq!(names[i].replace(['.', ' '], "").to_lowercase(), s.to_lowercase(), "{at}: default choice")
                }
                (ParamKind::Other, _) => {}
                (k, v) => panic!("{at}: described as {k:?}, saved as {v}"),
            }
        }
    }
}

#[test]
fn defaults_lie_on_their_sliders() {
    for t in node_types() {
        for p in t.desc.params {
            assert!(p.range.is_none(), "{}.{}: no grade parameter refuses a value", t.desc.id, p.key);
            if let Some((lo, hi)) = p.soft {
                assert!(lo < hi, "{}.{}: slider range", t.desc.id, p.key);
                assert!((lo..=hi).contains(&p.default), "{}.{}: default {} off its slider {lo}..{hi}", t.desc.id, p.key, p.default);
            }
        }
    }
}

#[test]
fn port_types_agree_with_the_kinds_port_roles() {
    for t in node_types() {
        for (i, port) in t.desc.outputs.iter().enumerate() {
            let matte = t.kind.output_port_role(i as u8) == PortRole::Matte;
            assert_eq!(port.ty == PortType::Matte, matte, "{}: output {}", t.desc.id, port.key);
        }
        for (i, port) in t.desc.inputs.iter().enumerate().filter(|(_, p)| p.key != "mask") {
            let matte = t.kind.input_port_role(i as u8) == PortRole::Matte;
            assert_eq!(port.ty == PortType::Matte, matte, "{}: input {}", t.desc.id, port.key);
        }
    }
}

/// A correction a matte limits has the optional `mask` input, and only
/// those: asked of `with_gates_applied` itself, with a matte wired to the
/// kind's port 1.
#[test]
fn a_mask_input_is_described_exactly_where_a_matte_limits_the_node() {
    for t in node_types() {
        if matches!(t.kind, NodeKind::Source | NodeKind::Output) {
            continue;
        }
        let node = |kind| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        let (src, n, matte) = (node(NodeKind::Source), node(t.kind), node(NodeKind::Mask));
        let edges = vec![
            Edge { from_node: src.id, from_port: 0, to_node: n.id, to_port: 0 },
            Edge { from_node: matte.id, from_port: 0, to_node: n.id, to_port: 1 },
        ];
        let g = NodeGraph { nodes: vec![src, n, matte], edges, ..Default::default() };
        let gated = g.with_gates_applied().is_some();
        let has_mask = t.desc.inputs.iter().any(|p| p.key == "mask" && p.ty == PortType::Matte && p.optional);
        assert_eq!(has_mask, gated, "{}: a matte on port 1 {} it", t.desc.id, if gated { "limits" } else { "does not limit" });
    }
}
