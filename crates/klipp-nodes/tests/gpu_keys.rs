//! Keys drawn on the GPU for effects' masks (#1266) against the CPU's:
//! two difference keys with different plates in one chain, in both orders,
//! a grade secondary's difference key and then an effect's, and a soloed key
//! or window, which must give its matte, not the GPU's placeholder.

use klipp_effects::mask_mix::MaskChannel;
use klipp_effects::BuiltinEffect;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;
/// GPU against the CPU: a level of an 8-bit picture.
const GPU_BAR: f32 = 1.0 / 255.0;

/// Luminance, hue and alpha running across the picture, its left strip the
/// first plate's grey.
fn frame() -> Vec<f32> {
    let plate = klipp_color::transfer::srgb_to_linear(128.0 / 255.0);
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            if i % W < W / 8 {
                [plate, plate, plate, 1.0]
            } else {
                [x, 0.15 + 0.7 * y * (1.0 - x), (1.0 - x) * 0.8 + 0.05, 1.0]
            }
        })
        .collect()
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "the same size");
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn effect() -> BuiltinEffect {
    klipp_effects::registry::effect_types().find(|t| t.desc.mixable).map(|t| (t.sample)()).expect("a mixable effect")
}

fn mixed(effect: &BuiltinEffect) -> Node {
    node(NodeKind::Effect, NodeParams { effects: vec![effect.clone()], effect_blend: Some(EffectBlend { mix: 0.75, channel: MaskChannel::Luminance }), ..Default::default() })
}

/// Two plates of one size: the frame's grey, and a brighter one, so the
/// two keys select different pixels.
struct Plates {
    dir: std::path::PathBuf,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
}

impl Drop for Plates {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn plates(name: &str) -> Plates {
    let dir = std::env::temp_dir().join(format!("klipp-gpu-keys-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.png"), dir.join("b.png"));
    image::RgbaImage::from_pixel(8, 6, image::Rgba([128, 128, 128, 255])).save(&a).unwrap();
    image::RgbaImage::from_pixel(8, 6, image::Rgba([40, 160, 200, 255])).save(&b).unwrap();
    Plates { dir, a, b }
}

fn difference_key(plate: &std::path::Path) -> Node {
    node(NodeKind::DifferenceKey, NodeParams {
        difference_key: Some(klipp_core::DifferenceKeySettings {
            enabled: true,
            plate_path: Some(plate.to_path_buf()),
            plate_frame: None,
            tolerance: 0.1,
            soft: 0.3,
            invert: false,
            match_space: klipp_core::DifferenceKeyMatchSpace::Rgb,
        }),
        ..Default::default()
    })
}

fn graph(nodes: Vec<Node>, edges: Vec<Edge>, output: NodeId) -> NodeGraph {
    NodeGraph { output_node_id: output, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn run(ex: &mut GraphExecutor, g: &NodeGraph) -> Vec<f32> {
    ex.execute_linear(&frame(), g, W, H, 0.5, 12, None, None).unwrap().0
}

/// Source -> fx1 -> fx2 -> Output, each effect masked by its own
/// difference key of the source, with plates a and b. `reversed` lists the
/// keys the other way round, so the other is drawn first.
fn two_plates(p: &Plates, reversed: bool) -> NodeGraph {
    let (s, o) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::Output, NodeParams::default()));
    let (ka, kb) = (difference_key(&p.a), difference_key(&p.b));
    let (f1, f2) = (mixed(&effect()), mixed(&effect()));
    let edges = vec![link(s.id, ka.id, 0), link(s.id, kb.id, 0), link(s.id, f1.id, 0), link(ka.id, f1.id, 1), link(f1.id, f2.id, 0), link(kb.id, f2.id, 1), link(f2.id, o.id, 0)];
    let out = o.id;
    let keys = if reversed { vec![kb, ka] } else { vec![ka, kb] };
    let mut nodes = vec![s];
    nodes.extend(keys);
    nodes.extend([f1, f2, o]);
    graph(nodes, edges, out)
}

/// Two difference keys with different plates of one size, drawn in one
/// chain: each keys against its own plate, in either order. An upload
/// lands before the whole open encoder runs, so the second plate would
/// land under the first plate's reader: the chain is submitted first,
/// which the count of submits shows.
#[test]
fn two_plates_in_one_chain_each_key_their_own() {
    let p = plates("two");
    let Some(mut ex) = gpu() else { return };
    for reversed in [false, true] {
        let g = two_plates(&p, reversed);
        let want = run(&mut GraphExecutor::new_cpu_only(), &g);
        let got = run(&mut ex, &g);
        let w = worst(&got, &want);
        let stats = ex.last_run_chain_stats();
        assert!(w <= GPU_BAR, "reversed {reversed}: worst {w} {stats:?}");
        assert_eq!(stats.cpu_nodes, 1, "both keys on the GPU, reversed {reversed}: {stats:?}");
        // The second plate's upload waits for the first plate's reader:
        // the chain is submitted once before it, and once at its end.
        assert_eq!(stats.submits, 2, "reversed {reversed}: {stats:?}");
    }
}

/// A grade secondary keyed on the chain by a difference key with plate a
/// (the secondary's shape, keyed off the primary), and an effect after it
/// masked by a difference key with plate b, keyed from the secondary's
/// result. The secondary's key reads plate a in the open chain before the
/// effect's key brings plate b: the CPU's picture all the same, both keys
/// on the GPU, and the second plate costs one submit.
#[test]
fn a_secondarys_plate_then_an_effects_plate_each_key_their_own() {
    let p = plates("secondary");
    let (s, o) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::Output, NodeParams::default()));
    let primary = node(NodeKind::Corrector, NodeParams { saturation: Some(0.8), ..Default::default() });
    let key_a = difference_key(&p.a);
    let cool = node(NodeKind::Corrector, NodeParams { temperature: Some(-45.0), ..Default::default() });
    let apply = node(NodeKind::ApplyMatte, NodeParams::default());
    let key_b = difference_key(&p.b);
    let f = mixed(&effect());
    let edges = vec![
        link(s.id, primary.id, 0),
        link(primary.id, key_a.id, 0),
        link(primary.id, cool.id, 0),
        link(cool.id, apply.id, 0),
        link(key_a.id, apply.id, 1),
        link(primary.id, apply.id, 2),
        link(apply.id, key_b.id, 0),
        link(apply.id, f.id, 0),
        link(key_b.id, f.id, 1),
        link(f.id, o.id, 0),
    ];
    let out = o.id;
    let g = graph(vec![s, primary, key_a, cool, apply, key_b, f, o], edges, out);
    let want = run(&mut GraphExecutor::new_cpu_only(), &g);
    if let Some(mut ex) = gpu() {
        let got = run(&mut ex, &g);
        let w = worst(&got, &want);
        let stats = ex.last_run_chain_stats();
        assert!(w <= GPU_BAR, "worst {w} {stats:?}");
        assert_eq!((stats.cpu_nodes, stats.flushes, stats.submits), (1, 1, 2), "{stats:?}");
    }
}

/// Source -> fx -> Output, masked by `producer` off the source.
fn masked_by(producer: Node) -> (NodeGraph, NodeId) {
    let (s, o) = (node(NodeKind::Source, NodeParams::default()), node(NodeKind::Output, NodeParams::default()));
    let f = mixed(&effect());
    let id = producer.id;
    let edges = vec![link(s.id, id, 0), link(s.id, f.id, 0), link(id, f.id, 1), link(f.id, o.id, 0)];
    let out = o.id;
    (graph(vec![s, producer, f, o], edges, out), id)
}

/// A soloed key or window that only masks an effect gives its matte, the
/// one it gives as the output. Drawn on the GPU, it left an empty
/// placeholder as the solo result, an empty picture.
#[test]
fn a_soloed_key_or_window_gives_its_matte() {
    let p = plates("solo");
    use klipp_core::{Mask, MaskShape, MaskStack};
    let window = node(NodeKind::Mask, NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.2, ..Mask::default() }] }), ..Default::default() });
    let luma = node(NodeKind::LumaKey, NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.45, high: 0.5, soft: 0.05, ..Default::default() }), ..Default::default() });
    for (what, producer) in [("difference key", difference_key(&p.a)), ("luma key", luma), ("window", window)] {
        let (g, id) = masked_by(producer);
        let Some(mut ex) = gpu() else { return };
        // Its matte as this executor makes it, with the producer as the
        // output (the CPU executor's luma key measures on 8 bits, a known
        // apart from the GPU's).
        let mut alone = g.clone();
        let out = alone.output_node_id;
        alone.edges.retain(|e| e.to_node != out);
        alone.edges.push(link(id, out, 0));
        let want = run(&mut ex, &alone);
        ex.set_solo_node(Some(id));
        let got = run(&mut ex, &g);
        ex.set_solo_node(None);
        assert_eq!(got.len(), want.len(), "{what}: a whole picture, not a placeholder");
        let w = worst(&got, &want);
        assert!(w <= GPU_BAR, "{what}: worst {w}");
    }
}
