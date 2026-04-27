//! Headless single-frame engine smoke tool.
//!
//! Replaces "open the GUI app, import a clip, scrub to N, screenshot,
//! report back" loops with a CLI you can run yourself in a tight
//! edit-build-test cycle.
//!
//! Usage:
//!   klipp-smoke --clip /path/to/file.mp4 --time 1.5 \
//!               [--width 1920] [--out frame.png] [--metrics perf.json]
//!
//! Behaviour:
//!   1. Open the file via klipp-engine's existing single-frame
//!      decoder (`generate_thumbnail_at_size`). This exercises the
//!      same demux + decode + colourspace conversion the GUI uses
//!      on the cold-start poster path, so any regression in those
//!      stages reproduces here.
//!   2. Write the decoded sRGB RGBA frame as a PNG to `--out` (or
//!      `frame.png` in the current directory by default).
//!   3. Write a JSON metrics blob with elapsed timing + dimensions
//!      to `--metrics` (default `perf.json`).
//!
//! Future arguments (not implemented yet, kept on the roadmap so
//! the CLI surface stays stable):
//!   --graph <name>   Run a named node graph (corrector_default,
//!                    bezier_mask_test, ...) before the PNG dump.
//!                    Plumbing into klipp_nodes::Executor lives in
//!                    klipp-engine; wiring it through here is a
//!                    separate task once the executor's standalone
//!                    entry point matures.
//!   --frames N       Decode N consecutive frames starting at --time
//!                    so soak-style perf checks can run without GUI.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::json;

#[derive(Debug, Clone)]
struct Args {
    clip: PathBuf,
    time_secs: f64,
    target_width: u32,
    out_png: PathBuf,
    metrics_json: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut clip: Option<PathBuf> = None;
    let mut time_secs: f64 = 0.0;
    let mut target_width: u32 = 1920;
    let mut out_png: PathBuf = PathBuf::from("frame.png");
    let mut metrics_json: PathBuf = PathBuf::from("perf.json");

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--clip" => clip = Some(PathBuf::from(
                iter.next().ok_or_else(|| anyhow!("--clip needs a path"))?,
            )),
            "--time" => time_secs = iter.next()
                .ok_or_else(|| anyhow!("--time needs a number"))?
                .parse()
                .context("--time must be a float (seconds)")?,
            "--width" => target_width = iter.next()
                .ok_or_else(|| anyhow!("--width needs a number"))?
                .parse()
                .context("--width must be a u32")?,
            "--out" => out_png = PathBuf::from(
                iter.next().ok_or_else(|| anyhow!("--out needs a path"))?,
            ),
            "--metrics" => metrics_json = PathBuf::from(
                iter.next().ok_or_else(|| anyhow!("--metrics needs a path"))?,
            ),
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument: {other} (try --help)")),
        }
    }

    let clip = clip.ok_or_else(|| {
        anyhow!("--clip <path> is required. Try --help.")
    })?;
    if !clip.exists() {
        return Err(anyhow!("clip not found: {}", clip.display()));
    }

    Ok(Args {
        clip,
        time_secs,
        target_width,
        out_png,
        metrics_json,
    })
}

fn print_usage() {
    eprintln!("klipp-smoke — headless single-frame engine harness\n");
    eprintln!("USAGE:");
    eprintln!("  klipp-smoke --clip <path> [options]\n");
    eprintln!("OPTIONS:");
    eprintln!("  --clip <path>        Path to the source media file (required)");
    eprintln!("  --time <secs>        Decode time in seconds (default 0.0)");
    eprintln!("  --width <n>          Target output width in pixels (default 1920)");
    eprintln!("  --out <path>         Output PNG path (default frame.png)");
    eprintln!("  --metrics <path>     Output JSON metrics path (default perf.json)");
    eprintln!("  -h, --help           Print this help\n");
    eprintln!("EXAMPLE:");
    eprintln!("  klipp-smoke --clip clip.mp4 --time 1.5 --width 1280 \\");
    eprintln!("              --out /tmp/f.png --metrics /tmp/p.json");
}

fn main() -> Result<()> {
    // Logs to stderr so stdout stays clean for any future
    // human-readable summary or pipe-friendly output.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(
                    "klipp_engine=info,klipp_smoke=info,wgpu=warn,naga=warn",
                ))
        )
        .init();

    let args = parse_args()?;
    tracing::info!(
        "klipp-smoke: decoding {:?} @ {:.3}s, target width {}",
        args.clip, args.time_secs, args.target_width,
    );

    let t_total = std::time::Instant::now();
    let t_decode = std::time::Instant::now();
    let (rgba, width, height) = klipp_engine::generate_thumbnail_at_size(
        &args.clip,
        args.time_secs,
        args.target_width,
    )
    .map_err(|e| anyhow!("decode failed: {e}"))?;
    let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;
    tracing::info!(
        "decoded {}x{} in {:.2} ms ({} bytes)",
        width, height, decode_ms, rgba.len(),
    );

    // Sanity check — generate_thumbnail_at_size returns RGBA u8.
    let expected_len = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected_len {
        return Err(anyhow!(
            "decoder returned {} bytes but expected {}x{}x4 = {}",
            rgba.len(), width, height, expected_len,
        ));
    }

    let t_encode = std::time::Instant::now();
    write_png(&args.out_png, &rgba, width, height)
        .with_context(|| format!("writing {}", args.out_png.display()))?;
    let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
    tracing::info!("PNG written to {} in {:.2} ms", args.out_png.display(), encode_ms);

    let metrics = json!({
        "clip": args.clip.to_string_lossy(),
        "time_secs": args.time_secs,
        "output": {
            "width": width,
            "height": height,
            "path": args.out_png.to_string_lossy(),
        },
        "timings_ms": {
            "decode": decode_ms,
            "png_encode": encode_ms,
            "total": total_ms,
        },
        "version": env!("CARGO_PKG_VERSION"),
    });
    std::fs::write(&args.metrics_json, serde_json::to_string_pretty(&metrics)?)
        .with_context(|| format!("writing {}", args.metrics_json.display()))?;
    tracing::info!("metrics written to {}", args.metrics_json.display());

    println!(
        "klipp-smoke OK — {}x{} decoded in {:.2} ms (total {:.2} ms)",
        width, height, decode_ms, total_ms,
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
