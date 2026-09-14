//! Headless engine harness.
//!
//! Replaces "open the GUI app, import a clip, scrub to N, apply a
//! grade, screenshot, report back" loops with a CLI that produces the
//! same pixels and a JSON of stage timings.
//!
//! Usage:
//!   klipp-smoke --clip /path/to/file.mp4 --time 1.5 \
//!               [--frames N] [--preset warm | --graph g.json] \
//!               [--width 1920] [--out frame.png] [--metrics perf.json]
//!
//! Two decode modes:
//!   * `--frames 1` (default) seeks to `--time` via
//!     `generate_thumbnail_at_size` — the GUI's cold-start poster path.
//!   * `--frames N` opens a `SequentialFrameDecoder` and pulls N
//!     consecutive frames, which is what playback actually does and is
//!     far cheaper than N seeks.
//!
//! With `--preset` or `--graph` the decoded frame is taken through the
//! real grade pipeline — sRGB u8 to linear f32, `GraphExecutor`, then
//! `display_transform_ws` back to u8 — so a grade can be inspected as
//! pixels without launching the app.
//!
//! Caveat worth knowing: the single-frame path goes through the
//! thumbnail decoder, which has no HDR/PQ handling (it never reads
//! `color_trc`). PQ sources therefore come out washed out here while
//! playback renders them correctly. That is a real difference between
//! the two paths, not an artefact of this tool.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use klipp_nodes::{
    executor::GraphExecutor,
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTracks, NodeParams},
};
use serde_json::json;

#[derive(Debug, Clone)]
struct Args {
    clip: PathBuf,
    time_secs: f64,
    frames: u32,
    target_width: u32,
    out_png: PathBuf,
    metrics_json: PathBuf,
    graph: Option<NodeGraph>,
    graph_label: String,
}

/// Built-in graphs, so the common "show me what a grade does to this
/// frame" case needs no JSON authoring. `--graph <file>` takes any
/// serialised `NodeGraph` when these are not enough.
fn preset_graph(name: &str) -> Result<NodeGraph> {
    let mut p = NodeParams::default();
    match name {
        "identity" => {}
        "warm" => {
            p.temperature = Some(0.25);
            p.global_mix = Some(1.0);
        }
        "cool" => {
            p.temperature = Some(-0.25);
            p.global_mix = Some(1.0);
        }
        "tint" => {
            p.tint = Some(0.25);
            p.global_mix = Some(1.0);
        }
        "hue" => {
            p.hue = Some(0.20);
            p.global_mix = Some(1.0);
        }
        other => {
            return Err(anyhow!(
                "unknown preset {other:?} — try identity, warm, cool, tint, hue, \
                 or pass --graph <file.json>"
            ))
        }
    }
    // `identity` deliberately still builds the Source -> ColorBalance ->
    // Output shape with default params: that exercises the executor
    // while `NodeGraph::is_identity()` stays true, which is exactly the
    // case the engine's direct-present path keys on.
    Ok(color_balance_graph(p))
}

fn color_balance_graph(params: NodeParams) -> NodeGraph {
    let source = NodeId::new_v4();
    let colorbal = NodeId::new_v4();
    let output = NodeId::new_v4();
    let nodes = vec![
        Node {
            id: source,
            kind: NodeKind::Source,
            label: "Source".into(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: vec![],
        },
        Node {
            id: colorbal,
            kind: NodeKind::ColorBalance,
            label: "CB".into(),
            enabled: true,
            params,
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: vec![],
        },
        Node {
            id: output,
            kind: NodeKind::Output,
            label: "Output".into(),
            enabled: true,
            params: NodeParams::default(),
            keyframes: NodeKeyframeTracks::default(),
            output_link: None,
            input_links: vec![],
        },
    ];
    let edges = vec![
        Edge { from_node: source, from_port: 0, to_node: colorbal, to_port: 0 },
        Edge { from_node: colorbal, from_port: 0, to_node: output, to_port: 0 },
    ];
    let mut positions = std::collections::HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        positions.insert(n.id, NodePos { x: (i as f32) * 180.0, y: 0.0 });
    }
    NodeGraph {
        nodes,
        edges,
        output_node_id: output,
        node_positions: positions,
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    }
}

fn parse_args() -> Result<Args> {
    let mut clip: Option<PathBuf> = None;
    let mut time_secs: f64 = 0.0;
    let mut frames: u32 = 1;
    let mut target_width: u32 = 1920;
    let mut out_png: PathBuf = PathBuf::from("frame.png");
    let mut metrics_json: PathBuf = PathBuf::from("perf.json");
    let mut graph: Option<NodeGraph> = None;
    let mut graph_label = String::from("none");

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--clip" => {
                clip = Some(PathBuf::from(
                    iter.next().ok_or_else(|| anyhow!("--clip needs a path"))?,
                ))
            }
            "--time" => {
                time_secs = iter
                    .next()
                    .ok_or_else(|| anyhow!("--time needs a number"))?
                    .parse()
                    .context("--time must be a float (seconds)")?
            }
            "--frames" => {
                frames = iter
                    .next()
                    .ok_or_else(|| anyhow!("--frames needs a number"))?
                    .parse()
                    .context("--frames must be a u32")?;
                if frames == 0 {
                    return Err(anyhow!("--frames must be at least 1"));
                }
            }
            "--width" => {
                target_width = iter
                    .next()
                    .ok_or_else(|| anyhow!("--width needs a number"))?
                    .parse()
                    .context("--width must be a u32")?
            }
            "--out" => {
                out_png = PathBuf::from(
                    iter.next().ok_or_else(|| anyhow!("--out needs a path"))?,
                )
            }
            "--metrics" => {
                metrics_json = PathBuf::from(
                    iter.next().ok_or_else(|| anyhow!("--metrics needs a path"))?,
                )
            }
            "--preset" => {
                let name = iter.next().ok_or_else(|| anyhow!("--preset needs a name"))?;
                graph = Some(preset_graph(&name)?);
                graph_label = format!("preset:{name}");
            }
            "--graph" => {
                let path = PathBuf::from(
                    iter.next().ok_or_else(|| anyhow!("--graph needs a path"))?,
                );
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                graph = Some(
                    serde_json::from_str(&text)
                        .with_context(|| format!("parsing {} as a NodeGraph", path.display()))?,
                );
                graph_label = format!("file:{}", path.display());
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument: {other} (try --help)")),
        }
    }

    let clip = clip.ok_or_else(|| anyhow!("--clip <path> is required. Try --help."))?;
    if !clip.exists() {
        return Err(anyhow!("clip not found: {}", clip.display()));
    }

    Ok(Args {
        clip,
        time_secs,
        frames,
        target_width,
        out_png,
        metrics_json,
        graph,
        graph_label,
    })
}

fn print_usage() {
    eprintln!("klipp-smoke — headless engine harness\n");
    eprintln!("USAGE:");
    eprintln!("  klipp-smoke --clip <path> [options]\n");
    eprintln!("OPTIONS:");
    eprintln!("  --clip <path>        Path to the source media file (required)");
    eprintln!("  --time <secs>        Decode time in seconds (default 0.0)");
    eprintln!("  --frames <n>         Decode n consecutive frames (default 1).");
    eprintln!("                       n > 1 uses the sequential decoder, not n seeks.");
    eprintln!("  --preset <name>      Built-in grade: identity, warm, cool, tint, hue");
    eprintln!("  --graph <path.json>  Apply a serialised NodeGraph instead");
    eprintln!("  --width <n>          Target output width in pixels (default 1920)");
    eprintln!("  --out <path>         Output PNG path (default frame.png).");
    eprintln!("                       With --frames n > 1, _0001 etc. is inserted.");
    eprintln!("  --metrics <path>     Output JSON metrics path (default perf.json)");
    eprintln!("  -h, --help           Print this help\n");
    eprintln!("EXAMPLES:");
    eprintln!("  klipp-smoke --clip clip.mp4 --time 1.5 --out /tmp/f.png");
    eprintln!("  klipp-smoke --clip clip.mp4 --frames 30 --preset warm \\");
    eprintln!("              --width 960 --out /tmp/warm.png");
}

/// `frame.png` + index 2 of 10 -> `frame_0002.png`. Single-frame runs
/// keep the path exactly as given.
fn indexed_path(base: &std::path::Path, index: u32, total: u32) -> PathBuf {
    if total <= 1 {
        return base.to_path_buf();
    }
    let stem = base.file_stem().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "frame".into());
    let ext = base.extension().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".into());
    let name = format!("{stem}_{index:04}.{ext}");
    match base.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

/// Take one decoded sRGB-u8 frame through the grade pipeline.
///
/// Mirrors the engine: expand to linear f32, run the graph, then
/// `display_transform_ws` with the viewer's settings (`ToneMapOp::None`,
/// Rec709 working space — what `PlaybackEngine` actually uses) back to
/// display-encoded u8.
fn apply_graph(
    exec: &mut GraphExecutor,
    graph: &NodeGraph,
    rgba: &[u8],
    width: u32,
    height: u32,
    frame_number: u64,
    clip_time_secs: f64,
) -> Result<Vec<u8>> {
    let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(rgba);
    let (graded, _tapped) = exec
        .execute_linear(
            &linear,
            graph,
            width,
            height,
            clip_time_secs,
            frame_number,
            None,
            None,
        )
        .map_err(|e| anyhow!("grade failed: {e:?}"))?;
    Ok(klipp_color::display::display_transform_ws(
        &graded,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
    ))
}

fn main() -> Result<()> {
    // Logs to stderr so stdout stays clean for the human-readable
    // summary and any pipe-friendly output.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "klipp_engine=info,klipp_smoke=info,wgpu=warn,naga=warn",
                )
            }),
        )
        .init();

    let args = parse_args()?;
    tracing::info!(
        "klipp-smoke: {:?} @ {:.3}s, {} frame(s), width {}, grade {}",
        args.clip, args.time_secs, args.frames, args.target_width, args.graph_label,
    );

    let t_total = std::time::Instant::now();
    let mut exec = GraphExecutor::new();
    let mut per_frame = Vec::new();
    let mut last_dims = (0u32, 0u32);

    // `--frames 1` keeps the original seek-to-time behaviour; more than
    // one frame switches to the sequential decoder, which is both what
    // playback does and much cheaper than repeated seeks.
    let mut sequential = if args.frames > 1 {
        Some(
            klipp_engine::SequentialFrameDecoder::open(&args.clip, args.target_width)
                .map_err(|e| anyhow!("opening sequential decoder: {e}"))?,
        )
    } else {
        None
    };

    for i in 0..args.frames {
        let t_decode = std::time::Instant::now();
        let (rgba, width, height) = match sequential.as_mut() {
            Some(dec) => match dec.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => {
                    tracing::warn!("stream ended after {i} frame(s)");
                    break;
                }
                Err(e) => return Err(anyhow!("decoding frame {i}: {e}")),
            },
            None => klipp_engine::generate_thumbnail_at_size(
                &args.clip,
                args.time_secs,
                args.target_width,
            )
            .map_err(|e| anyhow!("decode failed: {e}"))?,
        };
        let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        let expected_len = (width as usize) * (height as usize) * 4;
        if rgba.len() != expected_len {
            return Err(anyhow!(
                "decoder returned {} bytes but expected {}x{}x4 = {}",
                rgba.len(), width, height, expected_len,
            ));
        }
        last_dims = (width, height);

        let t_grade = std::time::Instant::now();
        let out_rgba = match args.graph.as_ref() {
            Some(g) => apply_graph(
                &mut exec, g, &rgba, width, height,
                i as u64,
                args.time_secs + (i as f64) / 60.0,
            )?,
            None => rgba,
        };
        let grade_ms = t_grade.elapsed().as_secs_f64() * 1000.0;

        let path = indexed_path(&args.out_png, i + 1, args.frames);
        let t_encode = std::time::Instant::now();
        write_png(&path, &out_rgba, width, height)
            .with_context(|| format!("writing {}", path.display()))?;
        let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;

        tracing::info!(
            "frame {}/{}: {}x{} decode {:.2} ms, grade {:.2} ms, png {:.2} ms -> {}",
            i + 1, args.frames, width, height, decode_ms, grade_ms, encode_ms,
            path.display(),
        );
        per_frame.push(json!({
            "index": i,
            "path": path.to_string_lossy(),
            "decode_ms": decode_ms,
            "grade_ms": grade_ms,
            "png_encode_ms": encode_ms,
        }));
    }

    if per_frame.is_empty() {
        return Err(anyhow!("no frames decoded"));
    }
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    let mean = |key: &str| -> f64 {
        let vals: Vec<f64> = per_frame
            .iter()
            .filter_map(|f| f.get(key).and_then(|v| v.as_f64()))
            .collect();
        if vals.is_empty() { 0.0 } else { vals.iter().sum::<f64>() / vals.len() as f64 }
    };

    let metrics = json!({
        "clip": args.clip.to_string_lossy(),
        "time_secs": args.time_secs,
        "frames": per_frame.len(),
        "grade": args.graph_label,
        // `has_gpu` is read after the run: the executor builds its GPU
        // pipeline lazily, so asking before the first execute always
        // answers "no".
        "gpu_executor": exec.has_gpu(),
        "output": {
            "width": last_dims.0,
            "height": last_dims.1,
            "path": args.out_png.to_string_lossy(),
        },
        "timings_ms": {
            "decode_mean": mean("decode_ms"),
            "grade_mean": mean("grade_ms"),
            "png_encode_mean": mean("png_encode_ms"),
            "total": total_ms,
        },
        "per_frame": per_frame,
        "version": env!("CARGO_PKG_VERSION"),
    });
    std::fs::write(&args.metrics_json, serde_json::to_string_pretty(&metrics)?)
        .with_context(|| format!("writing {}", args.metrics_json.display()))?;
    tracing::info!("metrics written to {}", args.metrics_json.display());

    println!(
        "klipp-smoke OK — {} frame(s) at {}x{}, decode {:.2} ms/f, grade {:.2} ms/f (total {:.2} ms)",
        per_frame.len(), last_dims.0, last_dims.1,
        mean("decode_ms"), mean("grade_ms"), total_ms,
    );
    Ok(())
}

fn write_png(path: &std::path::Path, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| anyhow!("png header: {e}"))?;
    writer.write_image_data(rgba).map_err(|e| anyhow!("png data: {e}"))?;
    Ok(())
}
