//! Frame-exact cut evidence in sequence space. Analysis is stored in source
//! time, so every suggestion is mapped through the clip that plays it.

use crate::poison::Recover;
use std::path::PathBuf;

use klipp_core::Timecode;
use klipp_timeline::{Clip, ClipKind, TrackKind};
use serde_json::{json, Value};

use crate::{cut_points, ControlContext};

#[derive(Clone)]
struct ClipContext {
    clip: Clip,
    track: usize,
    track_name: String,
    locked: bool,
    heard: bool,
    audio_track: bool,
    path: Option<PathBuf>,
    media_id: Option<String>,
    media_duration_secs: Option<f64>,
    /// The file's frame rate, for placing a change in its picture (#691).
    media_fps: Option<f64>,
    /// The media item, to judge whether its beat sheet is a screen's (#1568).
    media: Option<klipp_media::MediaItem>,
}

/// Match the editorial resolver's audibility rule: a linked video's sound
/// comes from its audio partner, with settings stored on the video clip.
fn clip_sound_is_heard(
    sequence: &klipp_timeline::Sequence,
    pool: &klipp_media::MediaPool,
    track: &klipp_timeline::Track,
    clip: &Clip,
) -> bool {
    if track.muted || !clip.enabled || !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
        return false;
    }
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|media| media.has_audio) {
        return false;
    }
    if track.kind == TrackKind::Video && clip.linked_clip_id.is_some_and(|id| {
        sequence.tracks.iter().any(|lane| lane.kind == TrackKind::Audio && lane.clips.iter().any(|partner| partner.id == id))
    }) {
        return false;
    }
    let settings_clip = clip.linked_clip_id.and_then(|id| {
        sequence.tracks.iter().filter(|lane| lane.kind == TrackKind::Video)
            .flat_map(|lane| lane.clips.iter()).find(|partner| partner.id == id)
    }).unwrap_or(clip);
    settings_clip.volume > 0.0
}

pub(crate) fn prepare(
    ctx: &ControlContext,
    from_secs: f64,
    to_secs: f64,
    near_secs: Option<f64>,
    intent: Option<String>,
    max_candidates: Option<usize>,
) -> Result<Value, String> {
    if !from_secs.is_finite()
        || !to_secs.is_finite()
        || from_secs < 0.0
        || to_secs <= from_secs
        || to_secs - from_secs > 300.0
    {
        return Err("give a positive timeline range no longer than 300 seconds".into());
    }
    if near_secs.is_some_and(|near| !near.is_finite() || near < from_secs || near > to_secs) {
        return Err("near_secs must be inside the requested timeline range".into());
    }
    let limit = max_candidates.unwrap_or(12);
    if !(1..=32).contains(&limit) {
        return Err("max_candidates must be 1 to 32".into());
    }
    let revision = ctx.shared.revision();
    let identity = ctx
        .shared
        .project
        .lock()
        .map_err(|_| "project status lock poisoned")?
        .identity;
    let sequence = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let pool = ctx
        .media_pool
        .read()
        .recovered();
    let fps = sequence.fps.as_f64();
    let fps_num = sequence.fps.num;
    let fps_den = sequence.fps.den;
    let from_frame = (from_secs * fps).ceil() as i64;
    let to_frame = (to_secs * fps).floor() as i64;
    let unlocked_tracks: Vec<Value> = sequence
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| !track.locked)
        .map(|(index, track)| json!({ "track": index, "name": track.name }))
        .collect();
    let mut clips = Vec::new();
    for (track, lane) in sequence.tracks.iter().enumerate() {
        for clip in &lane.clips {
            if !clip.enabled { continue; }
            if clip.timeline_out.to_secs_f64() <= from_secs
                || clip.timeline_in.to_secs_f64() >= to_secs
            {
                continue;
            }
            let media_id = clip.media_id();
            let item = media_id.and_then(|id| pool.get(&id));
            clips.push(ClipContext {
                clip: clip.clone(),
                track,
                track_name: lane.name.clone(),
                locked: lane.locked,
                heard: clip_sound_is_heard(&sequence, &pool, lane, clip),
                audio_track: lane.kind == TrackKind::Audio,
                path: item.map(|item| item.path.clone()),
                media_id: media_id.map(|id| id.0.to_string()),
                media_duration_secs: item.map(|item| item.duration.to_secs_f64()),
                media_fps: item.map(|item| item.fps.as_f64()).filter(|f| *f > 0.0),
                media: item.cloned(),
            });
        }
    }
    drop(pool);
    drop(sequence);
    if clips.len() > 64 {
        return Err("more than 64 clips overlap this range; narrow the timeline range".into());
    }
    let context: Vec<Value> = clips.iter().map(|item| {
        let clip = &item.clip;
        let source_in = clip.source_in.to_secs_f64();
        let source_out = clip.source_out.to_secs_f64();
        json!({
            "clip_id": clip.id.0.to_string(), "track": item.track, "track_name": item.track_name,
            "locked": item.locked, "sound_heard": item.heard,
            "linked_clip_id": clip.linked_clip_id.map(|id| id.0.to_string()),
            "kind": if matches!(clip.kind, ClipKind::Media { .. }) { "media" } else { "other" },
            "media_id": item.media_id,
            "timeline_in_frame": (clip.timeline_in.to_secs_f64() * fps).round() as i64,
            "timeline_out_frame": (clip.timeline_out.to_secs_f64() * fps).round() as i64,
            "source_in_secs": source_in, "source_out_secs": source_out,
            "speed": clip.speed, "speed_ramp": !clip.speed_keyframes.is_empty(),
            "reversed": clip.is_reversed(),
            "source_handle_before_secs": source_in.max(0.0),
            "source_handle_after_secs": item.media_duration_secs.map(|duration| (duration - source_out).max(0.0)),
        })
    }).collect();

    let mut candidates = Vec::new();
    let mut speech_checks: Vec<(Clip, Vec<(f64, f64)>, bool)> = Vec::new();
    for item in &clips {
        if item.audio_track && !item.heard { continue; }
        let Some(path) = &item.path else { continue };
        let report = klipp_analysis::report::MediaAnalysis::load(path);
        let clip = &item.clip;
        let clip_in = clip.timeline_in.to_secs_f64();
        let start = from_secs.max(clip_in);
        let end = to_secs.min(clip.timeline_out.to_secs_f64());
        if end <= start {
            continue;
        }
        let source_a = clip.source_secs_at(start - clip_in, fps);
        let source_b = clip.source_secs_at(end - clip_in, fps);
        let source_range = (source_a.min(source_b), source_a.max(source_b));
        let near_source = near_secs
            .filter(|near| *near >= start && *near <= end)
            .map(|near| clip.source_secs_at(near - clip_in, fps));
        let words: Vec<(f64, f64, String)> = report
            .transcript
            .as_ref()
            .filter(|_| item.heard)
            .map(|t| {
                t.words
                    .iter()
                    .map(|w| (w.start_secs, w.end_secs, w.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if item.heard {
            speech_checks.push((
                clip.clone(),
                words.iter().map(|(start, end, _)| (*start, *end)).collect(),
                !words.is_empty(),
            ));
        }
        let segments: Vec<(f64, f64, String)> = report
            .transcript
            .as_ref()
            .filter(|_| item.heard)
            .map(|t| {
                t.segments
                    .iter()
                    .map(|s| (s.start_secs, s.end_secs, s.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let silences: Vec<(f64, f64)> = report
            .audio
            .as_ref()
            .filter(|_| item.heard)
            .map(|audio| {
                audio
                    .silences
                    .iter()
                    .map(|s| (s.start_secs, s.end_secs))
                    .collect()
            })
            .unwrap_or_default();
        let shots: Vec<f64> = report
            .shots
            .as_ref()
            .map(|shots| shots.boundaries.clone())
            .unwrap_or_default();
        let points = cut_points(
            &words,
            &segments,
            &silences,
            &shots,
            near_source,
            source_range,
        );
        for point in points["points"].as_array().into_iter().flatten() {
            let Some(source_secs) = point["secs"].as_f64() else {
                continue;
            };
            let kind = point["kind"].as_str().unwrap_or("evidence");
            let why = point["why"].as_str().unwrap_or("source analysis");
            let strength = point["strength"].as_u64().unwrap_or(1) as u8;
            add_candidate(
                &mut candidates,
                item,
                fps,
                from_frame,
                to_frame,
                source_secs,
                kind,
                why,
                strength,
                None,
                &words,
            );
        }
        if let Some(sheet) = item.media.as_ref().and_then(|m| report.screen_beatsheet(m)) {
            for point in &sheet.cut_points {
                add_candidate(
                    &mut candidates,
                    item,
                    fps,
                    from_frame,
                    to_frame,
                    point.secs,
                    "screen_change",
                    &point.why,
                    point.strength,
                    None,
                    &words,
                );
            }
            for &source_secs in &sheet.reveals {
                let hold = readable_hold_secs(clip, source_secs, &sheet.segments, fps);
                if hold >= 1.2 {
                    add_candidate(
                        &mut candidates,
                        item,
                        fps,
                        from_frame,
                        to_frame,
                        source_secs,
                        "settled_reveal",
                        "the screen has settled and can be held to read",
                        3,
                        Some(hold),
                        &words,
                    );
                }
            }
        }
    }
    exclude_spoken_frames(&mut candidates, &speech_checks, fps);
    if ctx.shared.revision() != revision
        || ctx
            .shared
            .project
            .lock()
            .map_err(|_| "project status lock poisoned")?
            .identity
            != identity
    {
        return Err("the timeline changed while cut evidence was gathered; read it again".into());
    }
    let target = near_secs.map(|near| (near * fps).round() as i64);
    let intent = intent.unwrap_or_default();
    let intent_lower = intent.to_ascii_lowercase();
    let wants_reveal = intent_lower.contains("reveal") || intent_lower.contains("screen");
    let wants_dialogue = ["dialogue", "speech", "sentence", "word", "pause"]
        .iter()
        .any(|word| intent_lower.contains(word));
    candidates.sort_by(|a, b| {
        let rank = |candidate: &Value| {
            let mut strength = candidate["strength"].as_i64().unwrap_or(0);
            if wants_reveal && candidate["kind"] == "settled_reveal" {
                strength += 2;
            }
            let distance = target
                .map(|frame| (candidate["frame"].as_i64().unwrap_or(0) - frame).abs())
                .unwrap_or(0);
            (strength * 100 - distance.min(200), -distance)
        };
        rank(b).cmp(&rank(a))
    });
    let mut merged: Vec<Value> = Vec::new();
    let mut indexes: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for candidate in candidates {
        let frame = candidate["frame"].as_i64().unwrap_or(-1);
        if let Some(&index) = indexes.get(&frame) {
            if let (Some(existing), Some(extra)) = (
                merged[index]["evidence"].as_array_mut(),
                candidate["evidence"].as_array(),
            ) {
                existing.extend(extra.iter().cloned());
            }
        } else {
            indexes.insert(frame, merged.len());
            merged.push(candidate);
        }
    }
    let mut candidates = merged;
    let competing = target.is_none()
        && candidates.len() > 1
        && candidates[0]["strength"] == candidates[1]["strength"];
    let ambiguous = candidates
        .first()
        .is_none_or(|first| first["strength"].as_u64().unwrap_or(0) < 2)
        || candidates.first().is_some_and(|first| {
            first["uncertain"] == true || (wants_dialogue && first["speech_known"] == false)
        })
        || competing;
    candidates.truncate(limit);
    for (rank, candidate) in candidates.iter_mut().enumerate() {
        let strength = candidate["strength"].as_u64().unwrap_or(0);
        let uncertain = candidate["uncertain"] == true
            || (wants_dialogue && candidate["speech_known"] == false);
        candidate["rank"] = json!(rank + 1);
        candidate["confidence"] = json!(if uncertain || strength < 2 {
            "low"
        } else if strength == 2 {
            "medium"
        } else {
            "high"
        });
        candidate["ranking_reason"] = json!(if target.is_some() {
            "evidence strength, then distance from the requested moment"
        } else {
            "evidence strength; give near_secs to resolve competing points"
        });
    }
    let next = if candidates.is_empty() {
        "No measured cut point in this range. Run analysis or inspect a contact sheet and audio excerpt; do not guess a frame."
    } else if ambiguous {
        "Evidence is weak or may overlap speech. Inspect a contact sheet or audio excerpt, then plan the chosen cut."
    } else {
        "Plan the chosen frame with klipp_plan, apply with this expected_revision and expected_project_identity, then verify the definitive write outcome."
    };
    Ok(json!({
        "revision": revision, "project_identity": identity,
        "range": { "from_frame": from_frame, "to_frame": to_frame, "fps_num": fps_num, "fps_den": fps_den },
        "clips": context, "candidates": candidates, "ambiguous": ambiguous,
        "unlocked_tracks": unlocked_tracks, "intent": intent, "next": next,
    }))
}

fn exclude_spoken_frames(
    candidates: &mut Vec<Value>,
    speech_checks: &[(Clip, Vec<(f64, f64)>, bool)],
    fps: f64,
) {
    candidates.retain_mut(|candidate| {
        let secs = candidate["secs"].as_f64().unwrap_or(-1.0);
        let mut speech_unknown = false;
        for (clip, words, known) in speech_checks {
            if secs < clip.timeline_in.to_secs_f64() || secs >= clip.timeline_out.to_secs_f64() {
                continue;
            }
            let source = clip.source_secs_at(secs - clip.timeline_in.to_secs_f64(), fps);
            if words
                .iter()
                .any(|(start, end)| source > *start + 1e-3 && source < *end - 1e-3)
            {
                return false;
            }
            speech_unknown |= !known;
        }
        if speech_unknown {
            candidate["uncertain"] = json!(true);
            candidate["speech_known"] = json!(false);
        }
        true
    });
}

fn readable_hold_secs(
    clip: &Clip,
    source_secs: f64,
    segments: &[klipp_analysis::beatsheet::Segment],
    fps: f64,
) -> f64 {
    segments
        .iter()
        .find(|segment| {
            segment.kind == "idle"
                && segment.start_secs <= source_secs
                && segment.end_secs > source_secs
        })
        .map(|segment| {
            let end = if clip.is_reversed() {
                segment.start_secs
            } else {
                segment.end_secs
            };
            let duration = (clip.timeline_out - clip.timeline_in).to_secs_f64();
            (clip.elapsed_for_source_secs(end, fps).min(duration)
                - clip.elapsed_for_source_secs(source_secs, fps))
            .max(0.0)
        })
        .unwrap_or(0.0)
}

#[allow(clippy::too_many_arguments)]
fn add_candidate(
    candidates: &mut Vec<Value>,
    item: &ClipContext,
    fps: f64,
    from_frame: i64,
    to_frame: i64,
    source_secs: f64,
    kind: &str,
    why: &str,
    strength: u8,
    hold_secs: Option<f64>,
    words: &[(f64, f64, String)],
) {
    let clip = &item.clip;
    // A change in the picture goes on the first frame that shows it, as
    // the decoders pick frames; rounding the mapped time put it a frame
    // early whenever the file's rate is not the sequence's (#691). A word
    // or a pause has no frame of its own and goes on the nearest.
    let picture = matches!(kind, "shot_change" | "screen_change" | "settled_reveal");
    let frame = if picture {
        let Some(f) = clip.first_frame_showing(source_secs, item.media_fps.unwrap_or(fps), fps) else { return };
        f
    } else {
        let source = Timecode::from_secs_f64(source_secs, clip.source_in.rate);
        let Some(timeline) = clip.sequence_time_for_source_time(source, fps) else {
            return;
        };
        (timeline.to_secs_f64() * fps).round() as i64
    };
    let clip_first = (clip.timeline_in.to_secs_f64() * fps).round() as i64;
    let clip_end = (clip.timeline_out.to_secs_f64() * fps).round() as i64;
    if frame <= clip_first || frame >= clip_end || frame < from_frame || frame > to_frame {
        return;
    }
    let secs = frame as f64 / fps;
    let mapped_source = clip.source_secs_at(secs - clip.timeline_in.to_secs_f64(), fps);
    let inside_word = words
        .iter()
        .any(|(start, end, _)| mapped_source > *start + 1e-3 && mapped_source < *end - 1e-3);
    if inside_word {
        return;
    }
    let uncertain = item.locked
        || (words.is_empty() && matches!(kind, "phrase" | "sentence_end" | "sentence_start"));
    candidates.push(json!({
        "frame": frame, "secs": secs, "clip_id": clip.id.0.to_string(), "track": item.track,
        "media_id": item.media_id, "source_secs": mapped_source,
        "kind": kind, "why": why, "strength": strength,
        "evidence": [{ "kind": kind, "source_secs": source_secs, "why": why }],
        "uncertain": uncertain, "speech_known": !words.is_empty(), "hold_secs": hold_secs,
        "linked_clip_id": clip.linked_clip_id.map(|id| id.0.to_string()),
        "locked": item.locked, "retimed": clip.is_varispeed() || !clip.speed_keyframes.is_empty(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_and_disabled_state_remove_audio_evidence() {
        let mut pool = klipp_media::MediaPool::new();
        let mut media = klipp_media::MediaItem::synthetic_for_test("/tmp/cut-prep-audibility.wav".into(), None);
        media.has_audio = true;
        let id = pool.add(media);
        let time = |secs| Timecode::from_secs_f64(secs, 24);
        let clip = Clip::media(id, time(0.0), time(2.0), time(0.0), time(2.0));
        let mut sequence = klipp_timeline::Sequence::new_default();
        let mut track = klipp_timeline::Track::new("A1", TrackKind::Audio);
        track.clips.push(clip);
        sequence.tracks = vec![track];
        assert!(clip_sound_is_heard(&sequence, &pool, &sequence.tracks[0], &sequence.tracks[0].clips[0]));
        sequence.tracks[0].muted = true;
        assert!(!clip_sound_is_heard(&sequence, &pool, &sequence.tracks[0], &sequence.tracks[0].clips[0]));
        sequence.tracks[0].muted = false;
        sequence.tracks[0].clips[0].enabled = false;
        assert!(!clip_sound_is_heard(&sequence, &pool, &sequence.tracks[0], &sequence.tracks[0].clips[0]));
    }

    #[test]
    fn a_trimmed_double_speed_pause_maps_to_a_sequence_frame_outside_words() {
        let time = |secs| Timecode::from_secs_f64(secs, 24);
        let mut clip = Clip::media(
            klipp_core::MediaId::new(),
            time(10.0),
            time(12.0),
            time(2.0),
            time(6.0),
        );
        clip.speed = 2.0;
        let item = ClipContext {
            clip,
            track: 0,
            track_name: "V1".into(),
            locked: false,
            heard: false,
            audio_track: false,
            path: None,
            media_id: None,
            media_duration_secs: Some(8.0),
            media_fps: Some(24.0),
            media: None,
        };
        let words = vec![(3.8, 4.2, "hello".into())];
        let mut candidates = Vec::new();
        add_candidate(
            &mut candidates,
            &item,
            24.0,
            240,
            288,
            4.0,
            "silence",
            "pause",
            3,
            None,
            &words,
        );
        assert!(
            candidates.is_empty(),
            "a rounded frame inside a spoken word is not safe"
        );
        add_candidate(
            &mut candidates,
            &item,
            24.0,
            240,
            288,
            4.5,
            "silence",
            "pause",
            3,
            None,
            &words,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["frame"], 270);
        assert_eq!(candidates[0]["secs"], 11.25);
        assert_eq!(candidates[0]["source_secs"], 4.5);
    }

    #[test]
    fn a_screen_reveal_needs_a_readable_timeline_hold_after_retiming() {
        let time = |secs| Timecode::from_secs_f64(secs, 24);
        let mut clip = Clip::media(
            klipp_core::MediaId::new(),
            time(10.0),
            time(12.0),
            time(2.0),
            time(6.0),
        );
        let segments = vec![klipp_analysis::beatsheet::Segment {
            start_secs: 4.0,
            end_secs: 6.0,
            kind: "idle".into(),
        }];
        clip.speed = 2.0;
        assert!((readable_hold_secs(&clip, 4.0, &segments, 24.0) - 1.0).abs() < 1e-6);
        clip.speed = 1.0;
        clip.timeline_out = time(14.0);
        assert!((readable_hold_secs(&clip, 4.0, &segments, 24.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_picture_candidate_inside_its_linked_sounds_word_is_excluded() {
        let time = |secs| Timecode::from_secs_f64(secs, 24);
        let mut sound = Clip::media(
            klipp_core::MediaId::new(),
            time(10.0),
            time(12.0),
            time(2.0),
            time(6.0),
        );
        sound.speed = 2.0;
        let mut candidates = vec![
            json!({ "secs": 11.0, "uncertain": false, "speech_known": true }),
            json!({ "secs": 11.25, "uncertain": false, "speech_known": true }),
        ];
        exclude_spoken_frames(&mut candidates, &[(sound, vec![(3.8, 4.2)], true)], 24.0);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["secs"], 11.25);
    }
}
