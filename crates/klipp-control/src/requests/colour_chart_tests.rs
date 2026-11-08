use super::*;
use std::sync::{Arc, Mutex, RwLock};

const W: u32 = 480;
const H: u32 = 270;
const CORNERS: [[f32; 2]; 4] = [[0.20, 0.15], [0.80, 0.22], [0.76, 0.85], [0.17, 0.78]];

/// A shot of a chart under a warm light and a stop-and-a-half's half under,
/// drawn through the clip's own grade with the grade's own white balance
/// and exposure: what a reading does to the picture is what the next reads.
fn drawn(seq: &klipp_timeline::Sequence) -> Vec<u8> {
    use klipp_color::chart::chart_rgba;
    use klipp_color::transfer::{linear_to_srgb, srgb_to_linear};
    let k = crate::primary_knobs(klipp_timeline::GradeAccess::grade(&seq.tracks[0].clips[0]));
    let (cw, ch) = (600u32, 400u32);
    let chart = chart_rgba(cw, ch);
    let map = klipp_core::color_grade::PlaneMap::from_quads(&CORNERS, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]).unwrap();
    let wb = klipp_color::white_balance::white_balance_matrix(k.temperature.unwrap_or(0.0) + 25.0, k.tint.unwrap_or(0.0) + 8.0, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
    let gain = (k.exposure.unwrap_or(0.0) - 0.5).exp2();
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let p = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let rgb = match map.apply(p) {
                Some((u, v)) if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) => {
                    let (cx, cy) = (((0.1 + 0.8 * u) * cw as f32) as u32, ((0.1 + 0.8 * v) * ch as f32) as u32);
                    let o = (cy.min(ch - 1) * cw + cx.min(cw - 1)) as usize * 4;
                    [chart[o], chart[o + 1], chart[o + 2]]
                }
                _ => [70, 90, 60],
            };
            let c = rgb.map(|v| srgb_to_linear(v as f32 / 255.0));
            let m = wb.map(|r| (r[0] * c[0] + r[1] * c[1] + r[2] * c[2]) * gain);
            out.extend(m.map(|v| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8));
            out.push(255);
        }
    }
    out
}

fn context() -> (ControlContext, String, Arc<Mutex<usize>>) {
    let root = std::env::temp_dir().join(format!("klipp-chart-read-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test(root.join("shot.mp4"), None));
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.retain(|t| t.kind == klipp_timeline::TrackKind::Video);
    sequence.tracks.truncate(1);
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let clip = klipp_timeline::Clip::media(media, time(0), time(48), time(0), time(48));
    let id = clip.id.0.to_string();
    sequence.tracks[0].clips = vec![clip];
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(W, H)));
    let shared = Arc::new(klipp_core::ControlShared::default());
    let seq = Arc::new(klipp_timeline::SharedSequence::new(sequence));
    let pool = Arc::new(RwLock::new(pool));
    let writes: Arc<Mutex<usize>> = Default::default();
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let (frame_buffer, host_shared, host_seq, host_pool, host_writes) = (fb.clone(), shared.clone(), seq.clone(), pool.clone(), writes.clone());
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
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
                *host_writes.lock().unwrap() += 1;
            }
            for command in envelope.commands {
                if matches!(command, AppCommand::SquashUndo { .. }) {
                    host_shared.bump_revision();
                }
                if let AppCommand::SeekTo(at) = command {
                    let rgba = drawn(&host_seq.snapshot());
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
    (ctx, id, writes)
}

/// Read, it says how far the chart is and changes nothing; applied, it
/// brings it near its published values as one undo step and says so.
#[test]
fn a_chart_in_the_shot_is_read_then_corrects_it() {
    let (ctx, id, writes) = context();
    let read = respond(&ctx, ControlCommand::ReadChart { clip_id: id.clone(), corners: CORNERS, secs: Some(1.0), apply: false, exposure: None }, &mut None).unwrap();
    assert_eq!(*writes.lock().unwrap(), 0, "a read changes nothing: {read}");
    let before = read["before"]["mean_de"].as_f64().unwrap();
    assert!(before > 6.0 && read["text"].as_str().unwrap().contains("apply true"), "{read}");
    let done = respond(&ctx, ControlCommand::ReadChart { clip_id: id.clone(), corners: CORNERS, secs: Some(1.0), apply: true, exposure: None }, &mut None).unwrap();
    let after = done["after"]["mean_de"].as_f64().unwrap();
    assert!(after < 1.5, "mean dE {before:.1} -> {after:.1}: {done}");
    assert_eq!(done["undo_steps"], serde_json::json!(1), "{done}");
    let k = crate::primary_knobs(klipp_timeline::GradeAccess::grade(&ctx.sequence.snapshot().tracks[0].clips[0]));
    assert!((k.temperature.unwrap() + 25.0).abs() < 3.0 && (k.exposure.unwrap() - 0.5).abs() < 0.1, "the shot's own light taken off: {k:?}");
}

/// Corners that read no chart are refused, changing nothing; exposure can
/// be left alone.
#[test]
fn a_chart_is_refused_or_left_its_exposure() {
    let (ctx, id, writes) = context();
    let wrong = respond(&ctx, ControlCommand::ReadChart { clip_id: id.clone(), corners: [[0.01, 0.01], [0.15, 0.01], [0.15, 0.12], [0.01, 0.12]], secs: Some(1.0), apply: true, exposure: None }, &mut None).unwrap_err();
    assert!(wrong.contains("does not read as a ColorChecker"), "{wrong}");
    assert_eq!(*writes.lock().unwrap(), 0);
    let off = respond(&ctx, ControlCommand::ReadChart { clip_id: id.clone(), corners: [[1.5, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], secs: None, apply: false, exposure: None }, &mut None).unwrap_err();
    assert!(off.contains("fractions"), "{off}");
    respond(&ctx, ControlCommand::ReadChart { clip_id: id, corners: CORNERS, secs: Some(1.0), apply: true, exposure: Some(false) }, &mut None).unwrap();
    let k = crate::primary_knobs(klipp_timeline::GradeAccess::grade(&ctx.sequence.snapshot().tracks[0].clips[0]));
    assert!(k.exposure.is_none() && k.temperature.is_some(), "{k:?}");
}
