use crate::clip_kind::ClipKind;
use crate::marker::Marker;
use klipp_core::{ClipId, Fps, GroupId, MediaId, RationalTime, Resolution, SequenceId, Timecode, TrackId, TransitionId};
use serde::{Deserialize, Serialize};

pub use klipp_core::types::FadeCurve;

// ═══════════════════════════════════════════════════════════════════
// Magnetic Timeline Types
// ═══════════════════════════════════════════════════════════════════

/// Magnetic role of a track — determines how clips are positioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TrackRole {
    /// Traditional free placement. No magnetic behaviour. (default)
    #[default]
    Free,
    /// The primary storyline. Always gapless. Exactly one per Sequence.
    Primary,
    /// Connected lane. Individual clips may have magnetic_anchor set.
    Connected,
    /// Connected Storyline. The whole track is anchored as one unit.
    ConnectedStoryline,
}

/// Anchor alignment mode for connected clips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorAlignment {
    /// clip.timeline_in = anchor.timeline_in + offset (most common)
    StartAligned,
    /// clip.timeline_out = anchor.timeline_in + offset
    EndAligned,
}

/// Connection from a clip (or storyline) to an anchor point on the primary storyline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagneticAnchor {
    /// Which primary clip this is anchored to.
    pub anchor_clip_id: ClipId,
    /// Time offset from the anchor clip's timeline_in.
    pub offset: RationalTime,
    /// Whether the offset is measured from the start or end of the connected clip.
    pub alignment: AnchorAlignment,
}

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
    /// Remembered playhead position for multi-sequence tab switching.
    #[serde(default)]
    pub last_playhead: Option<RationalTime>,
}

impl Sequence {
    pub fn new_default() -> Self {
        Self {
            id: SequenceId::new(),
            name: "Main Sequence".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            tracks: {
                let mut tracks = vec![
                    Track::new("Video 2", TrackKind::Video),
                    Track::new("Video 1", TrackKind::Video),
                    Track::new("Audio 1", TrackKind::Audio),
                ];
                tracks[1].magnetic_role = TrackRole::Primary;
                tracks[2].magnetic_role = TrackRole::Connected;
                tracks
            },
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
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

    /// Add a new text track. Inserts before the first Audio track (same region as video).
    pub fn add_text_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Text);
        let id = track.id;
        let insert_pos = self.tracks.iter()
            .position(|t| t.kind == TrackKind::Audio)
            .unwrap_or(self.tracks.len());
        self.tracks.insert(insert_pos, track);
        id
    }

    /// Add a new subtitle track at the end (after audio tracks).
    pub fn add_subtitle_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Subtitle);
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

    // ═══════════════════════════════════════════════════════════════════
    // Magnetic Timeline Helpers
    // ═══════════════════════════════════════════════════════════════════

    /// Find the index of the Primary track. Returns None if no primary track set.
    pub fn primary_track_index(&self) -> Option<usize> {
        self.tracks.iter().position(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find the single Primary track.
    pub fn primary_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find the single Primary track mutably.
    pub fn primary_track_mut(&mut self) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find a clip by id across all tracks. Returns (track_index, clip_index).
    pub fn find_clip(&self, id: ClipId) -> Option<(usize, usize)> {
        for (ti, track) in self.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                if clip.id == id {
                    return Some((ti, ci));
                }
            }
        }
        None
    }

    /// Set a clip's absolute position directly (used by undo/redo restore).
    pub fn set_clip_position(&mut self, id: ClipId, new_in: RationalTime, new_out: RationalTime) {
        if let Some((ti, ci)) = self.find_clip(id) {
            self.tracks[ti].clips[ci].timeline_in = new_in;
            self.tracks[ti].clips[ci].timeline_out = new_out;
        }
    }

    /// Iterator over all clips in all tracks.
    pub fn all_clips(&self) -> impl Iterator<Item = &Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter())
    }

    /// Mutable iterator over all clips in all tracks.
    pub fn all_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut())
    }

    /// Enforce primary invariant: primary track has no gaps.
    /// Call this after every magnetic edit as a debug assertion.
    #[cfg(debug_assertions)]
    pub fn assert_primary_gapless(&self) {
        if let Some(primary) = self.primary_track() {
            let mut expected = RationalTime::ZERO;
            for clip in &primary.clips {
                let clip_in = clip.timeline_in.rescale_to(expected.rate);
                assert_eq!(
                    clip_in, expected,
                    "Primary storyline gap detected before clip {:?} (expected {:?}, got {:?})",
                    clip.id, expected, clip.timeline_in
                );
                expected = clip.timeline_out.rescale_to(expected.rate);
            }
        }
    }

    /// Generate the next auto-increment name for a track kind (e.g. "Video 3", "Audio 2").
    pub fn next_track_name(&self, kind: TrackKind) -> String {
        let prefix = match kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
            TrackKind::Text => "Text",
            TrackKind::Subtitle => "Subtitle",
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
    #[serde(default)]
    pub locked: bool,
    /// Magnetic role of this track. Defaults to Free — backward compatible.
    #[serde(default)]
    pub magnetic_role: TrackRole,
    /// If this track is a Connected Storyline, its anchor on the primary.
    #[serde(default)]
    pub storyline_anchor: Option<MagneticAnchor>,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_clip_volume() -> f32 { 1.0 }

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
            locked: false,
            magnetic_role: TrackRole::Free,
            storyline_anchor: None,
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
            kind: ClipKind::Media { media_id },
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: source_duration.rate },
            source_out: source_duration,
            node_graph: klipp_nodes::NodeGraph::default(),
            effects: Vec::new(),
            builtin_effects: Vec::new(),
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
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
            kind: clip.kind.clone(),
            timeline_in: clip.timeline_in,
            timeline_out: cut_at,
            source_in: clip.source_in,
            source_out: clip.source_in + offset,
            node_graph: clip.node_graph.clone(),
            effects: clip.effects.clone(),
            builtin_effects: clip.builtin_effects.clone(),
            linked_clip_id: clip.linked_clip_id,
            group_id: clip.group_id,
            magnetic_anchor: clip.magnetic_anchor.clone(),
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
        };
        let clip_b = Clip {
            id: ClipId::new(),
            kind: clip.kind.clone(),
            timeline_in: cut_at,
            timeline_out: clip.timeline_out,
            source_in: clip.source_in + offset,
            source_out: clip.source_out,
            node_graph: clip.node_graph.clone(),
            effects: clip.effects.clone(),
            builtin_effects: clip.builtin_effects.clone(),
            linked_clip_id: clip.linked_clip_id,
            group_id: clip.group_id,
            magnetic_anchor: None, // right half gets no anchor
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
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
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    DipToBlack,
    DipToWhite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Text,
    Subtitle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub kind: ClipKind,
    pub timeline_in: Timecode,
    pub timeline_out: Timecode,
    pub source_in: Timecode,
    pub source_out: Timecode,
    #[serde(default)]
    pub node_graph: klipp_nodes::NodeGraph,
    #[serde(default)]
    pub effects: Vec<klipp_plugin::AppliedEffect>,
    #[serde(default)]
    pub builtin_effects: Vec<klipp_effects::BuiltinEffect>,
    #[serde(default)]
    pub linked_clip_id: Option<ClipId>,
    #[serde(default)]
    pub group_id: Option<GroupId>,
    /// If Some, this clip is connected to an anchor on the primary storyline.
    #[serde(default)]
    pub magnetic_anchor: Option<MagneticAnchor>,
    #[serde(default = "default_clip_volume")]
    pub gain: f32,
    #[serde(default = "default_clip_volume")]
    pub volume: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub fade_in_secs: f32,
    #[serde(default)]
    pub fade_out_secs: f32,
    #[serde(default)]
    pub fade_in_curve: FadeCurve,
    #[serde(default)]
    pub fade_out_curve: FadeCurve,
}

impl Clip {
    pub fn media_id(&self) -> Option<MediaId> {
        match &self.kind {
            ClipKind::Media { media_id } => Some(*media_id),
            _ => None,
        }
    }

    pub fn title_data(&self) -> Option<&crate::clip_kind::TitleData> {
        match &self.kind {
            ClipKind::Title(data) => Some(data),
            _ => None,
        }
    }

    pub fn duration(&self) -> Timecode {
        self.timeline_out - self.timeline_in
    }

    pub fn source_duration(&self) -> Timecode {
        self.source_out - self.source_in
    }

    /// Create a gap placeholder clip with the given timeline extent.
    /// Rescales timeline_out to match timeline_in's rate if they differ.
    pub fn gap(timeline_in: Timecode, timeline_out: Timecode) -> Self {
        let timeline_out = if timeline_out.rate != timeline_in.rate {
            timeline_out.rescale_to(timeline_in.rate)
        } else {
            timeline_out
        };
        Self {
            id: ClipId::new(),
            kind: ClipKind::Gap,
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: timeline_in.rate },
            source_out: timeline_out - timeline_in,
            node_graph: klipp_nodes::NodeGraph::default(),
            effects: Vec::new(),
            builtin_effects: Vec::new(),
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
        }
    }

    /// Create a media clip with the given source and timeline extents.
    pub fn media(
        media_id: MediaId,
        timeline_in: Timecode,
        timeline_out: Timecode,
        source_in: Timecode,
        source_out: Timecode,
    ) -> Self {
        Self {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in,
            timeline_out,
            source_in,
            source_out,
            node_graph: klipp_nodes::NodeGraph::default(),
            effects: Vec::new(),
            builtin_effects: Vec::new(),
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
        }
    }
}
