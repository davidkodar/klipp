//! Burnt-in captions are laid out for the frame they are exported to:
//! a 9:16 export of a 16:9 timeline drew them at 16:9 size inside
//! the picture's band, a small line a phone can't read. They take the
//! tall frame's caption size and sit above the Shorts player's controls,
//! in the person's style. Needs ffmpeg's encoder to open; a missing GPU
//! draws them on the CPU, the same.

use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use klipp_render::*;
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleBackground, TitleData, TitlePosition, Track, TrackKind};

/// A 640x360 timeline, blue, with one white caption on a dark backing low
/// in it, as captions are made for a wide frame.
fn sequence() -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 640, height: 360 };
    let tc = |s: f64| Timecode::from_secs_f64(s, 25);
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color: [30, 60, 160, 255] }), ..Clip::gap(tc(0.0), tc(1.0)) }];
    let (size, y) = klipp_timeline::title_box::caption_look(seq.resolution);
    let mut td = TitleData::single_text(TextBox { text: "Hello there".into(), font_size: size, color: [255, 255, 255, 255], ..Default::default() });
    td.background = TitleBackground::Color { color: [0, 0, 0, 235], padding: 12.0, lines: None };
    td.objects[0].transform.center = TitlePosition { x: 0.5, y };
    let mut subs = Track::new("S1", TrackKind::Subtitle);
    subs.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(1.0)) }];
    seq.tracks.push(subs);
    seq
}

/// The white text's box in a `w`-wide linear frame: left, top, right,
/// bottom, in pixels.
fn white_box(px: &[f32], w: usize) -> Option<[usize; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[0] > 0.4 && p[1] > 0.4 && p[2] > 0.4 {
            let (x, y) = (i % w, i / w);
            b = Some(b.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]));
        }
    }
    b
}

#[test]
fn a_tall_export_of_a_wide_timeline_lays_its_captions_out_for_the_tall_frame() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-captions-export-frame");
    let (w, h) = (360u32, 640u32);
    let settings = ExportSettings {
        output_path: dir.join("tall.mp4"),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        resolution: Some(Resolution { width: w, height: h }),
        audio_codec: ExportAudioCodec::Copy,
        preset: EncoderPreset::Fast,
        burn_in_subtitles: true,
        embed_subtitles: false,
        ..Default::default()
    };
    let snap = SequenceSnapshot::from_sequence(&sequence(), &MediaPool::new(), &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let probe = dir.join("probe.mp4");
    let Ok(encoder) = Encoder::open(&probe, &settings, &snap) else {
        eprintln!("skipping: no H.264 encoder");
        return;
    };
    let mut maker = ExportFrameMaker::new(&settings, &snap, &encoder, "test");
    drop(encoder);
    assert!(maker.letterboxed(), "a wide picture barred in a tall frame");
    let mut renderer = export_renderer(&snap, "test");
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    let ExportFrame::Linear(px) = maker.frame(&mut renderer, &snap.video.sequence, RationalTime { value: 12, rate: 25 }, &mut source).unwrap() else {
        panic!("a barred frame is padded on the CPU");
    };
    let [left, top, right, bottom] = white_box(&px, w as usize).expect("the caption is drawn");
    // The tall frame's caption size, in pixels of this frame: 58 at 1080
    // on the short side. Its letters stand more than half of that; drawn
    // in the band, at 40 on its 202 px, they stood 5 px.
    let em = klipp_timeline::title_box::caption_look(Resolution { width: w, height: h }).0 * w as f32 / 1080.0;
    let tall = (bottom - top + 1) as f32;
    assert!(tall >= em * 0.5, "the caption's letters stand {tall} px, under half of the tall frame's {em:.1} px size");
    // Above the Shorts player's title, channel and sound, from 65% down.
    assert!((bottom as f32) < 0.65 * h as f32, "the caption ends at {bottom} px, under the player's controls from {} px", 0.65 * h as f32);
    let centre = (left + right) as f32 / 2.0 / w as f32;
    assert!((centre - 0.5).abs() < 0.05, "centred as it was: at {centre}");
    let _ = std::fs::remove_dir_all(&dir);
}
