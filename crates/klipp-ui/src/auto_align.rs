//! Auto Align Audio (#615): a camera clip, with its scratch sound, and a
//! separate recording selected on the timeline, the recording moved so its
//! sound lines up with the camera's, by waveform or by timecode.
//!
//! The rules here are pure and tested: which of the selected clips is the
//! reference and which the recording, whether each method can run, and
//! where timecode puts the recording. The app runs the waveform match on a
//! worker and moves the recording with one undoable MoveClip.

use klipp_core::{ClipId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, TrackKind};

/// The two sides of an alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignPair {
    /// The clip whose sound is the reference: the camera's scratch audio
    /// when it is on its own track, else the camera clip itself.
    pub reference: ClipId,
    /// The recording that moves.
    pub recording: ClipId,
}

/// How to align.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignMethod {
    #[default]
    Waveform,
    Timecode,
}

/// The pair a selection names, or why it names none. A camera clip and
/// its linked scratch audio count as one: the selection is a camera (a
/// picture clip, or the sound linked to one) and one unlinked recording
/// on an audio track.
pub fn pair_from_selection(seq: &Sequence, selected: &[ClipId]) -> Result<AlignPair, String> {
    let clip = |id: ClipId| seq.find_clip(id).map(|(t, c)| (&seq.tracks[t], &seq.tracks[t].clips[c]));
    let mut units: Vec<Vec<ClipId>> = Vec::new();
    for &id in selected {
        if units.iter().any(|u| u.contains(&id)) {
            continue;
        }
        let Some((_, c)) = clip(id) else { continue };
        let mut unit = vec![id];
        if let Some(partner) = c.linked_clip_id.filter(|p| clip(*p).is_some()) {
            unit.push(partner);
        }
        units.push(unit);
    }
    if units.len() != 2 {
        return Err("Select one camera clip and one separate recording".into());
    }
    let is_camera = |unit: &Vec<ClipId>| unit.iter().any(|&id| clip(id).is_some_and(|(t, _)| t.kind == TrackKind::Video));
    let (camera, recording) = match (is_camera(&units[0]), is_camera(&units[1])) {
        (true, false) => (&units[0], &units[1]),
        (false, true) => (&units[1], &units[0]),
        (true, true) => return Err("Both selections are camera clips: select one camera clip and one recording".into()),
        (false, false) => return Err("Neither selection is a camera clip: select one camera clip and one recording".into()),
    };
    let [recording] = recording.as_slice() else {
        return Err("The recording is linked to another clip: unlink it, or select the separate recording".into());
    };
    if !clip(*recording).is_some_and(|(t, _)| t.kind == TrackKind::Audio) {
        return Err("The recording is not on an audio track".into());
    }
    // The camera's own sound: its linked audio-track clip, else itself.
    let reference = camera.iter().copied().find(|&id| clip(id).is_some_and(|(t, _)| t.kind == TrackKind::Audio)).unwrap_or(camera[0]);
    Ok(AlignPair { reference, recording: *recording })
}

/// Why the waveform match cannot run, if it cannot: both sides need their
/// sound prepared (conformed).
pub fn waveform_unavailable(seq: &Sequence, pool: &MediaPool, pair: AlignPair) -> Option<String> {
    for (side, id) in [("camera", pair.reference), ("recording", pair.recording)] {
        let item = media_of(seq, pool, id);
        if !item.is_some_and(|m| m.has_audio) {
            return Some(format!("the {side} has no sound"));
        }
        if !item.is_some_and(|m| m.audio_cache.is_some()) {
            return Some(format!("the {side}'s sound is still being prepared; try again in a moment"));
        }
    }
    None
}

/// Where timecode puts the recording on the timeline, or why it cannot:
/// both media files need a start timecode. The same moment of the shoot
/// sits under both clips: the reference's source start (timecode plus its
/// in point) is at its timeline in, and the recording follows from there.
pub fn timecode_position(seq: &Sequence, pool: &MediaPool, pair: AlignPair) -> Result<Timecode, String> {
    let clock = |side: &str, id: ClipId| -> Result<(f64, f64, f64), String> {
        let (t, c) = seq.find_clip(id).ok_or_else(|| format!("the {side} is not on the timeline"))?;
        let clip = &seq.tracks[t].clips[c];
        let item = media_of(seq, pool, id).ok_or_else(|| format!("the {side} has no media"))?;
        let tc = item.shot.start_timecode.as_deref().ok_or_else(|| format!("the {side}'s file has no start timecode"))?;
        let fps = if item.fps.num == 0 { seq.fps } else { item.fps };
        let start = klipp_core::sync::timecode_to_secs(tc, fps).ok_or_else(|| format!("the {side}'s start timecode {tc} cannot be read"))?;
        Ok((start + clip.source_in.to_secs_f64(), clip.timeline_in.to_secs_f64(), clip.speed))
    };
    let (ref_at, ref_in, ref_speed) = clock("camera", pair.reference)?;
    let (rec_at, _, rec_speed) = clock("recording", pair.recording)?;
    if (ref_speed - 1.0).abs() > 1e-6 || (rec_speed - 1.0).abs() > 1e-6 {
        return Err("a retimed clip cannot be lined up by timecode".into());
    }
    let new_in = ref_in + (rec_at - ref_at);
    if new_in < 0.0 {
        return Err(format!("the recording would start {:.2} s before the timeline does", -new_in));
    }
    Ok(Timecode::from_secs_f64(new_in, seq.fps.rate()))
}

fn media_of<'a>(seq: &Sequence, pool: &'a MediaPool, id: ClipId) -> Option<&'a klipp_media::MediaItem> {
    let (t, c) = seq.find_clip(id)?;
    pool.get(&seq.tracks[t].clips[c].media_id()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_media::MediaItem;
    use klipp_timeline::{Clip, Track};

    fn t(secs: f64) -> Timecode {
        Timecode::from_secs_f64(secs, 25)
    }

    struct Rig {
        seq: Sequence,
        pool: MediaPool,
        video: ClipId,
        scratch: ClipId,
        recording: ClipId,
    }

    /// A camera clip linked to its scratch audio, and a separate recording.
    fn rig(camera_tc: Option<&str>, recorder_tc: Option<&str>) -> Rig {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut pool = MediaPool::new();
        let mut cam = MediaItem::synthetic_for_test("/tmp/A001.mov".into(), None);
        cam.has_audio = true;
        cam.shot.start_timecode = camera_tc.map(str::to_string);
        let mut rec = MediaItem::synthetic_for_test("/tmp/ZOOM0001.wav".into(), None);
        rec.has_audio = true;
        rec.shot.start_timecode = recorder_tc.map(str::to_string);
        let (cam_id, rec_id) = (pool.add(cam), pool.add(rec));
        let mut video = Clip::media(cam_id, t(10.0), t(20.0), t(2.0), t(12.0));
        let mut scratch = Clip::media(cam_id, t(10.0), t(20.0), t(2.0), t(12.0));
        video.linked_clip_id = Some(scratch.id);
        scratch.linked_clip_id = Some(video.id);
        let recording = Clip::media(rec_id, t(0.0), t(30.0), t(0.0), t(30.0));
        let ids = (video.id, scratch.id, recording.id);
        let mut v = Track::new("Video 1", TrackKind::Video);
        v.clips.push(video);
        let mut a1 = Track::new("Audio 1", TrackKind::Audio);
        a1.clips.push(scratch);
        let mut a2 = Track::new("Audio 2", TrackKind::Audio);
        a2.clips.push(recording);
        seq.tracks = vec![v, a1, a2];
        Rig { seq, pool, video: ids.0, scratch: ids.1, recording: ids.2 }
    }

    #[test]
    fn a_camera_and_its_scratch_count_as_one_and_the_scratch_is_the_reference() {
        let r = rig(None, None);
        let want = Ok(AlignPair { reference: r.scratch, recording: r.recording });
        assert_eq!(pair_from_selection(&r.seq, &[r.video, r.scratch, r.recording]), want, "all three selected");
        assert_eq!(pair_from_selection(&r.seq, &[r.recording, r.video]), want, "the camera picture alone");
        assert_eq!(pair_from_selection(&r.seq, &[r.scratch, r.recording]), want, "the scratch sound alone");
    }

    #[test]
    fn a_selection_without_one_camera_and_one_recording_is_refused() {
        let r = rig(None, None);
        assert!(pair_from_selection(&r.seq, &[r.video]).is_err());
        assert!(pair_from_selection(&r.seq, &[r.recording]).is_err());
        assert!(pair_from_selection(&r.seq, &[]).is_err());
    }

    #[test]
    fn timecode_lines_up_the_same_moment_under_both_clips() {
        // Camera file starts at 10:00:00:00, recorder at 09:59:55:00: the
        // recorder started five seconds earlier. The camera clip uses its
        // source from 2 s, at timeline 10 s, so the shoot's 10:00:02 is at
        // timeline 10 s; the recording's 0 s is 09:59:55, seven seconds
        // before, at timeline 3 s.
        let r = rig(Some("10:00:00:00"), Some("09:59:55:00"));
        let pair = pair_from_selection(&r.seq, &[r.video, r.recording]).unwrap();
        let at = timecode_position(&r.seq, &r.pool, pair).unwrap();
        assert!((at.to_secs_f64() - 3.0).abs() < 1e-6, "{}", at.to_secs_f64());
    }

    #[test]
    fn timecode_says_why_it_cannot_run() {
        let r = rig(Some("10:00:00:00"), None);
        let pair = pair_from_selection(&r.seq, &[r.video, r.recording]).unwrap();
        let why = timecode_position(&r.seq, &r.pool, pair).unwrap_err();
        assert!(why.contains("recording's file has no start timecode"), "{why}");
        // A recorder started a minute before the camera would put the
        // recording's start 52 s before the timeline's.
        let r = rig(Some("10:00:00:00"), Some("09:59:00:00"));
        let pair = pair_from_selection(&r.seq, &[r.video, r.recording]).unwrap();
        assert!(timecode_position(&r.seq, &r.pool, pair).unwrap_err().contains("before the timeline"));
    }

    #[test]
    fn waveform_needs_both_sounds_prepared() {
        let r = rig(None, None);
        let pair = pair_from_selection(&r.seq, &[r.video, r.recording]).unwrap();
        let why = waveform_unavailable(&r.seq, &r.pool, pair).expect("no audio caches in the rig");
        assert!(why.contains("still being prepared"), "{why}");
    }
}
