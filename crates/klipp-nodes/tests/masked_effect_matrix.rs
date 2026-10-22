//! The AR1 matrix (the acceptance spec's C9, phase 2b M2 step 17): every
//! mixable effect masked by every matte producer, through each mask
//! channel, is the oracle's: `klipp_effects::mask_mix` over the pieces the
//! same graph gives without the mix (the input, the effect taken whole,
//! the matte), on the CPU bit for bit and on the GPU within a level, with
//! no effect breaking the chain and no mix refused.
//!
//! The GPU is held to the oracle over its own pieces: the mix is what this
//! proves. A producer's own CPU/GPU gap is its parity test's (a luma key
//! keys 8-bit encoded values on the CPU and floats on the GPU, a step of
//! input moving the matte by a step over its soft edge:
//! `key_matte_parity`), and over a narrow soft edge it would stand in for
//! the mix here.
//!
//! Each producer is reached directly in the clip's graph here. Reaching it
//! through a published Grade-group port needs a picture-truth clip (9c-2),
//! and so do the spec's strict counters for GPU producers: those cases are
//! ignored, by name, until then. A missing GPU leaves the GPU half
//! unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::collections::HashMap;

use klipp_effects::mask_mix::{MaskChannel, MaskMix};
use klipp_effects::BuiltinEffect;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;
const MIX: f32 = 0.75;
/// GPU against the oracle: a level of an 8-bit picture.
const GPU_BAR: f32 = 1.0 / 255.0;
const CHANNELS: [MaskChannel; 3] = [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha];

/// Mixable effects whose node breaks the GPU chain today, masked or not.
/// None: Equalize, the last, records its histogram on the GPU since #1215.
/// The list is exact: an effect that starts declining fails the matrix
/// until it is added here, with its reason.
const KNOWN_DECLINES: [&str; 0] = [];

/// A frame whose luminance, hue and alpha (0 to 1) each run across the
/// picture, so every producer's coverage spans 0 to 1; its left strip is
/// the clean plate's grey, which the difference key leaves out.
fn frame() -> Vec<f32> {
    let plate = klipp_color::transfer::srgb_to_linear(128.0 / 255.0);
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            let alpha = y;
            if i % W < W / 8 {
                [plate, plate, plate, alpha]
            } else {
                [x, 0.15 + 0.7 * y * (1.0 - x), (1.0 - x) * 0.8 + 0.05, alpha]
            }
        })
        .collect()
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn link(a: NodeId, b: NodeId, port: u8) -> Edge {
    Edge { from_node: a, from_port: 0, to_node: b, to_port: port }
}

/// The matte producers C9 names: the GPU's (Qualifier, LumaKey,
/// DifferenceKey, a window Mask) and the CPU's (ExternalMatte,
/// CombineMattes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Producer {
    Qualifier,
    LumaKey,
    DifferenceKey,
    Window,
    ExternalMatte,
    CombineMattes,
    /// The picture itself wired into the mask: the one source whose alpha
    /// varies, since every producer above makes an opaque matte (A = 1).
    Picture,
}

const PRODUCERS: [Producer; 7] = [Producer::Qualifier, Producer::LumaKey, Producer::DifferenceKey, Producer::Window, Producer::ExternalMatte, Producer::CombineMattes, Producer::Picture];

/// The files the disk-reading producers load: a mid-grey clean plate and
/// a horizontal ramp painted as a matte.
struct Files {
    dir: std::path::PathBuf,
    plate: std::path::PathBuf,
    ramp: std::path::PathBuf,
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn files() -> Files {
    static RUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("klipp-c9-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plate = dir.join("plate.png");
    image::RgbaImage::from_pixel(8, 6, image::Rgba([128, 128, 128, 255])).save(&plate).unwrap();
    let ramp = dir.join("ramp.png");
    image::RgbaImage::from_fn(W, H, |x, _| {
        let v = (x * 255 / (W - 1)) as u8;
        image::Rgba([v, v, v, 255])
    })
    .save(&ramp)
    .unwrap();
    Files { dir, plate, ramp }
}

fn luma_key() -> Node {
    node(NodeKind::LumaKey, NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.45, high: 0.5, soft: 0.05, ..Default::default() }), ..Default::default() })
}

fn window() -> Node {
    use klipp_core::{Mask, MaskShape, MaskStack};
    let masks = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.2, ..Mask::default() }] };
    node(NodeKind::Mask, NodeParams { masks: Some(masks), ..Default::default() })
}

/// The producer's nodes, fed from `source`; the last is the matte.
fn producer(p: Producer, source: NodeId, files: &Files) -> (Vec<Node>, Vec<Edge>, NodeId) {
    let one = |n: Node, fed: bool| {
        let id = n.id;
        let edges = if fed { vec![link(source, id, 0)] } else { vec![] };
        (vec![n], edges, id)
    };
    match p {
        Producer::Qualifier => one(
            node(NodeKind::Qualifier, NodeParams {
                qualifier: Some(klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 90.0, sat_low: 0.05, ..Default::default() }),
                ..Default::default()
            }),
            true,
        ),
        Producer::LumaKey => one(luma_key(), true),
        Producer::DifferenceKey => one(
            node(NodeKind::DifferenceKey, NodeParams {
                difference_key: Some(klipp_core::DifferenceKeySettings {
                    enabled: true,
                    plate_path: Some(files.plate.clone()),
                    tolerance: 0.1,
                    soft: 0.3,
                    invert: false,
                    match_space: klipp_core::DifferenceKeyMatchSpace::Rgb,
                }),
                ..Default::default()
            }),
            true,
        ),
        Producer::Window => one(window(), true),
        Producer::ExternalMatte => one(node(NodeKind::ExternalMatte, NodeParams { matte_image_path: Some(files.ramp.clone()), ..Default::default() }), false),
        Producer::Picture => (vec![], vec![], source),
        Producer::CombineMattes => {
            let (a, b) = (luma_key(), window());
            let c = node(NodeKind::CombineMattes, NodeParams::default());
            let edges = vec![link(source, a.id, 0), link(source, b.id, 0), link(a.id, c.id, 0), link(b.id, c.id, 1)];
            let id = c.id;
            (vec![a, b, c], edges, id)
        }
    }
}

/// The pieces of a case.
#[derive(Clone, Copy)]
enum Shape {
    /// The effect's input: the source.
    Input,
    /// The effect taken whole.
    Whole,
    /// The producer's matte alone.
    Matte(Producer),
    /// The effect mixed by `MIX` through `channel`, masked by the producer.
    Mixed(Producer, MaskChannel),
}

/// Where a case's effect sits: straight off the source, or after a grade
/// on a live chain, its producer keyed from the source or from the graded
/// picture (AR1's graded half).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Graded {
    None,
    KeyedFromSource,
    KeyedFromGrade,
}

/// Source -> [Effect] -> Output, a producer off the source into the
/// effect's mask; with `grade`'s Exposure before the effect, the effect's
/// input is the graded picture, and the producer keys what `grade` says.
fn graph_graded(effect: &BuiltinEffect, shape: Shape, files: &Files, grade: Graded) -> NodeGraph {
    let s = node(NodeKind::Source, NodeParams::default());
    let o = node(NodeKind::Output, NodeParams::default());
    let mut nodes = vec![s.clone()];
    let mut edges = Vec::new();
    // The effect's input, and what the producer keys.
    let (picture, keyed) = match grade {
        Graded::None => (s.id, s.id),
        _ => {
            let e = node(NodeKind::Exposure, NodeParams { exposure: Some(0.05), ..Default::default() });
            edges.push(link(s.id, e.id, 0));
            let id = e.id;
            nodes.push(e);
            (id, if grade == Graded::KeyedFromGrade { id } else { s.id })
        }
    };
    let last = match shape {
        Shape::Input => picture,
        Shape::Matte(p) => {
            let (n, e, m) = producer(p, keyed, files);
            nodes.extend(n);
            edges.extend(e);
            m
        }
        Shape::Whole | Shape::Mixed(..) => {
            let blend = match shape {
                Shape::Mixed(_, channel) => Some(EffectBlend { mix: MIX, channel }),
                _ => None,
            };
            let fx = node(NodeKind::Effect, NodeParams { effects: vec![effect.clone()], effect_blend: blend, ..Default::default() });
            edges.push(link(picture, fx.id, 0));
            if let Shape::Mixed(p, _) = shape {
                let (n, e, m) = producer(p, keyed, files);
                nodes.extend(n);
                edges.extend(e);
                edges.push(link(m, fx.id, 1));
            }
            let id = fx.id;
            nodes.push(fx);
            id
        }
    };
    edges.push(link(last, o.id, 0));
    nodes.push(o.clone());
    NodeGraph { output_node_id: o.id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn run_graded(ex: &mut GraphExecutor, effect: &BuiltinEffect, shape: Shape, files: &Files, grade: Graded) -> Vec<f32> {
    ex.execute_linear(&frame(), &graph_graded(effect, shape, files, grade), W, H, 0.5, 12, None, None).unwrap().0
}

fn rows(v: &[f32]) -> Vec<[f32; 4]> {
    v.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
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

/// `klipp_effects::mask_mix` over the pieces an executor gives.
fn oracle(input: &[[f32; 4]], whole: &[[f32; 4]], matte: &[[f32; 4]], channel: MaskChannel) -> Vec<f32> {
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let mix = MaskMix::new(MIX, channel, luma).unwrap();
    let mut out = vec![[0.0f32; 4]; input.len()];
    mix.apply(input, whole, Some(matte), &mut out).unwrap();
    out.into_iter().flatten().collect()
}

/// Every registered effect that takes a mask and a mix (D5's list).
fn mixable_effects() -> Vec<(&'static str, BuiltinEffect)> {
    klipp_effects::registry::effect_types().filter(|t| t.desc.mixable).map(|t| (t.tag, (t.sample)())).collect()
}

/// The matrix, on the CPU and the GPU: each combination against the
/// oracle, every failure named before the test fails.
#[test]
fn every_mixable_node_masked_by_every_matte_matches_the_oracle() {
    check_matrix(Graded::None);
}

/// AR1's graded half: the same matrix with a grade before the effect, so
/// the effect runs on a live chain, and each producer keyed from the
/// source or from the graded picture.
#[test]
fn every_mixable_node_masked_after_a_grade_matches_the_oracle() {
    check_matrix(Graded::KeyedFromSource);
    check_matrix(Graded::KeyedFromGrade);
}

/// C9 for `grade`: every mixable effect by every producer and channel
/// whose coverage spans 0 to 1, on the CPU bit for bit and on the GPU
/// within a level, each against the oracle over the same graph's pieces.
fn check_matrix(grade: Graded) {
    let effects = mixable_effects();
    assert!(!effects.is_empty(), "no mixable effect is registered");
    let files = files();
    let mut cpu = GraphExecutor::new_cpu_only();
    let mut gpu = gpu();
    let none = BuiltinEffect::Gamma { gamma: 1.0 };
    let mattes: HashMap<Producer, Vec<[f32; 4]>> = PRODUCERS.iter().map(|p| (*p, rows(&run_graded(&mut cpu, &none, Shape::Matte(*p), &files, grade)))).collect();
    let gpu_mattes: HashMap<Producer, Vec<[f32; 4]>> = match gpu.as_mut() {
        Some(ex) => PRODUCERS.iter().map(|p| (*p, rows(&run_graded(ex, &none, Shape::Matte(*p), &files, grade)))).collect(),
        None => HashMap::new(),
    };
    // Each case reads its coverage from one channel of the matte, and that
    // channel spans 0 to 1, or the case proves nothing. The producers make
    // an opaque matte, so their alpha is flat: they stay off the alpha
    // channel, which the picture itself covers. Any other flat channel is
    // a fixture that has stopped keying, and fails.
    let luma = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default());
    let coverage = |px: &[f32; 4], channel: MaskChannel| match channel {
        MaskChannel::Red => px[0],
        MaskChannel::Luminance => luma[0] * px[0] + luma[1] * px[1] + luma[2] * px[2],
        MaskChannel::Alpha => px[3],
    };
    let mut cases: Vec<(Producer, MaskChannel)> = Vec::new();
    for p in PRODUCERS {
        for channel in CHANNELS {
            // The picture stands in for the alpha the producers don't make;
            // their mattes cover red and luminance.
            if p == Producer::Picture && channel != MaskChannel::Alpha {
                continue;
            }
            let (lo, hi) = mattes[&p].iter().map(|px| coverage(px, channel)).fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
            if lo < 0.1 && hi > 0.9 {
                cases.push((p, channel));
            } else {
                assert!(channel == MaskChannel::Alpha && p != Producer::Picture && lo == 1.0 && hi == 1.0, "{p:?}'s {channel:?} runs only {lo}..{hi}");
            }
        }
    }
    for channel in CHANNELS {
        assert!(cases.iter().any(|(_, c)| *c == channel), "no mask source spans the {channel:?} channel");
    }
    let input = rows(&run_graded(&mut cpu, &none, Shape::Input, &files, grade));
    let gpu_input = gpu.as_mut().map(|ex| rows(&run_graded(ex, &none, Shape::Input, &files, grade)));
    let mut failures = Vec::new();
    let mut checked = 0;
    for (tag, effect) in &effects {
        let whole = rows(&run_graded(&mut cpu, effect, Shape::Whole, &files, grade));
        let gpu_whole = gpu.as_mut().map(|ex| rows(&run_graded(ex, effect, Shape::Whole, &files, grade)));
        for &(p, channel) in &cases {
            {
                checked += 1;
                let want = oracle(&input, &whole, &mattes[&p], channel);
                let case = format!("{tag} by {p:?} ({channel:?})");
                let got = run_graded(&mut cpu, effect, Shape::Mixed(p, channel), &files, grade);
                if bits(&got) != bits(&want) {
                    failures.push(format!("cpu: {case}: worst {}", worst(&got, &want)));
                }
                if let (Some(ex), Some(gi), Some(gw)) = (gpu.as_mut(), gpu_input.as_ref(), gpu_whole.as_ref()) {
                    let want = oracle(gi, gw, &gpu_mattes[&p], channel);
                    let got = run_graded(ex, effect, Shape::Mixed(p, channel), &files, grade);
                    let w = worst(&got, &want);
                    let stats = ex.last_run_chain_stats();
                    if w > GPU_BAR {
                        failures.push(format!("gpu: {case}: worst {w} {stats:?}"));
                    }
                    // C9's CPU producers (spec, the CPU half): their matte is
                    // made on the CPU, one node each, and the effect and its
                    // mix stay on the GPU in one flush. The source handing
                    // over its CPU pixels is the other CPU node; a combined
                    // matte is a luma key, a window and the combine, all on
                    // the CPU.
                    let producer_nodes = match p {
                        Producer::ExternalMatte => Some(1),
                        Producer::CombineMattes => Some(3),
                        _ => None,
                    };
                    // Keying the graded picture, a CPU producer reads it from the
                    // CPU: the chain is read back for it, and starts again for
                    // the effect (two flushes). The external matte reads no
                    // picture.
                    let flushes = if grade == Graded::KeyedFromGrade && p == Producer::CombineMattes { 2 } else { 1 };
                    if let Some(n) = producer_nodes.filter(|_| !KNOWN_DECLINES.contains(tag)) {
                        if (stats.cpu_nodes, stats.flushes, stats.effect_breaks) != (1 + n, flushes, 0) {
                            failures.push(format!("gpu: {case}: a CPU producer's counters {stats:?}, want {} CPU nodes and {flushes} flush(es)", 1 + n));
                        }
                    }
                    let declined = stats.effect_breaks != 0 || stats.effect_mixes_refused != 0;
                    if declined != KNOWN_DECLINES.contains(tag) {
                        failures.push(format!("gpu: {case}: {} ({} breaks, {} mixes refused)", if declined { "declined" } else { "no longer declines: take it off KNOWN_DECLINES" }, stats.effect_breaks, stats.effect_mixes_refused));
                    }
                }
            }
        }
    }
    eprintln!("C9 ({grade:?}): {checked} cases ({} effects x {} producer-channel pairs), {} failed", effects.len(), cases.len(), failures.len());
    assert!(failures.is_empty(), "{} of {checked} cases:\n{}", failures.len(), failures.join("\n"));
}

/// C9's counters for the GPU's producers: the matte made on the chain, so
/// the one CPU node is the source handing over its CPU pixels (as in C9's
/// CPU-producer counts, 1 + the producer's) and the case is one flush
/// (#1242 for a window, #1266 for the keys): keyed from the picture the
/// effect takes, from the graded picture, or from the source while the
/// effect sits after a grade.
#[test]
fn gpu_producers_keep_the_whole_case_on_the_chain() {
    let files = files();
    let Some(mut ex) = gpu() else { return };
    for (tag, effect) in mixable_effects() {
        for p in [Producer::Qualifier, Producer::LumaKey, Producer::DifferenceKey, Producer::Window] {
            for grade in [Graded::None, Graded::KeyedFromGrade, Graded::KeyedFromSource] {
                run_graded(&mut ex, &effect, Shape::Mixed(p, MaskChannel::Luminance), &files, grade);
                let stats = ex.last_run_chain_stats();
                assert_eq!((stats.cpu_nodes, stats.flushes), (1, 1), "{tag} by {p:?} ({grade:?}): {stats:?}");
            }
        }
    }
}

/// The control for the counters above: a key the GPU doesn't draw for an
/// effect's mask is counted as a CPU node, beside the source's. A
/// qualifier with finesse (its blur runs on the CPU, as the grade's
/// secondary leaves it, #1013), and a key read by a blend too, so its
/// pixels are wanted on the CPU.
#[test]
fn a_key_the_gpu_does_not_draw_counts_as_a_cpu_node() {
    let files = files();
    let Some(mut ex) = gpu() else { return };
    let (_, effect) = mixable_effects().into_iter().next().expect("a mixable effect");
    let mut g = graph_graded(&effect, Shape::Mixed(Producer::Qualifier, MaskChannel::Luminance), &files, Graded::None);
    let q = g.nodes.iter_mut().find(|n| n.kind == NodeKind::Qualifier).expect("the qualifier");
    q.params.qualifier.as_mut().unwrap().blur = 2.0;
    ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap();
    assert!(ex.last_run_chain_stats().cpu_nodes >= 2, "finesse: {:?}", ex.last_run_chain_stats());
    let mut g = graph_graded(&effect, Shape::Mixed(Producer::LumaKey, MaskChannel::Luminance), &files, Graded::None);
    let key = g.nodes.iter().find(|n| n.kind == NodeKind::LumaKey).expect("the key").id;
    let blend = node(NodeKind::Blend, NodeParams { mix: Some(0.5), ..Default::default() });
    let fx = g.edges.iter().find(|e| e.from_node == key).unwrap().to_node;
    let out = g.output_node_id;
    g.edges.retain(|e| e.to_node != out);
    g.edges.extend([link(fx, blend.id, 0), link(key, blend.id, 1), link(blend.id, out, 0)]);
    g.nodes.push(blend);
    ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap();
    assert!(ex.last_run_chain_stats().cpu_nodes >= 2, "a second reader: {:?}", ex.last_run_chain_stats());
    // And the same graph without either reason takes the source's alone.
    ex.execute_linear(&frame(), &graph_graded(&effect, Shape::Mixed(Producer::Qualifier, MaskChannel::Luminance), &files, Graded::None), W, H, 0.5, 12, None, None).unwrap();
    assert_eq!(ex.last_run_chain_stats().cpu_nodes, 1);
}

// C9 through a published Grade-group port (D1) is
// klipp-compose's `published_producer_masks.rs`: the lowering from a
// clip's published port to the run's nodes is the renderer's (9c-2).
