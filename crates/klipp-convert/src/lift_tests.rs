use crate::lift::{auto_placement_index, lift_clip, ClipKind, LiftCtx, DRAWS_OUTSIDE, KEYFRAME_ALIASES};
use crate::to_v2::clip_to_v2;
use klipp_graph_format::lifted::{types as lt, ClipPicture, LiftNote};
use klipp_graph_format::{types, Graph, Node, ParamValue};
use serde_json::{json, Value};
use uuid::Uuid;

const CLIP: &str = "00000000-0000-0000-0000-00000000c11b";
const SRC: &str = "00000000-0000-0000-0000-000000000001";
const COR: &str = "00000000-0000-0000-0000-000000000002";
const KEY: &str = "00000000-0000-0000-0000-000000000003";
const LAY: &str = "00000000-0000-0000-0000-000000000004";
const OUT: &str = "00000000-0000-0000-0000-000000000005";
const SAT: &str = "00000000-0000-0000-0000-000000000006";

fn id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn gn(id: &str, kind: &str) -> Value {
    json!({"id": id, "kind": kind, "label": kind, "enabled": true})
}

fn edge(from: &str, fp: u8, to: &str, tp: u8) -> Value {
    json!({"from_node": from, "from_port": fp, "to_node": to, "to_port": tp})
}

fn lift(clip: Value, kind: ClipKind) -> (ClipPicture, Vec<LiftNote>) {
    let record = clip_to_v2(clip.as_object().unwrap()).unwrap();
    let lifted = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind }).unwrap();
    lifted.0.graph.check_lifted(false).unwrap_or_else(|e| panic!("not a valid lifted graph: {e}"));
    lifted
}

fn clip(extra: Value) -> Value {
    let mut c = json!({"id": CLIP});
    c.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    c
}

fn types_of(g: &Graph) -> Vec<&str> {
    g.nodes.iter().map(|n| n.type_id.as_str()).collect()
}

fn body(p: &ClipPicture) -> &Graph {
    &p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().group.as_ref().unwrap().graph
}

fn find<'a>(g: &'a Graph, s: &str) -> &'a Node {
    g.nodes.iter().find(|n| n.id == id(s)).unwrap()
}

fn has_edge(g: &Graph, from: Uuid, to: Uuid, port: &str) -> bool {
    g.edges.iter().any(|e| e.from.0 == from && e.to.0 == to && e.to.1.as_str() == port)
}

/// A grade with a gated corrector, a corrector whose gate has no picture
/// (port 1 wired, port 0 not), a layer with no background, and an output.
fn grade() -> Value {
    json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Corrector"), gn(KEY, "Qualifier"), gn(SAT, "Saturation"), gn(LAY, "Layer"), gn(OUT, "Output")],
        "edges": [
            edge(SRC, 0, COR, 0), edge(KEY, 0, COR, 1),
            edge(KEY, 0, SAT, 1),
            edge(COR, 0, LAY, 0),
            edge(LAY, 0, OUT, 0)
        ],
        "output_node_id": OUT
    })
}

#[test]
fn the_grade_group_decides_gates_first_then_makes_reads_explicit() {
    let (p, notes) = lift(clip(json!({"node_graph": grade()})), ClipKind::Media);
    let b = body(&p);
    // v1's Source and Output are the group's ends.
    assert_eq!(find(b, SRC).type_id.as_str(), lt::GROUP_INPUT);
    assert_eq!(find(b, OUT).type_id.as_str(), lt::GROUP_OUTPUT);
    // Both ports wired: a gate, alpha forced to one, the mask edge kept.
    assert!(find(b, COR).legacy_flags.as_ref().unwrap().alpha_one);
    assert!(has_edge(b, id(KEY), id(COR), "mask"));
    // Port 1 wired with port 0 empty: never a gate. The edge is parked on
    // the group, out of the graph, and reported.
    assert!(!has_edge(b, id(KEY), id(SAT), "mask"));
    assert!(find(b, SAT).legacy_flags.is_none());
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    assert_eq!(group.legacy_flags.as_ref().unwrap().parked_edges.len(), 1);
    assert!(notes.iter().any(|n| matches!(n, LiftNote::ParkedEdge { .. })));
    // Only then implicit reads: Saturation's empty primary and the layer's
    // empty background read the frame, now as edges from the input. The
    // parked gate didn't make Saturation's port 0 count as wired.
    assert!(has_edge(b, id(SRC), id(SAT), "in"));
    assert!(has_edge(b, id(SRC), id(LAY), "bg"));
    assert!(has_edge(b, id(COR), id(LAY), "fg"), "the wired foreground stays");
    assert!(!has_edge(b, id(SRC), id(LAY), "fg"));
    assert!(has_edge(b, id(SRC), id(KEY), "in"), "the qualifier's empty input reads the frame");
    // The layer is a merge with its legacy op.
    let lay = find(b, LAY);
    assert_eq!(lay.type_id.as_str(), lt::MERGE);
    assert_eq!(lay.params[&klipp_graph_format::ParamKey::from("op")], ParamValue::Text("layer_v1".into()));
    // The qualifier stays a legacy node with its v1 entry.
    assert_eq!(find(b, KEY).type_id.as_str(), types::LEGACY_NODE);
}

/// A wireless link counts as wiring, as `input_of` reads it.
#[test]
fn a_wireless_link_is_wiring() {
    let mut g = grade();
    g["nodes"][3]["input_links"] = json!(["plate"]);
    g["nodes"][1]["output_link"] = json!("plate");
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let b = body(&p);
    // Saturation's port 0 is now wired wirelessly, so its gate is real.
    assert!(find(b, SAT).legacy_flags.as_ref().unwrap().alpha_one);
    assert!(has_edge(b, id(KEY), id(SAT), "mask"));
    assert!(!has_edge(b, id(SRC), id(SAT), "in"), "no implicit read over a wireless one");
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::ParkedEdge { .. })));
}

#[test]
fn the_chain_runs_in_render_order() {
    let tnr = json!({"type": "temporal_denoise", "strength": 0.5, "threshold": 0.1});
    let blur = json!({"type": "gaussian_blur", "radius": 3.0});
    let (p, _) = lift(
        clip(json!({"node_graph": grade(), "effects": [{"plugin_name": "Glow", "enabled": true}], "builtin_effects": [blur.clone(), tnr]})),
        ClipKind::Media,
    );
    assert_eq!(
        types_of(&p.graph),
        [lt::SOURCE, lt::TEMPORAL_DENOISE, lt::GROUP, lt::SCENE_TO_DISPLAY, types::LEGACY_OFX, types::LEGACY_EFFECT, lt::PLACEMENT, lt::OUTPUT]
    );
    assert_eq!(p.graph.output, p.graph.nodes.last().unwrap().id);
    for (e, w) in p.graph.edges.iter().zip(p.graph.nodes.windows(2)) {
        assert_eq!((e.from.0, e.to.0), (w[0].id, w[1].id));
    }
    // No enabled Transform: placement is Auto, at the end.
    assert_eq!(p.graph.nodes[6].params[&klipp_graph_format::ParamKey::from("mode")], ParamValue::Enum(lt::PLACEMENT_AUTO));
}

#[test]
fn head_nodes_run_on_media_clips_only() {
    let tnr = |s: f64| json!({"type": "temporal_denoise", "strength": s, "threshold": 0.1});
    let ramp = json!({"type": "speed_ramp", "speed": 0.5, "blend_mode": 1, "ramp_curve": 0, "ramp_start": 0.0, "ramp_end": 1.0, "reverse": false, "freeze_at": 0.0, "freeze": false});
    let c = clip(json!({"builtin_effects": [tnr(0.0), tnr(0.4), tnr(0.8), ramp]}));
    let (p, notes) = lift(c.clone(), ClipKind::Media);
    let head: Vec<(&str, bool)> = p.graph.nodes[1..5].iter().map(|n| (n.type_id.as_str(), n.enabled)).collect();
    // The first enabled denoise is the clip's, at strength 0 too (today
    // it reads that one and stops: this clip is never denoised); the
    // others are kept off.
    assert_eq!(head, [(lt::TEMPORAL_DENOISE, true), (lt::TEMPORAL_DENOISE, false), (lt::TEMPORAL_DENOISE, false), (lt::FRAME_BLEND, true)]);
    assert_eq!(notes.iter().filter(|n| matches!(n, LiftNote::NeverRan { .. })).count(), 2);
    assert!(!types_of(&p.graph).contains(&types::LEGACY_SPEED_RAMP), "a speed ramp is no node");

    let (t, notes) = lift(c, ClipKind::Title);
    assert!(t.graph.nodes[1..5].iter().all(|n| !n.enabled), "on a title none of it ever ran");
    assert_eq!(notes.iter().filter(|n| matches!(n, LiftNote::NeverRan { .. })).count(), 4);

    // A frozen or non-blending ramp gives no frame blend; its unread fields
    // with values are reported.
    let frozen = json!({"type": "speed_ramp", "speed": 0.5, "blend_mode": 1, "ramp_curve": 2, "ramp_start": 0.0, "ramp_end": 1.0, "reverse": true, "freeze_at": 0.0, "freeze": true});
    let (f, notes) = lift(clip(json!({"builtin_effects": [frozen]})), ClipKind::Media);
    assert!(!types_of(&f.graph).contains(&lt::FRAME_BLEND));
    let dropped: Vec<&str> = notes.iter().filter_map(|n| match n { LiftNote::DroppedField { field, .. } => Some(field.as_str()), _ => None }).collect();
    assert_eq!(dropped, ["ramp_curve", "reverse"]);
}

#[test]
fn opacity_follows_todays_three_rules() {
    let op = |v: f64| json!({"type": "opacity", "value": v});
    let wiggle = json!({"type": "wiggle", "amplitude": 0.1, "frequency": 1.0});
    let looped = json!({"type": "loop", "period_secs": 1.0});
    let clip_keys = json!([{"time_secs": 0.0, "value": 0.2}]);

    // No Opacity effect: the clip's keys and modifiers, default 1.
    let (p, _) = lift(clip(json!({"opacity_keyframes": clip_keys.clone(), "param_modifiers": {"opacity": {"modifiers": [wiggle.clone()]}}})), ClipKind::Media);
    assert_eq!(p.opacity, 1.0);
    let a = p.opacity_anim.unwrap();
    assert_eq!((a.keys.len(), a.modifiers.unwrap()["modifiers"].as_array().unwrap().len()), (1, 1));

    // The last enabled effect has keys: those keys, its own modifiers.
    let (p, notes) = lift(
        clip(json!({
            "builtin_effects": [op(0.3), op(0.6)],
            "effect_keyframes": {"1:value": [{"time_secs": 0.0, "value": 0.9}], "0:value": [{"time_secs": 0.0, "value": 0.1}]},
            "opacity_keyframes": clip_keys.clone()
        })),
        ClipKind::Media,
    );
    assert_eq!(p.opacity, 0.6);
    assert_eq!(p.opacity_anim.as_ref().unwrap().keys, [json!({"time_secs": 0.0, "value": 0.9})]);
    assert!(notes.iter().any(|n| matches!(n, LiftNote::OrphanKeys { .. })), "the earlier effect's keys never ran");
    assert!(!types_of(&p.graph).contains(&types::LEGACY_OPACITY), "no Opacity effect is a node");

    // No keys of its own: the clip's keys and modifiers, then its value
    // modifiers; its time modifiers never ran.
    let (p, notes) = lift(
        clip(json!({
            "builtin_effects": [op(1.7)],
            "param_modifiers": {"opacity": {"modifiers": [wiggle.clone()]}, "effect:0:value": {"modifiers": [wiggle.clone(), looped]}},
            "opacity_keyframes": clip_keys
        })),
        ClipKind::Media,
    );
    assert_eq!(p.opacity, 1.7, "kept: the renderer clamps after the modifiers, not before");
    let mods = p.opacity_anim.unwrap().modifiers.unwrap();
    assert_eq!(mods["modifiers"].as_array().unwrap().len(), 2, "the clip's wiggle, then the effect's");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::OrphanKeys { keys, .. } if keys.iter().any(|k| k.ends_with("#time")))));

    // Still, it is clamped as the renderer clamps it.
    let (p, _) = lift(clip(json!({"builtin_effects": [op(1.7)]})), ClipKind::Media);
    assert_eq!((p.opacity, p.opacity_anim), (1.0, None));

    // Adjustment layers: the first effect's value, enabled or not.
    let mut first_off = clip(json!({"builtin_effects": [op(0.4), op(0.8)], "builtin_effects_enabled": [false, true]}));
    let (p, _) = lift(first_off.take(), ClipKind::Adjustment);
    assert_eq!(p.opacity, 0.4);
}

#[test]
fn placement_takes_the_three_rules_and_one_position() {
    let tf = |x: f64, tilt: f64| json!({"type": "transform", "position_x": x, "position_y": 0.0, "scale_x": 1.5, "scale_y": 1.5, "rotation": 0.0,
                                       "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": tilt, "rotation_y": 0.0, "perspective": 0.0});
    let blur = json!({"type": "gaussian_blur", "radius": 3.0});
    let (p, notes) = lift(
        clip(json!({"builtin_effects": [tf(0.1, 5.0), blur, tf(0.2, 9.0)], "builtin_effects_enabled": [false, true, true],
                    "transform_pos_x_keyframes": [{"time_secs": 0.0, "value": 0.0}], "param_modifiers": {"position_x": {"modifiers": []}}})),
        ClipKind::Media,
    );
    // Values from the first Transform of any state; tilt from the first
    // enabled.
    assert_eq!((p.placement.pos_x, p.placement.scale, p.placement.tilt_x), (Some(0.1), Some(1.5), Some(9.0)));
    assert!(p.placement_anim.contains_key(&klipp_graph_format::ParamKey::from("pos_x")));
    // Fixed at the first enabled Transform's place; the disabled first one
    // is kept off, for the record.
    assert_eq!(
        types_of(&p.graph),
        [lt::SOURCE, lt::GROUP, lt::SCENE_TO_DISPLAY, types::LEGACY_TRANSFORM, types::LEGACY_EFFECT, lt::PLACEMENT, lt::OUTPUT]
    );
    assert!(!p.graph.nodes[3].enabled);
    assert_eq!(p.graph.nodes[5].params[&klipp_graph_format::ParamKey::from("mode")], ParamValue::Enum(lt::PLACEMENT_FIXED));
    assert_eq!(notes.iter().filter(|n| matches!(n, LiftNote::ExtraTransform { .. })).count(), 1);

    // No Transform: nothing of its own, every field None.
    let (p, _) = lift(clip(json!({})), ClipKind::Media);
    assert!(p.placement.is_empty());
}

#[test]
fn effect_keyframes_are_keyed_by_parameter() {
    let key = json!({"type": "chroma_key", "soft_edge": 0.1});
    let (p, _) = lift(
        clip(json!({"builtin_effects": [key], "effect_keyframes": {"0:spill": [{"time_secs": 0.0, "value": 0.3}]},
                    "param_modifiers": {"effect:0:tolerance": {"modifiers": []}}})),
        ClipKind::Media,
    );
    let fx = p.graph.nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_EFFECT).unwrap();
    let names: Vec<&str> = fx.anim.keys().map(|k| k.as_str()).collect();
    assert_eq!(names, ["soft_edge", "tolerance"], "spill drives soft_edge, as the renderer reads it");
    assert_eq!(KEYFRAME_ALIASES, [("chroma_key", "spill", "soft_edge")]);
}

#[test]
fn what_claims_nothing_is_kept_on_the_source() {
    let (p, notes) = lift(clip(json!({"effect_keyframes": {"junk": []}, "param_modifiers": {"mystery": {"modifiers": []}, "volume": {}}})), ClipKind::Media);
    let orphans = &p.graph.nodes[0].legacy_flags.as_ref().unwrap().orphan_keys;
    assert_eq!(orphans.keys().collect::<Vec<_>>(), ["junk", "mystery"]);
    assert!(notes.iter().any(|n| matches!(n, LiftNote::OrphanKeys { node: None, .. })));
}

/// Lifting the same record twice gives the same picture.
#[test]
fn the_lift_is_deterministic() {
    let c = clip(json!({"node_graph": grade(), "builtin_effects": [json!({"type": "gaussian_blur", "radius": 3.0})]}));
    assert_eq!(serde_json::to_string(&lift(c.clone(), ClipKind::Media).0).unwrap(), serde_json::to_string(&lift(c, ClipKind::Media).0).unwrap());
}

/// The lift's copy of the keyframe aliases is the registry's: every
/// parameter keyframed under a name other than its field, as the
/// renderer reads it.
#[test]
fn the_keyframe_aliases_are_the_registrys() {
    let mut registry: Vec<(&str, &str, &str)> = klipp_effects::registry::effect_types()
        .flat_map(|t| t.desc.params.iter().filter(|p| p.index.is_none() && p.key != p.field).map(move |p| (t.tag, p.key, p.field)))
        .collect();
    registry.sort();
    let mut ours = KEYFRAME_ALIASES.to_vec();
    ours.sort();
    assert_eq!(ours, registry);
}

/// An Auto placement goes before the first effect that draws outside the
/// picture, over the chain with the track's effects spliced in.
#[test]
fn auto_placement_goes_before_the_first_effect_drawing_outside() {
    let blur = json!({"type": "gaussian_blur", "radius": 2.0});
    let shadow = json!({"type": "drop_shadow"});
    let glow = json!({"type": "outer_glow"});
    assert_eq!(auto_placement_index(&[&blur, &blur]), 2, "none draws outside: at the end");
    assert_eq!(auto_placement_index(&[&blur, &shadow, &glow]), 1);
    // A clip's own blur, then its track's shadow: the shadow, spliced after
    // the clip's effects, still runs after the transform.
    assert_eq!(auto_placement_index(&[&blur, &blur, &shadow]), 2);
}

/// The copied list is today's function, for every effect the registry has.
#[test]
fn the_draws_outside_list_is_todays() {
    for t in klipp_effects::registry::effect_types() {
        let sample = (t.sample)();
        assert_eq!(DRAWS_OUTSIDE.contains(&t.tag), klipp_effects::draws_outside_the_picture(&sample), "{}", t.tag);
    }
}

/// A transform whose two scales differ loses the vertical one, as the
/// renderer ignores it, and says so.
#[test]
fn a_dropped_vertical_scale_is_reported() {
    let tf = json!({"type": "transform", "position_x": 0.0, "position_y": 0.0, "scale_x": 1.0, "scale_y": 2.0, "rotation": 0.0,
                    "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": 0.0, "rotation_y": 0.0, "perspective": 0.0});
    let (p, notes) = lift(clip(json!({"builtin_effects": [tf]})), ClipKind::Media);
    assert_eq!(p.placement.scale, Some(1.0));
    assert!(notes.iter().any(|n| matches!(n, LiftNote::DroppedField { field, .. } if field == "scale_y")));
}

/// Odd edges an old grade graph keeps get today's reading, each with its
/// note, and the result is a valid lifted graph.
#[test]
fn odd_edges_read_as_today() {
    const GONE: &str = "00000000-0000-0000-0000-0000000000ff";
    const NOWHERE: &str = "00000000-0000-0000-0000-0000000000ee";
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Corrector"), gn(SAT, "Saturation")],
        "edges": [
            edge(SRC, 0, COR, 0),
            edge(SRC, 0, COR, 0),          // a second edge into the same port
            edge(COR, 0, NOWHERE, 0),      // to a node that isn't there
            edge(SRC, 0, SAT, 4),          // into a port Saturation doesn't read
            edge(GONE, 0, SAT, 0),         // from a node that isn't there, into a primary
            edge(SAT, 0, OUT, 0)           // into the output, which is missing
        ],
        "output_node_id": OUT
    });
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let b = body(&p);
    let why: Vec<&str> = notes.iter().filter_map(|n| match n { LiftNote::EdgeDropped { why, .. } => Some(why.as_str()), _ => None }).collect();
    assert_eq!(why, ["after another edge into the same port", "to a node that isn't there", "into a port the kind doesn't read"]);
    assert!(notes.iter().any(|n| matches!(n, LiftNote::EdgeRewired { .. })));
    assert!(notes.iter().any(|n| matches!(n, LiftNote::MissingOutput { .. })));
    // The missing output is made with its saved id; the frame now feeds
    // Saturation explicitly, as today's executor read it.
    assert_eq!(find(b, OUT).type_id.as_str(), lt::GROUP_OUTPUT);
    assert!(has_edge(b, id(SRC), id(SAT), "in"));
    assert!(has_edge(b, id(SAT), id(OUT), "in"));
}

#[test]
fn a_key_under_a_name_another_drives_is_kept_as_an_orphan() {
    // ChromaKey's `soft_edge` is animated by keys saved as `spill`; one
    // saved as `soft_edge` drove nothing, and must not become its
    // animation.
    let key = |v: f64| json!([{"time_secs": 0.0, "value": v}]);
    let ck = json!({"type": "chroma_key", "key_color": [0, 255, 0], "tolerance": 0.2, "soft_edge": 0.1, "spill_method": "channel_clamp", "spill_strength": 0.6});
    let (p, notes) = lift(
        clip(json!({
            "builtin_effects": [ck],
            "effect_keyframes": {"0:soft_edge": key(0.8), "0:spill": key(0.3)},
            "param_modifiers": {"effect:0:soft_edge": {"modifiers": []}}
        })),
        ClipKind::Media,
    );
    let n = p.graph.nodes.iter().find(|n| n.type_id.as_str() == types::LEGACY_EFFECT).unwrap();
    let a = &n.anim[&klipp_graph_format::ParamKey::new("soft_edge")];
    assert_eq!((a.keys.as_slice(), &a.modifiers), (key(0.3).as_array().unwrap().as_slice(), &None), "the spill keys, alone");
    let orphans: Vec<&String> = n.legacy_flags.as_ref().unwrap().orphan_keys.keys().collect();
    assert_eq!(orphans, ["0:soft_edge", "effect:0:soft_edge"]);
    assert!(notes.iter().any(|x| matches!(x, LiftNote::OrphanKeys { node: Some(id), .. } if *id == n.id)));
}

/// The inputs today fills from another when they're unwired become edges
/// from what they read: Blend's B from its A, RGBCombiner's G and B from
/// its R, ApplyMatte's pre-grade from the frame.
#[test]
fn filled_inputs_become_edges() {
    const BLEND: &str = "00000000-0000-0000-0000-000000000010";
    const RGB: &str = "00000000-0000-0000-0000-000000000011";
    const AM: &str = "00000000-0000-0000-0000-000000000012";
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Corrector"), gn(BLEND, "Blend"), gn(RGB, "RGBCombiner"), gn(AM, "ApplyMatte"), gn(KEY, "Qualifier"), gn(OUT, "Output")],
        "edges": [
            edge(SRC, 0, COR, 0),
            edge(COR, 0, BLEND, 0),
            edge(BLEND, 0, RGB, 0),
            edge(RGB, 0, AM, 0), edge(KEY, 0, AM, 1),
            edge(AM, 0, OUT, 0)
        ],
        "output_node_id": OUT
    });
    let (p, _) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let b = body(&p);
    assert!(has_edge(b, id(COR), id(BLEND), "b"), "B reads A");
    assert!(has_edge(b, id(BLEND), id(RGB), "g") && has_edge(b, id(BLEND), id(RGB), "b"), "G and B read R");
    assert!(has_edge(b, id(SRC), id(AM), "pregrade"), "the pre-grade reads the frame");
}
