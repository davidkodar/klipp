//! The grade node registry describes every node kind, and describes it
//! the way the executor behaves (#1003, filled in by #1009).
//!
//! Every variant of `NodeKind`, read from its source, is either described
//! or on [`NOT_YET_DESCRIBED`]. A description is checked against the code:
//! its keyframable parameters against `NodeParams::accepts` (what a
//! keyframe track can drive today), and its ports against the kind's port
//! counts.

use std::collections::BTreeSet;

use klipp_nodes::registry::node_types;
use klipp_nodes::NodeParams;

/// Node kinds with no description yet: all of them until #1009.
const NOT_YET_DESCRIBED: &[&str] = &[
    "Source", "Output", "InputTransform", "Corrector", "Exposure", "Contrast", "Saturation", "ToneControls",
    "ColorWheels", "WheelSatBri", "RGBCurves", "HueSatCurves", "ColorBalance", "Qualifier", "LumaKey", "SoftClip",
    "Mask", "CombineMattes", "ExternalMatte", "DifferenceKey", "Layer", "Blend", "ApplyMatte", "LayerMixer", "Lut3D",
    "FilmGrain", "ColorMatch", "ColorWarper", "Denoise", "Compound", "RGBSplitter", "RGBCombiner", "PrimariesMatrix",
    "MaskCutout", "Reroute",
];

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
