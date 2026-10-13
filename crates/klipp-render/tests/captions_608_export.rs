//! An export carries its subtitle track as CEA-608 captions inside the
//! video stream (#1663): ATSC A/53 caption data in an SEI before each
//! picture. Read back by ffmpeg's own caption decoder, from the video
//! stream alone, the captions are the cues written, at their times, in
//! H.264 and H.265, MP4 and MKV.
use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Two cues on a subtitle track over four seconds of picture at `fps`.
fn sequence(fps: Fps) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: 320, height: 180 };
    let tc = |s: f64| Timecode::from_secs_f64(s, fps.num.div_ceil(fps.den));
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color: [40, 40, 40, 255] }), ..Clip::gap(tc(0.0), tc(4.0)) }];
    let mut subs = Track::new("Subtitles", TrackKind::Subtitle);
    let cue = |text: &str, from, to| Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: text.into(), ..Default::default() })), ..Clip::gap(tc(from), tc(to)) };
    subs.clips = vec![cue("HELLO THERE", 1.0, 2.0), cue("SECOND LINE", 2.5, 3.5)];
    seq.tracks.push(subs);
    seq
}

fn export(seq: &Sequence, out: &std::path::Path, codec: ExportVideoCodec, format: ExportFormat) {
    let pool = MediaPool::new();
    let settings = ExportSettings {
        output_path: out.to_path_buf(),
        video_codec: codec,
        format,
        audio_codec: ExportAudioCodec::Copy,
        crf: 18,
        preset: EncoderPreset::Fast,
        burn_in_subtitles: false,
        captions_608: true,
        ..Default::default()
    };
    let snapshot = SequenceSnapshot::from_sequence(seq, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
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
        total_frames: frames,
        analyser: None,
    }
    .run()
    .unwrap();
}

/// The captions ffmpeg's 608 decoder reads from `path`'s video stream,
/// as (start, end, text) from its SRT.
fn read_back(path: &std::path::Path) -> Vec<(f64, f64, String)> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("movie={}[out0+subcc]", path.display()))
        .args(["-map", "0:s", "-f", "srt", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let srt = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    let secs = |t: &str| {
        let (hms, ms) = t.trim().split_once(',').unwrap();
        let p: Vec<f64> = hms.split(':').map(|v| v.parse().unwrap()).collect();
        p[0] * 3600.0 + p[1] * 60.0 + p[2] + ms.parse::<f64>().unwrap() / 1000.0
    };
    srt.split("\n\n")
        .filter_map(|block| {
            let lines: Vec<&str> = block.lines().collect();
            let times = lines.iter().position(|l| l.contains("-->"))?;
            let (a, b) = lines[times].split_once("-->")?;
            let text = lines[times + 1..].join(" ");
            // The decoder's own text styling, as SRT tags.
            let text = text.replace("<font face=\"Monospace\">", "").replace("</font>", "").replace("{\\an7}", "").trim().to_string();
            Some((secs(a), secs(b), text))
        })
        .filter(|c| !c.2.is_empty())
        .collect()
}

#[test]
fn captions_ride_in_the_video_stream() {
    if !ffmpeg_ok() {
        eprintln!("skipping: ffmpeg unavailable");
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-captions-608");
    for (codec, format, fps, name) in [
        (ExportVideoCodec::H264, ExportFormat::Mp4, Fps::new(30000, 1001), "h264.mp4"),
        (ExportVideoCodec::H264, ExportFormat::Mkv, Fps::new(25, 1), "h264.mkv"),
        (ExportVideoCodec::H265, ExportFormat::Mp4, Fps::new(24, 1), "hevc.mp4"),
        (ExportVideoCodec::H265, ExportFormat::Mkv, Fps::new(30000, 1001), "hevc.mkv"),
    ] {
        let out = dir.join(name);
        export(&sequence(fps), &out, codec, format);
        let cues = read_back(&out);
        eprintln!("{name}: {cues:?}");
        let texts: Vec<&str> = cues.iter().map(|c| c.2.as_str()).collect();
        assert_eq!(texts, ["HELLO THERE", "SECOND LINE"], "{name}: the cues written");
        for ((start, end, _), (want_start, want_end)) in cues.iter().zip([(1.0, 2.0), (2.5, 3.5)]) {
            assert!((start - want_start).abs() < 0.1, "{name}: shown at {start}, written for {want_start}");
            assert!((end - want_end).abs() < 0.15, "{name}: cleared at {end}, written for {want_end}");
        }
    }
}

/// Not asked for, the video carries no captions.
#[test]
fn no_captions_unless_asked() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-captions-608-off");
    let out = dir.join("off.mp4");
    let seq = sequence(Fps::new(25, 1));
    let pool = MediaPool::new();
    let settings = ExportSettings { output_path: out.clone(), video_codec: ExportVideoCodec::H264, format: ExportFormat::Mp4, audio_codec: ExportAudioCodec::Copy, burn_in_subtitles: false, captions_608: false, hdr10plus: false, ..Default::default() };
    let snapshot = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.clone(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
        total_frames: frames, analyser: None,
    }
    .run()
    .unwrap();
    assert!(read_back(&out).is_empty(), "no captions in {}", out.display());
}
