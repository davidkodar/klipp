use super::*;
use serde_json::json;

fn id(n: u128) -> NodeId {
    Uuid::from_u128(n)
}

fn legacy(payload: Value) -> Option<Legacy> {
    Some(Legacy { slot: None, payload, keyframes: None, modifiers: BTreeMap::new() })
}

fn node(n: u128, type_id: &str) -> Node {
    Node {
        id: id(n),
        type_id: type_id.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: BTreeMap::new(),
        anim: BTreeMap::new(),
        mix: None,
        wireless: Wireless::default(),
        group: None,
        legacy: legacy(json!({})),
        legacy_flags: None,
        origin: None,
    }
}

fn edge(from: u128, to: u128, port: &str) -> Edge {
    Edge { from: (id(from), "out".into()), to: (id(to), port.into()) }
}

/// A clip graph in the phase-1 shape: a grade group holding an old grade
/// graph, then a plug-in and two effects in slot order.
fn clip_graph() -> Graph {
    let mut corrector = node(11, types::LEGACY_NODE);
    corrector.label = "Corrector".into();
    corrector.legacy = Some(Legacy {
        slot: None,
        payload: json!({"kind": "Corrector", "params": {"lift": [0.0, 0.0, 0.0], "qualifier": null}}),
        keyframes: Some(json!({"tracks": {"gain": [{"t": 0.0, "v": 1.0}]}})),
        modifiers: BTreeMap::new(),
    });
    corrector.wireless.input_links.insert("in".into(), "plate".into());
    let mut output = node(12, types::LEGACY_NODE);
    output.legacy = legacy(json!({"kind": "Output"}));
    let mut ui = GraphUi::default();
    ui.positions.insert(id(11), json!({"x": 10.0, "y": 20.0}));
    ui.sticky_notes.push(json!({"id": "n1", "text": "check skin"}));
    let grade = Graph {
        graph_version: GRAPH_VERSION,
        nodes: vec![corrector, output],
        edges: vec![edge(11, 12, "in"), edge(11, 11, "p7")],
        output: id(12),
        ui,
        working_space_override: Some(json!("Rec709")),
    };
    let mut group = node(1, types::LEGACY_GRADE);
    group.group = Some(Box::new(GroupBody { graph: grade, ports: vec!["in".into(), "out".into()] }));
    let mut ofx = node(2, types::LEGACY_OFX);
    ofx.legacy.as_mut().unwrap().slot = Some(0);
    let mut blur = node(3, types::LEGACY_EFFECT);
    blur.enabled = false;
    blur.legacy = Some(Legacy {
        slot: Some(0),
        payload: json!({"GaussianBlur": {"radius": 4.0}}),
        keyframes: Some(json!({"0:radius": [{"t": 1.0, "v": 8.0}]})),
        modifiers: BTreeMap::from([("Effect:0:radius".to_owned(), json!({"modifiers": []}))]),
    });
    let mut opacity = node(4, types::LEGACY_OPACITY);
    opacity.legacy.as_mut().unwrap().slot = Some(1);
    Graph {
        graph_version: GRAPH_VERSION,
        nodes: vec![group, ofx, blur, opacity],
        edges: vec![edge(1, 2, "in"), edge(2, 3, "in"), edge(2, 3, "mask"), edge(3, 4, "in")],
        output: id(4),
        ui: GraphUi::default(),
        working_space_override: None,
    }
}

fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug>(value: &T) -> Value {
    let bytes = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&bytes).unwrap();
    assert_eq!(&back, value);
    assert_eq!(serde_json::to_string(&back).unwrap(), bytes, "a second write differs from the first");
    serde_json::from_str(&bytes).unwrap()
}

#[test]
fn a_clip_record_round_trips_unchanged() {
    let mut record = ClipRecordV2 { graph: clip_graph(), legacy_clip: LegacyClipAnim::default() };
    record.legacy_clip.fields.insert("opacity_keyframes".into(), json!([{"t": 0.0, "v": 0.5}]));
    record.legacy_clip.fields.insert("transform_tilt_y_keyframes".into(), json!([]));
    record.legacy_clip.param_modifiers.insert("Opacity".into(), json!({"modifiers": [{"Wiggle": {}}]}));
    record.check().unwrap();
    let json = round_trip(&record);
    assert_eq!(json["graph"]["graph_version"], GRAPH_VERSION);
}

#[test]
fn every_parameter_value_round_trips() {
    let mut n = node(5, types::LEGACY_EFFECT);
    n.mix = Some(0.25);
    n.anim.insert(
        "radius".into(),
        Animation { keys: vec![json!({"t": 0.0, "v": 1.0})], modifiers: Some(json!({"modifiers": []})) },
    );
    for (key, value) in [
        ("f", ParamValue::Float(0.1)),
        ("i", ParamValue::Int(-3)),
        ("b", ParamValue::Bool(false)),
        ("e", ParamValue::Enum(2)),
        ("c", ParamValue::Color { rgb: [0.2, 0.4, 1.5], space: "acescg".into() }),
        ("p", ParamValue::Point2([0.5, -0.25])),
        ("t", ParamValue::Text("Ünïcode".into())),
        ("m", ParamValue::Matrix3([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
    ] {
        n.params.insert(key.into(), value);
    }
    round_trip(&n);
}

/// Absent and `null` are different in an old file, and both survive.
#[test]
fn absent_and_null_parts_stay_as_read() {
    let with_null: Legacy = serde_json::from_value(json!({"payload": null, "keyframes": null})).unwrap();
    assert_eq!(with_null.keyframes, Some(Value::Null));
    assert_eq!(serde_json::to_value(&with_null).unwrap(), json!({"payload": null, "keyframes": null}));
    let without: Legacy = serde_json::from_value(json!({"payload": {"a": 1}})).unwrap();
    assert_eq!(without.keyframes, None);
    assert_eq!(serde_json::to_value(&without).unwrap(), json!({"payload": {"a": 1}}));

    let graph: Graph = serde_json::from_value(json!({
        "graph_version": 2, "nodes": [], "edges": [], "output": id(1), "working_space_override": null
    }))
    .unwrap();
    assert_eq!(graph.working_space_override, Some(Value::Null));
}

/// Defaults are written as nothing, and read back as the default.
#[test]
fn a_plain_node_is_written_briefly() {
    let mut n = node(6, types::LEGACY_EFFECT);
    n.legacy = legacy(json!(1));
    assert_eq!(
        serde_json::to_value(&n).unwrap(),
        json!({"id": id(6), "type_id": "klipp.legacy.effect", "legacy": {"payload": 1}})
    );
    round_trip(&n);
}

/// A field this build doesn't know means a newer writer: refuse, never drop.
#[test]
fn unknown_fields_are_refused() {
    let base = serde_json::to_value(clip_graph()).unwrap();
    let mut cases = Vec::new();
    let mut graph = base.clone();
    graph["merges"] = json!([]);
    cases.push(graph);
    let mut node = base.clone();
    node["nodes"][1]["origin"] = json!("track");
    cases.push(node);
    let mut legacy = base.clone();
    legacy["nodes"][2]["legacy"]["origin"] = json!("clip");
    cases.push(legacy);
    let mut group = base;
    group["nodes"][0]["group"]["published"] = json!([]);
    cases.push(group);
    for case in cases {
        assert!(serde_json::from_value::<Graph>(case).is_err());
    }
    assert!(serde_json::from_value::<ClipRecordV2>(json!({"graph": clip_graph(), "picture": {}})).is_err());
    assert!(serde_json::from_value::<Edge>(json!({
        "from": [id(1), "out"], "to": [id(2), "in"], "channel": "red"
    }))
    .is_err());
}

#[test]
fn check_refuses_what_revision_one_cannot_hold() {
    let mut g = clip_graph();
    g.graph_version = 3;
    assert_eq!(g.check(), Err(FormatError::WrongGraphVersion(3)));

    let mut g = clip_graph();
    g.nodes[2].id = id(2);
    assert_eq!(g.check(), Err(FormatError::DuplicateNode(id(2))));

    let mut g = clip_graph();
    g.output = id(99);
    assert_eq!(g.check(), Err(FormatError::MissingOutput(id(99))));

    let mut g = clip_graph();
    g.edges.push(edge(3, 98, "in"));
    assert_eq!(g.check(), Err(FormatError::DanglingEdge(id(98))));

    let mut g = clip_graph();
    g.nodes[1].type_id = "klipp.fx.gaussian_blur".into();
    assert!(matches!(g.check(), Err(FormatError::NotStructural { .. })));

    let mut g = clip_graph();
    g.nodes[3].legacy = None;
    assert_eq!(g.check(), Err(FormatError::MissingLegacy(id(4))));

    let mut g = clip_graph();
    g.nodes[0].group = None;
    assert_eq!(g.check(), Err(FormatError::MissingGroupBody(id(1))));

    let mut g = clip_graph();
    g.nodes[1].group = Some(Box::new(GroupBody { graph: clip_graph(), ports: Vec::new() }));
    assert!(matches!(g.check(), Err(FormatError::NotStructural { .. })));

    // Inside a grade body only old grade nodes belong, each with its entry.
    let mut g = clip_graph();
    g.nodes[0].group.as_mut().unwrap().graph.nodes[0].type_id = types::LEGACY_EFFECT.into();
    assert!(matches!(g.check(), Err(FormatError::NotStructural { .. })));
    let mut g = clip_graph();
    g.nodes[0].group.as_mut().unwrap().graph.nodes[1].legacy = None;
    assert_eq!(g.check(), Err(FormatError::MissingLegacy(id(12))));
    let mut g = clip_graph();
    g.nodes[0].group.as_mut().unwrap().graph.graph_version = 1;
    assert_eq!(g.check(), Err(FormatError::WrongGraphVersion(1)));
    // And an old grade node doesn't belong at the converter's level.
    let mut g = clip_graph();
    g.nodes[1].type_id = types::LEGACY_NODE.into();
    assert!(matches!(g.check(), Err(FormatError::NotStructural { .. })));

    let mut record = ClipRecordV2 { graph: clip_graph(), legacy_clip: LegacyClipAnim::default() };
    record.legacy_clip.fields.insert("volume_keyframes".into(), json!([]));
    assert_eq!(record.check(), Err(FormatError::UnknownClipField("volume_keyframes".into())));
}

/// Old grade graphs keep their oddities verbatim: an edge to a node that
/// isn't there, an output naming a missing node, a node id twice, a
/// duplicate edge into one port. Today's reader accepts all of them, so
/// revision 1 holds them without complaint and the lift judges them.
#[test]
fn check_keeps_old_oddities() {
    let mut g = clip_graph();
    let dup = g.edges[1].clone();
    g.edges.push(dup);
    let body = &mut g.nodes[0].group.as_mut().unwrap().graph;
    body.edges.push(edge(11, 96, "in"));
    body.edges.push(edge(95, 12, "in"));
    body.output = id(94);
    let twin = body.nodes[0].clone();
    body.nodes.push(twin);
    g.check().unwrap();
    round_trip(&g);
}

#[test]
fn errors_read_as_sentences() {
    let e = FormatError::NotStructural { node: id(1), type_id: "group".into() };
    assert_eq!(e.to_string(), format!("node {} is a group, which revision 1 can't hold", id(1)));
}

/// Maps are ordered, so the same graph writes the same bytes however it
/// was built: insertion order doesn't leak into the file.
#[test]
fn the_same_graph_writes_the_same_bytes() {
    let mut a = node(7, types::LEGACY_EFFECT);
    let mut b = a.clone();
    for k in ["zeta", "alpha", "mid"] {
        a.params.insert(k.into(), ParamValue::Float(1.0));
    }
    for k in ["mid", "zeta", "alpha"] {
        b.params.insert(k.into(), ParamValue::Float(1.0));
    }
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
    assert_eq!(serde_json::to_string(&clip_graph()).unwrap(), serde_json::to_string(&clip_graph()).unwrap());
}

/// A grade node's parameters ride in the payload exactly as stored: a
/// parameter left out (`None`) and one set to its default value
/// (`Some(default)`) mean different things, and both come back.
#[test]
fn grade_parameters_keep_none_and_some_default_apart() {
    let params = klipp_nodes::NodeParams { exposure: Some(0.0), ..Default::default() };
    assert_eq!(params.contrast, None);
    let mut n = node(8, types::LEGACY_NODE);
    n.legacy = legacy(json!({"kind": "Exposure", "params": serde_json::to_value(&params).unwrap()}));
    let json = round_trip(&n);
    let back: klipp_nodes::NodeParams = serde_json::from_value(json["legacy"]["payload"]["params"].clone()).unwrap();
    assert_eq!(back, params);
    assert_eq!(back.exposure, Some(0.0));
    assert_eq!(back.contrast, None);
}

#[test]
fn port_numbers_become_the_named_keys() {
    use ports::{input_key, output_key};
    let cases: [(&str, &[&str]); 11] = [
        ("ApplyMatte", &["in", "matte", "pregrade", "p3"]),
        ("Corrector", &["in", "mask", "p2"]),
        ("RGBCurves", &["in", "mask"]),
        ("Layer", &["fg", "bg", "p2"]),
        ("Blend", &["a", "b"]),
        ("CombineMattes", &["a", "b"]),
        ("MaskCutout", &["in", "matte"]),
        ("RGBCombiner", &["r", "g", "b", "p3"]),
        ("LayerMixer", &["in0", "in1", "in2", "in3"]),
        ("ColorMatch", &["in", "ref"]),
        ("Saturation", &["in", "mask"]),
    ];
    for (kind, keys) in cases {
        for (port, key) in keys.iter().enumerate() {
            assert_eq!(input_key(kind, port as u8).as_str(), *key, "{kind} input {port}");
        }
    }
    assert_eq!(input_key("Hue", 0).as_str(), "in");
    assert_eq!(input_key("Hue", 1).as_str(), "p1");
    assert_eq!(input_key("Source", 0).as_str(), "p0");
    assert_eq!(output_key("RGBSplitter", 2).as_str(), "b");
    assert_eq!(output_key("RGBSplitter", 3).as_str(), "p3");
    assert_eq!(output_key("Qualifier", 0).as_str(), "out");
    assert_eq!(output_key("Output", 0).as_str(), "p0");
}

/// Every port number comes back from its key, on every kind, so no edge
/// changes ports on the way through; and a key a kind never gives, like
/// `p0` where port 0 is `in`, isn't read as a port.
#[test]
fn port_keys_go_back_to_their_numbers() {
    use ports::{input_index, input_key, output_index, output_key};
    let kinds = [
        "Source", "Output", "ExternalMatte", "ApplyMatte", "Layer", "Blend", "CombineMattes", "MaskCutout",
        "RGBCombiner", "RGBSplitter", "LayerMixer", "ColorMatch", "Corrector", "Exposure", "Hue", "Qualifier",
        "SomeKindFromLater",
    ];
    for kind in kinds {
        for port in 0..=u8::MAX {
            assert_eq!(input_index(kind, &input_key(kind, port)), Some(port), "{kind} input {port}");
            assert_eq!(output_index(kind, &output_key(kind, port)), Some(port), "{kind} output {port}");
        }
    }
    assert_eq!(input_index("Hue", &"p0".into()), None);
    assert_eq!(input_index("Layer", &"in".into()), None);
    assert_eq!(output_index("RGBSplitter", &"out".into()), None);
}

/// A store with no picture is the empty graph, and only that exact shape
/// may lack an output node.
#[test]
fn the_empty_graph_is_the_one_without_an_output() {
    let empty = Graph::empty();
    assert!(empty.is_empty());
    empty.check().unwrap();
    round_trip(&empty);
    let mut not_quite = Graph::empty();
    not_quite.output = id(1);
    assert_eq!(not_quite.check(), Err(FormatError::MissingOutput(id(1))));
    let mut edged = Graph::empty();
    edged.edges.push(edge(1, 2, "in"));
    assert!(edged.check().is_err());
}

#[test]
fn unclaimed_effect_keyframes_round_trip() {
    let mut record = ClipRecordV2 { graph: Graph::empty(), legacy_clip: LegacyClipAnim::default() };
    record.legacy_clip.effect_keyframes.insert("9:radius".into(), json!([]));
    record.legacy_clip.effect_keyframes.insert("not a key".into(), json!([{"time_secs": 0.0, "value": 1.0}]));
    record.legacy_clip.param_modifiers.insert("effect:7:left".into(), json!({"modifiers": []}));
    record.check().unwrap();
    round_trip(&record);
}

mod lifted_checks {
    use super::*;
    use crate::lifted::{types as lt, LiftedError, Origin};

    fn plain(n: u128, type_id: &str) -> Node {
        let mut x = node(n, type_id);
        x.legacy = None;
        x
    }

    fn legacy_node(n: u128, kind: &str) -> Node {
        let mut x = node(n, types::LEGACY_NODE);
        x.legacy = legacy(json!({"kind": kind}));
        x
    }

    fn e(from: u128, to: u128, port: &str) -> Edge {
        Edge { from: (id(from), "out".into()), to: (id(to), port.into()) }
    }

    /// source -> group(input -> Corrector -> output) -> tone map ->
    /// placement -> output, every input wired.
    fn valid() -> Graph {
        let body = Graph {
            graph_version: GRAPH_VERSION,
            nodes: vec![plain(11, lt::GROUP_INPUT), legacy_node(12, "Corrector"), legacy_node(13, "Output")],
            edges: vec![e(11, 12, "in"), e(12, 13, "in")],
            output: id(13),
            ui: GraphUi::default(),
            working_space_override: None,
        };
        let mut body = body;
        body.nodes[2].type_id = lt::GROUP_OUTPUT.into();
        let mut group = plain(2, lt::GROUP);
        group.group = Some(Box::new(GroupBody { graph: body, ports: Vec::new() }));
        let nodes = vec![plain(1, lt::SOURCE), group, plain(3, lt::SCENE_TO_DISPLAY), plain(4, lt::PLACEMENT), plain(5, lt::OUTPUT)];
        Graph {
            graph_version: GRAPH_VERSION,
            edges: nodes.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect(),
            nodes,
            output: id(5),
            ui: GraphUi::default(),
            working_space_override: None,
        }
    }

    fn body(g: &mut Graph) -> &mut Graph {
        &mut g.nodes[1].group.as_mut().unwrap().graph
    }

    #[test]
    fn a_valid_lifted_graph_passes() {
        valid().check_lifted(false).unwrap();
    }

    /// Every rule refuses what it exists to refuse.
    #[test]
    fn every_rule_refuses() {
        let refuse = |f: &dyn Fn(&mut Graph), want: &dyn Fn(&LiftedError) -> bool| {
            let mut g = valid();
            f(&mut g);
            let err = g.check_lifted(false).unwrap_err();
            assert!(want(&err), "{err:?}");
        };
        refuse(&|g| { g.nodes.remove(0); g.edges.remove(0); }, &|e| matches!(e, LiftedError::Ends { what: "start", .. }));
        refuse(&|g| g.nodes.push(plain(9, lt::OUTPUT)), &|e| matches!(e, LiftedError::Ends { what: "end", .. }));
        refuse(&|g| g.output = id(3), &|e| matches!(e, LiftedError::Output { .. }));
        refuse(&|g| g.nodes[3].id = id(3), &|e| matches!(e, LiftedError::DuplicateNode(_)));
        refuse(&|g| g.edges.push(e(1, 99, "in")), &|e| matches!(e, LiftedError::DanglingEdge(_)));
        refuse(&|g| { g.edges.remove(2); }, &|e| matches!(e, LiftedError::Unwired { .. }));
        refuse(&|g| g.edges.push(e(1, 3, "in")), &|e| matches!(e, LiftedError::TwoInputs { .. }));
        refuse(&|g| g.nodes.insert(4, plain(8, lt::PLACEMENT)), &|e| matches!(e, LiftedError::Unwired { .. } | LiftedError::TwoPlacements));
        refuse(&|g| g.nodes[2].origin = Some(Origin::Track { track: id(7) }), &|e| matches!(e, LiftedError::Origin(_)));
        // Inside the group body.
        refuse(&|g| { body(g).edges.remove(0); }, &|e| matches!(e, LiftedError::Unwired { .. }));
        refuse(&|g| { let b = body(g); b.nodes[1].legacy = legacy(json!({"kind": "Hue"})); b.edges.push(e(11, 12, "mask")); }, &|e| matches!(e, LiftedError::MaskNotGateable(_)));
        refuse(&|g| { let b = body(g); b.nodes.remove(0); }, &|e| matches!(e, LiftedError::Ends { what: "start", .. }));
        refuse(&|g| { let b = body(g); b.nodes[1].type_id = lt::MERGE.into(); b.nodes[1].legacy = legacy(json!({"kind": "Blend"})); }, &|e| matches!(e, LiftedError::UnknownMergeOp(_) | LiftedError::Unwired { .. }));
    }

    /// Two placements alone, every input wired, is refused for the count.
    #[test]
    fn two_placements_are_refused() {
        let mut g = valid();
        g.nodes.insert(4, plain(8, lt::PLACEMENT));
        g.edges = g.nodes.windows(2).map(|w| Edge { from: (w[0].id, "out".into()), to: (w[1].id, "in".into()) }).collect();
        assert_eq!(g.check_lifted(false), Err(LiftedError::TwoPlacements));
    }

    /// A wireless link with a sender counts as wiring; one without doesn't.
    #[test]
    fn wireless_wiring_needs_a_sender() {
        let mut g = valid();
        body(&mut g).edges.remove(0);
        body(&mut g).nodes[1].wireless.input_links.insert("in".into(), "plate".into());
        assert!(matches!(g.check_lifted(false), Err(LiftedError::Unwired { .. })));
        body(&mut g).nodes[0].wireless.output_link = Some("plate".into());
        g.check_lifted(false).unwrap();
    }

    /// A loop is refused, through an edge or a wireless link.
    #[test]
    fn a_loop_is_refused() {
        let mut g = valid();
        // A gated correction: a mask without the gate is refused first.
        let gated = || Some(crate::lifted::LegacyFlags { alpha_one: true, ..Default::default() });
        body(&mut g).edges.push(e(13, 12, "mask"));
        body(&mut g).nodes[1].legacy = legacy(json!({"kind": "Corrector"}));
        body(&mut g).nodes[1].legacy_flags = gated();
        assert!(matches!(g.check_lifted(false), Err(LiftedError::Cycle { .. })));
        let mut g = valid();
        let b = body(&mut g);
        b.nodes[1].legacy = legacy(json!({"kind": "Corrector"}));
        b.nodes[1].legacy_flags = gated();
        b.nodes[2].wireless.output_link = Some("back".into());
        b.nodes[1].wireless.input_links.insert("mask".into(), "back".into());
        assert!(matches!(g.check_lifted(false), Err(LiftedError::Cycle { .. })));
    }

    /// A merge must name a known op.
    #[test]
    fn a_merge_needs_a_known_op() {
        let mut g = valid();
        let b = body(&mut g);
        b.nodes[1].type_id = lt::MERGE.into();
        b.nodes[1].legacy = legacy(json!({"kind": "Blend"}));
        b.edges[0].to.1 = "a".into();
        b.edges.push(e(11, 12, "b"));
        assert_eq!(g.check_lifted(false), Err(LiftedError::UnknownMergeOp(id(12))));
        body(&mut g).nodes[1].params.insert("op".into(), ParamValue::Text("blend_v1".into()));
        g.check_lifted(false).unwrap();
    }
}
