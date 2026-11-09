use super::*;
use std::sync::{Arc, Mutex, RwLock};

const W: u32 = 64;
const H: u32 = 36;

/// A reference at 0-2 s (L* 50, greys neutral), and a target at 2-4 s drawn
/// from its own grade: L* 30 with its greys reading b* +10 until it is
/// brightened, then L* 50 and b* +2, each mired taking off a quarter of a
/// b*. The cast read in the dark is false, as on a real shot, whose
/// darkened picture hands the neutrals to other pixels.
fn drawn(seq: &klipp_timeline::Sequence, at: klipp_core::Timecode) -> Vec<u8> {
    use klipp_color::measure::{lab_to_srgb8, Lab};
    let target = seq.tracks[0].clips[1].clone();
    let lab = if at >= target.timeline_in {
        let k = crate::primary_knobs(klipp_timeline::GradeAccess::grade(&target));
        let (l, b) = if k.exposure.unwrap_or(0.0) > 0.2 { (50.0, 2.0) } else { (30.0, 10.0) };
        Lab { l, a: 0.0, b: b - 0.25 * k.temperature.unwrap_or(0.0) }
    } else {
        Lab { l: 50.0, a: 0.0, b: 0.0 }
    };
    let c = lab_to_srgb8(lab);
    [c[0], c[1], c[2], 255].repeat((W * H) as usize)
}

/// The app as the handlers see it, its edits kept: the corrector's knobs
/// after each.
type Written = Arc<Mutex<Vec<crate::MatchKnobs>>>;

fn context() -> (ControlContext, String, String, Written, Arc<Mutex<usize>>) {
    context_drawing(drawn)
}

fn context_drawing(draw: fn(&klipp_timeline::Sequence, klipp_core::Timecode) -> Vec<u8>) -> (ControlContext, String, String, Written, Arc<Mutex<usize>>) {
    let root = std::env::temp_dir().join(format!("klipp-match-order-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test(root.join("shot.mp4"), None));
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.retain(|t| t.kind == klipp_timeline::TrackKind::Video);
    sequence.tracks.truncate(1);
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let clips = [klipp_timeline::Clip::media(media, time(0), time(48), time(0), time(48)), klipp_timeline::Clip::media(media, time(48), time(96), time(0), time(48))];
    let (reference, target) = (clips[0].id.0.to_string(), clips[1].id.0.to_string());
    sequence.tracks[0].clips = clips.to_vec();
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(W, H)));
    let shared = Arc::new(klipp_core::ControlShared::default());
    let seq = Arc::new(klipp_timeline::SharedSequence::new(sequence));
    let pool = Arc::new(RwLock::new(pool));
    let (written, squashed): (Written, Arc<Mutex<usize>>) = Default::default();
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let (frame_buffer, host_shared, host_seq, host_pool, host_written, host_squashed) = (fb.clone(), shared.clone(), seq.clone(), pool.clone(), written.clone(), squashed.clone());
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
                host_written.lock().unwrap().push(crate::primary_knobs(klipp_timeline::GradeAccess::grade(&seqs[0].tracks[0].clips[1])));
                host_seq.replace(seqs.pop().unwrap());
                host_shared.bump_revision();
            }
            for command in envelope.commands {
                if matches!(command, AppCommand::SquashUndo { .. }) {
                    *host_squashed.lock().unwrap() += 1;
                    host_shared.bump_revision();
                }
                if let AppCommand::SeekTo(at) = command {
                    let rgba = draw(&host_seq.snapshot(), at);
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
    (ctx, target, reference, written, squashed)
}

/// Brightness first: the tone goes on alone, the picture is read again, and
/// the colour is judged from that, all as one undo step. Judged in the dark
/// it took the false +10 and moved the temperature four times too far.
#[test]
fn a_match_sets_the_brightness_then_reads_the_colour() {
    let (ctx, target, reference, written, squashed) = context();
    let reply = respond(&ctx, ControlCommand::MatchGrade { clip_id: target, reference_clip_id: reference, secs: Some(3.0), reference_secs: Some(1.0) }, &mut None).unwrap();
    let steps = written.lock().unwrap().clone();
    assert_eq!(steps.len(), 2, "tone, then colour: {steps:?}");
    assert!(steps[0].exposure.is_some_and(|e| e > 0.2) && steps[0].temperature.is_none() && steps[0].tint.is_none(), "the tone alone first: {:?}", steps[0]);
    let temperature = steps[1].temperature.expect("then the colour");
    assert!((-14.0..-2.0).contains(&temperature), "from the b* +2 read once brightened, not the false +10: {temperature}");
    assert_eq!(steps[1].exposure, steps[0].exposure, "the colour step leaves the tone");
    assert_eq!(reply["undo_steps"], serde_json::json!(1), "{reply}");
    assert_eq!(*squashed.lock().unwrap(), 1, "folded");
    assert!(reply["text"].as_str().unwrap().contains("eyedropper in Colour Balance"), "where the exact tool is: {reply}");
    assert!(reply["next"].as_str().unwrap().contains("pick_grey"), "{reply}");
}

/// The same match again changes nothing and says why: each click measured
/// on the last one's result walked the grade. Asked at another moment it is
/// a new match, and so it is once the clip's grade has changed.
#[test]
fn the_same_match_again_changes_nothing() {
    let (ctx, target, reference, written, _) = context();
    let ask = |secs: f64| ControlCommand::MatchGrade { clip_id: target.clone(), reference_clip_id: reference.clone(), secs: Some(secs), reference_secs: Some(1.0) };
    respond(&ctx, ask(3.0), &mut None).unwrap();
    let first = written.lock().unwrap().len();
    assert!(first > 0, "the first match grades");
    let again = respond(&ctx, ask(3.0), &mut None).unwrap();
    assert_eq!(again["applied"], serde_json::json!(false), "{again}");
    assert_eq!(again["already_matched"], serde_json::json!(true), "{again}");
    assert_eq!(written.lock().unwrap().len(), first, "nothing written");
    // The clip's grade changed since: no longer the grade the match left.
    let id = klipp_core::ClipId(uuid::Uuid::parse_str(&target).unwrap());
    ctx.shared.last_matches.lock().unwrap().get_mut(&id).unwrap().left[0] = Some(9.0);
    let changed = respond(&ctx, ask(3.0), &mut None).unwrap();
    assert_ne!(changed["already_matched"], serde_json::json!(true), "{changed}");
}

/// Match to Hero again changes nothing and repeats what it reported, for the
/// panel to show: each click on the last one's result walked the grades. A
/// shot graded since is a new match, and a report alone is never remembered.
#[test]
fn the_same_match_to_hero_again_changes_nothing() {
    let (ctx, _target, _reference, written, _) = context();
    let ask = |apply| ControlCommand::MatchScenes { apply, rounds: None, clip_ids: None };
    assert!(respond(&ctx, ask(false), &mut None).unwrap().get("already_matched").is_none());
    assert!(ctx.shared.last_scene_match.lock().unwrap().is_none(), "a report remembers nothing");
    let first = respond(&ctx, ask(true), &mut None).unwrap();
    assert!(first.get("already_matched").is_none(), "{first}");
    let writes = written.lock().unwrap().len();
    let again = respond(&ctx, ask(true), &mut None).unwrap();
    assert_eq!(again["already_matched"], serde_json::json!(true), "{again}");
    assert_eq!(again["applied"], serde_json::json!(false));
    assert_eq!(written.lock().unwrap().len(), writes, "nothing written");
    assert!(again["text"].as_str().unwrap().contains("left as they are"), "{again}");
    assert_eq!(again["scenes"], first["scenes"], "the report kept");
    // A shot graded since: no longer the grades the match left.
    ctx.shared.last_scene_match.lock().unwrap().as_mut().unwrap().0[0].1 = "other".into();
    let changed = respond(&ctx, ask(true), &mut None).unwrap();
    assert!(changed.get("already_matched").is_none(), "{changed}");
}

/// A grey picked is taken to grey, on top of the clip's grade, as one
/// undo step, and the reply says what the point reads after. A point off
/// the frame is refused, changing nothing.
#[test]
fn a_picked_grey_is_balanced_on_and_read_again() {
    let (ctx, target, _, written, _) = context();
    crate::handle(&ctx, ControlCommand::Apply { ops: vec![serde_json::from_value(serde_json::json!({ "op": "grade", "clip_id": target, "exposure": 0.5 })).unwrap()], expected_revision: None, expected_project_identity: None, label: None, detail: false }, &mut None).unwrap_or_else(|e| panic!("{e}"));
    written.lock().unwrap().clear();
    let off = respond(&ctx, ControlCommand::PickGrey { clip_id: target.clone(), x: 1.5, y: 0.5, secs: Some(3.0) }, &mut None).unwrap_err();
    assert!(off.contains("fractions"), "{off}");
    assert!(written.lock().unwrap().is_empty(), "nothing changed");
    let reply = respond(&ctx, ControlCommand::PickGrey { clip_id: target, x: 0.5, y: 0.5, secs: Some(3.0) }, &mut None).unwrap();
    let steps = written.lock().unwrap().clone();
    assert_eq!(steps.len(), 1, "one step: {steps:?}");
    let temperature = steps[0].temperature.expect("a move");
    assert!((-14.0..-2.0).contains(&temperature), "b* +2 on the picked grey takes a few mireds off: {temperature}");
    assert_eq!(steps[0].exposure, Some(0.5), "the grade kept");
    let text = reply["text"].as_str().unwrap();
    assert!(text.contains("it now reads"), "{text}");
}

/// A match that leaves a shot further from its hero is taken back: on a
/// real interview one went from 5.6 to 18.0 and stayed, saturation up by
/// three. The shot ends as it began, and the reply says why.
#[test]
fn a_match_that_brings_a_shot_no_closer_is_taken_back() {
    fn worse_when_graded(seq: &klipp_timeline::Sequence, at: klipp_core::Timecode) -> Vec<u8> {
        use klipp_color::measure::{lab_to_srgb8, Lab};
        let target = seq.tracks[0].clips[1].clone();
        let l = if at < target.timeline_in {
            50.0
        } else if crate::primary_knobs(klipp_timeline::GradeAccess::grade(&target)).exposure.unwrap_or(0.0).abs() > 0.01 {
            75.0
        } else {
            44.0
        };
        let c = lab_to_srgb8(Lab { l, a: 0.0, b: 0.0 });
        [c[0], c[1], c[2], 255].repeat((W * H) as usize)
    }
    let (ctx, target, _reference, written, _) = context_drawing(worse_when_graded);
    let reply = respond(&ctx, ControlCommand::MatchScenes { apply: true, rounds: None, clip_ids: None }, &mut None).unwrap();
    assert!(!written.lock().unwrap().is_empty(), "it tried");
    let shot = reply["scenes"].as_array().unwrap().iter().flat_map(|s| s["shots"].as_array().unwrap()).find(|s| s["clip_id"] == serde_json::json!(target)).unwrap().clone();
    assert_eq!(shot["left_as_it_was"], serde_json::json!("a match brought it no closer"), "{reply}");
    assert_eq!(shot["rounds"], serde_json::json!(0));
    assert!(reply["text"].as_str().unwrap().contains("left as it was"), "{reply}");
    let seq = ctx.sequence.snapshot();
    let knobs = crate::primary_knobs(klipp_timeline::GradeAccess::grade(&seq.tracks[0].clips[1]));
    assert!(knobs.exposure.unwrap_or(0.0).abs() < 0.001, "back where it began: {knobs:?}");
}
