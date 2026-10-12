//! A 3D LUT and a colour-space transform as picture nodes (#1746): added
//! to a clip's picture as the Picture view adds them, and rendered the
//! same by the GPU (the viewer's route) and the CPU, each as the effect's
//! kernels draw it on the clip's picture. A LUT whose file can't be read
//! passes the picture and is a declined node that fails an export, never
//! a silent passthrough.

use std::path::PathBuf;
use std::sync::Arc;

use klipp_compose::colour_effects::ColourEffects;
use klipp_compose::{FrameSource, Inspect, Inspection, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, WorkingColorspace};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.05 + 0.8 * fx, 0.1 + 0.6 * fy, 0.7 * (1.0 - fx) * fy + 0.02, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A warm, crushed look as a `.cube` on disk, written once per process.
fn look_file() -> PathBuf {
    static FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("klipp-colour-effects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("warm.cube");
        let n = 9;
        let mut rows = Vec::new();
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let [r, g, b] = [r, g, b].map(|i| i as f32 / (n - 1) as f32);
                    rows.push([(r * 1.1 + 0.03).min(1.0), g.powf(1.2), b * 0.8]);
                }
            }
        }
        klipp_color::write_cube(&path, &rows, n as u32, "warm").unwrap();
        path
    })
    .clone()
}

fn lut(path: &str) -> E {
    E::Lut3D { path: path.into(), opacity: 0.9, tetrahedral: true, prepared: None }
}

/// S-Gamut3.Cine / S-Log3 read into the working space, as for a log clip
/// taken in as if it were not.
fn transform() -> E {
    E::ColourSpaceTransform { input_gamut: 6, input_curve: 2, output_gamut: 0, output_curve: 0, prepared: None }
}

/// A clip of the chart with `fx` added to its picture as the Picture view
/// adds nodes: each after the last, from the display step.
fn clip(fx: &[E]) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let mut after = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let mut ops = Vec::new();
    for e in fx {
        let tag = serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_owned();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(&tag),
            "legacy": { "payload": serde_json::to_value(e).unwrap() },
        }))
        .unwrap();
        let id = node.id;
        ops.push(GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: c.id.0 }, [after]), node: Box::new(node) });
        after = id;
    }
    if !ops.is_empty() {
        c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("adding {fx:?}: {e}"));
    }
    c
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(gpu: bool, c: &Clip) -> (Vec<f32>, Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(&sequence(c.clone()), tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default())
}

/// The worst difference in 8-bit sRGB levels, colour channels.
fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// `fx` drawn by their kernels on the clip's plain picture, prepared as
/// the renderer prepares them.
fn oracle(fx: &[E]) -> Vec<f32> {
    let (plain, _) = render(false, &clip(&[]));
    let mut fx = fx.to_vec();
    ColourEffects::default().prepare(&mut fx, WorkingColorspace::Rec709Linear);
    klipp_effects::apply_effects(&fx, plain, W, H)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

#[test]
fn each_node_renders_the_same_on_the_gpu_and_the_cpu() {
    let path = look_file();
    let (plain, _) = render(false, &clip(&[]));
    for (name, fx) in [
        ("LUT", vec![lut(path.to_str().unwrap())]),
        ("transform", vec![transform()]),
        ("transform then LUT", vec![transform(), lut(path.to_str().unwrap())]),
    ] {
        let c = clip(&fx);
        let want = oracle(&fx);
        assert!(worst_levels(&want, &plain) > 4, "{name}: the node changes the picture");
        let (cpu, found) = render(false, &c);
        assert!(found.declines.is_empty(), "{name}: {:?}", found.declines);
        let worst = worst_levels(&cpu, &want);
        eprintln!("{name}: CPU against its kernels: worst {worst} levels");
        assert!(worst <= 1, "{name}: the CPU draws {worst} levels from the kernels");
        if !gpu() {
            eprintln!("UNVERIFIED: no GPU");
            continue;
        }
        let (on_gpu, found) = render(true, &c);
        assert!(found.declines.is_empty(), "{name}: {:?}", found.declines);
        assert_eq!((found.stats.effects_on_cpu, found.stats.effects_on_cpu_kernels), (0, 0), "{name}: the GPU ran them");
        let worst = worst_levels(&on_gpu, &want);
        eprintln!("{name}: GPU against its kernels: worst {worst} levels");
        assert!(worst <= 1, "{name}: the GPU draws {worst} levels from the kernels");
    }
}

#[test]
fn a_lut_whose_file_is_gone_is_a_failing_decline_with_its_path() {
    let (plain, _) = render(false, &clip(&[]));
    let c = clip(&[lut("/nowhere/gone.cube")]);
    for gpu_on in [false, true] {
        if gpu_on && !gpu() {
            continue;
        }
        let (out, found) = render(gpu_on, &c);
        assert_eq!(worst_levels(&out, &plain), 0, "gpu {gpu_on}: the picture passes");
        let missing: Vec<_> = found.declines.iter().filter(|d| d.outcome == klipp_nodes::DeclineOutcome::FileMissing).collect();
        assert_eq!(missing.len(), 1, "gpu {gpu_on}: one decline: {:?}", found.declines);
        let d = missing[0];
        assert!(d.fails() && d.shown(), "an export refuses it and the viewer names it");
        assert!(d.reason.contains("/nowhere/gone.cube"), "{}", d.plain());
        assert_eq!(d.clip, Some(c.id));
        let node = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("lut_3d")).unwrap().id;
        assert_eq!(d.node, node, "the LUT's own node is named");
    }
}

#[test]
fn a_lut_file_changed_on_disk_is_read_again() {
    let dir = std::env::temp_dir().join(format!("klipp-colour-effects-change-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("look.cube");
    let identity: Vec<[f32; 3]> = (0..8).map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect();
    klipp_color::write_cube(&path, &identity, 2, "id").unwrap();
    let mut c = ColourEffects::default();
    let first = c.lut(path.to_str().unwrap(), WorkingColorspace::Rec709Linear).unwrap();
    let dark: Vec<[f32; 3]> = identity.iter().map(|p| p.map(|v| v * 0.5)).collect();
    // A later modification time than the first write's, on any file system.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    klipp_color::write_cube(&path, &dark, 2, "dark").unwrap();
    let second = c.lut(path.to_str().unwrap(), WorkingColorspace::Rec709Linear).unwrap();
    assert_ne!(first.data, second.data, "the changed file is read again");
    let _ = std::fs::remove_dir_all(dir);
}
