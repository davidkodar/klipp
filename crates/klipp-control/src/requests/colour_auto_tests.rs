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

struct Shot;

impl klipp_compose::FrameSource for Shot {
    fn frame(&mut self, _c: &klipp_timeline::Clip, _secs: f64, w: u32, h: u32) -> Option<klipp_compose::SourceFrame> {
        Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(Arc::new(mangled(w, h))), width: w, height: h, pts: None })
    }
}

/// The sequence at `at`, as the viewer shows it: display bytes.
fn drawn(seq: &klipp_timeline::Sequence, at: klipp_core::RationalTime) -> Vec<u8> {
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (linear, _) = r.render_inspected(seq, at, W, H, &mut Shot, klipp_compose::RenderOptions::default(), klipp_compose::Inspect::default());
    klipp_color::display::display_transform_ws_to(&linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn context() -> (ControlContext, klipp_core::ClipId) {
    let root = std::env::temp_dir().join(format!("klipp-auto-colour-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test(root.join("shot.mp4"), None));
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
                    let rgba = drawn(&host_seq.snapshot(), at);
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
    let measure = |ctx: &ControlContext| klipp_color::measure::measure_frame(&drawn(&ctx.sequence.snapshot(), at), W, H, &[]);
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
