//! Auto colour (#2758): a shot made too dark, too warm and too flat,
//! graded by the one measuring path grade_to_target has, through the
//! apply op, lands within stated bounds of neutral, as one undo step that
//! leaves an ordinary grade. The host here is real enough to close the
//! loop: it applies each edit to the sequence and renders the graded
//! frame on the CPU for each read.

use super::*;
use std::sync::{Arc, Mutex, RwLock};

const W: u32 = 96;
const H: u32 = 54;

/// A neutral chart, a grey ramp over grey patches, made too dark,
/// too warm and too flat.
fn mangled(w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let grey = 0.01 + 0.85 * fx * fx;
            let c = if fy > 0.55 {
                match (fx * 4.0) as u32 { 0 => [0.05, 0.05, 0.05], 1 => [0.18, 0.18, 0.18], 2 => [0.4, 0.4, 0.4], _ => [0.8, 0.8, 0.8] }
            } else {
                [grey, grey, grey]
            };
            let flat = |v: f32| 0.07 + (v - 0.07) * 0.65;
            [flat(c[0]) * 0.3 * 1.15, flat(c[1]) * 0.3, flat(c[2]) * 0.3 * 0.85, 1.0]
        })
        .collect()
}

/// The shot's picture: the mangled chart, or a frame of real footage
/// (linear, `W` × `H`).
#[derive(Clone, Default)]
struct Shot(Option<Arc<Vec<f32>>>);

impl klipp_compose::FrameSource for Shot {
    fn frame(&mut self, _c: &klipp_timeline::Clip, _secs: f64, w: u32, h: u32) -> Option<klipp_compose::SourceFrame> {
        let pixels = self.0.clone().unwrap_or_else(|| Arc::new(mangled(w, h)));
        Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(pixels), width: w, height: h, pts: None })
    }
}

/// The sequence at `at`, as the viewer shows it: display bytes.
fn drawn(seq: &klipp_timeline::Sequence, at: klipp_core::RationalTime, shot: &Shot) -> Vec<u8> {
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (linear, _) = r.render_inspected(seq, at, W, H, &mut shot.clone(), klipp_compose::RenderOptions::default(), klipp_compose::Inspect::default());
    klipp_color::display::display_transform_ws_to(&linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn context() -> (ControlContext, klipp_core::ClipId) {
    context_of(None, Shot::default())
}

/// The host over `shot`, its media item at `file` when given (where the
/// file's own frame is read), else at a path that is not there.
fn context_of(file: Option<std::path::PathBuf>, shot: Shot) -> (ControlContext, klipp_core::ClipId) {
    let root = std::env::temp_dir().join(format!("klipp-auto-colour-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test(file.unwrap_or_else(|| root.join("shot.mp4")), None);
    item.resolution = klipp_core::Resolution { width: W, height: H };
    let media = pool.add(item);
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.resolution = klipp_core::Resolution { width: W, height: H };
    sequence.tracks.retain(|t| t.kind == klipp_timeline::TrackKind::Video);
    sequence.tracks.truncate(1);
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let clip = klipp_timeline::Clip::media(media, time(0), time(96), time(0), time(96));
    let id = clip.id;
    sequence.tracks[0].clips = vec![clip];
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(W, H)));
    let shared = Arc::new(klipp_core::ControlShared::default());
    let seq = Arc::new(klipp_timeline::SharedSequence::new(sequence));
    let pool = Arc::new(RwLock::new(pool));
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let (frame_buffer, host_shared, host_seq, host_pool) = (fb.clone(), shared.clone(), seq.clone(), pool.clone());
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            // The edits, applied as the app applies them.
            if !envelope.edits.is_empty() {
                let mut seqs = vec![(*host_seq.snapshot()).clone()];
                let mut p = host_pool.read().unwrap().clone();
                let mut fps = seqs[0].fps;
                let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
                for e in &envelope.edits {
                    view.apply_forward(e);
                }
                host_seq.replace(seqs.pop().unwrap());
                host_shared.bump_revision();
            }
            for command in envelope.commands {
                // The pass's steps folded into one, as the app folds them.
                if matches!(command, AppCommand::SquashUndo { .. }) {
                    host_shared.bump_revision();
                }
                if let AppCommand::SeekTo(at) = command {
                    let rgba = drawn(&host_seq.snapshot(), at, &shot);
                    let mut frame = frame_buffer.lock().unwrap();
                    frame.write_frame(rgba, W, H);
                    frame.set_presented(at, true);
                }
            }
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck {
                    revision: host_shared.revision(), refused: None, refused_op: None,
                    renamed: Vec::new(), ungated: Vec::new(), picture_refused: None,
                }).unwrap();
            }
        }
    });
    let ctx = ControlContext {
        commands, shared, wake_ui, sequence: seq, media_pool: pool, frame_buffer: fb,
        perf_ring: klipp_engine::perf::shared(), log_path: root.join("unused.log"), others: Default::default(),
    };
    (ctx, id)
}

/// Bounds of a neutral starting grade: neutrals within 2.5 of grey on a*
/// and b*, the median within 8 L* of mid grey (48), the 5–95 range within
/// 12 of a full picture's (65).
fn near_neutral(m: &klipp_color::measure::FrameMeasure) -> bool {
    m.cast_a.abs() <= 2.5 && m.cast_b.abs() <= 2.5 && (m.l_p50 - 48.0).abs() <= 8.0 && (m.contrast_range - 65.0).abs() <= 12.0
}

/// Auto colour, sent as an apply op, measures the shot through
/// grade_to_target's own path and lands it within those bounds from well
/// outside them, as one undo step that leaves an ordinary Corrector on
/// the clip; sent with another op, it is refused.
#[test]
fn auto_colour_brings_a_dark_warm_flat_shot_near_neutral() {
    let (ctx, id) = context();
    let at = klipp_core::Timecode::from_frames(48, 24);
    let measure = |ctx: &ControlContext| klipp_color::measure::measure_frame(&drawn(&ctx.sequence.snapshot(), at, &Shot::default()), W, H, &[]);
    let before = measure(&ctx);
    assert!(!near_neutral(&before) && before.l_p50 < 30.0 && before.cast_b > 4.0 && before.contrast_range < 45.0, "made dark, warm and flat: {before:?}");
    let op: klipp_document::ops::Op = serde_json::from_value(serde_json::json!({ "op": "auto_colour", "clips": [id.0.to_string()], "strength": "bold" })).unwrap();
    let reply = crate::handle(&ctx, ControlCommand::Apply { ops: vec![op.clone()], expected_revision: None, expected_project_identity: None, label: None, detail: false }, &mut None).unwrap_or_else(|e| panic!("{e}"));
    let after = measure(&ctx);
    assert!(near_neutral(&after), "near neutral: median L* {:.1}, cast {:.1} {:.1}, range {:.1}; {}", after.l_p50, after.cast_a, after.cast_b, after.contrast_range, reply["text"]);
    assert_eq!(reply["undo_steps"], serde_json::json!(1), "one undo: {reply}");
    let seq = ctx.sequence.snapshot();
    let clip = seq.tracks[0].clips.iter().find(|c| c.id == id).unwrap();
    use klipp_timeline::GradeAccess;
    assert!(clip.grade().nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Corrector), "an ordinary grade to edit");
    // Planned with another step, refused: it measures the picture.
    let other: klipp_document::ops::Op = serde_json::from_value(serde_json::json!({ "op": "slip", "clip_id": id.0.to_string(), "secs": 0.1 })).unwrap();
    let why = crate::handle(&ctx, ControlCommand::Apply { ops: vec![op, other], expected_revision: None, expected_project_identity: None, label: None, detail: false }, &mut None).unwrap_err();
    assert!(why.contains("alone"), "{why}");
}

/// Flat footage, as shot (a hazy aerial with no true black): auto colour
/// stretches it toward the starting grade, its black point down and its
/// range up to the look's, with no more than half a percent of the frame
/// taken to black or white, and says what it did. Its green cast is left:
/// correcting it turns the leaves, and the tone is graded without it. Read from the showcase's own file; where
/// that is not on this machine the case says so and passes.
#[test]
fn auto_colour_stretches_flat_footage_within_a_clipping_bound() {
    let file = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Videos/Klipp Showcase/originals/video/flat-01-coastal-town-aerial.mp4");
    if !file.exists() {
        eprintln!("UNVERIFIED: {} is not on this machine", file.display());
        return;
    }
    // The frame the clip shows at 2 s of the timeline: 2 s of the file.
    let (rgba, w, h) = crate::frames::decode_frame_rgba_sized(&file, 2.0, W, H).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((w, h), (W, H));
    let shot = Shot(Some(Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&rgba))));
    let (ctx, id) = context_of(Some(file), shot.clone());
    let at = klipp_core::Timecode::from_frames(48, 24);
    let measure = |ctx: &ControlContext| klipp_color::measure::measure_frame(&drawn(&ctx.sequence.snapshot(), at, &shot), W, H, &[]);
    let before = measure(&ctx);
    assert!(before.l_p1 > 15.0, "flat as shot: its darkest is L* {:.1}", before.l_p1);
    let op: klipp_document::ops::Op = serde_json::from_value(serde_json::json!({ "op": "auto_colour", "clips": [id.0.to_string()] })).unwrap();
    let reply = crate::handle(&ctx, ControlCommand::Apply { ops: vec![op], expected_revision: None, expected_project_identity: None, label: None, detail: false }, &mut None).unwrap_or_else(|e| panic!("{e}"));
    let after = measure(&ctx);
    let said = reply["text"].as_str().unwrap_or_default();
    eprintln!("flat footage: black L* {:.1} -> {:.1}, range {:.1} -> {:.1}, at black {:.2}% -> {:.2}%, at white {:.2}% -> {:.2}%\n{said}", before.l_p1, after.l_p1, before.contrast_range, after.contrast_range, before.clip_lo_pct, after.clip_lo_pct, before.clip_hi_pct, after.clip_hi_pct);
    assert!(after.l_p1 < before.l_p1 - 5.0, "the black point down: L* {:.1} to {:.1}; {said}", before.l_p1, after.l_p1);
    assert!(after.contrast_range > before.contrast_range + 3.0, "the range toward 65: {:.1} to {:.1}; {said}", before.contrast_range, after.contrast_range);
    assert!(after.clip_lo_pct <= 0.5 && after.clip_hi_pct <= before.clip_hi_pct.max(0.5), "clipping bounded: black {:.2}%, white {:.2}%", after.clip_lo_pct, after.clip_hi_pct);
    assert!(said.contains("the source is flat") && said.contains("black point L*"), "says what it did: {said}");
}
