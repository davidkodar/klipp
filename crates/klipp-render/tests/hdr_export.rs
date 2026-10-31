//! HDR deliveries: an HLG or PQ export writes HDR pixels, not an SDR
//! picture tagged as HDR.
//!
//! Each case exports through the real `ExportJob` at 10 bits and reads
//! back the Y'CbCr codes the file carries. Expected codes come from the
//! published curves and BT.2408's reference levels, not from Klipp.
//! Sources are generated with the ffmpeg CLI; the tests skip without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 640;
const H: u32 = 360;
const FRAMES: u64 = 3;
const BAND: u32 = W / 4;

fn tmp(name: &str) -> PathBuf {
    let d = klipp_testing::scratch::scratch_dir("klipp-hdr-export");
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Whether a 10-bit HEVC file can be made here, which these exports
/// write; said when not, and the test passes over it.
fn hevc_10_bit_here() -> bool {
    let here = klipp_testing::encoders::libx265_encodes_10_bit();
    if !here {
        eprintln!("skipping: this ffmpeg's libx265 encodes no 10-bit");
    }
    here
}

fn clip(kind: klipp_timeline::ClipKind) -> klipp_timeline::Clip {
    let tc = |f| RationalTime::from_frames(f, 25);
    klipp_timeline::Clip { kind, source_in: tc(0), source_out: tc(FRAMES as i64), ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES as i64)) }
}

/// Export a one-clip timeline as 10-bit H.265, near-lossless, adjusted
/// by `tweak`, from a BT.709-linear project.
fn export(c: klipp_timeline::Clip, media: Option<(MediaId, MediaFile)>, out: &Path, tweak: impl FnOnce(&mut ExportSettings)) {
    export_ws(c, media, out, WorkingColorspace::Rec709Linear, tweak)
}

/// [`export`], from a project in `ws`.
fn export_ws(
    c: klipp_timeline::Clip,
    media: Option<(MediaId, MediaFile)>,
    out: &Path,
    ws: WorkingColorspace,
    tweak: impl FnOnce(&mut ExportSettings),
) {
    export_seq(c, media, out, ws, |_| {}, tweak)
}

/// [`export_ws`], with the sequence adjusted by `seq` first.
fn export_seq(
    c: klipp_timeline::Clip,
    media: Option<(MediaId, MediaFile)>,
    out: &Path,
    ws: WorkingColorspace,
    seq: impl FnOnce(&mut klipp_timeline::Sequence),
    tweak: impl FnOnce(&mut ExportSettings),
) {
    let mut video = VideoTimeline::default();
    seq(&mut video.sequence);
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![c];
    video.sequence.tracks.push(t);
    video.media.extend(media);
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: ws, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let mut settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H265,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 10, hdr_mode: HdrMode::Pq, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
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

fn media_clip(path: PathBuf, log_format: Option<LogFormat>) -> (klipp_timeline::Clip, Option<(MediaId, MediaFile)>) {
    let id = MediaId::new();
    (clip(klipp_timeline::ClipKind::Media { media_id: id }), Some((id, MediaFile { path, log_format, source_rotation: 0, ..Default::default() })))
}

fn white() -> klipp_timeline::Clip {
    clip(klipp_timeline::ClipKind::Generator(GeneratorData::SolidColor { color: [255, 255, 255, 255] }))
}

/// A lossless 10-bit 4:2:2 clip of four vertical bands, each a flat
/// limited-range Y'CbCr colour. FFV1 decodes in software, like ProRes.
fn band_clip(name: &str, bands: [[u16; 3]; 4], tags: &[&str]) -> Option<PathBuf> {
    if !ffmpeg_ok() {
        return None;
    }
    let raw = tmp(&format!("{name}.yuv"));
    let out = tmp(&format!("{name}.mkv"));
    let mut bytes = Vec::new();
    for _ in 0..FRAMES {
        for plane in 0..3 {
            let w = if plane == 0 { W } else { W / 2 };
            for _ in 0..H {
                for x in 0..w {
                    let band = (x * W / w / BAND) as usize;
                    bytes.extend_from_slice(&bands[band][plane].to_le_bytes());
                }
            }
        }
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

/// The first frame's 10-bit Y'CbCr at (x, y): the codes in the file, no
/// conversion.
fn codes_at(path: &Path, x: u32, y: u32) -> [i32; 3] {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let s: Vec<u16> = out.stdout.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let (cw, ch) = (W / 2, H / 2);
    let yp = &s[..(W * H) as usize];
    let up = &s[(W * H) as usize..][..(cw * ch) as usize];
    let vp = &s[(W * H + cw * ch) as usize..][..(cw * ch) as usize];
    [yp[(y * W + x) as usize] as i32, up[(y / 2 * cw + x / 2) as usize] as i32, vp[(y / 2 * cw + x / 2) as usize] as i32]
}

fn band_codes(path: &Path, band: u32) -> [i32; 3] {
    codes_at(path, band * BAND + BAND / 2, H / 2)
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v"]).args(args)
        .arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn pq_signal(nits: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let y = (nits / 10000.0f64).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

/// A limited-range 10-bit luma code for a signal in [0, 1].
fn y10(signal: f64) -> i32 {
    (64.0 + signal * 876.0).round() as i32
}

fn pq_y(nits: f64) -> u16 {
    y10(pq_signal(nits)) as u16
}

const TAGS_PQ: &[&str] = &["-color_range", "tv", "-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "smpte2084"];

fn assert_codes(label: &str, got: [i32; 3], want: [i32; 3], tol: i32) {
    eprintln!("{label}: file {got:?}, want {want:?}");
    for c in 0..3 {
        assert!((got[c] - want[c]).abs() <= tol, "{label}: the file carries {got:?}, want {want:?}");
    }
}

/// An ungraded PQ clip exported as PQ comes back as it went in — every
/// level up to the 1000-nit master's peak, a saturated BT.2020 colour
/// outside the BT.709 working gamut included — and a 4000-nit highlight is
/// held at the peak. The export used to write the SDR tone-mapped picture,
/// sRGB-encoded, under PQ tags: 100 nits came out as 0.56 of PQ's range.
#[test]
fn a_pq_clip_exports_to_pq_as_it_came() {
    if !hevc_10_bit_here() {
        return;
    }
    let bands = [[pq_y(5.0), 512, 512], [pq_y(203.0), 512, 512], [450, 400, 650], [pq_y(4000.0), 512, 512]];
    let Some(src) = band_clip("pq_src", bands, TAGS_PQ) else { return };
    let out = tmp("pq_to_pq.mp4");
    let (c, m) = media_clip(src, None);
    export(c, m, &out, |_| {});
    for band in 0..3 {
        let want = bands[band as usize].map(|v| v as i32);
        assert_codes(&format!("band {band}"), band_codes(&out, band), want, 3);
    }
    assert_codes("4000 nits", band_codes(&out, 3), [pq_y(1000.0) as i32, 512, 512], 3);

    let tags = probe(&out, &["-show_entries", "stream=color_space,color_transfer,color_primaries", "-of", "default=nw=1"]);
    for want in ["color_space=bt2020nc", "color_transfer=smpte2084", "color_primaries=bt2020"] {
        assert!(tags.contains(want), "PQ export lacks {want}: {tags}");
    }
    // MaxCLL is the held peak; MaxFALL the bands' average brightest
    // channel, (5 + 203 + the colour's red + 1000) / 4.
    let red = {
        let (y, cr) = ((450.0 - 64.0) / 876.0, (650.0 - 512.0) / 896.0);
        let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
        let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
        let p = f64::powf(y + 1.4746 * cr, 1.0 / m2);
        10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
    };
    let fall = (5.0 + 203.0 + red + 1000.0) / 4.0;
    let side = probe(&out, &["-show_streams", "-of", "default=nw=1"]);
    let value = |key: &str| -> f64 {
        let line = side.lines().find(|l| l.starts_with(key)).unwrap_or_else(|| panic!("no {key} in {side}"));
        let v = line[key.len() + 1..].split('/').map(|p| p.parse::<f64>().unwrap()).collect::<Vec<_>>();
        if v.len() == 2 { v[0] / v[1] } else { v[0] }
    };
    eprintln!("MaxCLL {} MaxFALL {} mastering peak {}", value("max_content"), value("max_average"), value("max_luminance"));
    assert!((value("max_content") - 1000.0).abs() <= 5.0);
    assert!((value("max_average") - fall).abs() <= 3.0, "MaxFALL {} want {fall}", value("max_average"));
    assert_eq!(value("max_luminance"), 1000.0);
}

/// SDR material sits where BT.2408 puts it in an HDR programme: white at
/// 203 cd/m², which is PQ 58% and HLG 75%.
#[test]
fn sdr_white_lands_on_the_bt2408_reference_level() {
    if !hevc_10_bit_here() {
        return;
    }
    if !ffmpeg_ok() {
        return;
    }
    let out = tmp("white_pq.mp4");
    export(white(), None, &out, |_| {});
    assert_codes("white, PQ", codes_at(&out, W / 2, H / 2), [y10(pq_signal(203.0)), 512, 512], 2);

    // ProRes, the usual HDR mastering format, the same.
    let out = tmp("white_pq.mov");
    export(white(), None, &out, |s| {
        s.video_codec = ExportVideoCodec::ProRes422HQ;
        s.format = ExportFormat::Mov;
    });
    assert_codes("white, PQ ProRes", codes_at(&out, W / 2, H / 2), [y10(pq_signal(203.0)), 512, 512], 2);

    let out = tmp("white_hlg.mp4");
    export(white(), None, &out, |s| s.hdr_mode = HdrMode::Hlg);
    assert_codes("white, HLG", codes_at(&out, W / 2, H / 2), [y10(0.75), 512, 512], 2);
    let tags = probe(&out, &["-show_entries", "stream=color_transfer,color_primaries", "-of", "default=nw=1"]);
    assert!(tags.contains("color_transfer=arib-std-b67") && tags.contains("color_primaries=bt2020"), "{tags}");
    // HDR10's light-level metadata describes PQ; HLG carries none.
    let side = probe(&out, &["-show_streams", "-of", "default=nw=1"]);
    assert!(!side.contains("max_content"), "HLG export signals MaxCLL: {side}");
}

/// The first frame's text level: its commonest 10-bit luma code above
/// the black, the inside of the letters (their edges ring a few codes
/// over in the encode).
fn text_luma(path: &Path) -> i32 {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut count = std::collections::HashMap::new();
    for b in out.stdout[..(W * H * 2) as usize].chunks_exact(2) {
        let y = u16::from_le_bytes([b[0], b[1]]) as i32;
        if y > 200 {
            *count.entry(y).or_insert(0u32) += 1;
        }
    }
    let mut top: Vec<_> = count.into_iter().collect();
    top.sort_by_key(|&(y, n)| (std::cmp::Reverse(n), y));
    eprintln!("{}: commonest text codes {:?}", path.display(), &top[..top.len().min(5)]);
    top.first().expect("text in the frame").0
}

/// A title's white in a PQ delivery is the sequence's title brightness
/// (#1723): SDR reference white unless set, the set level when it is,
/// and never above the master's peak. Its black stays black.
#[test]
fn a_title_in_a_pq_export_is_as_bright_as_the_sequence_asks() {
    if !hevc_10_bit_here() {
        return;
    }
    if !ffmpeg_ok() {
        return;
    }
    let title = || {
        clip(klipp_timeline::ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox {
            text: "HH".into(),
            font_size: 400.0,
            ..Default::default()
        })))
    };
    let level = |name: &str, nits: f32, peak: f32| {
        let out = tmp(name);
        export_seq(title(), None, &out, WorkingColorspace::Rec709Linear, |s| s.title_nits = nits, |s| s.mastering_peak_nits = peak);
        assert_eq!(codes_at(&out, 2, 2)[0], 64, "{name}: the background is black");
        text_luma(&out)
    };
    let (at_default, brighter) = (level("title_203.mp4", 203.0, 1000.0), level("title_600.mp4", 600.0, 1000.0));
    eprintln!("title white: 203 nits → {at_default}, 600 nits → {brighter}");
    assert!((at_default - y10(pq_signal(203.0))).abs() <= 2, "{at_default}");
    assert!((brighter - y10(pq_signal(600.0))).abs() <= 2, "{brighter}");
    let held = level("title_2000.mp4", 2000.0, 1000.0);
    assert!((held - y10(pq_signal(1000.0))).abs() <= 2, "2000 nits on a 1000-nit master: {held}");
    // An SDR export of the same sequence is untouched by it.
    let out = tmp("title_sdr.mp4");
    export_seq(title(), None, &out, WorkingColorspace::Rec709Linear, |s| s.title_nits = 600.0, |s| s.hdr_mode = HdrMode::Sdr);
    assert!((text_luma(&out) - 940).abs() <= 2, "SDR white: {}", text_luma(&out));
}

/// H.264 at 10 bits delivers HDR. This is the combination the export
/// dialog offers and that nothing here covered: every HDR case above is
/// H.265 or ProRes, and every H.264 case elsewhere is 8-bit SDR, so the
/// one cell a user actually clicked — "H.264" plus "HDR" — was the hole in
/// the matrix. It failed at `avcodec_open2` with nothing but "Generic
/// error in an external library", because the 10-bit request went to
/// `h264_nvenc`, whose H.264 block is 8-bit only.
///
/// Both transfers, because the fallback has to survive the choice of
/// curve, and the codes are checked rather than just `run()` returning
/// Ok: dropping to a software encoder must still deliver HDR, not a
/// silently 8-bit picture under HDR tags.
#[test]
fn h264_delivers_hdr_at_ten_bits() {
    if !ffmpeg_ok() {
        return;
    }
    for (mode, want, label) in [
        (HdrMode::Pq, y10(pq_signal(203.0)), "PQ"),
        (HdrMode::Hlg, y10(0.75), "HLG"),
    ] {
        let out = tmp(&format!("white_h264_{label}.mp4"));
        export(white(), None, &out, |s| {
            s.video_codec = ExportVideoCodec::H264;
            s.hdr_mode = mode;
        });
        assert_codes(&format!("white, {label} H.264"), codes_at(&out, W / 2, H / 2), [want, 512, 512], 2);
        // And the file really is 10-bit: an 8-bit encoder that accepted
        // the job would put reference white at the same *fraction*, so
        // the codes above would pass while the delivery lost two bits.
        let fmt = probe(&out, &["-show_entries", "stream=pix_fmt", "-of", "default=nw=1"]);
        assert!(fmt.contains("10"), "{label} H.264 export is not 10-bit: {fmt}");
    }
}

fn slog3_to_linear(code: f64) -> f64 {
    if code >= 171.210_294_692_9 {
        10f64.powf((code - 420.0) / 261.5) * 0.19 - 0.01
    } else {
        (code - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
    }
}

/// Camera log renders for the HDR display: black at black, 18% grey at
/// BT.2408's 26 cd/m² (PQ 38%), 90% white near SDR white, a highlight
/// five stops over white rolled into the 1000-nit peak rather than
/// clipped at SDR white as before.
#[test]
fn slog3_exports_to_pq_with_bt2408_grey_and_its_highlights() {
    if !hevc_10_bit_here() {
        return;
    }
    let bands = [95u16, 420, 598, 1000].map(|y| [y, 512, 512]);
    let tags_709 = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
    let Some(src) = band_clip("slog3_src", bands, &tags_709) else { return };
    let out = tmp("slog3_pq.mp4");
    let (c, m) = media_clip(src, Some(LogFormat::SLog3));
    export(c, m, &out, |_| {});
    assert_codes("black", band_codes(&out, 0), [64, 512, 512], 2);
    assert_codes("18% grey", band_codes(&out, 1), [y10(pq_signal(26.0)), 512, 512], 3);
    assert!((slog3_to_linear(598.0) - 0.9).abs() < 1e-3);
    let white = band_codes(&out, 2)[0];
    assert!((y10(pq_signal(160.0))..=y10(pq_signal(200.0))).contains(&white), "90% white at code {white}");
    let hot = band_codes(&out, 3)[0];
    assert!((y10(pq_signal(900.0))..y10(pq_signal(1000.0))).contains(&hot), "highlight at code {hot}");
}

/// An ACES Rec.2100 PQ output transform writes PQ pixels, so its file is
/// tagged PQ whatever the HDR setting says. It was tagged BT.709.
#[test]
fn an_aces_pq_transform_is_a_pq_delivery() {
    if !hevc_10_bit_here() {
        return;
    }
    if !ffmpeg_ok() {
        return;
    }
    let out = tmp("aces_pq.mp4");
    export(white(), None, &out, |s| {
        s.hdr_mode = HdrMode::Sdr;
        s.odt = OdtChoice::Rec2100Pq1000;
    });
    let tags = probe(&out, &["-show_entries", "stream=color_transfer,color_primaries,color_space", "-of", "default=nw=1"]);
    for want in ["color_space=bt2020nc", "color_transfer=smpte2084", "color_primaries=bt2020"] {
        assert!(tags.contains(want), "ACES PQ export lacks {want}: {tags}");
    }
}

/// The mastering-display max luminance the file carries, cd/m².
fn mastering_peak(path: &Path) -> f64 {
    let side = probe(path, &["-show_streams", "-of", "default=nw=1"]);
    let line = side
        .lines()
        .find(|l| l.starts_with("max_luminance"))
        .unwrap_or_else(|| panic!("no max_luminance in {side}"));
    let v: Vec<f64> = line["max_luminance".len() + 1..]
        .split('/')
        .map(|p| p.parse::<f64>().unwrap())
        .collect();
    if v.len() == 2 { v[0] / v[1] } else { v[0] }
}

/// The mastering peak is the user's to pick. The same PQ source, mastered
/// for a 4000-nit display, keeps the highlights a 1000-nit master holds at
/// its peak — and the file says which display it was graded for, so a
/// player tone-maps it for the one in front of it. Everything below either
/// peak is untouched by the choice: reference white stays where BT.2408
/// puts it.
#[test]
fn a_pq_export_masters_to_the_peak_the_user_picked() {
    if !hevc_10_bit_here() {
        return;
    }
    let bands = [[pq_y(203.0), 512, 512], [pq_y(2000.0), 512, 512], [pq_y(4000.0), 512, 512], [pq_y(500.0), 512, 512]];
    let Some(src) = band_clip("peak_src", bands, TAGS_PQ) else { return };

    // The default 1000-nit master: both highlights held at the peak.
    let out = tmp("peak_1000.mp4");
    let (c, m) = media_clip(src.clone(), None);
    export(c, m, &out, |_| {});
    assert_codes("203 nits, 1000-nit master", band_codes(&out, 0), [pq_y(203.0) as i32, 512, 512], 3);
    assert_codes("2000 nits, 1000-nit master", band_codes(&out, 1), [pq_y(1000.0) as i32, 512, 512], 3);
    assert_codes("4000 nits, 1000-nit master", band_codes(&out, 2), [pq_y(1000.0) as i32, 512, 512], 3);
    assert_codes("500 nits, 1000-nit master", band_codes(&out, 3), [pq_y(500.0) as i32, 512, 512], 3);
    assert_eq!(mastering_peak(&out), 1000.0);

    // The same source mastered for 4000 nits: the 2000-nit highlight is
    // below the peak now and comes back as it came.
    let out = tmp("peak_4000.mp4");
    let (c, m) = media_clip(src, None);
    export(c, m, &out, |s| s.mastering_peak_nits = 4000.0);
    assert_codes("203 nits, 4000-nit master", band_codes(&out, 0), [pq_y(203.0) as i32, 512, 512], 3);
    assert_codes("2000 nits, 4000-nit master", band_codes(&out, 1), [pq_y(2000.0) as i32, 512, 512], 3);
    assert_codes("4000 nits, 4000-nit master", band_codes(&out, 2), [pq_y(4000.0) as i32, 512, 512], 3);
    assert_codes("500 nits, 4000-nit master", band_codes(&out, 3), [pq_y(500.0) as i32, 512, 512], 3);
    assert_eq!(mastering_peak(&out), 4000.0);
}

/// A BT.2020 project is the shortest path for HDR: the most common HDR
/// source arrives in the basis the grade already works in, and every HDR
/// delivery leaves in it. Neither end converts, so the file comes back
/// carrying the codes it went in with — including a saturated BT.2020
/// colour that has no BT.709 expression at all, which in a BT.709
/// project makes the round trip only because the two matrices undo each
/// other exactly.
///
/// The second half is the one that would catch a regression: the same
/// source through a BT.709 project must land on the same codes. If
/// `working_to_bt2020` ever converted a BT.2020 project again, or the
/// decoder converted it in, the two would differ.
#[test]
fn a_bt2020_project_exports_a_bt2020_source_as_it_came() {
    if !hevc_10_bit_here() {
        return;
    }
    let bands = [[pq_y(5.0), 512, 512], [pq_y(203.0), 512, 512], [450, 400, 650], [pq_y(1000.0), 512, 512]];
    let Some(src) = band_clip("bt2020_ws_src", bands, TAGS_PQ) else { return };

    let wide = tmp("bt2020_project.mp4");
    let (c, m) = media_clip(src.clone(), None);
    export_ws(c, m, &wide, WorkingColorspace::Bt2020Linear, |_| {});
    for band in 0..4 {
        let want = bands[band as usize].map(|v| v as i32);
        assert_codes(&format!("band {band}, BT.2020 project"), band_codes(&wide, band), want, 3);
    }

    let narrow = tmp("rec709_project.mp4");
    let (c, m) = media_clip(src, None);
    export_ws(c, m, &narrow, WorkingColorspace::Rec709Linear, |_| {});
    for band in 0..4 {
        assert_codes(
            &format!("band {band}, BT.709 project"),
            band_codes(&narrow, band),
            band_codes(&wide, band),
            3,
        );
    }
}

/// An SDR delivery of a BT.2020 project is converted into BT.709 rather
/// than encoded as if it were already there. Neutral is untouched —
/// shared D65 — and a saturated colour is not: encoding BT.2020 values
/// straight to sRGB, as the old stub did, reads as a more saturated
/// colour than the file holds.
#[test]
fn an_sdr_export_of_a_bt2020_project_is_converted() {
    if !hevc_10_bit_here() {
        return;
    }
    let bands = [[pq_y(203.0), 512, 512], [450, 400, 650], [pq_y(203.0), 512, 512], [pq_y(5.0), 512, 512]];
    let Some(src) = band_clip("bt2020_sdr_src", bands, TAGS_PQ) else { return };

    let sdr = |out: &Path, ws| {
        let (c, m) = media_clip(src.clone(), None);
        export_ws(c, m, out, ws, |s| {
            s.hdr_mode = HdrMode::Sdr;
            s.bit_depth = 10;
        });
    };
    let wide = tmp("bt2020_sdr.mp4");
    let narrow = tmp("rec709_sdr.mp4");
    sdr(&wide, WorkingColorspace::Bt2020Linear);
    sdr(&narrow, WorkingColorspace::Rec709Linear);

    // Grey stays grey, and lands where the BT.709 project puts it.
    for band in [0, 2] {
        let g = band_codes(&wide, band);
        assert!((g[1] - 512).abs() <= 4 && (g[2] - 512).abs() <= 4, "grey drifted to {g:?}");
        assert_codes(&format!("grey band {band}"), g, band_codes(&narrow, band), 4);
    }
    // The saturated band does not: it is a different colour once read as
    // BT.2020, and BT.709 cannot hold it.
    let (w, n) = (band_codes(&wide, 1), band_codes(&narrow, 1));
    assert!(w != n, "saturated colour still encoded as if it were BT.709: {w:?}");
}
