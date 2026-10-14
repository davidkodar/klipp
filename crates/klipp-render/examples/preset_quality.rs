//! Every built-in export preset's output, measured (#1142).
//!
//! ```text
//! cargo run --release -p klipp-render --example preset_quality -- <out dir> [<table.md>]
//! ```
//!
//! A fixed test project is made from generated sources (no footage): a
//! moving chart, a cross-dissolve into a 16-bit horizontal ramp, and a
//! title over both, 8 s at 1080p25. It is exported through every built-in
//! preset and through the delivery choices no preset covers (10-bit, PQ,
//! HLG, ProRes HQ and 4444), and each file is checked:
//!
//! - **Quality:** VMAF (the 4K model at 2160p) and PSNR against a lossless
//!   master of the same project at the file's size: Klipp's own export as
//!   H.264 at crf 0, lossless at 8 bits. Both are compared as 10-bit 4:2:0,
//!   so a 10-bit file is not dithered down to the master's depth. The score is the codec's loss on
//!   Klipp's own pictures; an error the master shares (a colour bug in the
//!   pipeline) is for the checks below.
//! - **Depth:** the pixel format's depth, and the number of distinct luma
//!   codes across the ramp, which an 8-bit step anywhere cuts to about a
//!   quarter in a 10-bit file.
//! - **Chroma:** the subsampling the pixel format has.
//! - **Tags:** primaries, transfer, matrix and range.
//! - **Light (PQ):** a second project of four patches at 5, 100, 203 and
//!   1000 cd/m², read back from the file's codes through the PQ curve.
//!
//! The results go to `<out dir>/results.json`, and a table to `<table.md>`
//! (default `<out dir>/export-quality.md`). Each preset is held to a floor
//! (`floor`), a product bar set before measuring rather than from the
//! result. Everything written is generated; `<out dir>` holds the exports.

use klipp_core::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind, Transition};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 25;
const SECS: f64 = 8.0;
const MODEL: &str = "/usr/share/model/vmaf_v0.6.1.json";
const MODEL_4K: &str = "/usr/share/model/vmaf_4k_v0.6.1.json";

/// The VMAF floors (mean, 1st percentile) a delivery must clear: masters
/// 98/95; high-rate web 95/90; standard web 93/85; social 90/80.
fn floor(name: &str) -> (f64, f64) {
    match name {
        "ProRes 422" | "Broadcast master (EBU R128)" | "DNxHD 115" | "ProRes 422 HQ" | "ProRes 4444" => (98.0, 95.0),
        "Vimeo 1080p" | "H.265 HQ (GPU)" => (95.0, 90.0),
        "YouTube 1080p" | "YouTube 4K" | "H.265 4K" | "Web 1080p (-14 LUFS)" => (93.0, 85.0),
        _ => (90.0, 80.0),
    }
}

fn run(cmd: &mut Command) -> std::process::Output {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(out.status.success(), "{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn ffmpeg() -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-nostdin", "-v", "error", "-y"]);
    c
}

/// The generated sources: a moving chart and a 16-bit horizontal ramp, 5 s
/// each, lossless; and four PQ patches.
fn sources(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    // Set on the frames, so the file carries them (FFV1 in Matroska keeps
    // what the frames say, not the output options).
    let bt709 = ",setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709";
    let chart = dir.join("chart.mkv");
    run(ffmpeg().args(["-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r={FPS}:d=5,format=yuv444p10le{bt709}")]).args(["-c:v", "ffv1"]).arg(&chart));
    let ramp = dir.join("ramp.mkv");
    run(ffmpeg()
        .args(["-f", "lavfi", "-i", &format!("nullsrc=s={W}x{H}:r={FPS}:d=5,format=gray16le,geq=lum='X*65535/W',format=yuv444p12le{bt709}")])
        .args(["-c:v", "ffv1"])
        .arg(&ramp));
    // Four vertical bands of known light, PQ-coded 10-bit luma, neutral chroma.
    let pq = dir.join("pq.mkv");
    let code = |nits: f64| (64.0 + klipp_color::hdr::pq_encode_nits(nits as f32) as f64 * 876.0).round() as u32;
    let bands = [5.0, 100.0, 203.0, 1000.0].map(code);
    let expr = format!("if(lt(X,W/4),{},if(lt(X,W/2),{},if(lt(X,3*W/4),{},{})))", bands[0], bands[1], bands[2], bands[3]);
    run(ffmpeg()
        .args(["-f", "lavfi", "-i", &format!("nullsrc=s={W}x{H}:r={FPS}:d=3,format=yuv422p10le,geq=lum='{expr}':cb=512:cr=512,setparams=range=tv:colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084")])
        .args(["-c:v", "ffv1"])
        .arg(&pq));
    (chart, ramp, pq)
}

fn tc(secs: f64) -> Timecode {
    Timecode::from_secs_f64(secs, FPS)
}

fn media(pool: &mut MediaPool, path: &Path, secs: f64) -> MediaId {
    let id = MediaId::new();
    pool.add(MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(secs), Fps::new(FPS, 1))
    });
    id
}

fn shot(id: MediaId, source_in: f64, from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(source_in), source_out: tc(source_in + to - from), ..Clip::gap(tc(from), tc(to)) }
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS, 1);
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

/// The chart, a second's dissolve into the ramp at 4 s, and a title over
/// 1 to 6 s.
fn sdr_project(chart: &Path, ramp: &Path) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::default();
    let (c, r) = (media(&mut pool, chart, 5.0), media(&mut pool, ramp, 5.0));
    let (a, b) = (shot(c, 0.0, 0.0, 4.0), shot(r, 0.5, 4.0, SECS));
    let mut v = Track::new("V1", TrackKind::Video);
    v.transitions = vec![Transition {
        id: TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(1.0),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    }];
    v.clips = vec![a, b];
    let mut t = Track::new("Titles", TrackKind::Text);
    t.clips = vec![Clip {
        kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Export quality".into(), font_size: 120.0, ..Default::default() })),
        ..Clip::gap(tc(1.0), tc(6.0))
    }];
    (sequence(vec![t, v]), pool)
}

fn hdr_project(pq: &Path) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::default();
    let p = media(&mut pool, pq, 3.0);
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![shot(p, 0.0, 0.0, 2.0)];
    (sequence(vec![v]), pool)
}

fn export(seq: &Sequence, pool: &MediaPool, settings: ExportSettings) -> Result<(), String> {
    let snapshot = SequenceSnapshot::from_sequence(seq, pool, &settings, None, false, WorkingColorspace::Rec709Linear).map_err(|e| format!("{e:?}"))?;
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: settings.output_path.clone(),
        settings,
        snapshot,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Default::default(),
        timings: Default::default(),
        total_frames: frames,
        analyser: None,
    }
    .run()
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

/// The file's first video stream as ffprobe reports it, frames counted.
fn probe(path: &Path) -> Value {
    let out = run(Command::new("ffprobe").args(["-v", "error", "-count_frames", "-show_streams", "-of", "json"]).arg(path));
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

/// VMAF mean and 1st percentile, and luma PSNR, of `file` against `master`.
fn score(file: &Path, master: &Path, height: u64, dir: &Path) -> (f64, f64, f64) {
    let log = dir.join(format!("{}.vmaf.json", file.file_name().unwrap().to_string_lossy()));
    let model = if height >= 2160 { MODEL_4K } else { MODEL };
    // Both at 10 bits: an 8-bit file steps up exactly, while taking a
    // 10-bit file down to 8 would add swscale's dither to the score.
    let graph = |metric: &str| format!("[0:v]format=yuv420p10le,setpts=PTS-STARTPTS[d];[1:v]format=yuv420p10le,setpts=PTS-STARTPTS[r];[d][r]{metric}");
    run(ffmpeg().arg("-i").arg(file).arg("-i").arg(master).args(["-lavfi", &graph(&format!("libvmaf=model='path={model}':log_fmt=json:log_path={}:n_threads=8", log.display())), "-f", "null", "-"]));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap_or_default()).unwrap_or(Value::Null);
    let mut per: Vec<f64> = v["frames"].as_array().map(|f| f.iter().filter_map(|x| x["metrics"]["vmaf"].as_f64()).collect()).unwrap_or_default();
    per.sort_by(f64::total_cmp);
    let mean = v["pooled_metrics"]["vmaf"]["mean"].as_f64().unwrap_or(f64::NAN);
    let p1 = per.get(per.len() / 100).copied().unwrap_or(f64::NAN);
    let out = Command::new("ffmpeg").args(["-nostdin", "-i"]).arg(file).arg("-i").arg(master).args(["-lavfi", &graph("psnr"), "-f", "null", "-"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    let psnr = text.lines().filter(|l| l.contains("PSNR y:")).find_map(|l| l.split("PSNR y:").nth(1)?.split_whitespace().next()?.parse().ok()).unwrap_or(f64::NAN);
    (mean, p1, psnr)
}

/// Distinct 16-bit luma values along the middle row of the ramp, at 6.5 s
/// (past the dissolve and the title).
fn ramp_levels(file: &Path, w: u64, h: u64) -> usize {
    let out = run(ffmpeg().args(["-ss", "6.5"]).arg("-i").arg(file).args(["-frames:v", "1", "-vf", "format=gray16le", "-f", "rawvideo", "-"]));
    let row = (h / 2) as usize * w as usize * 2;
    let mut v: Vec<u16> = out.stdout.get(row..row + w as usize * 2).unwrap_or_default().chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

/// The four PQ patches' light, cd/m², from the file's luma codes: its own
/// limited-range codes, the luma plane of 10-bit 4:2:0 (a grey format
/// would stretch them to full range).
fn pq_patches(file: &Path, w: u64, h: u64) -> Vec<f64> {
    let out = run(ffmpeg().arg("-i").arg(file).args(["-frames:v", "1", "-pix_fmt", "yuv420p10le", "-f", "rawvideo", "-"]));
    let row = (h / 2) as usize * w as usize * 2;
    (0..4)
        .map(|b| {
            let x = (b * 2 + 1) as usize * w as usize / 8;
            let i = row + x * 2;
            let code = out.stdout.get(i..i + 2).map_or(0, |c| u16::from_le_bytes([c[0], c[1]])) as f32;
            klipp_color::hdr::pq_decode_nits(((code - 64.0) / 876.0).clamp(0.0, 1.0)) as f64
        })
        .collect()
}

fn subsampling(pix_fmt: &str) -> &'static str {
    if pix_fmt.contains("444") || pix_fmt.starts_with("gbr") || pix_fmt.starts_with("rgb") {
        "4:4:4"
    } else if pix_fmt.contains("422") {
        "4:2:2"
    } else if pix_fmt.contains("420") || pix_fmt.starts_with("nv12") || pix_fmt.starts_with("p010") {
        "4:2:0"
    } else {
        "?"
    }
}

fn base_settings(path: PathBuf, format: ExportFormat) -> ExportSettings {
    ExportSettings { output_path: path, format, burn_in_subtitles: true, audio_codec: ExportAudioCodec::Copy, ..Default::default() }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(dir) = args.get(1).map(PathBuf::from) else {
        eprintln!("usage: preset_quality <out dir> [<table.md>]");
        std::process::exit(2);
    };
    let table_path = args.get(2).map(PathBuf::from).unwrap_or_else(|| dir.join("export-quality.md"));
    std::fs::create_dir_all(&dir).unwrap();
    let (chart, ramp, pq) = sources(&dir);
    let (sdr, sdr_pool) = sdr_project(&chart, &ramp);
    let (hdr, hdr_pool) = hdr_project(&pq);

    // A lossless master per output size, made on first need.
    let mut masters: std::collections::BTreeMap<(u32, u32, u8), PathBuf> = Default::default();
    let mut master_for = |w: u32, h: u32, depth: u8| -> PathBuf {
        masters
            .entry((w, h, depth))
            .or_insert_with(|| {
                let p = dir.join(format!("master-{w}x{h}-{depth}bit.mkv"));
                let s = ExportSettings { video_codec: ExportVideoCodec::H264, crf: 0, bit_depth: depth, resolution: Some(Resolution { width: w, height: h }), ..base_settings(p.clone(), ExportFormat::Mkv) };
                export(&sdr, &sdr_pool, s).expect("the master");
                p
            })
            .clone()
    };

    // Every built-in preset, then the choices no preset covers.
    let mut cases: Vec<(String, ExportSettings, bool)> = Vec::new();
    for p in builtin_presets() {
        let s = &p.settings;
        let file = dir.join(format!("{}.{}", p.name.replace(['/', ' ', '(', ')', '.', ','], "_"), s.format.extension()));
        let mut e = base_settings(file, s.format);
        (e.video_codec, e.resolution, e.fps, e.crf, e.preset, e.rate_control, e.video_bitrate_kbps) = (s.video_codec, s.resolution, s.fps, s.crf, s.preset, s.rate_control, s.video_bitrate_kbps);
        cases.push((p.name.clone(), e, false));
    }
    let extra = |name: &str, codec: ExportVideoCodec, format: ExportFormat, depth: u8, hdr: HdrMode| {
        let file = dir.join(format!("{}.{}", name.replace([' ', '.', ','], "_"), format.extension()));
        (name.to_string(), ExportSettings { video_codec: codec, bit_depth: depth, hdr_mode: hdr, crf: 16, ..base_settings(file, format) }, hdr != HdrMode::Sdr)
    };
    cases.push(extra("H.264 10-bit", ExportVideoCodec::H264, ExportFormat::Mkv, 10, HdrMode::Sdr));
    cases.push(extra("H.265 10-bit", ExportVideoCodec::H265, ExportFormat::Mkv, 10, HdrMode::Sdr));
    cases.push(extra("AV1 10-bit", ExportVideoCodec::Av1, ExportFormat::Mkv, 10, HdrMode::Sdr));
    cases.push(extra("ProRes 422 HQ", ExportVideoCodec::ProRes422HQ, ExportFormat::Mov, 10, HdrMode::Sdr));
    cases.push(extra("ProRes 4444", ExportVideoCodec::ProRes4444, ExportFormat::Mov, 10, HdrMode::Sdr));
    cases.push(extra("H.264 8-bit (control)", ExportVideoCodec::H264, ExportFormat::Mkv, 8, HdrMode::Sdr));
    cases.push(extra("H.265 PQ", ExportVideoCodec::H265, ExportFormat::Mkv, 10, HdrMode::Pq));
    cases.push(extra("H.265 HLG", ExportVideoCodec::H265, ExportFormat::Mkv, 10, HdrMode::Hlg));
    cases.push(extra("AV1 PQ", ExportVideoCodec::Av1, ExportFormat::Mkv, 10, HdrMode::Pq));
    cases.push(extra("ProRes 4444 PQ", ExportVideoCodec::ProRes4444, ExportFormat::Mov, 10, HdrMode::Pq));

    let mut results = Vec::new();
    for (name, settings, is_hdr) in cases {
        let file = settings.output_path.clone();
        eprintln!("{name} → {}", file.display());
        let (seq, pool) = if is_hdr { (&hdr, &hdr_pool) } else { (&sdr, &sdr_pool) };
        let mut row = json!({ "name": name, "codec": settings.video_codec.label(), "container": settings.format.extension() });
        if let Err(e) = export(seq, pool, settings.clone()) {
            row["error"] = json!(e);
            results.push(row);
            continue;
        }
        let p = probe(&file);
        let streams = p["streams"].as_array().cloned().unwrap_or_default();
        // Sound alone (#1813): one audio stream and no picture, measured by
        // the audio export's own tests.
        if settings.format.is_audio_only() {
            let audio: Vec<Value> = streams.iter().filter(|s| s["codec_type"] == "audio").map(|s| s["codec_name"].clone()).collect();
            let video = streams.iter().any(|s| s["codec_type"] == "video");
            row["codec_name"] = audio.first().cloned().unwrap_or(Value::Null);
            row["pass"] = json!(audio.len() == 1 && !video);
            row["note"] = json!("audio only, no video stream");
            results.push(row);
            continue;
        }
        let Some(v) = streams.iter().find(|s| s["codec_type"] == "video").cloned() else {
            row["streams"] = json!(streams.iter().map(|s| s["codec_name"].clone()).collect::<Vec<_>>());
            row["error"] = json!("no video stream");
            results.push(row);
            continue;
        };
        let (w, h) = (v["width"].as_u64().unwrap_or(0), v["height"].as_u64().unwrap_or(0));
        let pix = v["pix_fmt"].as_str().unwrap_or("?").to_string();
        for key in ["codec_name", "pix_fmt", "color_range", "color_space", "color_transfer", "color_primaries", "bits_per_raw_sample", "r_frame_rate", "nb_read_frames"] {
            row[key] = v[key].clone();
        }
        row["size"] = json!(format!("{w}x{h}"));
        row["chroma"] = json!(subsampling(&pix));
        if settings.format == ExportFormat::Gif {
            // A palette at its own rate: its size, rate and frames, no score.
            let want = settings.fps.map_or(FPS, |f| f.num / f.den.max(1));
            let frames = v["nb_read_frames"].as_str().and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
            row["pass"] = json!(v["r_frame_rate"] == json!(format!("{want}/1")) && (frames - SECS * want as f64).abs() <= 1.0);
            row["note"] = json!(format!("{} frames at {}", frames, s_of(&v["r_frame_rate"])));
        } else if is_hdr {
            if settings.delivery() == HdrMode::Pq {
                row["pq_patches_nits"] = json!(pq_patches(&file, w, h));
            }
        } else {
            row["ramp_levels"] = json!(ramp_levels(&file, w, h));
            // A 10-bit file against a 10-bit master: an 8-bit one's rounding
            // would count against the file that is the more accurate.
            let deep = v["bits_per_raw_sample"].as_str().and_then(|b| b.parse::<u32>().ok()).unwrap_or(8) > 8 || pix.contains("p10") || pix.contains("p12");
            let master = master_for(w as u32, h as u32, if deep { 10 } else { 8 });
            let (mean, p1, psnr) = score(&file, &master, h, &dir);
            let (fm, fp) = floor(&name);
            row["vmaf_mean"] = json!(mean);
            row["vmaf_p1"] = json!(p1);
            row["psnr_y"] = json!(psnr);
            row["floor"] = json!([fm, fp]);
            row["pass"] = json!(mean >= fm && p1 >= fp);
        }
        results.push(row);
    }
    std::fs::write(dir.join("results.json"), serde_json::to_string_pretty(&results).unwrap()).unwrap();
    std::fs::write(&table_path, table(&results)).unwrap();
    eprintln!("results: {}, table: {}", dir.join("results.json").display(), table_path.display());
}

fn s_of(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

fn table(results: &[Value]) -> String {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => "–".into(),
        v => v.to_string(),
    };
    let f = |v: &Value, d: usize| v.as_f64().map_or("–".into(), |x| format!("{x:.d$}"));
    let mut out = String::from(
        "| Export | Codec | Size | Pixel format | Chroma | Range | Primaries / transfer / matrix | Ramp levels | VMAF mean / p1 | PSNR Y | Floor | |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in results {
        if let Some(e) = r.get("error") {
            out.push_str(&format!("| {} | {} | | | | | | | | | | error: {} |\n", s(&r["name"]), s(&r["codec"]), s(e)));
            continue;
        }
        let vmaf = if r["vmaf_mean"].is_null() { "–".into() } else { format!("{} / {}", f(&r["vmaf_mean"], 1), f(&r["vmaf_p1"], 1)) };
        let floor = r["floor"].as_array().map_or("–".into(), |a| format!("{} / {}", s(&a[0]), s(&a[1])));
        let note = r["note"].as_str().map(|n| format!(" ({n})")).unwrap_or_default();
        let verdict = match r["pass"].as_bool() {
            Some(true) => format!("pass{note}"),
            Some(false) => format!("**fails**{note}"),
            None if !note.is_empty() => note.trim().trim_matches(['(', ')']).to_string(),
            None => r["pq_patches_nits"].as_array().map_or(String::new(), |p| format!("PQ patches {} cd/m²", p.iter().map(|x| f(x, 0)).collect::<Vec<_>>().join(", "))),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} / {} / {} | {} | {} | {} | {} | {} |\n",
            s(&r["name"]),
            s(&r["codec_name"]),
            s(&r["size"]),
            s(&r["pix_fmt"]),
            s(&r["chroma"]),
            s(&r["color_range"]),
            s(&r["color_primaries"]),
            s(&r["color_transfer"]),
            s(&r["color_space"]),
            s(&r["ramp_levels"]),
            vmaf,
            f(&r["psnr_y"], 1),
            floor,
            verdict,
        ));
    }
    out
}
