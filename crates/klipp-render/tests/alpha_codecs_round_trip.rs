//! #3264: ProRes 4444 and FFV1 keep a frame's alpha when asked to, and
//! read back what the encoder was given: FFV1 sample for sample, ProRes
//! within its 10-bit coding. Without alpha asked for, neither writes one.

use std::path::Path;

use klipp_core::*;
use klipp_render::snapshot::SequenceSnapshot;

const W: u32 = 64;
const H: u32 = 32;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn snapshot() -> SequenceSnapshot {
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0,
        fps_num: 25,
        fps_den: 1,
        width: W,
        height: H,
        sample_rate: 48_000,
        duration_frames: 3,
        export_range: None,
        video: Default::default(),
        audio_tracks: Vec::new(),
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
        grade_encoding: GradeEncoding::Bt1886,
    }
}

/// A frame of linear light, straight alpha: a colour ramp across, an
/// alpha ramp down, with fully transparent and fully opaque rows.
fn frame(seed: f32) -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            [x * 0.9, (seed + 0.5 * y).min(1.0), 0.25 * (1.0 - x), y]
        })
        .collect()
}

/// The file's stored pixel format, by ffprobe.
fn pix_fmt(path: &Path) -> String {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Every frame of the file as 16-bit RGBA.
fn decode(path: &Path) -> Vec<u16> {
    let o = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba64le", "-"])
        .output()
        .unwrap();
    o.stdout.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect()
}

/// Encode three frames with `codec` in `format`, alpha as asked; the
/// file, and the 16-bit RGBA the encoder was given.
fn encode(format: ExportFormat, codec: ExportVideoCodec, alpha: bool) -> (std::path::PathBuf, Vec<u16>) {
    let dir = klipp_testing::scratch::scratch_dir("klipp-alpha-codecs");
    let path = dir.join(format!("{codec:?}-{alpha}.{}", format.extension()));
    let settings = ExportSettings { output_path: path.clone(), format, video_codec: codec, alpha, audio_codec: ExportAudioCodec::Copy, ..Default::default() };
    let mut enc = klipp_render::Encoder::open(&path, &settings, &snapshot()).unwrap_or_else(|e| panic!("{codec:?}: {e}"));
    let display = klipp_render::encoder::export_display(&settings, WorkingColorspace::Rec709Linear);
    let mut given = Vec::new();
    for f in 0..3 {
        let linear = frame(f as f32 * 0.2);
        given.extend(display.frame(&linear, |v, n| klipp_color::display::quantise(v, n, 65535.0) as u16));
        enc.encode_linear_frame(&linear, W, H).unwrap();
    }
    enc.finalise().unwrap();
    (path, given)
}

fn worst(a: &[u16], b: &[u16], channel: impl Fn(usize) -> bool) -> i32 {
    a.iter().zip(b).enumerate().filter(|(i, _)| channel(i % 4)).map(|(_, (x, y))| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0)
}

#[test]
fn prores_4444_and_ffv1_keep_alpha_and_read_back_what_was_given() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    // FFV1: lossless, sample for sample.
    let (path, given) = encode(ExportFormat::Mkv, ExportVideoCodec::Ffv1, true);
    assert_eq!(pix_fmt(&path), "gbrap16le");
    let back = decode(&path);
    assert_eq!(back.len(), given.len(), "three frames");
    assert_eq!(worst(&back, &given, |c| c < 3), 0, "FFV1 colour, exactly");
    assert_eq!(worst(&back, &given, |c| c == 3), 0, "FFV1 alpha, exactly");
    eprintln!("MEASURE FFV1 with alpha: {} bytes for 3 frames of {W}x{H}", std::fs::metadata(&path).unwrap().len());

    // ProRes 4444: alpha as a plane of its own; colour within its 10-bit
    // 4:4:4 coding (a level of ten bits is 64 of sixteen).
    let (path, given) = encode(ExportFormat::Mov, ExportVideoCodec::ProRes4444, true);
    assert!(pix_fmt(&path).starts_with("yuva444p"), "{}", pix_fmt(&path));
    // Tagged with the colour it was made in, so no decoder guesses by
    // the frame's size.
    let tags = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_space,color_primaries,color_transfer", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&tags.stdout).trim(), "bt709,bt709,bt709", "ProRes tagged BT.709");
    let back = decode(&path);
    assert_eq!(back.len(), given.len());
    let (colour, alpha) = (worst(&back, &given, |c| c < 3), worst(&back, &given, |c| c == 3));
    eprintln!("MEASURE ProRes 4444 with alpha: worst colour {colour}, worst alpha {alpha} (of 65535)");
    assert!(colour <= 4 * 64, "ProRes colour within four 10-bit levels: {colour}");
    assert!(alpha <= 64, "ProRes alpha within a 10-bit level: {alpha}");
    let transparent = back.chunks_exact(4).take(W as usize).all(|p| p[3] < 64);
    let opaque = back.chunks_exact(4).skip(((H - 1) * W) as usize).take(W as usize).all(|p| p[3] > 65535 - 64);
    assert!(transparent && opaque, "the top row transparent, the bottom row opaque");

    // Without alpha asked for, no alpha plane, and every pixel opaque.
    for (format, codec, want) in [(ExportFormat::Mkv, ExportVideoCodec::Ffv1, "gbrp16le"), (ExportFormat::Mov, ExportVideoCodec::ProRes4444, "yuv444p")] {
        let (path, _) = encode(format, codec, false);
        let fmt = pix_fmt(&path);
        assert!(fmt.starts_with(want) && !fmt.contains('a') || fmt == want, "{codec:?} without alpha: {fmt}");
        assert!(decode(&path).chunks_exact(4).all(|p| p[3] == 65535), "{codec:?}: opaque");
    }
}

/// Alpha is only for codecs that hold one: another codec asked for it
/// writes none.
#[test]
fn alpha_is_written_only_where_the_codec_holds_one() {
    let ask = |video_codec| ExportSettings { video_codec, alpha: true, ..Default::default() }.writes_alpha();
    assert!(ask(ExportVideoCodec::ProRes4444) && ask(ExportVideoCodec::Ffv1));
    assert!(!ask(ExportVideoCodec::H264) && !ask(ExportVideoCodec::ProRes422HQ) && !ask(ExportVideoCodec::Av1));
    assert!(!ExportSettings { video_codec: ExportVideoCodec::ProRes4444, ..Default::default() }.writes_alpha(), "off unless asked");
    assert!(ExportFormat::Mkv.holds_video(ExportVideoCodec::Ffv1) && !ExportFormat::Mov.holds_video(ExportVideoCodec::Ffv1) && !ExportFormat::Mp4.holds_video(ExportVideoCodec::Ffv1));
}

/// Klipp reads the alpha back as it decodes any media into linear light:
/// a bounced clip lands on the timeline with its transparent parts.
#[test]
fn klipp_reads_the_alpha_of_prores_4444_and_ffv1_back() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    for (format, codec) in [(ExportFormat::Mkv, ExportVideoCodec::Ffv1), (ExportFormat::Mov, ExportVideoCodec::ProRes4444)] {
        let (path, _) = encode(format, codec, true);
        let mut reader = klipp_media::frame_reader::SeekableFrameReader::open_linear(&path, None, WorkingColorspace::Rec709Linear).unwrap_or_else(|| panic!("{codec:?} opens"));
        let (linear, w, h) = reader.decode_at(0.0).unwrap_or_else(|| panic!("{codec:?} decodes"));
        assert_eq!((w, h), (W, H));
        let want = frame(0.0);
        let worst = linear.iter().zip(&want).skip(3).step_by(4).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // Colour, through the display encode and back, compared encoded
        // (sRGB), where the codec's levels are even: within the codec.
        let enc = |v: f32| klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
        let colour = linear.chunks_exact(4).zip(want.chunks_exact(4)).map(|(a, b)| (0..3).map(|c| (enc(a[c]) - enc(b[c])).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        eprintln!("MEASURE {codec:?} read back by Klipp: worst alpha {worst:.5}, worst colour {:.2} 10-bit levels (encoded)", colour * 1023.0);
        let (alpha_bound, colour_bound) = match codec {
            // Lossless at 16 bits: what is left is the encode's dither.
            ExportVideoCodec::Ffv1 => (1.0 / 65535.0 + 1e-6, 2.0 / 65535.0),
            // 10-bit 4:4:4: ffmpeg reads it back within three levels.
            // Klipp's native Y'CbCr decode sits about four levels off
            // on this frame's steep ramps (64 px across), sampling the
            // chroma half a pixel over, as #3266 finds between the
            // decoders; not the alpha's doing.
            _ => (2.0 / 1023.0, 6.0 / 1023.0),
        };
        assert!(worst <= alpha_bound, "{codec:?}: alpha read back, worst {worst}");
        assert!(colour <= colour_bound, "{codec:?}: colour read back, worst {colour}");
    }
}
