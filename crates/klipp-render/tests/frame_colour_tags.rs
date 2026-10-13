//! Every exported picture carries the colour the stream is tagged with
//! (#1142). ProRes writes each frame's own primaries, transfer and matrix
//! into its frame header; sent without them, every frame said
//! "unspecified", decoders read it as BT.601, and saturated colours came
//! out shifted: the red (255, 24, 0) decoded as (233, 2, 2).
use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

fn export(out: &std::path::Path, codec: ExportVideoCodec, format: ExportFormat, color: [u8; 4]) {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 320, height: 180 };
    let tc = |s: f64| Timecode::from_secs_f64(s, 25);
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color }), ..Clip::gap(tc(0.0), tc(0.2)) }];
    let settings = ExportSettings { output_path: out.to_path_buf(), video_codec: codec, format, audio_codec: ExportAudioCodec::Copy, ..Default::default() };
    let pool = MediaPool::new();
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
        total_frames: frames,
        analyser: None,
    }
    .run()
    .unwrap();
}

/// The first frame's own colour tags, as its decoder reports them.
fn frame_tags(path: &std::path::Path) -> String {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-read_intervals", "%+#1", "-show_entries", "frame=color_space,color_primaries,color_transfer", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The middle pixel of the first frame as RGB, decoded the way the file says.
fn rgb(path: &std::path::Path) -> [u8; 3] {
    let o = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-frames:v", "1", "-vf", "format=rgb24", "-f", "rawvideo", "-"]).output().unwrap();
    let i = (90 * 320 + 160) * 3;
    [o.stdout[i], o.stdout[i + 1], o.stdout[i + 2]]
}

#[test]
fn prores_frames_carry_their_colour_and_decode_to_it() {
    if !std::process::Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-frame-colour");
    let red = [255, 24, 0, 255];
    for (codec, format, name) in [
        (ExportVideoCodec::ProRes422, ExportFormat::Mov, "prores422.mov"),
        (ExportVideoCodec::ProRes4444, ExportFormat::Mov, "prores4444.mov"),
        (ExportVideoCodec::H264, ExportFormat::Mp4, "h264.mp4"),
    ] {
        let out = dir.join(name);
        export(&out, codec, format, red);
        let tags = frame_tags(&out);
        for want in ["color_space=bt709", "color_primaries=bt709", "color_transfer=bt709"] {
            assert!(tags.contains(want), "{name}: its frames say {tags}");
        }
        let got = rgb(&out);
        eprintln!("{name}: {got:?}");
        assert!(got.iter().zip([255u8, 24, 0]).all(|(g, w)| g.abs_diff(w) <= 4), "{name}: the red decodes as {got:?}");
    }
}
