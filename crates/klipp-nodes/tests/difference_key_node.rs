//! The DifferenceKey node: the matte it produces, and that caching the
//! resized plate does not serve a stale one.
//!
//! The algorithm has unit tests in klipp-color. The *node* had none,
//! which is how it kept resizing the plate to the frame on every single
//! frame — a full-frame write, 33 MB at 4K, for a plate and a resolution
//! that never change.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

/// The plate is an 8-bit sRGB image on disk, so the value the node sees
/// is this, not 128/255.
fn plate_linear_value() -> f32 {
    klipp_color::transfer::srgb_to_linear(128.0 / 255.0)
}

fn write_plate(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("plate.png");
    // Uniform mid-grey, deliberately a different size from the frames
    // below so the resize path actually runs.
    let img = image::RgbaImage::from_pixel(8, 6, image::Rgba([128, 128, 128, 255]));
    img.save(&path).expect("write plate");
    path
}

/// Left half matches the plate, right half does not.
fn live_frame(w: u32, h: u32) -> Vec<f32> {
    let matched = plate_linear_value();
    let mut px = vec![0.0f32; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if x < w / 2 { matched } else { 0.9 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 1.0;
        }
    }
    px
}

fn graph(path: &std::path::Path) -> NodeGraph {
    let (src, dk, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let settings = klipp_core::DifferenceKeySettings {
        enabled: true,
        plate_path: Some(path.to_path_buf()),
        plate_frame: None,
        tolerance: 0.05,
        soft: 0.1,
        invert: false,
        match_space: klipp_core::DifferenceKeyMatchSpace::Rgb,
    };
    let nodes = vec![
        Node {
            id: src, kind: NodeKind::Source, label: "S".into(), enabled: true,
            params: Default::default(), keyframes: Default::default(),
            output_link: None, input_links: Vec::new(),
        },
        Node {
            id: dk, kind: NodeKind::DifferenceKey, label: "DK".into(), enabled: true,
            params: NodeParams { difference_key: Some(settings), ..Default::default() },
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        },
        Node {
            id: out, kind: NodeKind::Output, label: "O".into(), enabled: true,
            params: Default::default(), keyframes: Default::default(),
            output_link: None, input_links: Vec::new(),
        },
    ];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 160.0, y: 0.0 }))
        .collect();
    NodeGraph {
        nodes,
        edges: vec![
            Edge { from_node: src, from_port: 0, to_node: dk, to_port: 0 },
            Edge { from_node: dk, from_port: 0, to_node: out, to_port: 0 },
        ],
        output_node_id: out,
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// The matte selects what differs from the plate and nothing else, and
/// says so identically on the second frame — the one served from the
/// resize cache.
#[test]
fn the_matte_selects_what_differs_from_the_plate() {
    let dir = std::env::temp_dir().join(format!("klipp-dk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = write_plate(&dir);
    let g = graph(&path);
    let (w, h) = (16u32, 8u32);

    let mut exec = GraphExecutor::new_cpu_only();
    let run = |exec: &mut GraphExecutor, w: u32, h: u32| -> Vec<f32> {
        exec.execute_linear(&live_frame(w, h), &g, w, h, 0.0, 0, None, None)
            .expect("graph runs")
            .0
    };

    let first = run(&mut exec, w, h);
    let at = |out: &[f32], x: u32, y: u32| out[((y * w + x) * 4) as usize];
    // Matching half selects nothing, differing half selects fully.
    assert_eq!(at(&first, 2, 4), 0.0, "the half that matches the plate must select nothing");
    assert_eq!(at(&first, 13, 4), 1.0, "the half that differs must select fully");

    // Frame two comes from the resize cache. It has to be the same
    // picture — a cache that returned a stale or wrong-sized plate would
    // show up here as a matte that moved.
    let second = run(&mut exec, w, h);
    assert_eq!(first, second, "the cached plate produced a different matte");

    // A resolution change must not be served the old buffer. Its length
    // alone would be wrong, which the algorithm treats as malformed and
    // answers with an all-zero matte — so this asserts the selection
    // survives, not merely that nothing panicked.
    let (w2, h2) = (24u32, 12u32);
    let bigger = exec
        .execute_linear(&live_frame(w2, h2), &g, w2, h2, 0.0, 0, None, None)
        .expect("graph runs at the new size")
        .0;
    assert_eq!(bigger.len(), (w2 * h2 * 4) as usize);
    let at2 = |x: u32, y: u32| bigger[((y * w2 + x) * 4) as usize];
    assert_eq!(at2(3, 6), 0.0, "matching half at the new size must still select nothing");
    assert_eq!(at2(20, 6), 1.0, "differing half at the new size must still select fully");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Invert flips the selection, and a disabled node selects everything
/// (#1039) — both through the real node rather than the algorithm alone.
#[test]
fn invert_and_disabled_go_through_the_node() {
    let dir = std::env::temp_dir().join(format!("klipp-dk-inv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = write_plate(&dir);
    let (w, h) = (16u32, 8u32);
    let mut exec = GraphExecutor::new_cpu_only();

    let with = |exec: &mut GraphExecutor, enabled: bool, invert: bool| -> Vec<f32> {
        let mut g = graph(&path);
        if let Some(n) = g.nodes.iter_mut().find(|n| n.kind == NodeKind::DifferenceKey) {
            if let Some(dk) = n.params.difference_key.as_mut() {
                dk.enabled = enabled;
                dk.invert = invert;
            }
        }
        exec.execute_linear(&live_frame(w, h), &g, w, h, 0.0, 0, None, None)
            .expect("graph runs")
            .0
    };
    let at = |out: &[f32], x: u32| out[((4 * w + x) * 4) as usize];

    let inverted = with(&mut exec, true, true);
    assert_eq!(at(&inverted, 2), 1.0, "inverted: the matching half selects fully");
    assert_eq!(at(&inverted, 13), 0.0, "inverted: the differing half selects nothing");

    // Off, it selects everything, as every matte producer does with
    // nothing chosen (#1039): it used to select nothing.
    let off = with(&mut exec, false, false);
    assert!(
        off.chunks_exact(4).all(|p| p[0] == 1.0 && p[3] == 1.0),
        "a disabled difference key selects everything",
    );

    let _ = std::fs::remove_dir_all(&dir);
}
