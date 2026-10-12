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
        // Luma at 0, so its sigma is the floor: the chain site must
        // pass the two sigmas in the same order as the per-node one.
        ("chained chroma-only denoise", denoise(0.0, 0.6), 1e-4),
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

// ── Luma and Chroma act separately (#1039) ───────────────────────────
//
// The filter splits each pixel into its luma `Y` and chroma
// `C = rgb − Y·(1,1,1)` and smooths each on its own, both edge-stopping
// on the luma difference: the Luma slider is Y's range sigma, the
// Chroma slider C's, and a slider at 0 leaves its part as it is. They
// used to be averaged into one sigma for all of rgb, so Chroma never
// smoothed colour noise by itself.

const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn denoise(luma: f32, chroma: f32) -> NodeParams {
    NodeParams {
        denoise_luma: Some(luma),
        denoise_chroma: Some(chroma),
        denoise_radius: Some(3),
        ..Default::default()
    }
}

/// A small deterministic noise source in [-1, 1].
struct Noise(u32);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }
}

fn y_of(px: &[f32], i: usize) -> f32 {
    REC709[0] * px[i * 4] + REC709[1] * px[i * 4 + 1] + REC709[2] * px[i * 4 + 2]
}

fn chroma_of(px: &[f32], i: usize) -> [f32; 3] {
    let y = y_of(px, i);
    [px[i * 4] - y, px[i * 4 + 1] - y, px[i * 4 + 2] - y]
}

/// What a camera frame's noise looks like to the filter: a warm, dim
/// left half (luma 0.2) and a brighter right half (luma 0.8), a hard
/// luma edge between, under both luma noise (the same offset on every
/// channel) and independent colour noise (offsets with no luma in them).
fn noisy_frame() -> Vec<f32> {
    // Two directions with no luma in them.
    let u = [REC709[1], -REC709[0], 0.0];
    let v = [0.0, REC709[2], -REC709[1]];
    // A warm tint with no luma either, so each half's luma is exact.
    let tint = [0.06 * u[0], 0.06 * u[1], 0.06 * u[2]];
    let mut n = Noise(7);
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let base = if x < W / 2 { 0.2 } else { 0.8 };
            let g = 0.04 * n.next();
            let (a, b) = (0.25 * n.next(), 0.25 * n.next());
            for c in 0..3 {
                px[i + c] = base + tint[c] + g + a * u[c] + b * v[c];
            }
            px[i + 3] = 1.0;
        }
    }
    px
}

fn run_on(src: &[f32], g: &NodeGraph, gpu: bool) -> Option<Vec<f32>> {
    let mut exec = if gpu { GraphExecutor::new() } else { GraphExecutor::new_cpu_only() };
    let out = exec.execute_linear(src, g, W, H, 0.0, 7, None, None).expect("graph runs").0;
    let stats = exec.last_run_chain_stats();
    if gpu && stats.extends == 0 && stats.flushes == 0 {
        eprintln!("no usable GPU: the GPU arm is skipped");
        return None;
    }
    Some(out)
}

/// The pixels of a block well inside one half, away from the frame edge
/// and the seam, so the edge clamp and the edge itself don't enter.
fn interior(left: bool) -> Vec<usize> {
    let x0 = if left { 10 } else { W / 2 + 10 };
    let mut v = Vec::new();
    for y in 10..(H - 10) {
        for x in x0..(x0 + W / 2 - 20) {
            v.push((y * W + x) as usize);
        }
    }
    v
}

/// The spread of a per-pixel quantity: its standard deviation.
fn spread(vals: impl Iterator<Item = f32>) -> f32 {
    let v: Vec<f32> = vals.collect();
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32).sqrt()
}

fn chroma_spread(px: &[f32], pixels: &[usize]) -> f32 {
    (0..3)
        .map(|c| spread(pixels.iter().map(|&i| chroma_of(px, i)[c])))
        .fold(0.0, f32::max)
}

fn luma_spread(px: &[f32], pixels: &[usize]) -> f32 {
    spread(pixels.iter().map(|&i| y_of(px, i)))
}

/// The mean luma of one column, rows clear of the frame edge.
fn column_luma(px: &[f32], x: u32) -> f32 {
    let rows = 10..(H - 10);
    let n = rows.len() as f32;
    rows.map(|y| y_of(px, (y * W + x) as usize)).sum::<f32>() / n
}

/// Runs one slider pair on [`noisy_frame`], on the CPU and on the GPU
/// when there is one, and checks the luma edge on both: the columns
/// either side of the seam keep their side's luma. Returns each arm's
/// (luma, chroma) spread ratios, after over before, worst of the halves.
fn ratios(luma: f32, chroma: f32) -> Vec<(&'static str, f32, f32)> {
    let src = noisy_frame();
    let g = one_node_graph(NodeKind::Denoise, denoise(luma, chroma));
    let mut found = Vec::new();
    for gpu in [false, true] {
        let Some(out) = run_on(&src, &g, gpu) else { continue };
        let side = if gpu { "GPU" } else { "CPU" };
        let (mut ly, mut lc) = (0.0f32, 0.0f32);
        for left in [true, false] {
            let px = interior(left);
            ly = ly.max(luma_spread(&out, &px) / luma_spread(&src, &px));
            lc = lc.max(chroma_spread(&out, &px) / chroma_spread(&src, &px));
        }
        eprintln!("{side} luma {luma} chroma {chroma}: luma spread x{ly:.3}, chroma spread x{lc:.3}");
        for x in [W / 2 - 2, W / 2 - 1, W / 2, W / 2 + 1] {
            let d = (column_luma(&out, x) - column_luma(&src, x)).abs();
            assert!(d < 0.005, "{side} luma {luma} chroma {chroma}: the luma edge blurred at column {x} by {d}");
        }
        found.push((side, ly, lc));
    }
    found
}

/// CPU and GPU agree for unequal sliders, either at 0, on a frame with
/// hue, brightness, an edge and noise in both — and on the realistic
/// noisy frame.
#[test]
fn denoise_matches_the_shader_for_every_slider_pair() {
    let mut hue = frame();
    let mut n = Noise(3);
    for p in hue.chunks_exact_mut(4) {
        for c in p.iter_mut().take(3) {
            *c = (*c + 0.03 * n.next()).max(0.0);
        }
    }
    for src in [hue, noisy_frame()] {
        for (luma, chroma) in [(0.4, 0.2), (0.0, 0.6), (0.6, 0.0), (0.05, 0.5), (0.3, 0.3), (0.02, 0.02)] {
            let g = one_node_graph(NodeKind::Denoise, denoise(luma, chroma));
            let cpu = run_on(&src, &g, false).unwrap();
            let Some(gpu) = run_on(&src, &g, true) else { return };
            let w = worst(&cpu, &gpu);
            eprintln!("denoise luma {luma} chroma {chroma}: worst CPU vs GPU = {w:e} linear");
            // 1e-4 linear is under one 8-bit level anywhere on the curve.
            assert!(w < 1e-4, "luma {luma} chroma {chroma}: CPU and GPU disagree by {w}");
            assert!(worst(&src, &cpu) > 0.001, "luma {luma} chroma {chroma} changed nothing");
        }
    }
}

/// Luma alone smooths the luma noise and leaves the colour noise as it
/// is: the chroma part is not averaged at all.
#[test]
fn luma_alone_smooths_luma_and_leaves_chroma() {
    for (side, ly, lc) in ratios(0.15, 0.0) {
        assert!(ly < 0.5, "{side}: luma noise not smoothed (x{ly})");
        assert!((lc - 1.0).abs() < 0.01, "{side}: chroma noise changed (x{lc})");
    }
}

/// Chroma alone smooths the colour noise and leaves the luma noise as
/// it is.
#[test]
fn chroma_alone_smooths_chroma_and_leaves_luma() {
    for (side, ly, lc) in ratios(0.0, 0.15) {
        assert!(lc < 0.5, "{side}: chroma noise not smoothed (x{lc})");
        assert!((ly - 1.0).abs() < 0.01, "{side}: luma noise changed (x{ly})");
    }
}

/// Both together smooth both.
#[test]
fn luma_and_chroma_together_smooth_both() {
    for (side, ly, lc) in ratios(0.15, 0.15) {
        assert!(ly < 0.5, "{side}: luma noise not smoothed (x{ly})");
        assert!(lc < 0.5, "{side}: chroma noise not smoothed (x{lc})");
    }
}

/// The filter before #1039, transcribed: one sigma, the sliders'
/// average, weighing all of rgb by the luma difference.
fn old_filter(px: &[f32], spatial_sigma: f32, color_sigma: f32) -> Vec<f32> {
    let (w, h) = (W as i32, H as i32);
    let kernel = (spatial_sigma * 2.0).ceil().min(10.0) as i32;
    let spatial_denom = 2.0 * spatial_sigma * spatial_sigma;
    let color_denom = 2.0 * color_sigma * color_sigma;
    let mut out = vec![0.0f32; px.len()];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let center_luma = y_of(px, idx);
            let mut sum = [0.0f32; 3];
            let mut total = 0.0f32;
            for dy in -kernel..=kernel {
                for dx in -kernel..=kernel {
                    let n = ((y + dy).clamp(0, h - 1) * w + (x + dx).clamp(0, w - 1)) as usize;
                    let spatial_w = (-((dx * dx + dy * dy) as f32) / spatial_denom).exp();
                    let d = center_luma - y_of(px, n);
                    let weight = spatial_w * (-d * d / color_denom).exp();
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += px[n * 4 + c] * weight;
                    }
                    total += weight;
                }
            }
            for c in 0..3 {
                out[idx * 4 + c] = sum[c] / total;
            }
            out[idx * 4 + 3] = px[idx * 4 + 3];
        }
    }
    out
}

/// With the two sliders equal, both parts share one weight, and
/// `Y' + C'` is that weight's mean of rgb: the old filter, whose one
/// sigma was the sliders' average, up to float rounding. Measured on
/// the noisy frame at radius 3, sliders 0.05 to 0.6: a worst of 1.1e-6
/// linear and a mean of 9e-8. The bound is 1e-5, far under one 8-bit
/// level.
#[test]
fn equal_sliders_are_the_old_filter() {
    let src = noisy_frame();
    for s in [0.05f32, 0.15, 0.3, 0.6] {
        let new = run_on(&src, &one_node_graph(NodeKind::Denoise, denoise(s, s)), false).unwrap();
        let old = old_filter(&src, 3.0, s);
        let worst_d = worst(&new, &old);
        let mean_d = new.iter().zip(&old).map(|(a, b)| (a - b).abs()).sum::<f32>() / new.len() as f32;
        eprintln!("equal sliders {s}: new vs old worst {worst_d:e}, mean {mean_d:e} linear");
        assert!(worst_d < 1e-5, "equal sliders {s}: moved by {worst_d} at worst");
    }
}
