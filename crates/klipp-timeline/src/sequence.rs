use klipp_core::{ClipId, Fps, MediaId, Resolution, SequenceId, Timecode, TrackId, TransitionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub fps: Fps,
    pub resolution: Resolution,
    pub tracks: Vec<Track>,
}

impl Sequence {
    pub fn new_default() -> Self {
        Self {
            id: SequenceId::new(),
            name: "Main Sequence".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            tracks: vec![
                Track {
                    id: TrackId::new(),
                    name: "Video 2".to_string(),
                    kind: TrackKind::Video,
                    clips: Vec::new(),
                    transitions: Vec::new(),
                },
                Track {
                    id: TrackId::new(),
                    name: "Video 1".to_string(),
                    kind: TrackKind::Video,
                    clips: Vec::new(),
                    transitions: Vec::new(),
                },
                Track {
                    id: TrackId::new(),
                    name: "Audio 1".to_string(),
                    kind: TrackKind::Audio,
                    clips: Vec::new(),
                    transitions: Vec::new(),
                },
            ],
        }
    }

    pub fn total_duration(&self) -> Timecode {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.last())
            .map(|c| c.timeline_out)
            .max()
            .unwrap_or(Timecode::zero())
    }

    /// Returns the primary video track (Video 1).
    pub fn video_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Video && t.name == "Video 1")
            .or_else(|| self.tracks.iter().find(|t| t.kind == TrackKind::Video))
    }

    /// Returns the primary video track (Video 1) mutably.
    pub fn video_track_mut(&mut self) -> Option<&mut Track> {
        // Find "Video 1" first, fall back to first video track
        let has_v1 = self.tracks.iter().any(|t| t.kind == TrackKind::Video && t.name == "Video 1");
        if has_v1 {
            self.tracks.iter_mut().find(|t| t.kind == TrackKind::Video && t.name == "Video 1")
        } else {
            self.tracks.iter_mut().find(|t| t.kind == TrackKind::Video)
        }
    }

    pub fn video_track2(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Video && t.name == "Video 2")
    }

    pub fn audio_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Audio)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub transitions: Vec<Transition>,
}

impl Track {
    pub fn append_clip(&mut self, media_id: MediaId, source_duration: Timecode) -> ClipId {
        let timeline_in = self
            .clips
            .last()
            .map(|c| c.timeline_out)
            .unwrap_or(Timecode::zero());
        // Rescale duration to match timeline_in's rate for consistent arithmetic
        let duration_rescaled = if timeline_in.rate != source_duration.rate && timeline_in != Timecode::zero() {
            source_duration.rescale_to(timeline_in.rate)
        } else {
            source_duration
        };
        let timeline_out = timeline_in + duration_rescaled;
        let clip_id = ClipId::new();
        self.clips.push(Clip {
            id: clip_id,
            media_id,
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: source_duration.rate },
            source_out: source_duration,
        });
        clip_id
    }

    pub fn remove_clip(&mut self, clip_id: ClipId) -> bool {
        if let Some(pos) = self.clips.iter().position(|c| c.id == clip_id) {
            self.clips.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn find_clip(&self, clip_id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == clip_id)
    }

    pub fn find_clip_mut(&mut self, clip_id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == clip_id)
    }

    pub fn clip_at_timecode(&self, tc: Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .find(|(_, c)| tc >= c.timeline_in && tc < c.timeline_out)
    }

    pub fn sort_clips(&mut self) {
        self.clips.sort_by_key(|c| c.timeline_in);
    }

    /// Remove clip and shift all downstream clips left by the removed clip's duration.
    pub fn ripple_remove(&mut self, clip_id: ClipId) -> Option<Clip> {
        let pos = self.clips.iter().position(|c| c.id == clip_id)?;
        let clip = self.clips.remove(pos);
        let gap = clip.timeline_out - clip.timeline_in;
        for c in &mut self.clips[pos..] {
            c.timeline_in -= gap;
            c.timeline_out -= gap;
        }
        Some(clip)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub duration: Timecode,
    pub clip_a_id: ClipId,
    pub clip_b_id: ClipId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionKind {
    CrossDissolve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub media_id: MediaId,
    pub timeline_in: Timecode,
    pub timeline_out: Timecode,
    pub source_in: Timecode,
    pub source_out: Timecode,
}

impl Clip {
    pub fn duration(&self) -> Timecode {
        self.timeline_out - self.timeline_in
    }

    pub fn source_duration(&self) -> Timecode {
        self.source_out - self.source_in
    }
}
