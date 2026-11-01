//! Render a saved project to a file, without the GUI.
//!
//! ```text
//! cargo run --release -p klipp-render --example export_project -- \
//!     <project.klipp dir> <out.mp4> [first_frame] [last_frame]
//! ```
//!
//! Everything the viewer shows a frame through — the clip grades, the
//! builtin effects, the working colourspace — comes from the project
//! file, so what lands in the file is what the timeline says, not a
//! reconstruction of it. Frame numbers are optional and default to the
//! whole timeline; they are there because a 4K vertical timeline is a
//! slow thing to render when all you want is to look at one cut.
//!
//! The point of it is being able to *see* a grade outside the viewer,
//! and to be able to do that from a script or a test rather than by
//! driving the UI.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use klipp_core::{
    EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, HdrMode,
    OdtChoice, RateControl, ToneMapOp, WorkingColorspace,
};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::ExportJob;
use klipp_timeline::GradeAccess;

fn main() {
    // The export logs what it chose — encoder, decoder, hardware — at
    // info level. Without a subscriber those go nowhere, and "why is
    // this slow" becomes guesswork.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(out)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: export_project <project.klipp dir> <out.mp4> [first_frame] [last_frame]"
        );
        std::process::exit(2);
    };
    let first: Option<u64> = args.next().and_then(|s| s.parse().ok());
    let last: Option<u64> = args.next().and_then(|s| s.parse().ok());

    let dir = PathBuf::from(dir);
    let out = PathBuf::from(out);

    let (sequences, pool, colour, ..) = match klipp_project::load_project(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not load {}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    let seq = match sequences.into_iter().next() {
        Some(s) => s,
        None => {
            eprintln!("{} has no sequences", dir.display());
            std::process::exit(1);
        }
    };

    // The working space is a project setting, and getting it wrong would
    // change every colour in the render — so it comes from the file too.
    let working = colour.working_colorspace;
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let fps = seq.fps.num as f64 / seq.fps.den as f64;
    let end = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.timeline_out.rescale_to(seq.fps.num).value)
        .max()
        .unwrap_or(0)
        .max(0) as u64;
    let (first, last) = (first.unwrap_or(0), last.unwrap_or(end));
    let frames = last.saturating_sub(first);
    if frames == 0 {
        eprintln!("nothing to render: frames {first}..{last}");
        std::process::exit(1);
    }

    describe(&seq, w, h, fps, first, last, working);

    let settings = ExportSettings {
        output_path: out.clone(),
        format: ExportFormat::Mp4,
        // NVENC has no 10-bit H.264, so a 10-bit delivery is H.265.
        video_codec: if std::env::var("KLIPP_EXPORT_BITS").as_deref() == Ok("10") {
            ExportVideoCodec::H265
        } else {
            ExportVideoCodec::H264
        },
        resolution: None,
        fps: None,
        // Near-lossless: the point is judging colour, so the encoder
        // should not be part of what is being judged.
        crf: 14,
        preset: EncoderPreset::Fast,
        audio_codec: ExportAudioCodec::Aac,
        audio_bitrate_kbps: Some(256),
        video_bitrate_kbps: None,
        rate_control: RateControl::Crf,
        burn_in_subtitles: false,
        embed_subtitles: false,
        target_lufs: None,
        true_peak_dbtp: None,
        // Linear is the default; the app passes the user's preference.
        pan_law: Default::default(),
        embed_chapters: false, captions_608: false, hdr10plus: false,
        // KLIPP_EXPORT_BITS=10 for a 10-bit delivery, to measure the
        // slower path against the 8-bit one.
        bit_depth: std::env::var("KLIPP_EXPORT_BITS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        alpha: false,
        hdr_mode: HdrMode::Sdr,
        tone_map_op: ToneMapOp::Hable,
        odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };

    // The snapshot the app builds, rather than one assembled here: it
    // resolves compound clips, carries the audio tracks, decides what
    // subtitles do, and fills the media map. Hand-rolling it is how this
    // tool first rendered a silent file — `audio_tracks: vec![]` is easy
    // to write and impossible to notice until you play the result.
    // The working colourspace is a parameter and not a field to patch
    // afterwards, because this tool is the one that forgot: it printed
    // "AcesCG" and rendered Rec.709, silently, until an ACES render came
    // out pixel-identical to a Rec.709 one.
    let mut snapshot =
        match SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, working) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("could not snapshot the sequence: {e:?}");
                std::process::exit(1);
            }
        };
    snapshot.export_range = Some((first, last));

    let progress = Arc::new(AtomicU64::new(0));
    // Reports every 2 s, and stops the moment `stop` is dropped. A sleep
    // here held the process up to 2 s past the export at the join, which
    // made wall-clock timings of this tool step in 2 s.
    let (stop, finished) = std::sync::mpsc::channel::<()>();
    let watch = {
        let progress = Arc::clone(&progress);
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut last_seen = 0;
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(std::time::Duration::from_secs(2)) {
                let done = progress.load(std::sync::atomic::Ordering::Relaxed);
                if done != last_seen {
                    let secs = start.elapsed().as_secs_f64();
                    eprintln!(
                        "  {done}/{frames} frames  {:.1} fps",
                        done as f64 / secs.max(0.001),
                    );
                    last_seen = done;
                }
            }
        })
    };

    let t0 = std::time::Instant::now();
    let job = ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.clone(),
        settings,
        snapshot,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::clone(&progress),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Default::default(),
        timings: Default::default(),
        total_frames: frames,
        analyser: None,
    };
    match job.run() {
        Ok(()) => {
            let secs = t0.elapsed().as_secs_f64();
            drop(stop);
            let _ = watch.join();
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            println!(
                "\nwrote {} — {frames} frames in {secs:.1}s ({:.1} fps), {:.1} MB",
                out.display(),
                frames as f64 / secs.max(0.001),
                size as f64 / 1e6,
            );
        }
        Err(e) => {
            eprintln!("export failed: {e:?}");
            std::process::exit(1);
        }
    }
}

/// What is about to be rendered, so a surprise in the file can be traced
/// to a surprise in the project rather than to the renderer.
fn describe(
    seq: &klipp_timeline::Sequence,
    w: u32,
    h: u32,
    fps: f64,
    first: u64,
    last: u64,
    working: WorkingColorspace,
) {
    println!("{w}x{h} @ {fps} fps, {working:?}, frames {first}..{last}");
    for track in &seq.tracks {
        for clip in &track.clips {
            let graph = clip.grade();
            let graded: Vec<String> = graph
                .nodes
                .iter()
                .filter(|n| !matches!(n.kind, klipp_nodes::NodeKind::Source | klipp_nodes::NodeKind::Output))
                .map(|n| format!("{:?}{}", n.kind, if n.enabled { "" } else { " (off)" }))
                .collect();
            println!(
                "  {}: {}..{}  {}",
                track.name,
                clip.timeline_in.rescale_to(seq.fps.num).value,
                clip.timeline_out.rescale_to(seq.fps.num).value,
                if graded.is_empty() { "ungraded".to_string() } else { graded.join(" → ") },
            );
        }
    }
}
