//! FilmGrain and Denoise: the CPU twins against the shaders.
//!
//! Both nodes had a CPU path that encoded the linear frame to 8-bit
//! sRGB, ran an entirely different algorithm on the bytes, and decoded
//! back. Not a colour-space slip — a different picture: the old grain
//! ignored `size` altogether, so it had no particles, scaled its noise
//! by 128 in byte units, and weighted by an encoded luma.
//!
//! The twins now transcribe `SHADER_GRAIN` and `SHADER_DENOISE`, so
//! these tests hold them to it. Grain is the sharper check of the two:
//! it is hash noise, so the two sides agree pixel for pixel or they do
//! not agree at all.

use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 160;
const H: u32 = 120;

fn node(id: NodeId, kind: NodeKind, params: NodeParams) -> Node {
    Node {
        id,
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

fn one_node_graph(kind: NodeKind, params: NodeParams) -> NodeGraph {
    let (src, n, out) = (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
    let nodes = vec![
        node(src, NodeKind::Source, Default::default()),
        node(n, kind, params),
        node(out, NodeKind::Output, Default::default()),
    ];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
        .collect();
    NodeGraph {
        output_node_id: out,
        nodes,
        edges: vec![
            Edge { from_node: src, from_port: 0, to_node: n, to_port: 0 },
            Edge { from_node: n, from_port: 0, to_node: out, to_port: 0 },
        ],
        node_positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

/// Brightness down, hue across, plus a hard vertical edge so the
/// bilateral filter has something it is supposed to preserve, and a
/// highlight above 1.0 that the old 8-bit bracket would have clipped.
fn frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / (W - 1) as f32;
            let mut v = 0.05 + 0.9 * (y as f32 / H as f32);
            if x > W / 2 {
                v *= 0.35;
            }
            if y > H - 8 {
                v = 3.5;
            }
            px[i] = v * (0.6 + 0.4 * t);
            px[i + 1] = v * (0.5 + 0.2 * (1.0 - t));
            px[i + 2] = v * (0.4 + 0.5 * (1.0 - t));
            px[i + 3] = 1.0;
        }
    }
    px
}

fn run(g: &NodeGraph, gpu: bool) -> (Vec<f32>, klipp_nodes::executor::ChainStats) {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let out = exec
        .execute_linear(&frame(), g, W, H, 0.0, 7, None, None)
        .expect("graph runs")
        .0;
    (out, exec.last_run_chain_stats())
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max)
}

fn grain(monochrome: bool, size: f32) -> NodeParams {
    NodeParams {
        grain: Some(klipp_core::FilmGrainSettings {
            enabled: true,
            amount: 0.25,
            size,
            shadows: 0.8,
            highlights: 0.3,
            monochrome,
        }),
        ..Default::default()
    }
}

fn check(name: &str, g: &NodeGraph, tol: f32) -> Vec<f32> {
    let (cpu, _) = run(g, false);
    let (gpu, stats) = run(g, true);
    if stats.extends == 0 && stats.flushes == 0 {
        eprintln!("skipping {name}: no usable GPU");
        return Vec::new();
    }
    let w = worst(&cpu, &gpu);
    eprintln!("{name}: worst CPU vs GPU = {w:e} linear ({stats:?})");
    assert!(w < tol, "{name}: CPU and GPU disagree by {w}");
    gpu
}

#[test]
fn colour_grain_matches_the_shader() {
    let g = one_node_graph(NodeKind::FilmGrain, grain(false, 2.0));
    let out = check("grain colour", &g, 1e-5);
    if out.is_empty() {
        return;
    }
    // And it must actually be doing something.
    let src = frame();
    assert!(worst(&src, &out) > 0.001, "the grain node changed nothing");
}

#[test]
fn monochrome_grain_matches_the_shader() {
    let g = one_node_graph(NodeKind::FilmGrain, grain(true, 3.0));
    let out = check("grain mono", &g, 1e-5);
    if out.is_empty() {
        return;
    }
    // Monochrome means one hash for all three channels, so the grain
    // offset is equal on each — the only thing separating the channels
    // is the picture underneath.
    let src = frame();
    let mut checked = 0;
    for i in (0..(W * H) as usize).step_by(29) {
        let d: Vec<f32> = (0..3).map(|c| out[i * 4 + c] - src[i * 4 + c]).collect();
        assert!(
            (d[0] - d[1]).abs() < 1e-5 && (d[1] - d[2]).abs() < 1e-5,
            "pixel {i}: monochrome grain should offset every channel alike, got {d:?}",
        );
        checked += 1;
    }
    assert!(checked > 100);
}

/// `size` quantises the coordinates before hashing, so neighbouring
/// pixels inside one block get the same grain. The old CPU path
/// ignored the control entirely; this pins that it reaches the picture.
#[test]
fn grain_size_makes_particles_rather_than_per_pixel_salt() {
    let src = frame();
    // shadows == highlights makes the tonal weight the same everywhere
    // — `(1 - l) * k + l * k` is `k` for any luma — so the difference
    // from the source IS the grain, not the grain times a weight that
    // varies with the picture underneath.
    let flat = |size: f32| NodeParams {
        grain: Some(klipp_core::FilmGrainSettings {
            enabled: true,
            amount: 0.25,
            size,
            shadows: 1.0,
            highlights: 1.0,
            monochrome: true,
        }),
        ..Default::default()
    };
    let offsets = |size: f32| -> Vec<f32> {
        let (out, _) = run(&one_node_graph(NodeKind::FilmGrain, flat(size)), false);
        out.iter().zip(src.iter()).map(|(a, b)| a - b).collect()
    };
    let fine = offsets(1.0);
    let coarse = offsets(8.0);

    // Count horizontal neighbours inside a row that share a grain
    // offset. At size 8 most do; at size 1 almost none.
    let run_of = |d: &[f32]| {
        let mut same = 0usize;
        let mut total = 0usize;
        for y in 10..(H - 10) {
            for x in 0..(W - 1) {
                let i = ((y * W + x) * 4) as usize;
                let j = ((y * W + x + 1) * 4) as usize;
                if (d[i] - d[j]).abs() < 1e-7 {
                    same += 1;
                }
                total += 1;
            }
        }
        same as f32 / total as f32
    };
    let (fine_share, coarse_share) = (run_of(&fine), run_of(&coarse));
    eprintln!("grain neighbours sharing an offset: size 1 = {fine_share:.3}, size 8 = {coarse_share:.3}");
    assert!(coarse_share > 0.8, "size 8 should give blocks, got {coarse_share}");
    assert!(fine_share < 0.2, "size 1 should be near per-pixel, got {fine_share}");
}

#[test]
fn denoise_matches_the_shader() {
    let g = one_node_graph(
        NodeKind::Denoise,
        NodeParams {
            denoise_luma: Some(0.4),
            denoise_chroma: Some(0.2),
            denoise_radius: Some(3),
            ..Default::default()
        },
    );
    let out = check("denoise", &g, 1e-4);
    if out.is_empty() {
        return;
    }
    let src = frame();
    assert!(worst(&src, &out) > 0.001, "the denoise node changed nothing");
    // The highlight band sits at 3.5 linear. The old 8-bit bracket
    // clamped it to 1.0 before filtering; nothing should now.
    let band = ((H - 4) * W + W / 4) as usize;
    assert!(
        out[band * 4] > 1.5,
        "a highlight above 1.0 must survive the filter, got {}",
        out[band * 4],
    );
}

/// A zero radius used to make the spatial term `exp(-0 / 0)` — a NaN
/// for every weight, and a black frame out of a control the inspector
/// lets you set.
#[test]
fn a_zero_radius_does_not_produce_nans() {
    let g = one_node_graph(
        NodeKind::Denoise,
        NodeParams {
            denoise_luma: Some(0.5),
            denoise_chroma: Some(0.0),
            denoise_radius: Some(0),
            ..Default::default()
        },
    );
    for gpu in [false, true] {
        let (out, stats) = run(&g, gpu);
        if gpu && stats.extends == 0 && stats.flushes == 0 {
            continue;
        }
        let where_ = if gpu { "GPU" } else { "CPU" };
        assert!(
            out.iter().all(|v| v.is_finite()),
            "{where_}: a zero radius produced non-finite pixels",
        );
    }
}

/// The same two nodes with a grade in front of them, so the GPU takes
/// `apply_grain_chain` / `apply_denoise_chain` instead of the per-node
/// entry points. Same shaders, different call path, and the one the
/// viewer actually runs — worth its own check, because the chain sites
/// build their parameters separately and used to skip the guards the
/// per-node sites applied.
#[test]
fn the_chain_variants_match_the_cpu_too() {
    for (name, params, tol) in [
        ("chained grain", grain(false, 2.0), 1e-5),
        (
            "chained denoise",
            NodeParams {
                denoise_luma: Some(0.4),
                denoise_chroma: Some(0.2),
                denoise_radius: Some(3),
                ..Default::default()
            },
            1e-4,
        ),
    ] {
        let kind = if params.grain.is_some() { NodeKind::FilmGrain } else { NodeKind::Denoise };
        let (src, corr, n, out) =
            (NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4(), NodeId::new_v4());
        let nodes = vec![
            node(src, NodeKind::Source, Default::default()),
            node(
                corr,
                NodeKind::Corrector,
                NodeParams { temperature: Some(-20.0), contrast: Some(0.06), ..Default::default() },
            ),
            node(n, kind, params),
            node(out, NodeKind::Output, Default::default()),
        ];
        let node_positions = nodes
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id, NodePos { x: i as f32 * 200.0, y: 0.0 }))
            .collect();
        let g = NodeGraph {
            output_node_id: out,
            nodes,
            edges: vec![
                Edge { from_node: src, from_port: 0, to_node: corr, to_port: 0 },
                Edge { from_node: corr, from_port: 0, to_node: n, to_port: 0 },
                Edge { from_node: n, from_port: 0, to_node: out, to_port: 0 },
            ],
            node_positions,
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };

        let (cpu, _) = run(&g, false);
        let (gpu, stats) = run(&g, true);
        if stats.extends == 0 && stats.flushes == 0 {
            eprintln!("skipping {name}: no usable GPU");
            continue;
        }
        // The Corrector starts the chain and the node extends it, so
        // one extend is what riding the chain looks like here.
        assert!(
            stats.extends >= 1 && stats.cpu_nodes <= 1,
            "{name} should ride the chain behind the grade: {stats:?}",
        );
        let w = worst(&cpu, &gpu);
        eprintln!("{name}: worst CPU vs GPU = {w:e} linear ({stats:?})");
        assert!(w < tol, "{name}: CPU and GPU disagree by {w}");
    }
}
