use crate::lift::{auto_placement_index, lift_clip, ClipKind, LiftCtx, DRAWS_OUTSIDE};
use crate::names::KEYFRAME_ALIASES;
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

/// The body's `frame` input: what an unwired input reads.
fn frame(g: &Graph) -> Uuid {
    g.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_FRAME).map(|n| n.id).expect("a frame input")
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
    let _ = group;
    let input = b.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_INPUT).unwrap();
    assert_eq!(input.legacy_flags.as_ref().unwrap().parked_edges.len(), 1, "kept on the body's input");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::ParkedEdge { .. })));
    // Only then implicit reads: Saturation's empty primary and the layer's
    // empty background read the frame, now as edges from the input. The
    // parked gate didn't make Saturation's port 0 count as wired.
    assert!(has_edge(b, frame(b), id(SAT), "in"));
    assert!(has_edge(b, frame(b), id(LAY), "bg"));
    assert!(has_edge(b, id(COR), id(LAY), "fg"), "the wired foreground stays");
    assert!(!has_edge(b, frame(b), id(LAY), "fg"));
    assert!(has_edge(b, frame(b), id(KEY), "in"), "the qualifier's empty input reads the frame");
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
    assert!(!has_edge(b, frame(b), id(SAT), "in"), "no implicit read over a wireless one");
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
    let ramp = json!({"type": "speed_ramp", "speed": 0.5, "blend_mode": 1});
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

    // A ramp that doesn't blend gives no frame blend (#1409: Speed and
    // Blend Mode are all a ramp keeps).
    let nearest = json!({"type": "speed_ramp", "speed": 0.5, "blend_mode": 0});
    let (f, _) = lift(clip(json!({"builtin_effects": [nearest]})), ClipKind::Media);
    assert!(!types_of(&f.graph).contains(&lt::FRAME_BLEND));
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
    // The earlier enabled one multiplies in with its own keys (#1386): kept
    // on, its keys on it, none orphaned.
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::OrphanKeys { .. })), "{notes:?}");
    // The anchor is kept off for the record (#1089): the property renders
    // it. Each entry keeps its slot and its stored enabled bit.
    let kept: Vec<&Node> = p.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OPACITY).collect();
    assert_eq!(kept.len(), 2);
    assert!(kept[0].enabled && kept[0].legacy_flags.as_ref().and_then(|f| f.was_enabled).is_none(), "the earlier one is on");
    assert!(kept[0].legacy.as_ref().unwrap().keyframes.as_ref().is_some_and(|k| k.get("0:value").is_some()), "with its own keys");
    assert!(!kept[1].enabled && kept[1].legacy_flags.as_ref().and_then(|f| f.was_enabled) == Some(true), "the anchor is kept off");
    assert_eq!(kept.iter().map(|n| n.legacy.as_ref().unwrap().slot).collect::<Vec<_>>(), [Some(0), Some(1)]);

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

    // An adjustment layer by the same rule (#1386): a disabled first
    // Opacity never counts, so [off 0.5, on 0.8] is 0.8, nothing multiplied.
    for kind in [ClipKind::Adjustment, ClipKind::Media] {
        let mut first_off = clip(json!({"builtin_effects": [op(0.5), op(0.8)], "builtin_effects_enabled": [false, true]}));
        let (p, _) = lift(first_off.take(), kind);
        assert_eq!(p.opacity, 0.8, "{kind:?}");
        let on: Vec<_> = p.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_OPACITY && n.enabled).collect();
        assert!(on.is_empty(), "{kind:?}: nothing else multiplies");
    }
}

#[test]
fn placement_takes_the_first_enabled_transform_and_one_position() {
    let tf = |x: f64, tilt: f64| json!({"type": "transform", "position_x": x, "position_y": 0.0, "scale_x": 1.5, "scale_y": 1.5, "rotation": 0.0,
                                       "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": tilt, "rotation_y": 0.0, "perspective": 0.0});
    let blur = json!({"type": "gaussian_blur", "radius": 3.0});
    let (p, notes) = lift(
        clip(json!({"builtin_effects": [tf(0.1, 5.0), blur, tf(0.2, 9.0)], "builtin_effects_enabled": [false, true, true],
                    "transform_pos_x_keyframes": [{"time_secs": 0.0, "value": 0.0}], "param_modifiers": {"position_x": {"modifiers": []}}})),
        ClipKind::Media,
    );
    // All placement values come from the first enabled Transform (#1207).
    assert_eq!((p.placement.pos_x, p.placement.scale, p.placement.tilt_x), (Some(0.2), Some(1.5), Some(9.0)));
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

/// A transform whose two scales differ keeps its aspect (height against
/// width) in the placement, and drops nothing (#1406); one whose scales
/// agree keeps the one scale and no aspect; a width of 0 keeps none.
#[test]
fn a_vertical_scale_apart_from_the_width_is_kept() {
    let tf = |y: f64| json!({"type": "transform", "position_x": 0.0, "position_y": 0.0, "scale_x": 1.0, "scale_y": y, "rotation": 0.0,
                    "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": 0.0, "rotation_y": 0.0, "perspective": 0.0});
    let (p, notes) = lift(clip(json!({"builtin_effects": [tf(2.0)]})), ClipKind::Media);
    assert_eq!((p.placement.scale, p.placement.aspect), (Some(1.0), Some(2.0)));
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::DroppedField { field, .. } if field == "scale_y")), "{notes:?}");
    let (p, _) = lift(clip(json!({"builtin_effects": [tf(1.0)]})), ClipKind::Media);
    assert_eq!((p.placement.scale, p.placement.aspect), (Some(1.0), None));
    assert_eq!(crate::names::aspect(Some(0.0), Some(0.5)), None, "a width of 0 has no aspect");
    assert_eq!(crate::names::aspect(Some(2.0), Some(0.5)), Some(0.25));
}

/// A negative scale is a flip (#2705): the placement keeps the flip and
/// the size, and the aspect between the sizes.
#[test]
fn a_negative_scale_is_kept_as_a_flip() {
    let tf = |x: f64, y: f64| json!({"type": "transform", "position_x": 0.0, "position_y": 0.0, "scale_x": x, "scale_y": y, "rotation": 0.0,
                    "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": 0.0, "rotation_y": 0.0, "perspective": 0.0});
    let (p, _) = lift(clip(json!({"builtin_effects": [tf(-0.5, 1.0)]})), ClipKind::Media);
    let pl = &p.placement;
    assert_eq!((pl.flip_x, pl.flip_y, pl.scale, pl.aspect), (true, false, Some(0.5), Some(2.0)));
    assert_eq!(pl.scales(), (-0.5, 1.0), "drawn as it was given");
    let (p, _) = lift(clip(json!({"builtin_effects": [tf(0.8, -0.8)]})), ClipKind::Media);
    assert_eq!((p.placement.flip_x, p.placement.flip_y, p.placement.aspect), (false, true, None));
    assert_eq!(p.placement.scales(), (0.8, -0.8));
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
    assert!(has_edge(b, frame(b), id(SAT), "in"));
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
    assert!(has_edge(b, frame(b), id(AM), "pregrade"), "the pre-grade reads the frame");
}

/// Reads of the primary's picture become edges from what feeds the
/// primary, wired or wireless; ApplyMatte's empty ungraded input reads the
/// frame.
#[test]
fn reads_of_the_primary_become_edges_from_its_feed() {
    const EXP: &str = "00000000-0000-0000-0000-000000000010";
    const BLD: &str = "00000000-0000-0000-0000-000000000011";
    const CMB: &str = "00000000-0000-0000-0000-000000000012";
    const APM: &str = "00000000-0000-0000-0000-000000000013";
    let mut exp = gn(EXP, "Exposure");
    exp["output_link"] = json!("graded");
    let mut cmb = gn(CMB, "RGBCombiner");
    cmb["input_links"] = json!(["graded"]);
    let g = json!({
        "nodes": [gn(SRC, "Source"), exp, gn(KEY, "Qualifier"), gn(BLD, "Blend"), cmb, gn(APM, "ApplyMatte"), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, EXP, 0), edge(EXP, 0, BLD, 0), edge(EXP, 0, APM, 0), edge(KEY, 0, APM, 1), edge(APM, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let (p, _) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let b = body(&p);
    let key = |kind: &str, port: u8| klipp_graph_format::ports::input_key(kind, port);
    assert!(has_edge(b, id(EXP), id(BLD), key("Blend", 1).as_str()), "Blend's B reads A");
    for port in [1, 2] {
        assert!(has_edge(b, id(EXP), id(CMB), key("RGBCombiner", port).as_str()), "the combiner's {port} reads its wireless primary");
    }
    assert!(!b.edges.iter().any(|e| e.to.0 == id(CMB) && e.to.1 == key("RGBCombiner", 0)), "the wireless primary stays wireless");
    assert!(has_edge(b, frame(b), id(APM), key("ApplyMatte", 2).as_str()), "ApplyMatte's ungraded input reads the frame");
}

#[test]
fn every_clip_kind_the_app_writes_is_known() {
    for (tag, kind) in [
        ("media", ClipKind::Media),
        ("title", ClipKind::Title),
        ("title_v2", ClipKind::Title),
        ("vector_title", ClipKind::Title),
        ("generator", ClipKind::Generator),
        ("sequence", ClipKind::Compound),
        ("precomposition", ClipKind::Precomposition),
        ("template_precomposition", ClipKind::Precomposition),
        ("adjustment_layer", ClipKind::Adjustment),
        ("gap", ClipKind::Gap),
    ] {
        assert_eq!(ClipKind::from_json(Some(&json!({"type": tag}))), Ok(kind), "{tag}");
    }
}

/// Explicit Source edges stay on the group's input; only implicit reads
/// take the frame, and the clip feeds the frame what enters the grade.
#[test]
fn the_frame_carries_only_implicit_reads() {
    let (p, _) = lift(clip(json!({"node_graph": grade(), "builtin_effects": [{"type": "temporal_denoise", "strength": 0.5, "threshold": 0.1}]})), ClipKind::Media);
    let b = body(&p);
    assert!(has_edge(b, id(SRC), id(COR), "in"), "the saved Source edge stays on the input");
    assert!(!has_edge(b, frame(b), id(COR), "in"));
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    let tnr = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::TEMPORAL_DENOISE).unwrap();
    assert!(has_edge(&p.graph, tnr.id, group.id, "frame"), "the frame is the picture entering the grade, after the head");
}

/// Two saved nodes sharing an id, one with a port-1 edge and no port-0
/// edge, used to queue the same parked edge for removal twice and panic.
/// The lift refuses the repeated id before anything: a stop for that
/// project, never an abort of the run.
#[test]
fn repeated_ids_with_a_parked_edge_stop_instead_of_panicking() {
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Corrector"), gn(COR, "Corrector"), gn(KEY, "Qualifier"), gn(OUT, "Output")],
        "edges": [edge(KEY, 0, COR, 1), edge(COR, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g})).as_object().unwrap()).unwrap();
    let r = std::panic::catch_unwind(|| lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }));
    assert!(r.expect("no panic").unwrap_err().contains("repeats node"));
}

/// A grade graph that loops never ran: today's ordering refuses it and the
/// picture passes through. The lift keeps it as saved in a group that is
/// off, which renders the same, and says so.
#[test]
fn a_looping_grade_is_kept_off() {
    let mut exp = gn(COR, "Corrector");
    exp["input_links"] = json!([null, "loop"]);
    let mut sat = gn(SAT, "Saturation");
    sat["output_link"] = json!("loop");
    let g = json!({
        "nodes": [gn(SRC, "Source"), exp, sat, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, SAT, 0), edge(SAT, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    assert!(!group.enabled, "off, as it never ran");
    assert!(group.group.as_ref().unwrap().graph.has_cycle(), "kept as saved");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::NeverRan { node, .. } if *node == group.id)));
}

/// A wireless link on a port that also has a wire is no dependency, as
/// today's ordering has it: the wire wins, so no loop, and the grade runs.
#[test]
fn a_wired_port_ignores_its_wireless_link() {
    let mut a = gn(COR, "Exposure");
    a["input_links"] = json!(["bus"]);
    let mut b = gn(SAT, "Saturation");
    b["output_link"] = json!("bus");
    let g = json!({
        "nodes": [gn(SRC, "Source"), a, b, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, SAT, 0), edge(SAT, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let typed: klipp_nodes::NodeGraph = serde_json::from_value(g.clone()).unwrap();
    assert!(typed.topo_sort().is_ok(), "today orders it");
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    assert!(group.enabled, "it runs");
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::NeverRan { .. })));
}

/// A wire into a port the kind doesn't read overrides a wireless link on
/// that port, so today's ordering never counted the link. Dropping the
/// wire drops the link too, or it would become a loop it never was.
#[test]
fn an_overridden_link_goes_with_its_dropped_wire() {
    // A LUT reads one input; its port 1 is not read. B sends "bus".
    let mut a = gn(COR, "Lut3D");
    a["input_links"] = json!([null, "bus"]);
    let mut b = gn(SAT, "Saturation");
    b["output_link"] = json!("bus");
    let g = json!({
        "nodes": [gn(SRC, "Source"), a, b, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(SRC, 0, COR, 1), edge(COR, 0, SAT, 0), edge(SAT, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let typed: klipp_nodes::NodeGraph = serde_json::from_value(g.clone()).unwrap();
    assert!(typed.topo_sort().is_ok(), "today orders it: the wire overrides the link");
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    assert!(group.enabled, "no loop, so it runs");
    assert!(find(body(&p), COR).wireless.input_links.is_empty(), "the overridden link went with its wire");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::EdgeDropped { why, .. } if why.contains("wireless"))));
}

/// A loop through an unread port: S -> A -> B -> O, plus a wire from B into A's port 9.
/// A's kind never reads port 9, but today's ordering counts every drawn
/// edge, so it refuses the graph and the grade never ran.
fn looping_through_an_unread_port() -> Value {
    json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Exposure"), gn(SAT, "Saturation"), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, SAT, 0), edge(SAT, 0, OUT, 0), edge(SAT, 0, COR, 9)],
        "output_node_id": OUT
    })
}

/// Whether a lifted clip's grade group is on.
fn grade_runs(g: &Value) -> bool {
    let (p, _) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().enabled
}

/// A loop only an edge the lift drops makes still never ran: the grade
/// stays off, decided before anything is dropped.
#[test]
fn a_loop_through_an_edge_the_lift_drops_stays_off() {
    let g = looping_through_an_unread_port();
    let typed: klipp_nodes::NodeGraph = serde_json::from_value(g.clone()).unwrap();
    assert!(typed.topo_sort().is_err(), "today refuses it");
    assert!(!grade_runs(&g), "so it stays off");
    // Without the unread wire, it runs.
    let mut ordered = g.clone();
    ordered["edges"].as_array_mut().unwrap().pop();
    assert!(serde_json::from_value::<klipp_nodes::NodeGraph>(ordered.clone()).unwrap().topo_sort().is_ok());
    assert!(grade_runs(&ordered));
}

/// A grade saved switched off is its Output wired into its own port 1, a
/// loop today's ordering refuses. That port is the Output's alpha (#898):
/// the grade still lifts off, and the marker is dropped, never read as a
/// matte that cuts the picture.
#[test]
fn a_grade_saved_off_lifts_off_with_no_alpha() {
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Exposure"), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, OUT, 0), edge(OUT, 0, OUT, 1)],
        "output_node_id": OUT
    });
    assert!(!grade_runs(&g), "off, as saved");
    let (p, notes) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    assert!(!body(&p).edges.iter().any(|e| e.to.1.as_str() == "alpha"), "no alpha wired");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::EdgeDropped { why, .. } if why.contains("itself"))));
}

/// An edge from a node that isn't there holds its target back forever in
/// today's ordering, so that grade never ran either.
#[test]
fn an_edge_from_a_missing_node_means_the_grade_never_ran() {
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Exposure"), gn(OUT, "Output")],
        "edges": [edge("00000000-0000-0000-0000-00000000dead", 0, COR, 0), edge(COR, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let typed: klipp_nodes::NodeGraph = serde_json::from_value(g.clone()).unwrap();
    assert_eq!(typed.topo_sort().is_err(), !grade_runs(&g), "the lift agrees with today");
}

/// A bare grade store (a group stage, a track or timeline grade, a
/// version) that loops the same way stops, as any looping store does,
/// rather than start rendering.
#[test]
fn a_bare_store_looping_through_a_dropped_edge_stops() {
    let group = json!({"id": CLIP, "pre": looping_through_an_unread_port()});
    let (pre, _) = crate::to_v2::stages_to_v2(group.as_object().unwrap()).unwrap();
    let r = crate::lift::lift_grade_only(&pre.unwrap(), false);
    assert_eq!(r.unwrap_err(), "a grade graph that loops, which today's renderer never ran");
}

/// Every edge the lift takes out of a grade, and every wireless link that
/// goes with one, comes back where it stood (#1089): a parked edge, one to
/// a missing node, one into an unread port with the wireless link it
/// overrode, a second into one port, one from a missing node the lift
/// reads from the frame, and a port the lift fills from another. As a
/// clip's grade and as a store standing alone.
#[test]
fn delift_puts_back_every_edge_the_lift_took_out() {
    const BLEND: &str = "00000000-0000-0000-0000-000000000007";
    const DEAD: &str = "00000000-0000-0000-0000-00000000dead";
    const GONE: &str = "00000000-0000-0000-0000-00000000600e";
    let mut cor = gn(COR, "Corrector");
    cor["output_link"] = json!("bus");
    let mut blend = gn(BLEND, "Blend");
    blend["input_links"] = json!([null, null, null, null, null, null, null, null, null, "bus"]);
    let g = json!({
        "nodes": [gn(SRC, "Source"), cor, gn(SAT, "Saturation"), blend, gn(OUT, "Output")],
        "edges": [
            edge(SRC, 0, SAT, 0),
            edge(SAT, 0, COR, 1),
            edge(SAT, 0, DEAD, 0),
            edge(SAT, 0, BLEND, 9),
            edge(SAT, 0, BLEND, 0),
            edge(GONE, 0, OUT, 0),
            edge(BLEND, 0, OUT, 0)
        ],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g.clone()})).as_object().unwrap()).unwrap();
    let (p, notes) = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }).unwrap();
    for want in ["parked_edge", "edge_dropped", "edge_rewired"] {
        assert!(notes.iter().any(|n| serde_json::to_value(n).unwrap()["note"] == want), "the case reaches {want}");
    }
    let input = body(&p).nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_INPUT).unwrap();
    let f = input.legacy_flags.as_ref().unwrap();
    assert_eq!((f.parked_edges.len(), f.dropped_edges.len(), f.rewired.len(), f.filled.len(), f.dropped_wireless.len()), (1, 3, 1, 1, 1));
    let back = crate::delift::clip(&p).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&record).unwrap(), "delift(lift(r)) == r");

    let seq = json!({"id": "00000000-0000-0000-0000-000000000501", "timeline_graph": g});
    let structural = crate::to_v2::timeline_to_v2(seq.as_object().unwrap()).unwrap().unwrap();
    let b = crate::lift::lift_grade_only(&structural, false).unwrap();
    let back = crate::delift::grade_only(&b, structural.nodes[0].id).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&structural).unwrap(), "and standing alone");
}

/// A matte reaching a correction's port 1 by a wireless link, with the
/// picture port unwired, was never a gate: the link is parked like a drawn
/// port-1 edge, `delift` puts it back, and the lifted node carries no mask.
#[test]
fn a_wireless_matte_on_an_ungated_correction_is_parked() {
    let mut cor = gn(COR, "Corrector");
    cor["input_links"] = json!([null, "matte"]);
    let mut key = gn(KEY, "LumaKey");
    key["output_link"] = json!("matte");
    let g = json!({
        "nodes": [gn(SRC, "Source"), key, cor, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, KEY, 0), edge(COR, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g})).as_object().unwrap()).unwrap();
    let (p, _) = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }).unwrap();
    p.graph.check_lifted(false).unwrap();
    let b = body(&p);
    let c = b.nodes.iter().find(|n| n.id == id(COR)).unwrap();
    assert!(c.wireless.input_links.is_empty(), "the link is parked, not live");
    assert!(!c.legacy_flags.as_ref().is_some_and(|f| f.alpha_one), "and the correction isn't gated");
    let input = b.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP_INPUT).unwrap();
    assert_eq!(input.legacy_flags.as_ref().unwrap().parked_links.len(), 1);
    let back = crate::delift::clip(&p).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&record).unwrap(), "delift puts it back");
}

/// A mask on a correction without the gate is refused, drawn or wireless:
/// it would read as gated where today it isn't.
#[test]
fn check_lifted_refuses_a_mask_without_the_gate() {
    let mut cor = gn(COR, "Corrector");
    cor["input_links"] = json!([null, "matte"]);
    let mut key = gn(KEY, "LumaKey");
    key["output_link"] = json!("matte");
    // Gated as saved: picture and matte both wired.
    let g = json!({
        "nodes": [gn(SRC, "Source"), key, cor, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, KEY, 0), edge(SRC, 0, COR, 0), edge(COR, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let (mut p, _) = lift(clip(json!({"node_graph": g})), ClipKind::Media);
    let group = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    let b = &mut group.group.as_mut().unwrap().graph;
    // Take the gate off: the wireless mask is refused.
    let c = b.nodes.iter_mut().find(|n| n.id == id(COR)).unwrap();
    c.legacy_flags = None;
    let err = p.graph.check_lifted(false).unwrap_err();
    assert!(matches!(err, klipp_graph_format::lifted::LiftedError::MaskWithoutGate(n) if n == id(COR)), "{err:?}");
    // A drawn mask edge without the gate is refused the same way.
    let group = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    let b = &mut group.group.as_mut().unwrap().graph;
    b.nodes.iter_mut().find(|n| n.id == id(COR)).unwrap().wireless.input_links.clear();
    b.edges.push(klipp_graph_format::Edge { from: (id(KEY), klipp_graph_format::ports::output_key("LumaKey", 0)), to: (id(COR), "mask".into()) });
    let err = p.graph.check_lifted(false).unwrap_err();
    assert!(matches!(err, klipp_graph_format::lifted::LiftedError::MaskWithoutGate(n) if n == id(COR)), "{err:?}");
}

/// A grade with a repeated node id doesn't panic the lift: the lift
/// refuses it, as a clip's grade and standing alone, so one project stops
/// rather than the run (#1102 review).
#[test]
fn a_repeated_node_id_stops_the_project_without_a_panic() {
    let g = json!({
        "nodes": [gn(SRC, "Source"), gn(SRC, "Source"), gn(COR, "Exposure"), gn(SAT, "Saturation"), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, SAT, 0), edge(SAT, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g.clone()})).as_object().unwrap()).unwrap();
    let r = std::panic::catch_unwind(|| lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }));
    let err = r.expect("no panic").expect_err("a lift error, so the project stops");
    assert!(err.contains("repeats node"), "{err}");
    // Standing alone, the same graph stops the same way.
    let seq = json!({"id": "00000000-0000-0000-0000-000000000501", "timeline_graph": g});
    let structural = crate::to_v2::timeline_to_v2(seq.as_object().unwrap()).unwrap().unwrap();
    let r = std::panic::catch_unwind(|| crate::lift::lift_grade_only(&structural, false));
    assert!(r.expect("no panic").unwrap_err().contains("repeats node"));
}

/// A grade group publishing a port can't be kept by today's grade model:
/// delift refuses it, naming the ports, rather than dropping them, so a
/// file holding one is never loaded and saved back without it (#1183).
#[test]
fn a_published_port_is_refused_not_dropped() {
    let record = clip_to_v2(clip(json!({"node_graph": json!({
        "nodes": [gn(SRC, "Source"), gn(COR, "Corrector"), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, COR, 0), edge(COR, 0, OUT, 0)],
        "output_node_id": OUT
    })})).as_object().unwrap()).unwrap();
    let (mut p, _) = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }).unwrap();
    assert!(crate::delift::clip(&p).is_ok(), "without one it reads");
    let group = p.graph.nodes.iter_mut().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    let body = group.group.as_mut().unwrap();
    let child = body.graph.nodes[0].id;
    body.interface.push(klipp_graph_format::GroupPort {
        key: "matte_out".into(),
        direction: klipp_graph_format::PortDirection::Output,
        ty: klipp_graph_format::PortType::Matte,
        binding: klipp_graph_format::PortBinding { node: child, port: "out".into() },
        label: String::new(),
    });
    let err = crate::delift::clip(&p).unwrap_err();
    assert!(err.contains("publishes ports (matte_out)"), "{err}");
    assert!(crate::unlift::clip(&p).is_err(), "and loading it is refused the same way");
}

/// A grade compound, two deep, lifts as a group within a group, each body
/// its subgraph lifted as a grade body is, each `frame` fed what feeds its
/// `in`; nothing of a subgraph stays in a payload, and `delift` gives the
/// record back (#1391, picture revision 4).
#[test]
fn a_grade_compound_lifts_as_a_group_and_back() {
    const CMP: &str = "00000000-0000-0000-0000-0000000000c1";
    const INNER: &str = "00000000-0000-0000-0000-0000000000c2";
    const ISRC: &str = "00000000-0000-0000-0000-0000000000a1";
    const IOUT: &str = "00000000-0000-0000-0000-0000000000a2";
    const JSRC: &str = "00000000-0000-0000-0000-0000000000b1";
    const JOUT: &str = "00000000-0000-0000-0000-0000000000b2";
    let compound = |at: &str, sub: Value| {
        let mut n = gn(at, "Compound");
        n["params"] = json!({ "compound_subgraph": sub });
        n
    };
    let innermost = json!({
        "nodes": [gn(JSRC, "Source"), gn(SAT, "Saturation"), gn(JOUT, "Output")],
        "edges": [edge(JSRC, 0, SAT, 0), edge(SAT, 0, JOUT, 0)],
        "output_node_id": JOUT
    });
    let mut cor = gn(COR, "Corrector");
    cor["params"] = json!({ "exposure": 0.4 });
    let middle = json!({
        "nodes": [gn(ISRC, "Source"), cor, compound(INNER, innermost), gn(IOUT, "Output")],
        "edges": [edge(ISRC, 0, COR, 0), edge(COR, 0, INNER, 0), edge(INNER, 0, IOUT, 0)],
        "output_node_id": IOUT
    });
    let g = json!({
        "nodes": [gn(SRC, "Source"), compound(CMP, middle), gn(OUT, "Output")],
        "edges": [edge(SRC, 0, CMP, 0), edge(CMP, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g})).as_object().unwrap()).unwrap();
    let (p, _) = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }).unwrap();
    p.graph.check_lifted(false).unwrap_or_else(|e| panic!("not a valid lifted graph: {e}"));
    let outer = find(body(&p), CMP);
    assert_eq!(outer.type_id.as_str(), lt::GROUP);
    assert!(outer.enabled);
    assert!(has_edge(body(&p), id(SRC), id(CMP), "frame"), "its frame is what feeds its in");
    let mid = &outer.group.as_ref().unwrap().graph;
    assert_eq!(find(mid, COR).legacy.as_ref().unwrap().payload["params"]["exposure"], json!(0.4));
    let inner = find(mid, INNER);
    assert_eq!(inner.type_id.as_str(), lt::GROUP);
    assert!(has_edge(mid, id(COR), id(INNER), "frame"));
    assert_eq!(types_of(&inner.group.as_ref().unwrap().graph).iter().filter(|t| **t == lt::GROUP_FRAME).count(), 1);
    assert!(!serde_json::to_string(&p).unwrap().contains("compound_subgraph"), "no subgraph stays a payload");
    let back = crate::delift::clip(&p).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&record).unwrap(), "delift(lift(r)) == r");
}

/// A compound whose subgraph loops passed its input through today: its
/// group is kept off, and `delift` gives its stored enabled bit back.
#[test]
fn a_compound_that_loops_is_kept_off() {
    const CMP: &str = "00000000-0000-0000-0000-0000000000c1";
    const ISRC: &str = "00000000-0000-0000-0000-0000000000a1";
    const IOUT: &str = "00000000-0000-0000-0000-0000000000a2";
    let mut cor = gn(COR, "Corrector");
    cor["input_links"] = json!([null, "loop"]);
    let mut sat = gn(SAT, "Saturation");
    sat["output_link"] = json!("loop");
    let mut cmp = gn(CMP, "Compound");
    cmp["params"] = json!({ "compound_subgraph": {
        "nodes": [gn(ISRC, "Source"), cor, sat, gn(IOUT, "Output")],
        "edges": [edge(ISRC, 0, COR, 0), edge(COR, 0, SAT, 0), edge(SAT, 0, IOUT, 0)],
        "output_node_id": IOUT
    }});
    let g = json!({
        "nodes": [gn(SRC, "Source"), cmp, gn(OUT, "Output")],
        "edges": [edge(SRC, 0, CMP, 0), edge(CMP, 0, OUT, 0)],
        "output_node_id": OUT
    });
    let record = clip_to_v2(clip(json!({"node_graph": g})).as_object().unwrap()).unwrap();
    let (p, notes) = lift_clip(&record, &LiftCtx { clip_id: id(CLIP), kind: ClipKind::Media }).unwrap();
    let group = find(body(&p), CMP);
    assert!(group.type_id.as_str() == lt::GROUP && !group.enabled, "off, as it passed through");
    assert!(notes.iter().any(|n| matches!(n, LiftNote::CompoundKeptOff { node, .. } if *node == id(CMP))));
    let back = crate::delift::clip(&p).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&record).unwrap(), "delift(lift(r)) == r");
}
