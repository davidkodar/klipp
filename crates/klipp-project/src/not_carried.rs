//! What a cuts-only interchange file does not carry (#2520): the EDL and
//! the Kdenlive project each name it, one line a thing, so an export that
//! leaves something out says so, as the FCPXML and OTIO exports do.

use crate::fcpxml::describe;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// How a format differs from the timeline, beyond what neither carries
/// (titles and generators, speed, grades and effects, sound levels,
/// markers, caption tracks).
pub(crate) struct Format {
    /// Its name, for the lines.
    pub name: &'static str,
    /// Whether its transitions come across.
    pub transitions: bool,
    /// How many picture and sound tracks it keeps apart, when it has a
    /// limit: an EDL has one picture track and names four sound channels.
    pub video_tracks: Option<usize>,
    pub audio_tracks: Option<usize>,
    /// Whether a clip switched off is left out (as against written as on).
    pub drops_disabled: bool,
}

/// Each thing `seq` holds that `format` does not carry.
pub(crate) fn not_carried(seq: &Sequence, pool: &MediaPool, format: &Format) -> Vec<String> {
    let mut out = Vec::new();
    let name = format.name;
    let (mut video, mut audio) = (0usize, 0usize);
    for track in &seq.tracks {
        let clips: Vec<&Clip> = track.clips.iter().filter(|c| !c.kind.is_gap()).collect();
        match track.kind {
            TrackKind::Video => video += 1,
            TrackKind::Audio => audio += 1,
            other => {
                if !clips.is_empty() {
                    out.push(format!("Track '{}': {} {other:?} clip(s) — {name} has no captions; use subtitle export for them", track.name, clips.len()));
                }
                continue;
            }
        }
        if let (TrackKind::Video, Some(max)) = (track.kind, format.video_tracks) {
            if video > max && !clips.is_empty() {
                out.push(format!("Track '{}': {name} has one picture track, so its clips are events among the first track's, not over them", track.name));
            }
        }
        if let (TrackKind::Audio, Some(max)) = (track.kind, format.audio_tracks) {
            if audio > max && !clips.is_empty() {
                out.push(format!("Track '{}': {name} names {max} sound channels, so its clips go out with no channel of their own", track.name));
            }
        }
        if !format.transitions {
            for t in &track.transitions {
                out.push(format!("Track '{}': a {:?} transition of {:.3} s — the file has a cut there", track.name, t.kind, t.duration.to_secs_f64()));
            }
        }
        if track.muted && track.kind == TrackKind::Audio {
            out.push(format!("Track '{}' is muted — its clips are written as playing", track.name));
        }
        if !track.visible && track.kind == TrackKind::Video {
            out.push(format!("Track '{}' is hidden — its clips are written as showing", track.name));
        }
        for clip in clips {
            let at = clip.timeline_in.to_secs_f64();
            let what = describe(clip, pool);
            if !matches!(clip.kind, ClipKind::Media { .. }) {
                out.push(format!("Track '{}': {what} at {at:.3} s — {name} cannot carry it; its length is kept empty", track.name));
                continue;
            }
            if !clip.enabled {
                out.push(format!("Track '{}': {what} at {at:.3} s is switched off — {}", track.name, if format.drops_disabled { "left out" } else { "written as on" }));
            }
            let mut left = Vec::new();
            if !clip.speed_keyframes.is_empty() || (clip.speed - 1.0).abs() > 1e-9 {
                left.push("its speed change (played at normal speed from its source in)");
            }
            if track.kind == TrackKind::Video && (!clip.grade_is_identity() || clip.has_plugin_entries() || clip.has_effect_entries()) {
                left.push("its grade and picture effects");
            }
            let sounds = track.kind == TrackKind::Audio || clip.linked_clip_id.is_none();
            if sounds && ((clip.volume - 1.0).abs() > 1e-6 || !clip.volume_keyframes.is_empty()) {
                left.push("its level");
            }
            if sounds && (clip.pan.abs() > 1e-6 || !clip.pan_keyframes.is_empty()) {
                left.push("its pan");
            }
            if clip.fade_in_secs > 0.0 || clip.fade_out_secs > 0.0 {
                left.push("its fades");
            }
            if !clip.audio_effects.is_empty() {
                left.push("its sound effects");
            }
            if !clip.clip_markers.is_empty() {
                left.push("its markers");
            }
            if !left.is_empty() {
                out.push(format!("Track '{}': {what} at {at:.3} s — {}", track.name, left.join(", ")));
            }
        }
    }
    if !seq.markers.is_empty() {
        out.push(format!("{} sequence marker(s) — {name} does not carry markers", seq.markers.len()));
    }
    out
}
