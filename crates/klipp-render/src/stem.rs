//! A frozen sound track's stem (#3207): what the track sends to its
//! fader, its clips summed and run through its insert chain, rendered once
//! by the export's own mixdown so playback can play it in their place.
//!
//! The stem is the track alone, its fader at unity, centred, with no
//! automation, sends or buses: the export's mix of that is exactly what
//! reaches the fader. Playback then applies the fader, pan, sends, mute
//! and solo live, as for any track.

use std::hash::{Hash, Hasher};

use klipp_core::TrackId;
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind};

use crate::audio_mixdown::AudioMixdown;
use crate::error::RenderError;
use crate::snapshot::SequenceSnapshot;

/// Bumped whenever the mixdown's output for a track changes.
pub const STEM_RENDER_VERSION: u32 = 1;

/// Why track `track` of `seq` can't be frozen as a stem, or `None` if it
/// can: a track that isn't a sound track, or whose chain or a clip's holds
/// a plug-in from outside Klipp, whose output can't be keyed or trusted
/// to come out the same.
pub fn live_reason(seq: &Sequence, track: TrackId) -> Option<&'static str> {
    let Some(t) = seq.tracks.iter().find(|t| t.id == track) else { return Some("no such track") };
    if t.kind != TrackKind::Audio {
        return Some("not a sound track");
    }
    let outside = |e: &klipp_core::AudioEffectInstance| e.effect_type.starts_with("lv2:") || e.effect_type.starts_with("clap:");
    if t.effects.audio.iter().any(outside) || t.clips.iter().any(|c| c.audio_effects.iter().any(outside)) {
        return Some("a plug-in from outside Klipp is in its chain");
    }
    None
}

/// The sequence the stem of `track` is the export of: that track alone,
/// heard, at unity and centred, with no automation and no sends or buses.
fn alone(seq: &Sequence, track: TrackId) -> Option<Sequence> {
    let mut t = seq.tracks.iter().find(|t| t.id == track)?.clone();
    t.volume = 1.0;
    t.pan = 0.0;
    t.muted = false;
    t.solo = false;
    t.automation = Default::default();
    t.sends.clear();
    let mut one = seq.clone();
    one.tracks = vec![t];
    one.buses.clear();
    Some(one)
}

/// The stem's key: everything its samples depend on (the track as
/// [`alone`] makes it, the sequences its compound clips open, each file's
/// identity from `media`, the rate) and nothing applied live over it.
/// `None` for a track that can't be frozen.
pub fn stem_key(seq: &Sequence, all_sequences: &[Sequence], track: TrackId, media: &dyn Fn(&klipp_core::MediaId) -> Option<u64>) -> Option<u64> {
    if live_reason(seq, track).is_some() {
        return None;
    }
    let one = alone(seq, track)?;
    let t = &one.tracks[0];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (STEM_RENDER_VERSION, 48_000u32, seq.fps.num, seq.fps.den).hash(&mut h);
    serde_json::to_string(t).ok()?.hash(&mut h);
    for c in &t.clips {
        if let Some(id) = c.media_id() {
            media(&id).hash(&mut h);
        }
        if let ClipKind::Sequence { sequence_id, .. } = &c.kind {
            let inner = all_sequences.iter().find(|s| s.id == *sequence_id);
            inner.and_then(|s| serde_json::to_string(s).ok()).hash(&mut h);
        }
    }
    Some(h.finish())
}

/// The export snapshot of the stem: see [`alone`].
pub fn stem_snapshot(seq: &Sequence, all_sequences: &[Sequence], pool: &MediaPool, track: TrackId) -> Result<Option<SequenceSnapshot>, RenderError> {
    let Some(one) = alone(seq, track) else { return Ok(None) };
    let settings = klipp_core::ExportSettings::default();
    let mut snap = SequenceSnapshot::from_sequence_with_all(&one, all_sequences, pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear)?;
    // Unity at centre, whatever the person's law: the fader pans it live.
    snap.pan_law = klipp_core::PanLaw::default();
    // On past its last clip for the longest tail an effect rings: the
    // live track rings on there, after the sequence's end too.
    snap.duration_frames += (f64::from(klipp_audio::dsp::MAX_TAIL_SECS) * snap.fps).ceil() as u64;
    Ok(Some(snap))
}

/// The sound linked to video track `video`'s clips, alone (#3264): every
/// audio clip linked to one of them, on its own track, each track as a
/// stem hears it ([`alone`]: unity, centred, no automation, sends or
/// buses), the rest silent. `None` when nothing is linked. What a bounced
/// track's file carries for sound.
pub fn linked_sound_snapshot(seq: &Sequence, all_sequences: &[Sequence], pool: &MediaPool, video: TrackId) -> Result<Option<SequenceSnapshot>, RenderError> {
    let Some(v) = seq.tracks.iter().find(|t| t.id == video) else { return Ok(None) };
    let ids: std::collections::HashSet<klipp_core::ClipId> = v.clips.iter().map(|c| c.id).collect();
    let linked = |c: &klipp_timeline::Clip| c.linked_clip_id.is_some_and(|l| ids.contains(&l)) || v.clips.iter().any(|vc| vc.linked_clip_id == Some(c.id));
    let mut tracks = Vec::new();
    for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
        let clips: Vec<_> = t.clips.iter().filter(|c| linked(c)).cloned().collect();
        if clips.is_empty() {
            continue;
        }
        let Some(mut one) = alone(seq, t.id) else { continue };
        one.tracks[0].clips = clips;
        tracks.push(one.tracks.remove(0));
    }
    if tracks.is_empty() {
        return Ok(None);
    }
    let mut only = seq.clone();
    only.tracks = tracks;
    only.buses.clear();
    let settings = klipp_core::ExportSettings::default();
    let mut snap = SequenceSnapshot::from_sequence_with_all(&only, all_sequences, pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear)?;
    snap.pan_law = klipp_core::PanLaw::default();
    Ok(Some(snap))
}

/// Why the sound linked to video track `video` can't go into a bounced
/// file, or `None` if it can: a plug-in from outside Klipp on a linked
/// clip or its track.
pub fn linked_sound_refusal(seq: &Sequence, video: TrackId) -> Option<&'static str> {
    let v = seq.tracks.iter().find(|t| t.id == video)?;
    let linked = |c: &klipp_timeline::Clip| c.linked_clip_id.is_some_and(|l| v.clips.iter().any(|vc| vc.id == l)) || v.clips.iter().any(|vc| vc.linked_clip_id == Some(c.id));
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.clips.iter().any(linked))
        .find_map(|t| live_reason(seq, t.id))
        .filter(|r| *r != "not a sound track")
}

/// Render the stem of `snap` ([`stem_snapshot`]): interleaved stereo f32
/// at its rate, unclipped, from the timeline's start to the track's end, a
/// frame at a time in order. `go_on` hears the share done before each
/// frame; `None` once it says stop.
pub fn render_stem(snap: &SequenceSnapshot, go_on: &mut dyn FnMut(f32) -> bool) -> Result<Option<Vec<f32>>, RenderError> {
    let mut mix = AudioMixdown::new(snap)?;
    let mut out = Vec::new();
    let n = snap.duration_frames.max(1) as f32;
    for f in 0..snap.duration_frames {
        if !go_on(f as f32 / n) {
            return Ok(None);
        }
        out.extend(mix.get_frame_audio_unclamped(f));
    }
    Ok(Some(out))
}

/// Write `samples` to `path` as the raw stereo f32 the PCM feeder plays:
/// to a `.tmp` beside it, then renamed, so a stem is whole or not there.
pub fn write_stem(path: &std::path::Path, samples: &[f32]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        // Never written through a link: the track's folder of a shared
        // project's cache leading elsewhere.
        if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} is a link", dir.display())));
        }
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("pcm.tmp");
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    /// A stem is never written through a track folder that is a link.
    #[test]
    fn a_stem_is_never_written_through_a_link() {
        let root = std::env::temp_dir().join(format!("klipp-stem-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(root.join("stems")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("stems").join("t1")).unwrap();
        assert!(write_stem(&root.join("stems").join("t1").join("k.pcm"), &[0.0; 4]).is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing landed outside");
        write_stem(&root.join("stems").join("t2").join("k.pcm"), &[0.5; 4]).expect("a real folder is written");
        assert_eq!(std::fs::metadata(root.join("stems").join("t2").join("k.pcm")).unwrap().len(), 16);
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;
    use klipp_core::{AudioEffectInstance, MediaId, RationalTime};
    use klipp_timeline::{Clip, Track};

    fn effect(kind: &str, params: &[(&str, f32)]) -> AudioEffectInstance {
        AudioEffectInstance { effect_type: kind.into(), enabled: true, wet_dry: 1.0, params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
    }

    /// A 3 s tone, `None` without ffmpeg.
    fn tone(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let path = dir.join("tone-3s.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
            .arg(&path)
            .status()
            .ok()?;
        made.success().then_some(path)
    }

    /// The stem, played through the playback mixer's fader and pan with no
    /// inserts, is the export of the live track: its clips, their fades
    /// and effects, and its insert chain with a reverb ringing on, all in
    /// the stem. Within a 1e-5 of the export, sample for sample.
    #[test]
    fn a_stem_played_through_the_fader_is_the_live_track() {
        let dir = std::env::temp_dir().join(format!("klipp-stem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = tone(&dir) else { return };
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.duration = RationalTime { value: 72, rate: 24 };
        item.video_codec.clear();
        item.audio_codec = "pcm_s16le".into();
        item.has_audio = true;
        let media: MediaId = pool.add(item);
        let at = |f: i64| RationalTime { value: f, rate: 24 };
        let mut a = Clip::media(media, at(0), at(24), at(0), at(24));
        a.audio_effects = vec![effect("eq", &[])];
        a.fade_out_secs = 0.25;
        let b = Clip::media(media, at(30), at(54), at(24), at(48));
        let mut track = Track::new("A1", TrackKind::Audio);
        track.clips = vec![a, b];
        track.volume = 0.6;
        track.pan = 0.3;
        track.effects.audio = vec![effect("compressor", &[]), effect("reverb", &[])];
        let id = track.id;
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(24, 1);
        seq.tracks = vec![track];

        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &klipp_core::ExportSettings::default(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let mut live_mix = AudioMixdown::new(&snap).unwrap();
        let live: Vec<Vec<f32>> = (0..snap.duration_frames).map(|f| live_mix.get_frame_audio_unclamped(f)).collect();

        let stem_snap = stem_snapshot(&seq, &[], &pool, id).unwrap().expect("a stem");
        let stem = render_stem(&stem_snap, &mut |_| true).unwrap().expect("rendered");
        assert!(stem.len() >= live.iter().map(Vec::len).sum::<usize>(), "the stem runs on past the clips");
        let mut at_sample = 0;
        let mut worst = 0.0f32;
        for block in &live {
            let mut mixer = klipp_audio::AudioMixer::new(1);
            mixer.pan_law = snap.pan_law;
            mixer.tracks[0].volume = 0.6;
            mixer.tracks[0].pan = 0.3;
            let played = mixer.mix(&[(0, &stem[at_sample..at_sample + block.len()])]);
            at_sample += block.len();
            worst = played.iter().zip(block).map(|(p, l)| (p - l).abs()).fold(worst, f32::max);
        }
        eprintln!("MEASURE stem through the fader vs the live export: worst {worst:e}");
        assert!(worst < 1e-5, "{worst}");

        // Its fader's automation is live too: the stem is the same with it.
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut automated = seq.clone();
        automated.tracks[0].automation.lanes.insert(klipp_timeline::TrackParam::Volume, vec![key(0.0, 1.0), key(2.0, 0.1)]);
        let again = render_stem(&stem_snapshot(&automated, &[], &pool, id).unwrap().unwrap(), &mut |_| true).unwrap().unwrap();
        assert!(again == stem, "the automation isn't in the stem");
        assert!(live.iter().flatten().any(|s| s.abs() > 0.01), "something to hear");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stem's key follows what it is made of and nothing laid over it.
    #[test]
    fn a_stem_s_key_follows_its_sound_not_its_fader() {
        let media = MediaId::new();
        let at = |f: i64| RationalTime { value: f, rate: 24 };
        let mut track = Track::new("A1", TrackKind::Audio);
        track.clips = vec![Clip::media(media, at(0), at(24), at(0), at(24))];
        let id = track.id;
        let mut seq = Sequence::new_default();
        seq.tracks = vec![track];
        let ident = |_: &MediaId| Some(1u64);
        let key = |s: &Sequence| stem_key(s, &[], id, &ident);
        let base = key(&seq).expect("a key");
        let mut faded = seq.clone();
        faded.tracks[0].volume = 0.2;
        faded.tracks[0].pan = -1.0;
        faded.tracks[0].muted = true;
        assert_eq!(key(&faded), Some(base), "the fader, pan and mute are live");
        let mut eq = seq.clone();
        eq.tracks[0].effects.audio = vec![effect("eq", &[])];
        assert_ne!(key(&eq), Some(base), "an insert is in it");
        let mut trimmed = seq.clone();
        trimmed.tracks[0].clips[0].timeline_out = at(12);
        assert_ne!(key(&trimmed), Some(base), "a trim is in it");
        assert_ne!(stem_key(&seq, &[], id, &|_| Some(2u64)), Some(base), "a changed file is in it");
        let mut plugin = seq.clone();
        plugin.tracks[0].effects.audio = vec![effect("lv2:http://example.org/x", &[])];
        assert_eq!(stem_key(&plugin, &[], id, &ident), None, "an outside plug-in keeps it live");
        assert!(live_reason(&plugin, id).is_some());
    }
}
