use klipp_core::{ClipId, Fps, MediaId, Resolution, SequenceId, Timecode, TrackId};
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
            tracks: vec![Track {
                id: TrackId::new(),
                name: "Video 1".to_string(),
                kind: TrackKind::Video,
                clips: Vec::new(),
            }],
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

    pub fn video_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Video)
    }

    pub fn video_track_mut(&mut self) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.kind == TrackKind::Video)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn append_clip(&mut self, media_id: MediaId, source_duration: Timecode) -> ClipId {
        let timeline_in = self
            .clips
            .last()
            .map(|c| c.timeline_out)
            .unwrap_or(Timecode::zero());
        let timeline_out = timeline_in + source_duration;
        let clip_id = ClipId::new();
        self.clips.push(Clip {
            id: clip_id,
            media_id,
            timeline_in,
            timeline_out,
            source_in: Timecode::zero(),
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
        Timecode(self.timeline_out.0 - self.timeline_in.0)
    }

    pub fn source_duration(&self) -> Timecode {
        Timecode(self.source_out.0 - self.source_in.0)
    }
}
