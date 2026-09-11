//! Export vs viewer: does an exported file show what the viewer showed?
//!
//! Each case exports through the real `ExportJob`, decodes the result the
//! way a player honouring the file's BT.709 tags does, and compares against
//! what the viewer shows for the same timeline. Sources are generated with
//! the ffmpeg CLI; the tests skip without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 1920;
const H: u32 = 1080;
const FRAMES: u64 = 5;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-export-parity-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn clip(kind: snapshot::ClipKindSnapshot, graph: klipp_nodes::NodeGraph) -> snapshot::ClipSnapshot {
    snapshot::ClipSnapshot {
        id: ClipId::new(),
        kind,
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        source_end_secs: FRAMES as f64 / 25.0,
        speed: 1.0,
        node_graph: graph,
        effects: vec![],
        builtin_effects: vec![],
        source_rotation: 0,
        stabilization_data: None,
        smoothed_trajectory: None,
        effect_keyframes: Default::default(),
        audio_effect_keyframes: Default::default(),
        opacity_keyframes: vec![],
        transform_pos_x_keyframes: vec![],
        transform_pos_y_keyframes: vec![],
        transform_scale_keyframes: vec![],
        transform_rotation_keyframes: vec![],
        blend_mode: BlendMode::Normal,
        matte_source: None,
        matte_stencil: None,
        consumed_as_matte: false,
        transform_link: None,
    }
}

fn export(c: snapshot::ClipSnapshot, out: &Path) {
    export_with(c, out, ExportVideoCodec::H264)
}

fn export_with(c: snapshot::ClipSnapshot, out: &Path, codec: ExportVideoCodec) {
    let snapshot = SequenceSnapshot {
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None,
        video_tracks: vec![snapshot::VideoTrackSnapshot { opacity: 1.0, visible: true, clips: vec![c], transitions: vec![] }],
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(), format: if codec == ExportVideoCodec::H264 { ExportFormat::Mp4 } else { ExportFormat::Mov }, video_codec: codec,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, embed_chapters: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
        total_frames: FRAMES,
    }
    .run()
    .unwrap();
}

/// First frame as RGB24, decoded as BT.709 limited — what a player does
/// with a BT.709-tagged file.
fn decode_709(path: &Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-frames:v", "1", "-vf", "scale=in_color_matrix=bt709:in_range=tv,format=rgb24", "-f", "rawvideo", "-"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn px(rgb: &[u8], x: u32, y: u32) -> [i32; 3] {
    // Median of a 5x5 block, per channel.
    let mut out = [0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut v: Vec<i32> = (0..25)
            .map(|k| {
                let (xx, yy) = (x + k % 5 - 2, y + k / 5 - 2);
                rgb[((yy * W + xx) * 3) as usize + c] as i32
            })
            .collect();
        v.sort();
        *o = v[12];
    }
    out
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Lossless SMPTE HD bars, BT.709 limited, tagged — the reference an
/// ungraded export must reproduce. `None` without ffmpeg.
fn bars_source() -> Option<PathBuf> {
    static SRC: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SRC.get_or_init(|| {
        if !ffmpeg_ok() { return None; }
        let out = tmp("bars_src.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "smptehdbars=size=1920x1080:rate=25",
                   "-t", "0.4", "-vf", "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709",
                   "-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&out).status().ok()?.success();
        ok.then_some(out)
    }).clone()
}

fn probe(path: &Path, entries: &str) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-show_entries", entries, "-of", "default=nw=1"])
        .arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An ungraded export reproduces its source. Export used swscale's default
/// BT.601 matrix to decode and to encode while tagging BT.709, and flat
/// SMPTE bars came back up to 27 levels off (magenta [190, 0, 191] as
/// [185, 27, 189]). Patch edges are skipped: any two scalers resample them
/// differently.
#[test]
fn ungraded_sdr_export_reproduces_the_source() {
    let Some(src) = bars_source() else { return };
    let out = tmp("bars.mp4");
    export(clip(snapshot::ClipKindSnapshot::Media { path: src.clone(), has_audio: false }, Default::default()), &out);
    let (a, b) = (decode_709(&src), decode_709(&out));
    let flat = |x: u32, y: u32| {
        let c = &a[((y * W + x) * 3) as usize..][..3];
        (0..81).all(|k| &a[(((y + k / 9 - 4) * W + x + k % 9 - 4) * 3) as usize..][..3] == c)
    };
    let mut worst = 0;
    let mut sampled = 0;
    for gy in 1..27 {
        for gx in 1..48 {
            let (x, y) = (gx * W / 48, gy * H / 27);
            if !flat(x, y) { continue; }
            sampled += 1;
            let (p, q) = (px(&a, x, y), px(&b, x, y));
            let d = (0..3).map(|c| (p[c] - q[c]).abs()).max().unwrap();
            if d > worst { worst = d; eprintln!("  ({x},{y}) source {p:?} export {q:?} diff {d}"); }
        }
    }
    eprintln!("SDR bars, no grade: worst {worst} over {sampled} flat samples");
    assert!(sampled > 100, "too few flat samples ({sampled}) to mean anything");
    assert!(worst <= 5, "ungraded export differs from its source by {worst} levels");
}

/// Generated pixels (solid colours here; titles take the same route) play
/// back as the colour they were given — the encoder writes the matrix the
/// file is tagged with.
#[test]
fn generated_colours_play_back_as_given() {
    if !ffmpeg_ok() { return; }
    for colour in [[200u8, 40, 40, 255], [40, 180, 60, 255], [50, 60, 200, 255], [128, 128, 128, 255]] {
        let out = tmp(&format!("solid_{}_{}_{}.mp4", colour[0], colour[1], colour[2]));
        export(clip(snapshot::ClipKindSnapshot::Generator(GeneratorData::SolidColor { color: colour }), Default::default()), &out);
        let got = px(&decode_709(&out), W / 2, H / 2);
        eprintln!("solid {:?}: export decodes as {got:?}", &colour[..3]);
        for c in 0..3 {
            assert!((got[c] - colour[c] as i32).abs() <= 3, "{:?} played back as {got:?}", &colour[..3]);
        }
    }
}

/// Every Y'CbCr export declares its range and colour, not only H.264/H.265.
#[test]
fn exports_are_fully_tagged() {
    if !ffmpeg_ok() { return; }
    for (codec, name) in [(ExportVideoCodec::H264, "tags.mp4"), (ExportVideoCodec::ProRes422HQ, "tags.mov")] {
        let out = tmp(name);
        export_with(clip(snapshot::ClipKindSnapshot::Generator(GeneratorData::SolidColor { color: [90, 120, 150, 255] }), Default::default()), &out, codec);
        let tags = probe(&out, "stream=color_range,color_space,color_transfer,color_primaries");
        eprintln!("{codec:?}: {}", tags.replace('\n', " "));
        for want in ["color_range=tv", "color_space=bt709", "color_transfer=bt709", "color_primaries=bt709"] {
            assert!(tags.contains(want), "{codec:?} export lacks {want}: {tags}");
        }
    }
}
