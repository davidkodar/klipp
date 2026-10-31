//! HDR10+ dynamic metadata (#468): an H.265 PQ export with it on carries
//! SMPTE ST 2094-40 per shot, read back by ffprobe as each frame's
//! HDR10+ side data with its shot's light; with it off the video stream
//! is the same, byte for byte, but for those SEIs.
use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

/// An ffmpeg that can make the 10-bit HEVC file HDR10+ rides in: Arch
/// Linux ARM's libx265 encodes 8-bit only.
fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) && klipp_testing::encoders::libx265_encodes_10_bit()
}

/// A dark grey shot, then a white one, a second each at 25.
fn sequence() -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 320, height: 180 };
    let tc = |s: f64| Timecode::from_secs_f64(s, 25);
    let solid = |c: u8, from, to| Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color: [c, c, c, 255] }), ..Clip::gap(tc(from), tc(to)) };
    seq.tracks[0].clips = vec![solid(60, 0.0, 1.0), solid(255, 1.0, 2.0)];
    seq
}

fn export(out: &std::path::Path, hdr10plus: bool) {
    let seq = sequence();
    let pool = MediaPool::new();
    let settings = ExportSettings {
        output_path: out.to_path_buf(),
        video_codec: ExportVideoCodec::H265,
        format: ExportFormat::Mkv,
        audio_codec: ExportAudioCodec::Copy,
        crf: 20,
        preset: EncoderPreset::Fast,
        bit_depth: 10,
        hdr_mode: HdrMode::Pq,
        hdr10plus,
        ..Default::default()
    };
    let snapshot = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.to_path_buf(),
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
    .unwrap();
}

/// Each frame's HDR10+ as ffprobe reads it: (pts, maxSCL in cd/m²,
/// average maxRGB in cd/m²).
fn probed(path: &std::path::Path) -> Vec<(i64, [f64; 3], f64)> {
    let p = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-show_frames", "-show_entries", "frame=pts:frame_side_data"])
        .arg(path)
        .output()
        .unwrap();
    assert!(p.status.success(), "{}", String::from_utf8_lossy(&p.stderr));
    let text = String::from_utf8_lossy(&p.stdout).into_owned();
    // ST 2094-40's light is in 0.1 cd/m² over 100000: n/100000 of 10000 cd/m².
    let nits = |v: &str| {
        let (n, d) = v.split_once('/').unwrap();
        n.parse::<f64>().unwrap() / d.parse::<f64>().unwrap() * 10000.0
    };
    text.split("[FRAME]")
        .skip(1)
        .filter(|f| f.contains("SMPTE2094-40"))
        .map(|f| {
            let pts = f.lines().find_map(|l| l.strip_prefix("pts=")).unwrap().parse().unwrap();
            let scl: Vec<f64> = f.lines().filter_map(|l| l.strip_prefix("maxscl=")).map(nits).collect();
            let avg = f.lines().find_map(|l| l.strip_prefix("average_maxrgb=")).map(nits).unwrap();
            (pts, [scl[0], scl[1], scl[2]], avg)
        })
        .collect()
}

#[test]
fn each_shot_carries_its_own_light() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-hdr10plus");
    let out = dir.join("on.mkv");
    export(&out, true);
    let frames = probed(&out);
    assert_eq!(frames.len(), 50, "every picture has it");
    // Grey 60 is 0.045 linear, 9.2 cd/m² at SDR white's 203; white is 203.
    for (pts, scl, avg) in &frames {
        let want = if *pts < 1000 { 9.2 } else { 203.0 };
        assert!(scl.iter().all(|v| (v - want).abs() <= want * 0.03 + 0.2), "frame at {pts} ms: maxSCL {scl:?}, want {want}");
        assert!((avg - want).abs() <= want * 0.03 + 0.2, "frame at {pts} ms: average {avg}, want {want}");
    }
}

/// The HEVC stream's NAL units, from an Annex B stream.
fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i..].starts_with(&[0, 0, 1]) {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut end = starts.get(k + 1).map_or(stream.len(), |&n| n - 3);
            // A four-byte start code's leading zero.
            while end > s && stream[end - 1] == 0 && starts.get(k + 1).is_some() {
                end -= 1;
            }
            &stream[s..end]
        })
        .collect()
}

#[test]
fn without_it_the_stream_is_the_same_but_for_its_seis() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-hdr10plus-off");
    let (on, off) = (dir.join("on.mkv"), dir.join("off.mkv"));
    export(&on, true);
    export(&off, false);
    let raw = |p: &std::path::Path| {
        let o = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(p).args(["-map", "0:v", "-c", "copy", "-bsf:v", "hevc_mp4toannexb", "-f", "hevc", "-"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        o.stdout
    };
    let (on_raw, off_raw) = (raw(&on), raw(&off));
    // A prefix SEI (39) whose message is HDR10+'s T.35: B5 00 3C.
    let hdr10plus = |n: &[u8]| n.len() > 7 && (n[0] >> 1) & 0x3F == 39 && n[2] == 4 && n[4..7] == [0xB5, 0x00, 0x3C];
    let on_units: Vec<&[u8]> = nal_units(&on_raw);
    let removed = on_units.iter().filter(|n| hdr10plus(n)).count();
    assert_eq!(removed, 50, "one HDR10+ SEI a picture");
    let kept: Vec<&[u8]> = on_units.into_iter().filter(|n| !hdr10plus(n)).collect();
    let off_units = nal_units(&off_raw);
    assert!(!off_units.iter().any(|n| hdr10plus(n)), "none without it");
    assert_eq!(kept.len(), off_units.len());
    assert!(kept == off_units, "the encoded pictures are the same, byte for byte");
}
