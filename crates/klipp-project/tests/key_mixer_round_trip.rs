//! A Key Mixer (#898) with three mattes wired and its inputs set comes
//! back from a save and a strict load as it went in: each matte on its
//! port, every gain and invert, the output gain and offset.

use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_media::MediaPool;
use klipp_nodes::{Edge, KeyMixerInput, KeyMixerParams, NodeKind};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{GradeAccess, Sequence, TrackKind};

fn node(kind: NodeKind, params: klipp_nodes::NodeParams) -> klipp_nodes::Node {
    klipp_nodes::Node {
        id: klipp_nodes::NodeId::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

#[test]
fn a_key_mixer_comes_back_as_it_went_in() {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut graph = klipp_nodes::NodeGraph::default();
    let corrector = graph.ensure_corrector();
    let source = graph.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("the Source").id;
    let mixer = KeyMixerParams {
        inputs: vec![KeyMixerInput { gain: 0.8, invert: false }, KeyMixerInput { gain: -0.5, invert: true }, KeyMixerInput { gain: 1.0, invert: false }],
        output_gain: 1.25,
        output_offset: -0.1,
    };
    let key_mixer = node(NodeKind::KeyMixer, klipp_nodes::NodeParams { key_mixer: Some(mixer.clone()), ..Default::default() });
    let km = key_mixer.id;
    let mut windows = Vec::new();
    for n in 0..3 {
        let circle = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.3 + 0.2 * n as f32, center_y: 0.5, radius: 0.2 }, ..Mask::default() }] };
        let w = node(NodeKind::Mask, klipp_nodes::NodeParams { masks: Some(circle), ..Default::default() });
        graph.edges.push(Edge { from_node: source, from_port: 0, to_node: w.id, to_port: 0 });
        graph.edges.push(Edge { from_node: w.id, from_port: 0, to_node: km, to_port: n });
        windows.push(w.id);
        graph.nodes.push(w);
    }
    graph.nodes.push(key_mixer);
    // The mixed key gates the Corrector.
    graph.edges.push(Edge { from_node: km, from_port: 0, to_node: corrector, to_port: 1 });
    clip.set_grade(graph);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    seq.tracks[v1].clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp-key-mixer-rt-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap();
    let clip = loaded[0].all_clips().find(|c| c.id == id).expect("the clip");
    let grade = clip.grade();
    let back = grade.nodes.iter().find(|n| n.id == km).expect("the Key Mixer came back");
    assert_eq!(back.kind, NodeKind::KeyMixer);
    assert_eq!(back.params.key_mixer.as_ref(), Some(&mixer), "its settings");
    for (port, w) in windows.iter().enumerate() {
        assert_eq!(grade.input_of(km, port as u8), Some(*w), "window {port} on port {port}");
    }
    assert_eq!(grade.input_of(corrector, 1), Some(km), "and it keys the Corrector");
    std::fs::remove_dir_all(dir).unwrap();
}
