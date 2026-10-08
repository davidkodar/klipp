//! Stack projection uses the real registry and lifted clip representation.
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::{lifted::types as lt, path::Root, types};
use klipp_graph_lift::stack_view::{project, Section};
use klipp_timeline::{Clip, RegistryCatalog};

fn clip() -> Clip {
    let t = |s| RationalTime::from_secs_f64(s, 24);
    Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0))
}
fn transform() -> E {
    E::Transform {
        position_x: 0.1,
        position_y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    }
}

#[test]
fn the_placement_row_never_duplicates_a_transform() {
    let mut c = clip();
    let disabled = c.push_effect(transform());
    c.set_effect_enabled(disabled, false);
    c.push_effect(E::GaussianBlur { radius: 2.0 });
    c.push_effect(transform()); // First enabled Transform: the anchor.
    c.push_effect(transform()); // A retained card, not another anchor.
    c.push_effect(E::Opacity { value: 0.5 });
    c.push_effect(E::SpeedRamp {
        speed: 2.0,
        blend_mode: 0,
        ramp_curve: 0,
        ramp_start: 0.0,
        ramp_end: 1.0,
        reverse: false,
        freeze_at: 0.0,
        freeze: false,
    });
    let picture = c.picture_v2().unwrap().picture.clone();
    let before = picture.clone();
    let view = project(
        Root::Clip { clip: c.id.0 },
        &picture.graph,
        &RegistryCatalog,
        None,
    )
    .unwrap();
    let rows: Vec<_> = view
        .rows()
        .filter(|r| !matches!(r.type_id.as_str(), lt::SCENE_TO_DISPLAY | lt::GROUP))
        .collect();
    assert_eq!(
        rows.iter().map(|r| r.type_id.as_str()).collect::<Vec<_>>(),
        [
            types::LEGACY_TRANSFORM,
            types::LEGACY_EFFECT,
            lt::PLACEMENT,
            types::LEGACY_TRANSFORM,
            types::LEGACY_OPACITY,
            types::LEGACY_SPEED_RAMP
        ]
    );
    assert_eq!(
        rows.iter().map(|r| r.enabled).collect::<Vec<_>>(),
        [false, true, true, true, true, true]
    );
    assert_eq!(
        rows.iter().map(|r| r.placed).collect::<Vec<_>>(),
        [true, false, false, true, true, true]
    );
    assert_eq!(view.rows().filter(|r| r.placement.is_some()).count(), 1);
    assert_eq!(picture, before);
}

#[test]
fn source_neighbours_use_the_source_section_and_placed_edit_route() {
    let mut c = clip();
    c.push_effect(E::GaussianBlur { radius: 2.0 });
    c.push_effect(E::TemporalDenoise {
        strength: 0.5,
        threshold: 0.25,
    });
    c.push_effect(E::SpeedRamp {
        speed: 2.0,
        blend_mode: 1,
        ramp_curve: 0,
        ramp_start: 0.0,
        ramp_end: 1.0,
        reverse: false,
        freeze_at: 0.0,
        freeze: false,
    });
    let picture = c.picture_v2().unwrap().picture.clone();
    let view = project(
        Root::Clip { clip: c.id.0 },
        &picture.graph,
        &RegistryCatalog,
        None,
    )
    .unwrap();
    let source: Vec<_> = view
        .rows()
        .filter(|r| r.section == Section::Source)
        .collect();
    assert_eq!(
        source
            .iter()
            .map(|r| r.type_id.as_str())
            .collect::<Vec<_>>(),
        [lt::TEMPORAL_DENOISE, lt::FRAME_BLEND]
    );
    assert!(source.iter().all(|r| r.placed && r.enabled));
}

/// Private corpus paths are supplied by the caller. Read raw JSON only: no
/// project loader, recovery, autosave, migration, or original-file writes.
#[test]
#[ignore = "requires KLIPP_STACK_CORPUS manifest of exactly 18 project directories"]
fn stack_projection_matches_the_effect_list_on_the_18() {
    use klipp_graph_format::lifted::ClipPicture;
    use serde_json::Value;
    let manifest =
        std::fs::read_to_string(std::env::var("KLIPP_STACK_CORPUS").expect("corpus manifest"))
            .unwrap();
    let projects: Vec<_> = manifest.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(projects.len(), 18);
    let mut clips = 0;
    let mut cards = 0;
    for directory in projects {
        let path = std::path::Path::new(directory).join("project.json");
        let bytes = std::fs::read(&path).unwrap();
        let saved: Value = serde_json::from_slice(&bytes).unwrap();
        let sequences: Vec<_> = match saved.get("sequences").and_then(Value::as_array) {
            Some(s) => s.iter().collect(),
            None => vec![saved.get("sequence").expect("sequence")],
        };
        for sequence in sequences {
            for track in sequence["tracks"].as_array().unwrap() {
                for c in track["clips"].as_array().unwrap() {
                    let picture: ClipPicture =
                        serde_json::from_value(c["graph_v2"].clone()).expect("converted clip");
                    let before = picture.clone();
                    let v1 = klipp_graph_lift::unlift::clip(&picture).unwrap();
                    let root = Root::Clip {
                        clip: serde_json::from_value(c["id"].clone()).unwrap(),
                    };
                    let view = project(root, &picture.graph, &RegistryCatalog, None).unwrap();
                    let effects = v1["builtin_effects"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let ofx = v1["effects"].as_array().cloned().unwrap_or_default();
                    let flags = v1["builtin_effects_enabled"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let mut seen = vec![false; effects.len()];
                    let mut plugin_slots = Vec::new();
                    let mut ordinary_slots = Vec::new();
                    let mut source_slots = Vec::new();
                    for row in view.rows() {
                        let node = picture
                            .graph
                            .nodes
                            .iter()
                            .find(|n| row.addr.nodes.last() == Some(&n.id))
                            .unwrap();
                        let Some(legacy) = &node.legacy else { continue };
                        let Some(slot) = legacy.slot.map(|n| n as usize) else {
                            continue;
                        };
                        if row.type_id.as_str() == types::LEGACY_OFX {
                            assert_eq!(legacy.payload, ofx[slot]);
                            assert_eq!(
                                row.enabled,
                                ofx[slot]
                                    .get("enabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true)
                            );
                            plugin_slots.push(slot);
                            continue;
                        }
                        if row.type_id.as_str() == lt::GROUP {
                            continue;
                        }
                        assert_eq!(legacy.payload, effects[slot], "card payload at {slot}");
                        assert!(!seen[slot], "duplicated card at {slot}");
                        seen[slot] = true;
                        assert_eq!(
                            row.enabled,
                            flags.get(slot).and_then(Value::as_bool).unwrap_or(true)
                        );
                        if row.section == Section::Source {
                            source_slots.push(slot);
                        } else {
                            ordinary_slots.push(slot);
                        }
                        cards += 1;
                    }
                    let mut expected_source: Vec<_> = effects
                        .iter()
                        .enumerate()
                        .filter_map(|(i, e)| (e["type"] == "temporal_denoise").then_some(i))
                        .collect();
                    if let Some((i, _)) = effects.iter().enumerate().find(|(i, e)| {
                        e["type"] == "speed_ramp"
                            && e["blend_mode"] == 1
                            && e["freeze"] != true
                            && flags.get(*i).and_then(Value::as_bool).unwrap_or(true)
                    }) {
                        expected_source.push(i);
                    }
                    assert_eq!(source_slots, expected_source);
                    assert!(seen.iter().all(|seen| *seen), "lost effect entry");
                    assert_eq!(plugin_slots, (0..ofx.len()).collect::<Vec<_>>());
                    // M2 moves temporal inputs to Source. The remaining list,
                    // including retained cards and the anchor, keeps its order.
                    let expected: Vec<_> = (0..effects.len())
                        .filter(|i| !source_slots.contains(i))
                        .collect();
                    assert_eq!(ordinary_slots, expected);
                    assert_eq!(picture, before);
                    clips += 1;
                }
            }
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "project changed during read-only proof"
        );
    }
    assert!(clips > 0);
    eprintln!("18 projects: {clips} clips, {cards} built-in cards checked");
}
