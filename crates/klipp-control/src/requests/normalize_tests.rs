//! `normalize_clips` over the socket (#1843): measured here, the gains
//! answered and sent as one `ApplyClipGains`.

use super::*;
use std::sync::{Arc, Mutex, RwLock};

/// `secs` of a 997 Hz tone at `db` dBFS on both channels, as float WAV.
fn tone(dir: &std::path::Path, name: &str, db: f64, secs: f64) -> Option<std::path::PathBuf> {
    let out = dir.join(format!("{name}.wav"));
    let a = 10f64.powf(db / 20.0);
    let wave = format!("{a}*sin(2*PI*997*t)");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={wave}|{wave}:s=48000:d={secs}"))
        .args(["-c:a", "pcm_f32le"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// Two clips of tones at -20 and -30 dBFS on an audio track, and what the
/// app was sent.
fn context() -> Option<(ControlContext, Vec<klipp_core::ClipId>, Arc<Mutex<Vec<AppCommand>>>)> {
    let root = std::env::temp_dir().join(format!("klipp-normalize-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut pool = klipp_media::MediaPool::new();
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.clear();
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let mut track = klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio);
    for (i, db) in [-20.0, -30.0].into_iter().enumerate() {
        let path = tone(&root, &format!("tone{i}"), db, 4.0)?;
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.has_audio = true;
        let media = pool.add(item);
        let start = i as i64 * 96;
        track.clips.push(klipp_timeline::Clip::media(media, time(start), time(start + 96), time(0), time(96)));
    }
    let ids = track.clips.iter().map(|c| c.id).collect();
    sequence.tracks.push(track);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (commands, receiver) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let got = sent.clone();
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            got.lock().unwrap().extend(envelope.commands);
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck { revision: 1, refused: None, refused_op: None, renamed: Vec::new(), ungated: Vec::new(), picture_refused: None }).unwrap();
            }
        }
    });
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(8, 8)));
    let ctx = ControlContext {
        commands, shared: Arc::new(klipp_core::ControlShared::default()), wake_ui,
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(pool)), frame_buffer: fb,
        perf_ring: klipp_engine::perf::shared(), log_path: root.join("unused.log"), others: Default::default(),
    };
    Some((ctx, ids, sent))
}

fn normalize(ctx: &ControlContext, ids: &[klipp_core::ClipId], group: bool, apply: Option<bool>) -> serde_json::Value {
    respond(ctx, ControlCommand::NormalizeClips {
        clip_ids: ids.iter().map(|c| c.to_string()).collect(),
        mode: klipp_core::normalize::NormalizeMode::Loudness,
        target: Some(-16.0),
        target_name: None,
        ceiling_dbtp: Some(-1.0),
        group,
        apply,
    }, &mut None).unwrap()
}

/// The gains sent, by clip, in dB.
fn sent_db(sent: &Mutex<Vec<AppCommand>>) -> Vec<(klipp_core::ClipId, f64)> {
    sent.lock().unwrap().iter().flat_map(|c| match c {
        AppCommand::ApplyClipGains { gains } => gains.iter().map(|(id, g)| (*id, 20.0 * (*g as f64).log10())).collect(),
        _ => Vec::new(),
    }).collect()
}

#[test]
fn each_clip_is_brought_to_the_target_and_the_gains_sent() {
    let Some((ctx, ids, sent)) = context() else { return };
    let reply = normalize(&ctx, &ids, false, None);
    let clips = reply["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2, "{reply}");
    let sent = sent_db(&sent);
    for (row, id) in clips.iter().zip(&ids) {
        let (lufs, gain) = (row["integrated_lufs"].as_f64().unwrap(), row["gain_db"].as_f64().unwrap());
        assert!((lufs + gain + 16.0).abs() < 0.02, "measured {lufs}, gain {gain}: {row}");
        let (_, applied) = sent.iter().find(|(c, _)| c == id).expect("the clip's gain was sent");
        assert!((applied - gain).abs() < 0.01, "sent {applied} dB, answered {gain}");
    }
}

#[test]
fn a_group_takes_one_gain() {
    let Some((ctx, ids, sent)) = context() else { return };
    let reply = normalize(&ctx, &ids, true, None);
    let sent = sent_db(&sent);
    assert_eq!(sent.len(), 2, "{reply}");
    assert!((sent[0].1 - sent[1].1).abs() < 1e-4, "one gain for both: {sent:?}");
    let together = reply["together"]["integrated_lufs"].as_f64().unwrap();
    assert!((together + sent[0].1 + 16.0).abs() < 0.02, "the group at the target: {reply}");
}

#[test]
fn a_preview_sets_nothing() {
    let Some((ctx, ids, sent)) = context() else { return };
    let reply = normalize(&ctx, &ids, false, Some(false));
    assert_eq!(reply["applied"], false);
    assert!(sent_db(&sent).is_empty(), "nothing sent");
    assert!(reply["clips"][0]["gain_db"].is_number(), "but it says what it would set: {reply}");
}

#[test]
fn it_is_refused_in_a_batch() {
    let Some((ctx, ids, _)) = context() else { return };
    let mut collect = Some(Collected::default());
    let got = respond(&ctx, ControlCommand::NormalizeClips {
        clip_ids: vec![ids[0].to_string()], mode: Default::default(), target: None, target_name: None, ceiling_dbtp: None, group: false, apply: None,
    }, &mut collect);
    assert!(got.is_err());
}

/// A loudness target by name (#2887): a standard's, any case, measures to
/// it as the number would; one that isn't there, a name with a number
/// too, or a name for a peak are refused, saying which there are.
#[test]
fn a_loudness_target_is_named() {
    let Some((ctx, ids, _)) = context() else { return };
    let ask = |mode, target: Option<f64>, name: &str| respond(&ctx, ControlCommand::NormalizeClips {
        clip_ids: vec![ids[0].to_string()], mode, target, target_name: Some(name.into()), ceiling_dbtp: None, group: false, apply: Some(false),
    }, &mut None);
    let loudness = klipp_core::normalize::NormalizeMode::Loudness;
    let why = ask(loudness, None, "Cinema").unwrap_err();
    assert!(why.contains("no loudness target \"Cinema\"") && why.contains("EBU R128 broadcast"), "{why}");
    assert!(ask(loudness, Some(-20.0), "Podcast").unwrap_err().contains("not both"));
    assert!(ask(klipp_core::normalize::NormalizeMode::Peak, None, "Podcast").unwrap_err().contains("peak"));
    assert!(ask(loudness, None, "podcast").is_ok(), "a standard by name, any case");
}

