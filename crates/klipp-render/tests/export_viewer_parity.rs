//! Export vs viewer: does an exported file show what the viewer showed?
//!
//! Each case exports through the real `ExportJob`, decodes the result the
//! way a player honouring the file's BT.709 tags does, and compares against
//! what the viewer shows for the same timeline. Sources are generated with
//! the ffmpeg CLI; the tests skip without it.

use klipp_timeline::GradeAccess;
use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 1920;
const H: u32 = 1080;
const FRAMES: u64 = 5;

/// See `viewer_matches_export`'s copy: a directory per test process so
/// parallel runs cannot read each other's half-written media, and a
/// sweep of finished runs' directories so they do not accumulate. These
/// were 42 MB apiece and nothing removed them.
fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-export-parity", name)
}

/// A clip for a one-track test timeline, with the media entry its file
/// needs.
struct TestClip {
    clip: klipp_timeline::Clip,
    media: Option<(MediaId, MediaFile)>,
}

fn base_clip(kind: klipp_timeline::ClipKind) -> klipp_timeline::Clip {
    let tc = |f| RationalTime::from_frames(f, 25);
    klipp_timeline::Clip { kind, source_in: tc(0), source_out: tc(FRAMES as i64), ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES as i64)) }
}

fn media(path: PathBuf, log_format: Option<LogFormat>) -> TestClip {
    let id = MediaId::new();
    TestClip {
        clip: base_clip(klipp_timeline::ClipKind::Media { media_id: id }),
        media: Some((id, MediaFile { path, log_format, source_rotation: 0, ..Default::default() })),
    }
}

fn generated(data: GeneratorData) -> TestClip {
    TestClip { clip: base_clip(klipp_timeline::ClipKind::Generator(data)), media: None }
}

impl TestClip {
    fn graded(mut self, graph: klipp_nodes::NodeGraph) -> Self {
        self.clip.set_grade(graph);
        self
    }
}

fn export(c: TestClip, out: &Path) {
    export_with(c, out, ExportVideoCodec::H264)
}

fn export_with(c: TestClip, out: &Path, codec: ExportVideoCodec) {
    let mut v = timeline(vec![vec![c.clip]]);
    v.media.extend(c.media);
    export_video(v, out, |s| {
        s.video_codec = codec;
        s.format = if codec == ExportVideoCodec::H264 { ExportFormat::Mp4 } else { ExportFormat::Mov };
    });
}

/// A 25 fps W×H timeline, tracks given top first.
fn timeline(tracks: Vec<Vec<klipp_timeline::Clip>>) -> VideoTimeline {
    let mut v = VideoTimeline::default();
    v.sequence.fps = Fps::new(25, 1);
    v.sequence.resolution = Resolution { width: W, height: H };
    for clips in tracks {
        let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
        t.clips = clips;
        v.sequence.tracks.push(t);
    }
    v
}

/// Export `video` (FRAMES long) to `out`: H.264, near-lossless, SDR,
/// "match viewer", adjusted by `tweak`.
fn export_video(video: VideoTimeline, out: &Path, tweak: impl FnOnce(&mut ExportSettings)) {
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let mut settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    tweak(&mut settings);
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        total_frames: FRAMES,
        analyser: None,
    }
    .run()
    .unwrap();
}

/// First frame as RGB24, decoded as BT.709 limited — what a player does
/// with a BT.709-tagged file. Accurate rounding: swscale's default
/// fixed-point path reads saturated blues up to 3 levels low, which would
/// hide a correct encode behind the decoder's error.
fn decode_709(path: &Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-frames:v", "1", "-vf", "scale=in_color_matrix=bt709:in_range=tv:flags=accurate_rnd+full_chroma_int,format=rgb24", "-f", "rawvideo", "-"])
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
    export(media(src.clone(), None), &out);
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
        export(generated(GeneratorData::SolidColor { color: colour }), &out);
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
        export_with(generated(GeneratorData::SolidColor { color: [90, 120, 150, 255] }), &out, codec);
        let tags = probe(&out, "stream=color_range,color_space,color_transfer,color_primaries");
        eprintln!("{codec:?}: {}", tags.replace('\n', " "));
        for want in ["color_range=tv", "color_space=bt709", "color_transfer=bt709", "color_primaries=bt709"] {
            assert!(tags.contains(want), "{codec:?} export lacks {want}: {tags}");
        }
    }
}

// ── Scene-referred sources (camera log, PQ) ─────────────────────────────
//
// Expected values come from the published curves, not from Klipp: Sony's
// S-Log3 and SMPTE ST 2084, then the viewer's documented ungraded look
// (log exposure 0.723171, PQ exposure 0.18, Narkowicz filmic, sRGB).

const BAND: u32 = W / 4;

fn filmic(x: f64) -> f64 {
    let (a, b, c, d, e) = (2.51, 0.03, 2.43, 0.59, 0.14);
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

fn srgb8(linear: f64) -> i32 {
    let v = if linear <= 0.0031308 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (v.clamp(0.0, 1.0) * 255.0).round() as i32
}

fn slog3_to_linear(code: f64) -> f64 {
    if code >= 171.210_294_692_9 {
        10f64.powf((code - 420.0) / 261.5) * 0.19 - 0.01
    } else {
        (code - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
    }
}

fn pq_code(nits: f64) -> u16 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let y = (nits / 10000.0f64).powf(m1);
    (64.0 + ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2) * 876.0).round() as u16
}

/// A lossless 10-bit 4:2:2 clip of four vertical luma bands, neutral
/// chroma. FFV1 decodes in software, like ProRes.
fn band_clip(name: &str, bands: [u16; 4], tags: &[&str]) -> Option<PathBuf> {
    if !ffmpeg_ok() { return None; }
    let raw = tmp(&format!("{name}.yuv"));
    let out = tmp(&format!("{name}.mkv"));
    let mut bytes = Vec::with_capacity((W * H * 4) as usize * 3);
    for _ in 0..3 {
        for _ in 0..H {
            for x in 0..W { bytes.extend_from_slice(&bands[(x / BAND) as usize].to_le_bytes()); }
        }
        for _ in 0..(W / 2) * H * 2 { bytes.extend_from_slice(&512u16.to_le_bytes()); }
    }
    std::fs::write(&raw, bytes).ok()?;
    let size = format!("{W}x{H}");
    let mut args: Vec<&str> = vec!["-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv422p10le", "-s", &size, "-r", "25"];
    args.extend_from_slice(tags);
    args.extend_from_slice(&["-i", raw.to_str()?, "-c:v", "ffv1", "-pix_fmt", "yuv422p10le"]);
    args.extend_from_slice(tags);
    args.push(out.to_str()?);
    let ok = std::process::Command::new("ffmpeg").args(&args).status().ok()?.success();
    let _ = std::fs::remove_file(&raw);
    ok.then_some(out)
}

const SLOG3: [u16; 4] = [95, 420, 598, 1000];
const TAGS_709: &[&str] = &["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

fn exposure_graph(stops: f32) -> klipp_nodes::NodeGraph {
    use klipp_nodes::*;
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let p = NodeParams {
        exposure: Some(stops),
        ..Default::default()
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, p), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph {
        output_node_id: nodes[2].id, nodes, edges, node_positions,
        working_space_override: None, frames: vec![], sticky_notes: vec![],
    }
}

fn check_bands(out: &Path, label: &str, want: [i32; 4]) {
    let rgb = decode_709(out);
    for (band, w) in want.iter().enumerate() {
        let got = px(&rgb, band as u32 * BAND + BAND / 2, H / 2);
        eprintln!("{label} band {band}: export {got:?}, viewer {w}");
        for c in got {
            assert!((c - w).abs() <= 2, "{label} band {band}: export {got:?}, the viewer shows {w}");
        }
    }
}

/// An ungraded S-Log3 clip exports as the viewer shows it: converted, not
/// flat. Export used to know nothing of the clip's log format and encoded
/// the log signal as if it were the picture (grey 118 came out 102, 90%
/// white 208 came out 145).
#[test]
fn ungraded_slog3_export_matches_the_viewer() {
    let Some(src) = band_clip("slog3_ungraded", SLOG3, TAGS_709) else { return };
    let out = tmp("slog3_ungraded.mp4");
    export(media(src, Some(LogFormat::SLog3)), &out);
    check_bands(&out, "S-Log3 ungraded", SLOG3.map(|c| srgb8(filmic(slog3_to_linear(c as f64) * 0.723171))));
}

/// The grade runs on scene-linear light, before the tone map, as in the
/// viewer. Two stops down brings the super-white band (~31x grey) to about
/// 8x grey — still near white after the tone map. Grading the 8-bit
/// preview, which has already clipped it to white, would give mid-grey.
#[test]
fn graded_slog3_is_graded_in_scene_linear() {
    let Some(src) = band_clip("slog3_graded", SLOG3, TAGS_709) else { return };
    let out = tmp("slog3_graded.mp4");
    export(media(src, Some(LogFormat::SLog3)).graded(exposure_graph(-2.0)), &out);
    check_bands(&out, "S-Log3 -2 stops", SLOG3.map(|c| srgb8(filmic(slog3_to_linear(c as f64) * 0.25 * 0.723171))));
}

/// PQ needs no log format: the file's tags say what it is.
#[test]
fn ungraded_pq_export_matches_the_viewer() {
    let nits = [1.0, 100.0, 1000.0, 4000.0];
    let codes = nits.map(pq_code);
    let tags = ["-color_range", "tv", "-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "smpte2084"];
    let Some(src) = band_clip("pq", codes, &tags) else { return };
    let out = tmp("pq_ungraded.mp4");
    export(media(src, None), &out);
    // Scene-linear from the codes actually written (rounded), 1.0 = 100 nits.
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let scene = |code: u16| {
        let p = ((code as f64 - 64.0) / 876.0).powf(1.0 / m2);
        100.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
    };
    check_bands(&out, "PQ ungraded", codes.map(|c| srgb8(filmic(scene(c) * 0.18))));
}

// ── The shared renderer's rules, in exported files ──────────────────────

fn solid(rgb: [u8; 3], from: i64, to: i64) -> klipp_timeline::Clip {
    let tc = |f| RationalTime::from_frames(f, 25);
    klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(GeneratorData::SolidColor { color: [rgb[0], rgb[1], rgb[2], 255] }),
        source_in: tc(0),
        source_out: tc(to - from),
        ..klipp_timeline::Clip::gap(tc(from), tc(to))
    }
}

/// Frame `n` of `path` as RGB24, decoded as BT.709.
fn decode_frame_709(path: &Path, n: u32, w: u32, h: u32) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-vf", &format!("select=eq(n\\,{n}),scale=in_color_matrix=bt709:in_range=tv:flags=accurate_rnd+full_chroma_int,format=rgb24"), "-frames:v", "1", "-f", "rawvideo", "-"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), (w * h * 3) as usize);
    out.stdout
}

fn srgb8_of_linear(v: f64) -> i32 {
    let e = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (e.clamp(0.0, 1.0) * 255.0).round() as i32
}

/// Tracks stack bottom to top and mix in linear light: blue at half
/// opacity over red is half of each *in light* — sRGB 188, not the 128 an
/// sRGB-byte composite gives.
#[test]
fn stacked_tracks_mix_in_linear_light() {
    if !ffmpeg_ok() { return; }
    let mut v = timeline(vec![vec![solid([0, 0, 255], 0, FRAMES as i64)], vec![solid([255, 0, 0], 0, FRAMES as i64)]]);
    v.sequence.tracks[0].opacity = 0.5;
    let out = tmp("stacked.mp4");
    export_video(v, &out, |_| {});
    let got = px(&decode_709(&out), W / 2, H / 2);
    let half = srgb8_of_linear(0.5);
    eprintln!("blue at half over red: export {got:?}, want [{half}, 0, {half}]");
    for (c, want) in got.iter().zip([half, 0, half]) {
        assert!((c - want).abs() <= 2, "export {got:?}, want [{half}, 0, {half}]");
    }
}

/// A precomposition in an exported file is the scene composed first and
/// faded after: green under half-red is (0.5, 0.5, 0) composed, and the
/// precomposition at half opacity puts (0.25, 0.25, 0) in the file —
/// through the real export, the real encoder, and the flattening the
/// viewer's timeline goes through.
#[test]
fn a_precomposition_exports_as_the_composed_scene_faded_once() {
    if !ffmpeg_ok() { return; }
    let red = klipp_timeline::test_clip::TestClip::of(solid([255, 0, 0], 0, FRAMES as i64)).lane("opacity", 0.0, 0.5).build();
    let scene = timeline(vec![vec![red], vec![solid([0, 255, 0], 0, FRAMES as i64)]]).sequence;
    let pre = klipp_timeline::test_clip::TestClip::of(base_clip(klipp_timeline::ClipKind::Precomposition { sequence_id: scene.id })).lane("opacity", 0.0, 0.5).build();
    let mut v = timeline(vec![vec![pre]]);
    v.sequence = klipp_timeline::flatten::flatten_sequence(&v.sequence, &[v.sequence.clone(), scene]);
    let out = tmp("precomp_faded.mp4");
    export_video(v, &out, |_| {});
    let got = px(&decode_709(&out), W / 2, H / 2);
    let q = srgb8_of_linear(0.25);
    eprintln!("the composed scene at half: export {got:?}, want [{q}, {q}, 0]");
    for (c, want) in got.iter().zip([q, q, 0]) {
        assert!((c - want).abs() <= 2, "export {got:?}, want [{q}, {q}, 0]");
    }
}

/// A precomposition of a media clip is decoded and exported like the
/// clip itself: the bars come through the scene, the instance's own
/// clip id, and the export's decoder streams, as they come through
/// directly. Flat patches compared, as `ungraded_sdr_export_reproduces_the_source` does.
#[test]
fn a_precomposition_of_media_reproduces_its_source() {
    let Some(src) = bars_source() else { return };
    let m = media(src.clone(), None);
    let scene = timeline(vec![vec![m.clip]]).sequence;
    let pre = base_clip(klipp_timeline::ClipKind::Precomposition { sequence_id: scene.id });
    let mut v = timeline(vec![vec![pre]]);
    v.media.extend(m.media);
    v.sequence = klipp_timeline::flatten::flatten_sequence(&v.sequence, &[v.sequence.clone(), scene]);
    let out = tmp("precomp_bars.mp4");
    export_video(v, &out, |_| {});
    let (a, b) = (decode_709(&src), decode_709(&out));
    let flat = |x: u32, y: u32| {
        let c = &a[((y * W + x) * 3) as usize..][..3];
        (0..81).all(|k| &a[(((y + k / 9 - 4) * W + x + k % 9 - 4) * 3) as usize..][..3] == c)
    };
    let (mut worst, mut sampled) = (0, 0);
    for gy in 1..27 {
        for gx in 1..48 {
            let (x, y) = (gx * W / 48, gy * H / 27);
            if !flat(x, y) { continue; }
            sampled += 1;
            let (p, q) = (px(&a, x, y), px(&b, x, y));
            worst = worst.max((0..3).map(|c| (p[c] - q[c]).abs()).max().unwrap());
        }
    }
    eprintln!("bars through a precomposition: worst {worst} over {sampled} flat samples");
    assert!(sampled > 100, "too few flat samples ({sampled}) to mean anything");
    assert!(worst <= 5, "the precomposition's export differs from the source by {worst} levels");
}

/// A dissolve's midpoint is half of each clip in linear light, in the
/// exported file too.
#[test]
fn a_dissolve_exports_as_a_linear_mix() {
    if !ffmpeg_ok() { return; }
    // 5 frames: red 0-2, blue 2-5, a 2-frame dissolve centred on the cut.
    let a = solid([255, 0, 0], 0, 2);
    let b = solid([0, 0, 255], 2, FRAMES as i64);
    let tr = klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: RationalTime::from_frames(2, 25),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    let mut v = timeline(vec![vec![a, b]]);
    v.sequence.tracks[0].transitions.push(tr);
    let out = tmp("dissolve.mp4");
    export_video(v, &out, |_| {});
    // Frame 2 is the cut: progress 0.5.
    let got = px(&decode_frame_709(&out, 2, W, H), W / 2, H / 2);
    let half = srgb8_of_linear(0.5);
    eprintln!("dissolve midpoint: export {got:?}, want [{half}, 0, {half}]");
    for (c, want) in got.iter().zip([half, 0, half]) {
        assert!((c - want).abs() <= 2, "export {got:?}, want [{half}, 0, {half}]");
    }
}

/// A 10-bit export carries 10 bits: a 10-bit source ramp keeps far more
/// than the ~220 luma levels 8-bit data has. The old export built every
/// frame at 8 bits and only padded it into a 10-bit container.
#[test]
fn a_10_bit_export_keeps_10_bit_levels() {
    if !ffmpeg_ok() { return; }
    // A full-width 10-bit ramp across the legal luma range.
    let raw = tmp("ramp10.yuv");
    let src = tmp("ramp10.mkv");
    let mut bytes = Vec::new();
    for _ in 0..FRAMES {
        for _ in 0..H {
            for x in 0..W {
                bytes.extend_from_slice(&((64 + x * 876 / (W - 1)) as u16).to_le_bytes());
            }
        }
        // Two 4:2:2 chroma planes, neutral.
        for _ in 0..(W / 2) * H * 2 { bytes.extend_from_slice(&512u16.to_le_bytes()); }
    }
    std::fs::write(&raw, bytes).unwrap();
    let tags = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
    let size = format!("{W}x{H}");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv422p10le", "-s", &size, "-r", "25"])
        .args(tags).arg("-i").arg(&raw).args(["-c:v", "ffv1", "-pix_fmt", "yuv422p10le"]).args(tags).arg(&src)
        .status().unwrap().success();
    assert!(ok);
    let out = tmp("ramp10_export.mov");
    export_with(media(src, None), &out, ExportVideoCodec::ProRes422HQ);
    let dec = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(&out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv422p10le", "-"])
        .output().unwrap();
    let row = H / 2;
    let levels: std::collections::BTreeSet<u16> = (0..W)
        .map(|x| u16::from_le_bytes([dec.stdout[((row * W + x) * 2) as usize], dec.stdout[((row * W + x) * 2 + 1) as usize]]))
        .collect();
    eprintln!("10-bit ramp: {} distinct luma levels across {W} px", levels.len());
    assert!(levels.len() > 600, "only {} levels: 8-bit data in a 10-bit file", levels.len());
}

/// An export at another aspect is letterboxed, not stretched: a 16:9
/// timeline into a square frame keeps its picture in a centred band with
/// black above and below.
#[test]
fn another_aspect_is_letterboxed() {
    if !ffmpeg_ok() { return; }
    let out = tmp("letterbox.mp4");
    export_video(timeline(vec![vec![solid([255, 255, 255], 0, FRAMES as i64)]]), &out, |s| {
        s.resolution = Some(Resolution { width: 1080, height: 1080 });
    });
    let rgb = decode_frame_709(&out, 0, 1080, 1080);
    let at = |x: u32, y: u32| rgb[((y * 1080 + x) * 3) as usize] as i32;
    // 1920x1080 fits 1080 wide as 1080x608 (even): bars of 236 px.
    assert!(at(540, 100) < 5, "top bar should be black: {}", at(540, 100));
    assert!(at(540, 980) < 5, "bottom bar should be black: {}", at(540, 980));
    assert!(at(540, 540) > 250, "picture should be white: {}", at(540, 540));
}


// ── Full-range sources ──────────────────────────────────────────────────
//
// Screen recorders and phones write full-range Y'CbCr, often with the
// BT.601 matrix and no transfer tag (wf-recorder: `pc`, `bt470bg`,
// transfer unknown). The expected colours are worked out here from the
// code values, the range and the matrix, not taken from Klipp.

/// Y'CbCr code values of the patches, left to right: black and white at
/// both ranges' ends, mid grey, and a colour that BT.601 and BT.709 read
/// 7 levels apart in red.
const PATCHES: [[u8; 3]; 6] = [[0, 128, 128], [16, 128, 128], [128, 128, 128], [235, 128, 128], [255, 128, 128], [120, 100, 170]];

/// The sRGB-coded colour a correct decode gives `code`.
fn patch_rgb([y, u, v]: [u8; 3], full: bool, bt601: bool) -> [i32; 3] {
    let (y, cb, cr) = if full {
        (y as f64 / 255.0, (u as f64 - 128.0) / 255.0, (v as f64 - 128.0) / 255.0)
    } else {
        ((y as f64 - 16.0) / 219.0, (u as f64 - 128.0) / 224.0, (v as f64 - 128.0) / 224.0)
    };
    let (kr, kb) = if bt601 { (0.299, 0.114) } else { (0.2126, 0.0722) };
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as i32)
}

/// A lossless H.264 clip of [`PATCHES`] as vertical bands, 4:2:0, tagged
/// with `tags` — the decode route a screen recording takes.
fn patch_clip(name: &str, tags: &[&str]) -> Option<PathBuf> {
    if !ffmpeg_ok() { return None; }
    let raw = tmp(&format!("{name}.yuv"));
    let out = tmp(&format!("{name}.mkv"));
    let band = |x: u32| PATCHES[(x * PATCHES.len() as u32 / W) as usize];
    let mut frame = Vec::with_capacity((W * H * 3 / 2) as usize);
    for _ in 0..H { frame.extend((0..W).map(|x| band(x)[0])); }
    for c in 1..3 {
        for _ in 0..H / 2 { frame.extend((0..W / 2).map(|x| band(x * 2)[c])); }
    }
    std::fs::write(&raw, frame.repeat(FRAMES as usize + 1)).ok()?;
    let size = format!("{W}x{H}");
    let mut args: Vec<&str> = vec!["-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-r", "25"];
    args.extend_from_slice(tags);
    args.extend_from_slice(&["-i", raw.to_str()?, "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p"]);
    args.extend_from_slice(tags);
    args.push(out.to_str()?);
    let ok = std::process::Command::new("ffmpeg").args(&args).status().ok()?.success();
    let _ = std::fs::remove_file(&raw);
    ok.then_some(out)
}

/// A full-range source exports at its own levels: its black stays black
/// and its white white, nothing is lifted or clipped, and its colour is
/// read through the matrix it is tagged with. A limited-range clip is the
/// control, its 16 and 235 being black and white.
#[test]
fn full_range_sources_export_at_their_code_values() {
    let cases: [(&str, &[&str], bool, bool); 3] = [
        ("full-601-untagged", &["-color_range", "pc", "-colorspace", "bt470bg"], true, true),
        ("full-709", &["-color_range", "pc", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"], true, false),
        ("limited-709", TAGS_709, false, false),
    ];
    for (name, tags, full, bt601) in cases {
        let Some(src) = patch_clip(name, tags) else { return };
        let out = tmp(&format!("{name}.mp4"));
        export(media(src, None), &out);
        let rgb = decode_709(&out);
        let n = PATCHES.len() as u32;
        let mut worst = 0;
        for (i, code) in PATCHES.into_iter().enumerate() {
            let want = patch_rgb(code, full, bt601);
            let got = px(&rgb, (2 * i as u32 + 1) * W / (2 * n), H / 2);
            let d = (0..3).map(|c| (got[c] - want[c]).abs()).max().unwrap();
            eprintln!("{name}: code {code:?} want {want:?} export {got:?}");
            worst = worst.max(d);
        }
        assert!(worst <= 3, "{name}: the export is {worst} levels off its source's code values");
    }
}
