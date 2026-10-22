//! A title composed whole (#1921): the compose op moves a title of two
//! texts across the frame as one, through a save and a reopen, to the
//! exported pixels and the viewer's; its texts stay as they were, and a
//! text's own motion goes on under the clip's.
use klipp_core::*;
use klipp_core::{Interpolation, Keyframe};
use klipp_document::{
    apply::DocumentView,
    ops::{self, Op},
};
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::Sequence;
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

const W: usize = 320;
const H: usize = 180;

/// The mean x of the bright pixels in rows `rows` of a frame of `channels`
/// bytes a pixel, as a fraction of the width; `None` when there are none.
fn ink_x_of(frame: &[u8], channels: usize, rows: std::ops::Range<usize>) -> Option<f64> {
    let (mut sum, mut n) = (0.0, 0usize);
    for y in rows {
        for x in 0..W {
            let at = (y * W + x) * channels;
            let p = &frame[at..at + 3];
            if p.iter().all(|&v| v > 180) {
                sum += x as f64;
                n += 1;
            }
        }
    }
    (n > 20).then(|| sum / n as f64 / W as f64)
}

fn ink_x(frame: &[u8], rows: std::ops::Range<usize>) -> Option<f64> {
    ink_x_of(frame, 3, rows)
}

/// No media: a card and a title draw themselves.
struct NoMedia;

impl klipp_compose::FrameSource for NoMedia {
    fn frame(&mut self, _c: &klipp_timeline::Clip, _secs: f64, _w: u32, _h: u32) -> Option<klipp_compose::SourceFrame> {
        None
    }
}

fn linear(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

#[test]
fn a_composed_title_moves_whole_through_save_and_export() {
    if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping export fixture: ffmpeg unavailable");
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-title-composed");
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W as u32, height: H as u32 };
    let ops: Vec<Op> = serde_json::from_value(json!([
        {"op":"generator", "data":{"SolidColor":{"color":[0,0,0,255]}},"at":{"secs":0},"secs":2},
        {"op":"title","text":"KLIPP","at":{"secs":0},"secs":2,"size":220,"font":"DejaVu Sans","color":"#ffffff"}
    ]))
    .unwrap();
    let mut pool = MediaPool::new();
    let plan = ops::resolve(&seq, &pool, &ops).unwrap();
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    // Two texts in the one title: one high, one low, both centred.
    let clip_id = {
        let clip = doc.sequences[0].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.title_data().is_some()).unwrap();
        let klipp_timeline::ClipKind::Title(td) = &mut clip.kind else { unreachable!() };
        let first = td.objects[0].id;
        td.duplicate_object(first).expect("a second text");
        td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.3 };
        td.objects[1].transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.7 };
        // The lower text has its own move, 0.45 to 0.55 of the frame over
        // the clip: the title's motion, under the clip's. Kept inside the
        // frame at both ends, so no ink is cut off at an edge.
        td.objects[1].transform.center_x_keyframes = vec![linear(0.0, 0.45), linear(2.0, 0.55)];
        clip.id
    };
    let texts = |seqs: &[Sequence]| seqs[0].tracks.iter().flat_map(|t| &t.clips).find_map(|c| c.title_data()).unwrap().clone();
    let before = texts(doc.sequences);
    // The issue's compose: left to right across the clip.
    let compose: Vec<Op> = serde_json::from_value(json!([
        {"op":"compose","clip_id":clip_id.0.to_string(),"poses":[{"secs":0,"x":0.3},{"from_end_secs":0,"x":0.7}]}
    ]))
    .unwrap();
    let plan = ops::resolve(&doc.sequences[0], doc.media_pool, &compose).unwrap_or_else(|r| panic!("refused: {}", r.why));
    doc.apply_forward(&plan.steps[0].edit);
    assert_eq!(texts(doc.sequences), before, "the texts are as they were, editable");

    let project = dir.join("composed.klipp");
    klipp_project::save_project(&project, &sequences, &pool).unwrap();
    let (sequences, pool, ..) = klipp_project::load_project(&project).unwrap();
    assert_eq!(texts(&sequences), before, "the texts survive the reopen");
    let moved = sequences[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.title_data().is_some()).unwrap();
    assert!(moved.truth_lane(&ParamId::PositionX).is_some_and(|(keys, _)| !keys.is_empty()), "and so does the move");

    let out = dir.join("composed.mkv");
    let settings = ExportSettings {
        output_path: out.clone(),
        video_codec: ExportVideoCodec::H264,
        format: ExportFormat::Mkv,
        audio_codec: ExportAudioCodec::Copy,
        burn_in_subtitles: true,
        crf: 1,
        preset: EncoderPreset::Fast,
        ..Default::default()
    };
    let snapshot = SequenceSnapshot::from_sequence(&sequences[0], &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.clone(),
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
    let decoded = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&out).args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).output().unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let size = W * H * 3;
    let frames: Vec<&[u8]> = decoded.stdout.chunks_exact(size).collect();
    let (first, last) = (frames[0], frames[frames.len() - 1]);
    // Each text read in its own half of the frame.
    let (top, bottom) = (0..H / 2, H / 2..H);
    let at = |f: &[u8], rows: std::ops::Range<usize>| ink_x(f, rows).expect("the text is in the frame");
    let (top0, bottom0, top1, bottom1) = (at(first, top.clone()), at(first, bottom.clone()), at(last, top.clone()), at(last, bottom.clone()));
    println!("export ink x: top {top0:.3} -> {top1:.3}, bottom {bottom0:.3} -> {bottom1:.3}");
    assert!((top0 - 0.3).abs() < 0.03 && (top1 - 0.7).abs() < 0.03, "the still text moves with the clip, 0.3 to 0.7: {top0:.3} -> {top1:.3}");
    // The moving text: the clip's move, and its own 0.1 of the frame on
    // top (0.098 by the last frame, a frame short of the clip's end).
    assert!((bottom0 - (top0 - 0.05)).abs() < 0.02 && (bottom1 - (top1 + 0.05)).abs() < 0.02, "its own motion goes on under the clip's: {bottom0:.3} -> {bottom1:.3} beside {top0:.3} -> {top1:.3}");
    assert!(((bottom1 - bottom0) - (top1 - top0) - 0.1).abs() < 0.025, "the clip's move plus its own: {:.3} against {:.3}", bottom1 - bottom0, top1 - top0);

    // The viewer's route, the GPU stack (a title takes its own sprite path
    // there), against the export.
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let end = Timecode::from_frames(frames.len() as i64 - 1, sequences[0].fps.rate());
    for (t, exported) in [(Timecode::from_frames(0, sequences[0].fps.rate()), first), (end, last)] {
        let Some((viewer, _)) = r.render_display_u8_on_gpu(&sequences[0], t, W as u32, H as u32, &mut NoMedia, klipp_compose::RenderOptions::default()) else {
            eprintln!("UNVERIFIED: the GPU stack declined (no usable GPU?); the viewer route isn't checked");
            return;
        };
        for rows in [top.clone(), bottom.clone()] {
            let (v, e) = (ink_x_of(&viewer, 4, rows.clone()).expect("the viewer draws the text"), at(exported, rows));
            println!("viewer {v:.3} export {e:.3}");
            assert!((v - e).abs() < 0.02, "the viewer places the text where the export does: {v:.3} against {e:.3}");
        }
    }
}
