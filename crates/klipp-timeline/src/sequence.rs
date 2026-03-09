use crate::marker::Marker;
use klipp_core::{ClipId, Fps, MediaId, Resolution, SequenceId, Timecode, TrackId, TransitionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub fps: Fps,
    pub resolution: Resolution,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub in_point: Option<Timecode>,
    #[serde(default)]
    pub out_point: Option<Timecode>,
    #[serde(default)]
    pub markers: Vec<Marker>,
}

impl Sequence {
    pub fn new_default() -> Self {
        Self {
            id: SequenceId::new(),
            name: "Main Sequence".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            tracks: vec![
                Track::new("Video 2", TrackKind::Video),
                Track::new("Video 1", TrackKind::Video),
                Track::new("Audio 1", TrackKind::Audio),
            ],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
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

    /// Add a new video track. Inserts before the first Audio track to maintain V/A ordering.
    pub fn add_video_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Video);
        let id = track.id;
        let insert_pos = self.tracks.iter()
            .position(|t| t.kind == TrackKind::Audio)
            .unwrap_or(self.tracks.len());
        self.tracks.insert(insert_pos, track);
        id
    }

    /// Add a new audio track at the end.
    pub fn add_audio_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Audio);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Remove a track. Returns Err if track has clips.
    pub fn remove_track(&mut self, track_id: TrackId) -> Result<(), &'static str> {
        let pos = self.tracks.iter().position(|t| t.id == track_id)
            .ok_or("track not found")?;
        if !self.tracks[pos].clips.is_empty() {
            return Err("track has clips");
        }
        self.tracks.remove(pos);
        Ok(())
    }

    /// Remove a track including all its clips.
    pub fn remove_track_with_clips(&mut self, track_id: TrackId) -> Result<(), &'static str> {
        let pos = self.tracks.iter().position(|t| t.id == track_id)
            .ok_or("track not found")?;
        self.tracks.remove(pos);
        Ok(())
    }

    /// Generate the next auto-increment name for a track kind (e.g. "Video 3", "Audio 2").
    pub fn next_track_name(&self, kind: TrackKind) -> String {
        let prefix = match kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
        };
        let count = self.tracks.iter().filter(|t| t.kind == kind).count();
        format!("{} {}", prefix, count + 1)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub transitions: Vec<Transition>,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub pan: f32,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_true() -> bool { true }

impl Track {
    pub fn new(name: &str, kind: TrackKind) -> Self {
        Self {
            id: TrackId::new(),
            name: name.to_string(),
            kind,
            clips: Vec::new(),
            transitions: Vec::new(),
            opacity: 1.0,
            volume: 1.0,
            muted: false,
            solo: false,
            visible: true,
            pan: 0.0,
        }
    }

    pub fn append_clip(&mut self, media_id: MediaId, source_duration: Timecode) -> ClipId {
        let timeline_in = self
            .clips
            .last()
            .map(|c| c.timeline_out)
            .unwrap_or(Timecode { value: 0, rate: source_duration.rate });
        // Rescale to source rate for frame-accurate timeline arithmetic
        let timeline_in = if timeline_in.rate != source_duration.rate {
            timeline_in.rescale_to(source_duration.rate)
        } else {
            timeline_in
        };
        let timeline_out = timeline_in + source_duration;
        let clip_id = ClipId::new();
        self.clips.push(Clip {
            id: clip_id,
            media_id,
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: source_duration.rate },
            source_out: source_duration,
            color_grade: ClipColorGrade::default(),
            effects: Vec::new(),
            linked_clip_id: None,
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

    /// Ripple trim left edge: adjust source_in/timeline_in and shift downstream clips.
    pub fn ripple_trim_left(&mut self, clip_id: ClipId, new_source_in: Timecode, new_timeline_in: Timecode) {
        let pos = match self.clips.iter().position(|c| c.id == clip_id) {
            Some(p) => p,
            None => return,
        };
        let old_timeline_in = self.clips[pos].timeline_in;
        let delta = new_timeline_in - old_timeline_in;
        self.clips[pos].source_in = new_source_in;
        self.clips[pos].timeline_in = new_timeline_in;
        // Shift this clip and all downstream clips by delta
        for c in &mut self.clips[pos..] {
            if c.id != clip_id {
                c.timeline_in += delta;
                c.timeline_out += delta;
            }
        }
    }

    /// Ripple trim right edge: adjust source_out/timeline_out and shift downstream clips.
    pub fn ripple_trim_right(&mut self, clip_id: ClipId, new_source_out: Timecode, new_timeline_out: Timecode) {
        let pos = match self.clips.iter().position(|c| c.id == clip_id) {
            Some(p) => p,
            None => return,
        };
        let old_timeline_out = self.clips[pos].timeline_out;
        let delta = new_timeline_out - old_timeline_out;
        self.clips[pos].source_out = new_source_out;
        self.clips[pos].timeline_out = new_timeline_out;
        // Shift all clips after this one by delta
        for c in &mut self.clips[pos + 1..] {
            c.timeline_in += delta;
            c.timeline_out += delta;
        }
    }

    /// Split a clip at the given timeline position into two clips.
    /// Returns (clip_a, clip_b) if successful.
    pub fn blade_at(&mut self, clip_id: ClipId, cut_at: Timecode) -> Option<(Clip, Clip)> {
        let pos = self.clips.iter().position(|c| c.id == clip_id)?;
        let clip = self.clips[pos].clone();

        // Validate cut position
        let one_frame = Timecode { value: 1, rate: clip.timeline_in.rate };
        if cut_at <= clip.timeline_in + one_frame || cut_at >= clip.timeline_out - one_frame {
            return None;
        }

        let offset = cut_at - clip.timeline_in;
        let clip_a = Clip {
            id: ClipId::new(),
            media_id: clip.media_id,
            timeline_in: clip.timeline_in,
            timeline_out: cut_at,
            source_in: clip.source_in,
            source_out: clip.source_in + offset,
            color_grade: clip.color_grade,
            effects: clip.effects.clone(),
            linked_clip_id: clip.linked_clip_id,
        };
        let clip_b = Clip {
            id: ClipId::new(),
            media_id: clip.media_id,
            timeline_in: cut_at,
            timeline_out: clip.timeline_out,
            source_in: clip.source_in + offset,
            source_out: clip.source_out,
            color_grade: clip.color_grade,
            effects: clip.effects.clone(),
            linked_clip_id: clip.linked_clip_id,
        };

        self.clips.remove(pos);
        self.clips.insert(pos, clip_b.clone());
        self.clips.insert(pos, clip_a.clone());

        Some((clip_a, clip_b))
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

/// Per-clip color grade. All fields default to identity (no change).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClipColorGrade {
    pub enabled: bool,
    /// Exposure in stops: -5.0 to +5.0, default 0.0
    pub exposure: f32,
    /// Contrast: -1.0 to +1.0, default 0.0
    pub contrast: f32,
    /// Saturation: 0.0 to 2.0, default 1.0
    pub saturation: f32,
    /// Temperature offset: -100.0 to +100.0, default 0.0
    pub temperature: f32,
    /// Tint offset: -100.0 to +100.0, default 0.0
    pub tint: f32,
}

impl Default for ClipColorGrade {
    fn default() -> Self {
        Self {
            enabled: false,
            exposure: 0.0,
            contrast: 0.0,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub media_id: MediaId,
    pub timeline_in: Timecode,
    pub timeline_out: Timecode,
    pub source_in: Timecode,
    pub source_out: Timecode,
    #[serde(default)]
    pub color_grade: ClipColorGrade,
    #[serde(default)]
    pub effects: Vec<klipp_plugin::AppliedEffect>,
    #[serde(default)]
    pub linked_clip_id: Option<ClipId>,
}

impl Clip {
    pub fn duration(&self) -> Timecode {
        self.timeline_out - self.timeline_in
    }

    pub fn source_duration(&self) -> Timecode {
        self.source_out - self.source_in
    }
}
