//! Every built-in effect, run as a node of a plan, draws what the legacy
//! effects chain draws (#1089, phase 2b M2, step 11a; the acceptance
//! spec's A1 and A2).
//!
//! Each registry type runs in a one-effect graph (source -> effect ->
//! output) at two settings: its reset value, and each numeric parameter at
//! 0.7 of its slider range. On the CPU the node's picture is the legacy
//! chain's (`apply_effects_with_luma`) bit for bit. On the GPU it is the
//! legacy GPU chain's (`GpuEffectsPipeline::apply_effects`) bit for bit,
//! in one segment: no CPU node but the source (which every run counts),
//! the one flush the output's read-back, one submit. A missing GPU leaves A2 unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::registry::{effect_types, EffectType, ParamKind};
use klipp_effects::BuiltinEffect;
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::NodeParams;
use klipp_nodes::GraphExecutor;

const W: u32 = 64;
const H: u32 = 36;

/// The types M2 doesn't make effect nodes, each with its reason. Every
/// other registry type must be covered.
const EXEMPT: &[(&str, &str)] = &[
    ("opacity", "a compositing weight: the clip's opacity, not a node"),
    ("transform", "the clip's placement, not a node"),
    ("speed_ramp", "acts on time, at the head"),
    ("temporal_denoise", "runs at the head, before the grade"),
    ("frei0r", "a CPU plug-in: a segment of its own (R-7.9)"),
    ("rolling_shutter_correction", "analysis-driven: a legacy node in the plan, step 12 (spec F)"),
    ("video_stabilization", "analysis-driven: a legacy node in the plan, step 12 (spec F)"),
    ("deflicker", "analysis-driven: a legacy node in the plan, step 12 (spec F)"),
    // Their samples have nothing prepared and pass the picture; prepared,
    // they're held to their kernels in klipp-effects' cpu_gpu_effect_parity
    // and klipp-compose's colour_effects_render.
    ("lut_3d", "reads a file the renderer prepares (#1746)"),
    ("colour_space_transform", "takes tables the renderer prepares (#1746)"),
    ("merge", "draws its fg port's picture with the layer mixer; alone it passes the picture (#1746)"),
    ("media_source", "draws another media item's frame the render reads for it; alone it is a clear picture (#1746)"),
    ("clip_below", "draws what the render composited below the clip; alone it is a clear picture (#1746)"),
    ("grade_compound", "a marker for a grade compound, which the plan runs as a compound node (#1746)"),
];

/// The patterned linear frame of `cpu_gpu_effect_parity`, with an alpha
/// ramp across x.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let mut px = [fx, fy, 1.0 - fx * fy, fx];
            if (10..20).contains(&x) && (5..15).contains(&y) {
                px = [0.9, 0.05, 0.05, fx];
            }
            if (30..40).contains(&x) && (5..15).contains(&y) {
                px = [0.05, 0.8, 0.1, fx];
            }
            if (45..55).contains(&x) && (20..30).contains(&y) {
                px = [0.02, 0.03, 0.9, fx];
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

/// `ty`'s effect with each numeric parameter at 0.7 of its slider range.
fn at_seven_tenths(ty: &EffectType) -> BuiltinEffect {
    let mut v = serde_json::to_value((ty.sample)()).unwrap();
    for p in ty.desc.params {
        let Some((lo, hi)) = p.soft else { continue };
        let value = lo + 0.7 * (hi - lo);
        let json = match p.kind {
            ParamKind::Float => serde_json::json!(value),
            ParamKind::Whole => serde_json::json!(value.round().max(0.0) as u32),
            _ => continue,
        };
        match p.index {
            None => v[p.field] = json,
            Some(i) => v[p.field][i] = json,
        }
    }
    serde_json::from_value(v).unwrap_or_else(|e| panic!("{}: {e}", ty.tag))
}

fn graph(effect: &BuiltinEffect) -> NodeGraph {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let s = node(NodeKind::Source, NodeParams::default());
    let e = node(NodeKind::Effect, NodeParams { effects: vec![effect.clone()], ..Default::default() });
    let o = node(NodeKind::Output, NodeParams::default());
    let edges = vec![
        Edge { from_node: s.id, from_port: 0, to_node: e.id, to_port: 0 },
        Edge { from_node: e.id, from_port: 0, to_node: o.id, to_port: 0 },
    ];
    NodeGraph { output_node_id: o.id, nodes: vec![s, e, o], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// The covered types, at both settings.
fn cases() -> (Vec<(&'static str, BuiltinEffect)>, usize) {
    let mut cases = Vec::new();
    let mut exempt = 0;
    for ty in effect_types() {
        if EXEMPT.iter().any(|(t, _)| *t == ty.tag) {
            exempt += 1;
            continue;
        }
        cases.push((ty.tag, (ty.sample)()));
        cases.push((ty.tag, at_seven_tenths(ty)));
    }
    (cases, exempt)
}

fn luma() -> [f32; 3] {
    klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::default())
}

/// The mean change a setting makes, in 8-bit levels of linear value.
fn mean_change(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32 * 255.0
}

#[test]
fn every_effect_node_matches_the_legacy_chain_on_the_cpu() {
    let (cases, exempt) = cases();
    assert_eq!(cases.len() / 2 + exempt, effect_types().count(), "covered + exempt is every type");
    // The luma key among them (#1638).
    assert_eq!(effect_types().count(), 67, "the registry's size the spec counts");
    let mut unchanged = Vec::new();
    for (tag, effect) in &cases {
        let mut ex = GraphExecutor::new_cpu_only();
        let got = ex.execute_linear(&frame(), &graph(effect), W, H, 0.5, 12, None, None).unwrap().0;
        let want = klipp_effects::apply_effects_with_luma(std::slice::from_ref(effect), frame(), W, H, luma());
        assert_eq!(bits(&got), bits(&want), "{tag} {effect:?}");
        if mean_change(&got, &frame()) < 0.05 {
            unchanged.push(*tag);
        }
    }
    // Each type changes the frame at one of its two settings at least.
    let silent: Vec<_> = unchanged.iter().filter(|t| unchanged.iter().filter(|u| u == t).count() == 2).collect();
    assert!(silent.is_empty(), "types that change nothing at either setting: {silent:?}");
}

#[test]
fn every_effect_node_matches_the_legacy_chain_on_the_gpu() {
    let mut probe = GraphExecutor::new();
    if probe.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let mut legacy = klipp_effects::gpu::GpuEffectsPipeline::new().unwrap();
    legacy.set_luma_coefs(luma());
    // The source is counted as a CPU node by every run: the effect adds none.
    let source_only = {
        let mut g = graph(&BuiltinEffect::Invert { mix: 0.0 });
        let (s, o) = (g.nodes[0].id, g.nodes[2].id);
        g.nodes.remove(1);
        g.edges = vec![Edge { from_node: s, from_port: 0, to_node: o, to_port: 0 }];
        let mut ex = GraphExecutor::new();
        ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap();
        ex.last_run_chain_stats().cpu_nodes
    };
    let (cases, _) = cases();
    for (tag, effect) in &cases {
        if klipp_effects::has_frei0r(std::slice::from_ref(effect)) {
            panic!("{tag} has no GPU arm but isn't exempt");
        }
        let mut ex = GraphExecutor::new();
        let got = ex.execute_linear(&frame(), &graph(effect), W, H, 0.5, 12, None, None).unwrap().0;
        let stats = ex.last_run_chain_stats();
        let want = legacy.apply_effects(std::slice::from_ref(effect), &frame(), W, H);
        assert_eq!(bits(&got), bits(&want), "{tag} {effect:?}: native GPU against the legacy GPU chain");
        // Every effect one segment, Equalize too since its histogram is
        // built on the GPU (#1215; it was A2's one pinned break).
        assert_eq!((stats.cpu_nodes, stats.flushes, stats.submits, stats.effect_breaks), (source_only, 1, 1, 0), "{tag}: one segment: {stats:?}");
    }
}

/// A3: eight GPU effects in a row, each its own node, run as one segment
/// (one submit, one flush: the output's read-back), draw the legacy chain's
/// picture, and a second identical frame builds no bind group.
#[test]
fn a_chain_of_effect_nodes_submits_once_per_segment() {
    let mut ex = GraphExecutor::new();
    if ex.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let tags = ["gaussian_blur", "outer_glow", "vignette", "film_grain", "h_s_v_adjust", "levels", "sharpen", "chromatic_aberration"];
    let effects: Vec<BuiltinEffect> = tags
        .iter()
        .map(|tag| at_seven_tenths(effect_types().find(|t| t.tag == *tag).unwrap_or_else(|| panic!("{tag} isn't a registry type"))))
        .collect();
    let mut g = graph(&effects[0]);
    let (out, mut prev) = (g.nodes[2].id, g.nodes[1].id);
    g.edges.retain(|e| e.to_node != out);
    for effect in &effects[1..] {
        let n = Node { id: NodeId::new_v4(), kind: NodeKind::Effect, label: String::new(), enabled: true, params: NodeParams { effects: vec![effect.clone()], ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: vec![] };
        g.edges.push(Edge { from_node: prev, from_port: 0, to_node: n.id, to_port: 0 });
        prev = n.id;
        g.nodes.push(n);
    }
    g.edges.push(Edge { from_node: prev, from_port: 0, to_node: out, to_port: 0 });

    let got = ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap().0;
    let stats = ex.last_run_chain_stats();
    assert_eq!((stats.flushes, stats.submits, stats.effect_breaks), (1, 1, 0), "{stats:?}");
    let mut legacy = klipp_effects::gpu::GpuEffectsPipeline::new().unwrap();
    legacy.set_luma_coefs(luma());
    assert_eq!(bits(&got), bits(&legacy.apply_effects(&effects, &frame(), W, H)), "the legacy chain's picture");
    let built = ex.gpu_pipeline_mut().unwrap().chain_bind_group_build_count();
    ex.execute_linear(&frame(), &g, W, H, 0.5, 12, None, None).unwrap();
    assert_eq!(ex.gpu_pipeline_mut().unwrap().chain_bind_group_build_count(), built, "a second frame builds no bind group");
}

/// A5: the effects that work on display-encoded values keep their
/// encoding as nodes, in every working space: native against legacy, on
/// the CPU and the GPU, with the working space's luminance weights.
/// (Their authored colours' conversion from BT.709 is the renderer's; see
/// klipp-compose's effects_follow_grade.)
#[test]
fn display_encoded_effect_nodes_keep_their_encoding_in_every_working_space() {
    use klipp_core::WorkingColorspace as Ws;
    let encoded: Vec<&EffectType> = effect_types()
        .filter(|t| t.desc.domain == klipp_effects::registry::ColourDomain::DisplayEncoded && !EXEMPT.iter().any(|(e, _)| *e == t.tag))
        .collect();
    assert!(encoded.len() >= 6, "the display-encoded types: {:?}", encoded.iter().map(|t| t.tag).collect::<Vec<_>>());
    let gpu_ok = GraphExecutor::new().gpu_pipeline_or_init().is_some();
    for ws in [Ws::Rec709Linear, Ws::AcesCG, Ws::Bt2020Linear] {
        let luma = klipp_color::linear::luma_coefs(ws);
        let mut cpu = GraphExecutor::new_cpu_only();
        cpu.set_working_colorspace(ws);
        let mut gpu = gpu_ok.then(|| {
            let mut ex = GraphExecutor::new();
            ex.set_working_colorspace(ws);
            let mut legacy = klipp_effects::gpu::GpuEffectsPipeline::new().unwrap();
            legacy.set_luma_coefs(luma);
            (ex, legacy)
        });
        for ty in &encoded {
            for effect in [(ty.sample)(), at_seven_tenths(ty)] {
                let got = cpu.execute_linear(&frame(), &graph(&effect), W, H, 0.5, 12, None, None).unwrap().0;
                let want = klipp_effects::apply_effects_with_luma(std::slice::from_ref(&effect), frame(), W, H, luma);
                assert_eq!(bits(&got), bits(&want), "cpu, {ws:?}, {}", ty.tag);
                if let Some((ex, legacy)) = gpu.as_mut() {
                    let got = ex.execute_linear(&frame(), &graph(&effect), W, H, 0.5, 12, None, None).unwrap().0;
                    assert_eq!(bits(&got), bits(&legacy.apply_effects(std::slice::from_ref(&effect), &frame(), W, H)), "gpu, {ws:?}, {}", ty.tag);
                }
            }
        }
    }
}

/// A6: scene-linear values past white and below black pass through the
/// nodes as through the legacy chain, with nothing turned into NaN or
/// infinity: every covered type, native against legacy, CPU and GPU.
#[test]
fn effect_nodes_pass_hdr_and_negative_values_as_the_legacy_chain_does() {
    let hdr: Vec<f32> = frame()
        .chunks_exact(4)
        .enumerate()
        .flat_map(|(i, p)| {
            let s = -0.05 + 8.05 * (i as f32 / (W * H) as f32);
            [p[0] * s, p[1] * s, p[2] * s, p[3]]
        })
        .collect();
    let mut cpu = GraphExecutor::new_cpu_only();
    let mut gpu = {
        let mut ex = GraphExecutor::new();
        ex.gpu_pipeline_or_init().is_some().then(|| {
            let mut legacy = klipp_effects::gpu::GpuEffectsPipeline::new().unwrap();
            legacy.set_luma_coefs(luma());
            (ex, legacy)
        })
    };
    let (cases, _) = cases();
    for (tag, effect) in &cases {
        let got = cpu.execute_linear(&hdr, &graph(effect), W, H, 0.5, 12, None, None).unwrap().0;
        let want = klipp_effects::apply_effects_with_luma(std::slice::from_ref(effect), hdr.clone(), W, H, luma());
        assert_eq!(bits(&got), bits(&want), "cpu, {tag}");
        if let Some((ex, legacy)) = gpu.as_mut() {
            let got = ex.execute_linear(&hdr, &graph(effect), W, H, 0.5, 12, None, None).unwrap().0;
            let want = legacy.apply_effects(std::slice::from_ref(effect), &hdr, W, H);
            assert_eq!(bits(&got), bits(&want), "gpu, {tag}");
            assert!(got.iter().all(|v| v.is_finite()), "gpu, {tag}: a NaN or an infinity");
        }
        assert!(got.iter().all(|v| v.is_finite()), "cpu, {tag}: a NaN or an infinity");
    }
}
