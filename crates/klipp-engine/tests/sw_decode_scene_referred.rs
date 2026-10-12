//! Scene-referred footage through the software decoders, end to end, on
//! real encoded files: the decode thread's CPU path (ProRes and everything
//! else NVDEC cannot take) and the single-frame decodes behind thumbnails,
//! filmstrips and browser skim frames.
//!
//! Clips are lossless FFV1 4:2:2 10-bit — software-only to decode, like
//! ProRes, and exact — made with the ffmpeg CLI (tests skip without it).
//! Expected values come from the published curves, not from Klipp: Sony's
//! S-Log3 (code 420 is 18% grey, 598 is 90% white, 1000 is ~31x grey) and
//! SMPTE ST 2084 (PQ 100 nits is scene-linear 1.0 in Klipp's convention),
//! then the viewer's documented ungraded look: exposure, Narkowicz filmic,
//! sRGB.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use klipp_core::{ColorTransfer, LogFormat};

const W: u32 = 64;
const H: u32 = 32;
const BAND: u32 = W / 4;

fn filmic(x: f64) -> f64 {
    let (a, b, c, d, e) = (2.51, 0.03, 2.43, 0.59, 0.14);
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

fn srgb8(linear: f64) -> u8 {
    let v = if linear <= 0.0031308 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Sony's published S-Log3 decode.
fn slog3_to_linear(code: f64) -> f64 {
    if code >= 171.210_294_692_9 {
        10f64.powf((code - 420.0) / 261.5) * 0.19 - 0.01
    } else {
        (code - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
    }
}

/// SMPTE ST 2084: legal-normalised signal → nits.
fn pq_to_nits(e: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let p = e.powf(1.0 / m2);
    10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

/// A 2-frame FFV1 yuv422p10le clip: four vertical bands of the given luma
/// codes, neutral chroma, tagged limited range BT.709 unless `tags` says
/// otherwise. `None` when ffmpeg is unavailable.
fn make_clip(name: &str, bands: [u16; 4], tags: &[&str]) -> Option<PathBuf> {
    make_clip_as(name, "yuv422p10le", bands, 512, tags)
}

/// [`make_clip`] in another 4:2:2 pixel format: `bands` and the neutral
/// `chroma` are codes at its depth.
fn make_clip_as(name: &str, pix_fmt: &str, bands: [u16; 4], chroma: u16, tags: &[&str]) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("klipp-sw-scene-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let raw = dir.join(format!("{name}.yuv"));
    let out = dir.join(format!("{name}.mkv"));
    let mut bytes = Vec::new();
    for _ in 0..2 {
        for _row in 0..H {
            for col in 0..W {
                bytes.extend_from_slice(&bands[(col / BAND) as usize].to_le_bytes());
            }
        }
        for _plane in 0..2 {
            for _ in 0..(W / 2) * H {
                bytes.extend_from_slice(&chroma.to_le_bytes());
            }
        }
    }
    std::fs::write(&raw, bytes).ok()?;
    let size = format!("{W}x{H}");
    // Same tags on input and output: tagging only the output makes ffmpeg
    // insert a range/matrix conversion and the codes stop being exact.
    let mut args: Vec<&str> = vec!["-nostdin", "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", pix_fmt, "-s", &size, "-r", "25"];
    args.extend_from_slice(tags);
    args.extend_from_slice(&["-i", raw.to_str()?, "-c:v", "ffv1", "-pix_fmt", pix_fmt]);
    args.extend_from_slice(tags);
    args.push(out.to_str()?);
    let ok = std::process::Command::new("ffmpeg").args(&args).status().ok()?.success();
    let _ = std::fs::remove_file(&raw);
    ok.then_some(out)
}

const SLOG3_BANDS: [u16; 4] = [95, 420, 598, 1000];
const TAGS_709: &[&str] = &["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

/// The S-Log3 clip, registered as S-Log3 the way the media pool registers a
/// file whose format the user picked. Every test here registers the same
/// map, so running them in parallel is harmless.
fn slog3_clip() -> Option<&'static Path> {
    Some(slog3_clips()?.0)
}

/// The 10-bit S-Log3 clip and the 12-bit one, both registered as S-Log3
/// in one map whichever test asks first.
fn slog3_clips() -> Option<(&'static Path, &'static Path)> {
    static CLIPS: OnceLock<Option<(PathBuf, PathBuf)>> = OnceLock::new();
    let (ten, twelve) = CLIPS
        .get_or_init(|| Some((make_clip("slog3", SLOG3_BANDS, TAGS_709)?, make_clip_as("slog3-12", "yuv422p12le", SLOG3_12_BANDS, 2048, TAGS_709)?)))
        .as_ref()?;
    klipp_engine::video_decoder::set_media_log_formats([(ten.clone(), LogFormat::SLog3), (twelve.clone(), LogFormat::SLog3)]);
    Some((ten, twelve))
}

/// 12-bit codes a quarter, a half and three quarters of a 10-bit code off
/// S-Log3's 10-bit 95, 420, 598 and 1000.
const SLOG3_12_BANDS: [u16; 4] = [381, 1681, 2394, 4001];

fn band_px(rgba: &[u8], width: u32, band: u32) -> [u8; 3] {
    let i = (((H / 2) * width + band * BAND + BAND / 2) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

#[test]
fn slog3_thumbnail_is_converted_like_the_viewer() {
    let Some(clip) = slog3_clip() else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    let (rgba, w, h) = klipp_engine::generate_thumbnail_at_size(clip, 0.0, W).unwrap();
    assert_eq!((w, h), (W, H));
    for (band, code) in SLOG3_BANDS.iter().enumerate() {
        let want = srgb8(filmic(slog3_to_linear(*code as f64) * 0.723171));
        let got = band_px(&rgba, w, band as u32);
        eprintln!("S-Log3 code {code}: thumbnail {got:?}, expected {want}");
        for c in got {
            assert!((c as i32 - want as i32).abs() <= 1, "code {code}: thumbnail {got:?}, expected {want}");
        }
    }
}

#[test]
fn pq_thumbnail_is_tone_mapped_not_washed_out() {
    // 1, 100, 1000 and 4000 nits.
    let codes: [u16; 4] = [1.0f64, 100.0, 1000.0, 4000.0].map(|nits| {
        let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
        let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
        let y = (nits / 10000.0f64).powf(m1);
        let e = ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2);
        (64.0 + e * 876.0).round() as u16
    });
    let tags = ["-color_range", "tv", "-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "smpte2084"];
    let Some(clip) = make_clip("pq", codes, &tags) else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    let (rgba, w, _) = klipp_engine::generate_thumbnail_at_size(&clip, 0.0, W).unwrap();
    for (band, code) in codes.iter().enumerate() {
        let nits = pq_to_nits((*code as f64 - 64.0) / 876.0);
        let want = srgb8(filmic(nits / 100.0 * 0.18));
        let got = band_px(&rgba, w, band as u32);
        eprintln!("PQ code {code} ({nits:.0} nits): thumbnail {got:?}, expected {want}");
        for c in got {
            assert!((c as i32 - want as i32).abs() <= 1, "PQ {nits:.0} nits: thumbnail {got:?}, expected {want}");
        }
    }
}

#[test]
fn slog3_through_the_software_decoder_carries_codes_to_the_grade() {
    let Some(clip) = slog3_clip() else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    let dec = klipp_engine::video_decoder::VideoDecoder::open(clip, 0).unwrap();
    // Decode threads start paused.
    dec.resume();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let frame = loop {
        if let Some(f) = dec.frame_queue().lock().unwrap().pop() {
            break f;
        }
        assert!(std::time::Instant::now() < deadline, "no frame decoded");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let hdr = frame.hdr.as_ref().expect("S-Log3 must reach the grade as codes, not display RGB");
    assert_eq!(hdr.transfer, klipp_color::hdr::HdrTransfer::Log(ColorTransfer::SLog3));
    let codes = hdr.cpu_codes();
    let codes = codes.packed10().expect("a 10-bit source gives 10-bit codes");
    for (band, code) in SLOG3_BANDS.iter().enumerate() {
        let i = ((H / 2) * frame.width + band as u32 * BAND + BAND / 2) as usize;
        let c = *code as u32;
        assert_eq!(klipp_color::hdr::unpack_rgb10(codes[i]), [c, c, c], "band {band}");
        // The display-referred preview is tone-mapped linear.
        let want = filmic(slog3_to_linear(*code as f64) * 0.723171);
        let got = frame.data[i * 4 + 1] as f64;
        assert!((got - want).abs() < 2e-3, "band {band}: preview {got}, expected {want}");
    }
}

/// #1226: a 12-bit S-Log3 clip reaches the grade at its own precision:
/// 16-bit codes, each band's scene-linear the published curve at the
/// band's 12-bit code (a quarter of a 10-bit code between them), not at
/// the 10-bit code it was rounded to.
#[test]
fn a_twelve_bit_clip_keeps_its_precision_to_the_grade() {
    let bands = SLOG3_12_BANDS;
    let Some((_, clip)) = slog3_clips() else {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    };
    let dec = klipp_engine::video_decoder::VideoDecoder::open(clip, 0).unwrap();
    dec.resume();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let frame = loop {
        if let Some(f) = dec.frame_queue().lock().unwrap().pop() {
            break f;
        }
        assert!(std::time::Instant::now() < deadline, "no frame decoded");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let hdr = frame.hdr.as_ref().expect("S-Log3 must reach the grade as codes");
    assert!(matches!(*hdr.cpu_codes(), klipp_color::hdr::SceneCodes::Deep16(_)), "a 12-bit source gives 16-bit codes");
    let linear = hdr.scene_linear();
    for (band, code) in bands.iter().enumerate() {
        let i = ((H / 2) * frame.width + band as u32 * BAND + BAND / 2) as usize;
        let want = slog3_to_linear(*code as f64 / 4.0);
        let got = linear[i * 4 + 1] as f64;
        // 0.05 % of the value: the 10-bit rounding it had was ~0.2 %.
        assert!((got - want).abs() <= want.abs().max(1e-3) * 5e-4, "band {band} (12-bit {code}): {got}, the curve says {want}");
    }
}

