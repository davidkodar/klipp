use crate::clip_kind::ClipKind;
use crate::marker::Marker;
use klipp_core::{AudioEffectInstance, ClipId, Fps, GroupId, MarkerColor, MediaId, ModifierStack, ParamId, RationalTime, Resolution, SequenceId, Timecode, TrackId, TransitionId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use klipp_core::types::{ChannelConfig, FadeCurve};

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

pub use klipp_core::ClipRole;

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
                    Track::new("Video 1", TrackKind::Video),
                    Track::new("Audio 1", TrackKind::Audio),
                ];
                tracks[0].magnetic_role = TrackRole::Primary;
                tracks[1].magnetic_role = TrackRole::Connected;
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

    /// Renumber video and audio tracks based on visual position.
    /// Video tracks are numbered bottom-up (last video in array = "Video 1").
    /// Audio tracks are numbered top-down (first audio = "Audio 1").
    /// Text/Subtitle tracks are numbered top-down.
    /// Only renames tracks that have default-pattern names ("Video N", "Audio N", etc.).
    /// User-customized names (e.g., "B-Roll", "Interview") are left unchanged.
    pub fn renumber_tracks(&mut self) {
        // Video: bottom-up (reverse order among video tracks)
        let video_indices: Vec<usize> = self.tracks.iter().enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, _)| i)
            .collect();
        for (num_from_bottom, &idx) in video_indices.iter().rev().enumerate() {
            let new_name = format!("Video {}", num_from_bottom + 1);
            let track = &mut self.tracks[idx];
            // Only rename if it has a default-pattern name
            if track.name.starts_with("Video ") || track.name.starts_with("V") {
                track.name = new_name;
            }
        }
        // Audio: top-down
        let mut audio_num = 1u32;
        for track in &mut self.tracks {
            if track.kind == TrackKind::Audio {
                if track.name.starts_with("Audio ") || track.name.starts_with("A") {
                    track.name = format!("Audio {}", audio_num);
                }
                audio_num += 1;
            }
        }
        // Text: top-down
        let mut text_num = 1u32;
        for track in &mut self.tracks {
            if track.kind == TrackKind::Text {
                if track.name.starts_with("Text ") {
                    track.name = format!("Text {}", text_num);
                }
                text_num += 1;
            }
        }
    }

    /// Returns all overlay video/text/subtitle tracks — every track that composites
    /// over the primary video track. The primary is identified by `primary_track_idx`
    /// (the engine's `current_track_index`). All other unmuted video/text/subtitle
    /// tracks are returned as overlays.
    pub fn overlay_video_tracks(&self, primary_track_idx: usize) -> Vec<(usize, &Track)> {
        self.tracks.iter().enumerate()
            .filter(|(i, t)| {
                !t.muted && match t.kind {
                    TrackKind::Text | TrackKind::Subtitle => true,
                    TrackKind::Video => *i != primary_track_idx,
                    _ => false,
                }
            })
            .collect()
    }

    pub fn audio_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Audio)
    }

    /// Add a new video track at the top of the video track group (above all existing
    /// video tracks). This makes it the highest-numbered overlay. V1 stays at the bottom.
    /// Renumbers all video tracks after insertion.
    pub fn add_video_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Video);
        let id = track.id;
        // Insert at index 0 — above everything, new track becomes highest V number
        self.tracks.insert(0, track);
        self.renumber_tracks();
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
    /// Also clears linked_clip_id on clips in other tracks that pointed to removed clips.
    pub fn remove_track_with_clips(&mut self, track_id: TrackId) -> Result<(), &'static str> {
        let pos = self.tracks.iter().position(|t| t.id == track_id)
            .ok_or("track not found")?;
        // Collect IDs of clips being removed so we can unlink them
        let removed_ids: Vec<ClipId> = self.tracks[pos].clips.iter().map(|c| c.id).collect();
        self.tracks.remove(pos);
        self.purge_dangling_linked_clip_ids(&removed_ids);
        Ok(())
    }

    /// Clear `linked_clip_id` on every clip whose link target is in
    /// `removed`. Use this from any code path that removes clips
    /// permanently (track removal, ripple_delete) so other clips on
    /// surviving tracks don't keep dangling references.
    ///
    /// Closes CODE_AUDIT T-3. The asymmetric-link case (third-party
    /// clip C points at deleted clip A while A's own linked_clip_id
    /// points at unrelated clip B) used to leak dangling refs from
    /// ripple_delete. Centralising the sweep here means every clip
    /// removal path sees the same cleanup.
    pub fn purge_dangling_linked_clip_ids(&mut self, removed: &[ClipId]) {
        if removed.is_empty() {
            return;
        }
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if let Some(linked) = clip.linked_clip_id {
                    if removed.contains(&linked) {
                        clip.linked_clip_id = None;
                    }
                }
            }
        }
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
        // Find the highest existing number for this kind to avoid name collisions
        // after track deletion (e.g., deleting "Video 2" from [1,2,3] then adding → "Video 4").
        let highest = self.tracks.iter()
            .filter(|t| t.kind == kind)
            .filter_map(|t| t.name.strip_prefix(prefix).and_then(|s| s.trim().parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        format!("{} {}", prefix, highest + 1)
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
    /// Custom track height in pixels. None = use default for track kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_height: Option<f32>,
    /// Stored height before collapse, so double-click can restore it.
    #[serde(skip)]
    pub pre_collapse_height: Option<f32>,
    /// Source file (for subtitle tracks imported from SRT/VTT/ASS/SSA).
    /// Set when a track is created via subtitle file import; displayed in
    /// the subtitle editor as a "Source: foo.srt" label.
    #[serde(default)]
    pub source_file: Option<std::path::PathBuf>,
    /// True when this audio track is "record-armed" — the Record button in
    /// the transport bar will capture to this track on stop.
    /// Only one track is armed at a time in the MVP (see #60 tier 2b).
    #[serde(default)]
    pub record_armed: bool,
    /// Per-track expandable automation lane state. `None` = collapsed (the
    /// inline rubber band on each clip is the only surface). `Some(param)`
    /// = the lane is visible below the clip area, showing the named
    /// param's curve at full lane height. Survives project reload via
    /// serde; defaults to `None` for back-compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_visible: Option<klipp_core::AudioParam>,
    /// User-overridden lane height in pixels. `None` falls back to the
    /// `AUTOMATION_LANE_HEIGHT` default (120 px) so projects saved
    /// before drag-resize landed still render at the original height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_height: Option<f32>,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_clip_volume() -> f32 { 1.0 }
fn default_speed() -> f64 { 1.0 }
fn default_enabled() -> bool { true }

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
            custom_height: None,
            pre_collapse_height: None,
            source_file: None,
            record_armed: false,
            automation_lane_visible: None,
            automation_lane_height: None,
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
            builtin_effects_enabled: Vec::new(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: std::collections::HashMap::new(),
            role: ClipRole::default(),
            param_modifiers: HashMap::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
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

    /// Returns the clip whose OUT edge is at or before `tc`, maximised.
    /// When the playhead sits inside a clip, returns that clip. When it sits
    /// on a gap, returns the clip immediately before. Used by "trim OUT to
    /// playhead" (Q) to pick its target without requiring a selection.
    pub fn clip_with_out_at_or_before(&self, tc: &Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| &c.timeline_out <= tc || (&c.timeline_in <= tc && tc < &c.timeline_out))
            .max_by_key(|(_, c)| c.timeline_out)
    }

    /// Returns the clip whose IN edge is at or after `tc`, minimised.
    /// When the playhead sits inside a clip, returns that clip. When it sits
    /// on a gap, returns the clip immediately after. Used by "trim IN to
    /// playhead" (W) to pick its target without requiring a selection.
    pub fn clip_with_in_at_or_after(&self, tc: &Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| &c.timeline_in >= tc || (&c.timeline_in <= tc && tc < &c.timeline_out))
            .min_by_key(|(_, c)| c.timeline_in)
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
        let cut_secs = offset.to_secs_f64();

        // Split keyframes at the cut point
        let split_kfs = |kfs: &[klipp_core::Keyframe]| -> (Vec<klipp_core::Keyframe>, Vec<klipp_core::Keyframe>) {
            let mut before = Vec::new();
            let mut after = Vec::new();
            for kf in kfs {
                if kf.time_secs < cut_secs {
                    before.push(kf.clone());
                } else {
                    let mut shifted = kf.clone();
                    shifted.time_secs -= cut_secs;
                    after.push(shifted);
                }
            }
            (before, after)
        };
        let (vol_a, vol_b) = split_kfs(&clip.volume_keyframes);
        let (pan_a, pan_b) = split_kfs(&clip.pan_keyframes);
        let (gain_a, gain_b) = split_kfs(&clip.gain_keyframes);
        let (opacity_a, opacity_b) = split_kfs(&clip.opacity_keyframes);
        let (pos_x_a, pos_x_b) = split_kfs(&clip.transform_pos_x_keyframes);
        let (pos_y_a, pos_y_b) = split_kfs(&clip.transform_pos_y_keyframes);
        let (scale_a, scale_b) = split_kfs(&clip.transform_scale_keyframes);
        let (rot_a, rot_b) = split_kfs(&clip.transform_rotation_keyframes);
        let (mfeath_a, mfeath_b) = split_kfs(&clip.matte_feather_keyframes);
        let (mchoke_a, mchoke_b) = split_kfs(&clip.matte_choke_keyframes);

        // Split effect keyframes at the cut point
        let mut effect_kfs_a = std::collections::HashMap::new();
        let mut effect_kfs_b = std::collections::HashMap::new();
        for (key, kfs) in &clip.effect_keyframes {
            let (a, b) = split_kfs(kfs);
            if !a.is_empty() {
                effect_kfs_a.insert(key.clone(), a);
            }
            if !b.is_empty() {
                effect_kfs_b.insert(key.clone(), b);
            }
        }

        // Split audio-effect keyframes at the cut point
        let mut audio_effect_kfs_a = std::collections::HashMap::new();
        let mut audio_effect_kfs_b = std::collections::HashMap::new();
        for (key, kfs) in &clip.audio_effect_keyframes {
            let (a, b) = split_kfs(kfs);
            if !a.is_empty() {
                audio_effect_kfs_a.insert(key.clone(), a);
            }
            if !b.is_empty() {
                audio_effect_kfs_b.insert(key.clone(), b);
            }
        }

        // Split clip markers at the cut point
        let mut markers_a = Vec::new();
        let mut markers_b = Vec::new();
        for m in &clip.clip_markers {
            if m.time_secs < cut_secs {
                markers_a.push(m.clone());
            } else {
                let mut shifted = m.clone();
                shifted.time_secs -= cut_secs;
                markers_b.push(shifted);
            }
        }

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
            builtin_effects_enabled: clip.builtin_effects_enabled.clone(),
            linked_clip_id: clip.linked_clip_id,
            linked_title_asset_id: clip.linked_title_asset_id,
            group_id: clip.group_id,
            magnetic_anchor: clip.magnetic_anchor.clone(),
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            channel_config: clip.channel_config,
            volume_keyframes: vol_a,
            pan_keyframes: pan_a,
            gain_keyframes: gain_a,
            opacity_keyframes: opacity_a,
            transform_pos_x_keyframes: pos_x_a,
            transform_pos_y_keyframes: pos_y_a,
            transform_scale_keyframes: scale_a,
            transform_rotation_keyframes: rot_a,
            matte_feather_keyframes: mfeath_a,
            matte_choke_keyframes: mchoke_a,
            clip_markers: markers_a,
            effect_keyframes: effect_kfs_a,
            role: clip.role,
            param_modifiers: clip.param_modifiers.clone(),
            speed: clip.speed,
            speed_keyframes: clip.speed_keyframes.clone(),
            source_rotation: clip.source_rotation,
            enabled: clip.enabled,
            solo: clip.solo,
            blend_mode: clip.blend_mode,
            matte_source: clip.matte_source.clone(),
            matte_stencil: clip.matte_stencil.clone(),
            consumed_as_matte: clip.consumed_as_matte,
            transform_link: None,
            grade_versions: clip.grade_versions.clone(),
            active_grade_version: clip.active_grade_version,
            multicam_group: clip.multicam_group,
            multicam_angle: clip.multicam_angle,
            multicam_cuts: clip.multicam_cuts.clone(),
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: audio_effect_kfs_a,
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
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
            builtin_effects_enabled: clip.builtin_effects_enabled.clone(),
            linked_clip_id: clip.linked_clip_id,
            linked_title_asset_id: clip.linked_title_asset_id,
            group_id: clip.group_id,
            magnetic_anchor: None, // right half gets no anchor
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            channel_config: clip.channel_config,
            volume_keyframes: vol_b,
            pan_keyframes: pan_b,
            gain_keyframes: gain_b,
            opacity_keyframes: opacity_b,
            transform_pos_x_keyframes: pos_x_b,
            transform_pos_y_keyframes: pos_y_b,
            transform_scale_keyframes: scale_b,
            transform_rotation_keyframes: rot_b,
            matte_feather_keyframes: mfeath_b,
            matte_choke_keyframes: mchoke_b,
            clip_markers: markers_b,
            effect_keyframes: effect_kfs_b,
            role: clip.role,
            param_modifiers: clip.param_modifiers.clone(),
            speed: clip.speed,
            speed_keyframes: clip.speed_keyframes.clone(),
            source_rotation: clip.source_rotation,
            enabled: clip.enabled,
            solo: clip.solo,
            blend_mode: clip.blend_mode,
            matte_source: clip.matte_source.clone(),
            matte_stencil: clip.matte_stencil.clone(),
            consumed_as_matte: clip.consumed_as_matte,
            transform_link: None,
            grade_versions: clip.grade_versions.clone(),
            active_grade_version: clip.active_grade_version,
            multicam_group: clip.multicam_group,
            multicam_angle: clip.multicam_angle,
            multicam_cuts: clip.multicam_cuts.clone(),
            audio_effects: clip.audio_effects,
            audio_effect_keyframes: audio_effect_kfs_b,
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
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
    /// Per-kind tunable parameters (feather, colour, rotations, etc.).
    /// Missing from older project files — defaults are kind-neutral.
    #[serde(default)]
    pub user_params: klipp_core::TransitionUserParams,
    /// Where the transition sits relative to the cut between its two clips.
    /// Defaults to centred for backwards compatibility with older projects.
    #[serde(default)]
    pub alignment: klipp_core::TransitionAlignment,
}

// TransitionKind is defined in klipp-core and re-exported here for backward compatibility.
pub use klipp_core::TransitionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Text,
    Subtitle,
}

/// A named snapshot of a clip's node graph (color grade).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeVersion {
    pub name: String,
    pub node_graph: klipp_nodes::NodeGraph,
}

/// A project-level reusable colour grade. The user saves the current
/// clip's node graph as a named "Look" and applies it to other clips
/// later. Persisted under `.klipp/looks/{id}.json`. Distinct from
/// `GradeVersion`, which is per-clip undo-style versioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Look {
    pub id: uuid::Uuid,
    pub name: String,
    pub node_graph: klipp_nodes::NodeGraph,
}

/// A marker placed at a specific time within a clip (relative to source_in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipMarker {
    /// Time relative to clip's source_in, in seconds.
    pub time_secs: f64,
    /// Display name.
    pub name: String,
    /// Color.
    pub color: MarkerColor,
}

/// Composition rule for a clip whose `transform_link` points at
/// another clip. `Absolute` overrides this clip's transform with the
/// target's; `Additive` composes (positions add, scales multiply,
/// rotations add). Anchor inheritance is always Absolute regardless
/// of mode — anchor is the *origin* the other ops happen from, so
/// "additive anchor" doesn't have a well-defined meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LinkMode {
    #[default]
    Absolute,
    Additive,
}

/// Inherit one clip's transform from another. Used by hyperzoom (V2
/// fill scales with V3 text), parenting (lower-third tracks a
/// speaker), compound rigs, etc. Per-axis inherit flags let a
/// child inherit just the bits it needs (e.g., "follow my parent's
/// position but keep my own scale"). Self-link, cycles, and
/// AdjustmentLayer targets are rejected at command-dispatch time.
///
/// Render-path cost is zero when no clip in the active sequence has
/// a `transform_link` set. When at least one does, the engine builds
/// a per-frame `HashMap<ClipId, ResolvedTransform>` cache so a deep
/// chain or many-children-of-one-parent setup resolves in O(N) rather
/// than O(N·depth).
/// Where a [`TransformLink`] inherits its values from. Today's
/// dominant case is `Clip(ClipId)` — inherit from another clip's
/// resolved transform. `Tracker { … }` is HZ-MATTE-NEXT-TRACKER-
/// DRIVER's growth point: the link target is a tracker on
/// `clip_id`'s `track_points[point_index]`, and the engine
/// synthesises a position-only `ResolvedTransform` from the
/// tracker's per-frame `(x, y)` output.
///
/// **Save format back-compat** is the load-bearing concern. Old
/// `.klipp` files have `"target": "<uuid>"` directly. The
/// `#[serde(untagged)]` attribute below makes this enum accept
/// both shapes:
/// * Bare uuid string → `Clip(ClipId)` (legacy + the default
///   for new files using `Clip` variant — same on-disk shape, no
///   migration needed)
/// * Tagged object `{ "clip_id": ..., "point_index": N }` →
///   `Tracker { ... }`
///
/// Round-trip stability: serializing `Clip(uuid)` produces a bare
/// uuid string (newtype-style serde for `ClipId`), and loading a
/// bare uuid back lands in `Clip(...)`. Serializing
/// `Tracker { ... }` produces an object; loading the object lands
/// in `Tracker(...)`. Tests pin both directions plus the legacy-
/// load path explicitly.
// `#[serde(untagged)]` dispatches between variants based on **shape**,
// not a tag field. That's deliberate — it lets the legacy bare-uuid
// form (a JSON string) deserialise as `Clip(uuid)` without a wire-
// format break. **The constraint this places on future variants**:
// any variant added here MUST serialise to a JSON shape that's
// structurally distinct from every existing variant. If a future
// variant happens to deserialise from the same shape as `Clip`
// (a string) or `Tracker` (an object with `clip_id` + `point_index`),
// untagged dispatch becomes ambiguous and serde picks the first
// matching variant — silent data corruption. When adding variants:
// either use a clearly different shape (a number, an array, an
// object with a unique field), or migrate this enum to
// `#[serde(tag = "kind")]` with explicit migration logic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransformLinkTarget {
    /// Inherit from another clip's resolved transform. Serialises
    /// as a **bare uuid string** — intentionally byte-identical to
    /// the pre-migration `target: ClipId` field. This means a
    /// project saved by current Klipp using the `Clip` variant can
    /// be loaded by an older Klipp build that pre-dates the enum
    /// migration. Forward-compat for the dominant case.
    Clip(ClipId),
    Tracker {
        clip_id: ClipId,
        point_index: u32,
    },
}

impl TransformLinkTarget {
    /// Convenience: when the caller only cares about the clip the
    /// link references (e.g. the `cascade_clear_transform_link`
    /// sweep, the cycle-detection walker, the inspector candidate
    /// filter), reduce both variants to a single `ClipId`. For
    /// `Clip(id)` it's the linked clip; for `Tracker { clip_id, .. }`
    /// it's the clip the tracker lives on.
    pub fn referenced_clip_id(&self) -> ClipId {
        match self {
            Self::Clip(id) => *id,
            Self::Tracker { clip_id, .. } => *clip_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformLink {
    /// What this link inherits from. `Clip(...)` for the dominant
    /// "follow another clip's resolved transform" case;
    /// `Tracker { ... }` for the "follow a tracker on a clip" case
    /// (HZ-MATTE-NEXT-TRACKER-DRIVER). Old project files load the
    /// bare-uuid form into `Clip(...)` via `#[serde(untagged)]` —
    /// see [`TransformLinkTarget`] for the migration story.
    pub target: TransformLinkTarget,
    /// Composition rule. See [`LinkMode`].
    #[serde(default)]
    pub mode: LinkMode,
    /// Inherit the target's `(scale_x, scale_y)`. Composed with the
    /// child's scale per `mode` (multiplied for Additive, replaced
    /// for Absolute). When `false`, the child uses its own scale.
    #[serde(default = "default_true")]
    pub inherit_scale: bool,
    /// Inherit the target's `(position_x, position_y)`. Composed
    /// per `mode` (added for Additive, replaced for Absolute).
    #[serde(default = "default_true")]
    pub inherit_position: bool,
    /// Inherit the target's `rotation` (degrees, Z-axis). Composed
    /// per `mode` (added for Additive, replaced for Absolute).
    #[serde(default = "default_true")]
    pub inherit_rotation: bool,
    /// Inherit the target's `(anchor_x, anchor_y)`. Always Absolute
    /// regardless of `mode` — see the type-level note on `LinkMode`.
    /// When `false`, the child uses its own anchor.
    #[serde(default = "default_true")]
    pub inherit_anchor: bool,
}

impl TransformLink {
    /// Build a link to `target` with all four inherit flags on, in
    /// the default `Absolute` mode. Matches the easy-path defaults a
    /// command-dispatch path uses when a user simply drops one clip
    /// onto another with "link transforms".
    ///
    /// Accepts a bare `ClipId` and wraps it in
    /// `TransformLinkTarget::Clip` — keeps the call-site API the
    /// same after the schema migration so the dominant
    /// "link clip → clip" path doesn't churn at every site. Use
    /// `TransformLink::new_tracker(...)` (or build the struct
    /// directly) for the tracker-target case.
    pub fn new(target: ClipId) -> Self {
        Self {
            target: TransformLinkTarget::Clip(target),
            mode: LinkMode::default(),
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        }
    }

    /// Build a link that follows a tracker on `clip_id`'s
    /// `track_points[point_index]`. Sibling to [`Self::new`] for
    /// the HZ-MATTE-NEXT-TRACKER-DRIVER case; defaults match
    /// `new` (Absolute mode, all four inherit flags on).
    pub fn new_tracker(clip_id: ClipId, point_index: u32) -> Self {
        Self {
            target: TransformLinkTarget::Tracker { clip_id, point_index },
            mode: LinkMode::default(),
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        }
    }
}

/// Binds another timeline clip as the alpha/luma source ("track matte") for
/// the clip that owns this binding. The render path resolves
/// `source_clip_id`'s rendered RGBA at the same timeline time, extracts the
/// chosen `channel`, applies `invert`/`feather_px`/`choke_px`, and multiplies
/// the consumer clip's alpha by the resulting mask before compositing.
///
/// The matte source clip is rendered as a normal clip; whether it is also
/// shown in the visible composite is controlled by the source clip's
/// `consumed_as_matte` flag, not by this binding (so a single matte source
/// can drive multiple consumers and one consumer can keep the source visible
/// for debugging).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatteBinding {
    /// Which clip's rendered output drives this matte. Validated at bind
    /// time: must not be self, must not introduce a cycle.
    pub source_clip_id: ClipId,
    /// Which channel of the source becomes the mask signal.
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    /// Invert the mask (mask = 1 - mask) after channel extraction.
    #[serde(default)]
    pub invert: bool,
    /// Soften the mask edges by this radius in pixels. 0 = hard edge.
    /// Applied as a Gaussian blur on the single-channel mask only.
    #[serde(default)]
    pub feather_px: f32,
    /// Erode the mask by this radius in pixels (negative-feather). 0 = no
    /// choke. Used by retouchers to tighten chromakey edges.
    #[serde(default)]
    pub choke_px: f32,
}

impl MatteBinding {
    /// Build a default binding pointing at `source_clip_id`. Defaults match
    /// the "easy path" UX: Luminance channel, not inverted, no feather/choke.
    pub fn new(source_clip_id: ClipId) -> Self {
        Self {
            source_clip_id,
            channel: klipp_nodes::params::MatteChannel::default(),
            invert: false,
            feather_px: 0.0,
            choke_px: 0.0,
        }
    }
}

/// Phase 4B — stencil settings on a clip. When set, the owning clip's
/// rendered RGBA acts as a mask that multiplies the alpha of EVERY
/// clip on a lower track at the same timeline time. The stencil clip
/// itself is implicitly hidden from the visible composite (AE
/// convention — the stencil composite mode replaces the layer's own
/// visibility with its mask role).
///
/// Distinct from `MatteBinding` (the consumer-side bind) so the two
/// can coexist on the same clip with different settings: e.g. a clip
/// could be both stencil-source for everything below AND have its
/// own alpha masked by a separate clip above.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StencilSettings {
    /// Which channel of the stencil clip's RGBA becomes the mask.
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    /// Invert the mask (mask = 1 - mask).
    #[serde(default)]
    pub invert: bool,
    /// Soften the edges (Gaussian blur radius, px).
    #[serde(default)]
    pub feather_px: f32,
    /// Erode the mask (px).
    #[serde(default)]
    pub choke_px: f32,
}

impl Default for StencilSettings {
    fn default() -> Self {
        Self {
            channel: klipp_nodes::params::MatteChannel::default(),
            invert: false,
            feather_px: 0.0,
            choke_px: 0.0,
        }
    }
}

fn default_blend_mode() -> klipp_core::BlendMode { klipp_core::BlendMode::Normal }

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
    /// Per-index enabled state for builtin effects (true = enabled). Grows lazily.
    #[serde(default)]
    pub builtin_effects_enabled: Vec<bool>,
    #[serde(default)]
    pub linked_clip_id: Option<ClipId>,
    /// If `Some`, this clip is a *linked instance* of a project-scoped
    /// title asset (a `MediaItem` with `MediaItemKind::SyntheticTitle`).
    /// The renderer reads the canonical `TitleData` from the asset
    /// store keyed by this `MediaId`, so editing the asset updates
    /// every linked instance on the timeline at once. The clip's
    /// inline `ClipKind::Title(TitleData)` is kept in sync as a
    /// fallback for paths that don't have asset-store access (export,
    /// snapshot tools), but the asset is the source of truth while
    /// linked. `None` = independent / unlinked title clip.
    #[serde(default)]
    pub linked_title_asset_id: Option<klipp_core::MediaId>,
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
    #[serde(default)]
    pub channel_config: ChannelConfig,
    #[serde(default)]
    pub volume_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub pan_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub gain_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub opacity_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub transform_pos_x_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub transform_pos_y_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub transform_scale_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub transform_rotation_keyframes: Vec<klipp_core::Keyframe>,
    /// Phase 4D — track-matte feather radius keyframes (px). Empty
    /// for the common case (constant feather); evaluated against
    /// `matte_source.feather_px` as the fallback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matte_feather_keyframes: Vec<klipp_core::Keyframe>,
    /// Phase 4D — track-matte choke radius keyframes (px). Same
    /// fallback contract as `matte_feather_keyframes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matte_choke_keyframes: Vec<klipp_core::Keyframe>,
    /// Per-clip markers at specific times within the clip.
    #[serde(default)]
    pub clip_markers: Vec<ClipMarker>,
    /// Per-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    #[serde(default)]
    pub effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// Content role — determines timeline color coding.
    #[serde(default)]
    pub role: ClipRole,
    /// Per-parameter modifier stacks (behaviors like Oscillate, Wiggle, etc.)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub param_modifiers: HashMap<ParamId, ModifierStack>,
    /// Playback speed multiplier (1.0 = normal, 2.0 = 2x fast, 0.5 = half speed)
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// Speed keyframes for variable speed envelopes (time-varying speed).
    /// When non-empty, these override the constant `speed` field.
    #[serde(default)]
    pub speed_keyframes: Vec<klipp_core::Keyframe>,
    /// Source media rotation in degrees (0, 90, 180, 270). Auto-detected from metadata.
    /// Applied as a pre-transform via the GPU pipeline. Users can override per-clip.
    #[serde(default)]
    pub source_rotation: i32,
    /// Whether this clip is enabled. Disabled clips are skipped during render/playback
    /// but remain visible (dimmed) on the timeline.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Solo: when true, only this clip's audio plays on its track (others muted).
    #[serde(default)]
    pub solo: bool,
    /// Per-clip blend mode for the visible composite. Default `Normal` keeps
    /// existing behaviour; the engine and export compositors are wired for
    /// all 13 modes already, just hard-coded to Normal at the call sites.
    #[serde(default = "default_blend_mode")]
    pub blend_mode: klipp_core::BlendMode,
    /// Track-matte binding: another clip's rendered RGBA drives this clip's
    /// alpha mask. `None` = no matte (default). Render path multiplies this
    /// clip's source alpha by the resolved mask before blending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_source: Option<MatteBinding>,
    /// Phase 4B — stencil mode: this clip's pixels mask EVERY clip on a
    /// lower track at the same timeline time. Implicit visibility-hide
    /// (the stencil clip itself only contributes as mask, not as
    /// visible pixels). `None` = not a stencil (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_stencil: Option<StencilSettings>,
    /// True when this clip is being used as a matte source by another clip
    /// AND the user wants it hidden from the visible composite (the
    /// "Hide matte source" inspector toggle, default on). When false, the
    /// clip renders both as a matte input and as a visible layer — useful
    /// for matte debugging or rare creative effects. The render path still
    /// renders the clip either way; this flag only gates the visible blend.
    #[serde(default)]
    pub consumed_as_matte: bool,
    /// Inherit transform from another clip in this sequence. When
    /// `Some`, the engine resolves the linked clip's evaluated
    /// transform at the same playhead and composes it into this
    /// clip's transform per `link.mode` and per-axis flags. None →
    /// no inheritance, this clip's own transform stands alone (the
    /// existing behaviour). Validated at command-dispatch time:
    /// self-link / cycle / AdjustmentLayer-target are rejected.
    /// Cascade-cleared in `remove_clip_impl` when the target is
    /// removed, so a stale `target` ClipId can never linger past
    /// the target's lifetime.
    #[serde(default)]
    pub transform_link: Option<TransformLink>,
    /// Named grade versions. Each is a complete NodeGraph snapshot.
    /// The active grade is always `node_graph` — versions are stored copies.
    #[serde(default)]
    pub grade_versions: Vec<GradeVersion>,
    /// Index of the currently active grade version in grade_versions (None = working grade, not saved as version yet).
    #[serde(default)]
    pub active_grade_version: Option<usize>,
    /// Multicam group ID: clips sharing this ID are different angles of the same event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam_group: Option<GroupId>,
    /// Multicam angle index within the group (0-based). Angle 0 = primary/default.
    #[serde(default)]
    pub multicam_angle: u8,
    /// For the primary multicam clip: cut list mapping timecodes to angle indices.
    /// Each entry is (time_secs_from_clip_start, angle_index).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicam_cuts: Vec<(f64, u8)>,
    /// Per-clip audio effect chain (EQ, compressor, reverb, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_effects: Vec<AudioEffectInstance>,
    /// Per-audio-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    /// Reserved param names: "__bypass" (0/1, step) and "__wetdry" (0..1, linear).
    #[serde(default)]
    pub audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// Pre-computed stabilization data. Not serialized — reanalysis needed on load.
    #[serde(skip)]
    pub stabilization_data: Option<klipp_effects::stabilization::StabilizationData>,
    /// Current smoothed trajectory. Recomputed when smoothing parameter changes.
    #[serde(skip)]
    pub smoothed_trajectory: Option<klipp_effects::stabilization::SmoothedTrajectory>,
    /// Tracked points for motion tracking.
    #[serde(default)]
    pub track_points: Vec<klipp_core::TrackPoint>,
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

    pub fn generator_data(&self) -> Option<&klipp_core::GeneratorData> {
        match &self.kind {
            ClipKind::Generator(data) => Some(data),
            _ => None,
        }
    }

    pub fn duration(&self) -> Timecode {
        self.timeline_out - self.timeline_in
    }

    pub fn source_duration(&self) -> Timecode {
        self.source_out - self.source_in
    }

    /// Evaluate speed at a given clip-relative time (seconds from clip start).
    /// Returns the constant speed if no keyframes, or the keyframed value.
    pub fn speed_at(&self, clip_time_secs: f64) -> f64 {
        if self.speed_keyframes.is_empty() {
            self.speed
        } else {
            klipp_core::keyframe::evaluate(&self.speed_keyframes, clip_time_secs, self.speed as f32) as f64
        }
    }

    /// Integrate the speed curve from 0 to `timeline_elapsed_secs` and return
    /// the source-time offset that a constant-speed clip would reach in the
    /// same interval. Uses `dt = 1/fps` Riemann sum clamped to [0.1, 16.0].
    pub fn speed_integral(&self, timeline_elapsed_secs: f64, fps: f64) -> f64 {
        speed_integral_keyframes(&self.speed_keyframes, timeline_elapsed_secs, fps, self.speed)
    }

    /// Map a sequence time (timeline-space) to the corresponding source
    /// time on this clip's media. Honours `clip.source_in` plus the
    /// constant `speed` field plus any `speed_keyframes` envelope.
    /// Returns `None` if `seq_time` falls outside the clip's timeline
    /// range — caller should treat that as "no source time at this
    /// position".
    ///
    /// Inverse of [`sequence_time_for_source_time`]. The two map cleanly
    /// in both directions for monotonic speed curves; the round-trip
    /// drift is bounded by the integration step (~1 frame at the
    /// project framerate).
    pub fn source_time_for_sequence_time(
        &self,
        seq_time: Timecode,
        fps: f64,
    ) -> Option<Timecode> {
        if seq_time < self.timeline_in || seq_time >= self.timeline_out {
            return None;
        }
        let timeline_elapsed = (seq_time - self.timeline_in).to_secs_f64().max(0.0);
        let source_offset = self.speed_integral(timeline_elapsed, fps);
        let source_secs = self.source_in.to_secs_f64() + source_offset;
        Some(Timecode::from_secs_f64(source_secs, self.source_in.rate))
    }

    /// Map a source time on this clip's media back to its sequence
    /// time. Inverse of [`source_time_for_sequence_time`]. Returns
    /// `None` if `source_time` falls outside `[clip.source_in,
    /// clip.source_out)`.
    ///
    /// For variable-speed clips this calls the same Riemann-integral
    /// search the duration helpers use, so the timeline_elapsed it
    /// returns is the t at which the speed integral first reaches the
    /// target source offset.
    pub fn sequence_time_for_source_time(
        &self,
        source_time: Timecode,
        fps: f64,
    ) -> Option<Timecode> {
        if source_time < self.source_in || source_time >= self.source_out {
            return None;
        }
        let source_offset = (source_time - self.source_in).to_secs_f64().max(0.0);
        let timeline_elapsed = effective_timeline_duration_for_keyframes(
            &self.speed_keyframes,
            source_offset,
            fps,
            self.speed,
        );
        let timeline_secs = self.timeline_in.to_secs_f64() + timeline_elapsed;
        // Clamp to the clip's timeline range so a near-edge round-trip
        // doesn't land 1 sample past timeline_out.
        let clamped = timeline_secs.min(self.timeline_out.to_secs_f64() - 1e-9);
        Some(Timecode::from_secs_f64(clamped, self.timeline_in.rate))
    }

    /// Set the InputTransform node's log_format on this clip's node
    /// graph. If the format is Srgb (default), does nothing. If no
    /// InputTransform node exists (current default graph is Source
    /// → Corrector → Output and ships without one), splice a fresh
    /// IDT in immediately after Source so the IDT runs before any
    /// grading. Without this splice, calling this method on a
    /// default-graph clip silently no-ops and the user sees flat
    /// log footage in the program viewer.
    pub fn set_input_log_format(&mut self, format: klipp_core::LogFormat) {
        if format == klipp_core::LogFormat::Srgb {
            return;
        }
        if let Some(node) = self.node_graph.nodes.iter_mut()
            .find(|n| n.kind == klipp_nodes::NodeKind::InputTransform)
        {
            node.params.log_format = Some(format);
            return;
        }
        // No IDT — splice one in after the Source node. We need the
        // existing Source → next-node edge so we can re-route it
        // through the new IDT.
        let source_id = match self.node_graph.nodes.iter()
            .find(|n| n.kind == klipp_nodes::NodeKind::Source)
            .map(|n| n.id)
        {
            Some(id) => id,
            None => return, // graph has no Source — bail rather than corrupt it
        };
        let new_idt_id = klipp_nodes::NodeId::new_v4();
        let mut idt_node = klipp_nodes::Node {
            id: new_idt_id,
            kind: klipp_nodes::NodeKind::InputTransform,
            label: "Input Transform".into(),
            enabled: true,
            params: klipp_nodes::NodeParams::default(),
            keyframes: klipp_nodes::NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        idt_node.params.log_format = Some(format);
        // Re-route every edge that started at Source to start at the
        // new IDT instead, then add the Source → IDT bridge. Keeps
        // every downstream consumer wired exactly as before, just
        // with one extra node in front.
        for edge in self.node_graph.edges.iter_mut() {
            if edge.from_node == source_id {
                edge.from_node = new_idt_id;
            }
        }
        self.node_graph.edges.push(klipp_nodes::Edge {
            from_node: source_id,
            from_port: 0,
            to_node: new_idt_id,
            to_port: 0,
        });
        // Insert the new node right after Source so editor layout
        // reads top-to-bottom in the expected order. Position it
        // halfway between Source and the next node so the V2 editor
        // doesn't draw it on top of an existing node.
        let source_idx = self.node_graph.nodes.iter()
            .position(|n| n.id == source_id)
            .unwrap_or(0);
        self.node_graph.nodes.insert(source_idx + 1, idt_node);
        // Position the new node 110 px right of Source — same step
        // size the default graph layout uses.
        let source_pos = self.node_graph.node_positions
            .get(&source_id)
            .copied()
            .unwrap_or(klipp_nodes::NodePos { x: 0.0, y: 0.0 });
        self.node_graph.node_positions.insert(
            new_idt_id,
            klipp_nodes::NodePos { x: source_pos.x + 110.0, y: source_pos.y },
        );
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
            builtin_effects_enabled: Vec::new(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: std::collections::HashMap::new(),
            role: ClipRole::default(),
            param_modifiers: HashMap::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
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
            builtin_effects_enabled: Vec::new(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: std::collections::HashMap::new(),
            role: ClipRole::default(),
            param_modifiers: HashMap::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
        }
    }

    /// Create a compound clip (nested sequence) with the given timeline extent.
    pub fn sequence(
        sequence_id: klipp_core::SequenceId,
        timeline_in: Timecode,
        timeline_out: Timecode,
    ) -> Self {
        let dur = timeline_out - timeline_in;
        Self {
            id: ClipId::new(),
            kind: ClipKind::Sequence { sequence_id },
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: timeline_in.rate },
            source_out: dur,
            node_graph: klipp_nodes::NodeGraph::default(),
            effects: Vec::new(),
            builtin_effects: Vec::new(),
            builtin_effects_enabled: Vec::new(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: std::collections::HashMap::new(),
            role: ClipRole::default(),
            param_modifiers: HashMap::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Time Remap — free helpers
// ═══════════════════════════════════════════════════════════════════

/// Speed clamp used by the time-remap integrator. Matches the UI graph range
/// (0.1× – 16×) and the `SetClipSpeed` handler clamp. Prevents zero/negative
/// step sizes from stalling integration or producing NaN source times.
pub const SPEED_MIN: f64 = 0.1;
pub const SPEED_MAX: f64 = 16.0;

/// Evaluate the keyframed speed curve at `clip_time_secs`. Returns `default_speed`
/// if there are no keyframes, otherwise interpolates via `klipp_core::keyframe::evaluate`.
/// Does NOT clamp — callers that feed this into the clock must clamp themselves.
pub fn speed_at_keyframes(
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    default_speed: f64,
) -> f64 {
    if keyframes.is_empty() {
        default_speed
    } else {
        klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_speed as f32) as f64
    }
}

/// Integrate the (clamped) speed curve from 0 to `timeline_elapsed_secs` using
/// `dt = 1/fps` Riemann sums with a partial-step correction on the tail.
///
/// Returns the corresponding source-time offset: for constant speed=1.0 this is
/// just `timeline_elapsed_secs`; for a 2× ramp this returns ~2× the elapsed.
///
/// Falls back to `default_speed * timeline_elapsed_secs` when keyframes is empty.
pub fn speed_integral_keyframes(
    keyframes: &[klipp_core::Keyframe],
    timeline_elapsed_secs: f64,
    fps: f64,
    default_speed: f64,
) -> f64 {
    if timeline_elapsed_secs <= 0.0 {
        return 0.0;
    }
    if keyframes.is_empty() {
        return default_speed.clamp(SPEED_MIN, SPEED_MAX) * timeline_elapsed_secs;
    }
    let dt = 1.0 / fps.max(1.0);
    // Number of full steps; the last partial step gets a separate correction.
    let full_steps = (timeline_elapsed_secs / dt).floor() as usize;
    let mut source_offset = 0.0f64;
    for i in 0..full_steps {
        let t = i as f64 * dt;
        let spd = speed_at_keyframes(keyframes, t, default_speed).clamp(SPEED_MIN, SPEED_MAX);
        source_offset += spd * dt;
    }
    // Tail: the fractional step between full_steps*dt and timeline_elapsed_secs.
    let tail_start = full_steps as f64 * dt;
    let tail_len = (timeline_elapsed_secs - tail_start).max(0.0);
    if tail_len > 0.0 {
        let spd = speed_at_keyframes(keyframes, tail_start, default_speed).clamp(SPEED_MIN, SPEED_MAX);
        source_offset += spd * tail_len;
    }
    source_offset
}

/// Given a clip's source duration and speed envelope, compute the timeline-space
/// duration that exhausts the source: i.e. find T such that
/// `speed_integral_keyframes(keyframes, T, fps, default) == source_duration_secs`.
///
/// Uses forward Riemann integration with `dt = 1/fps`. Falls back to
/// `source_duration_secs / default_speed` for the no-keyframes case.
///
/// Caps at 20× the source duration as a safety valve against a curve that
/// dwells near `SPEED_MIN` forever.
pub fn effective_timeline_duration_for_keyframes(
    keyframes: &[klipp_core::Keyframe],
    source_duration_secs: f64,
    fps: f64,
    default_speed: f64,
) -> f64 {
    if source_duration_secs <= 0.0 {
        return 0.0;
    }
    if keyframes.is_empty() {
        let spd = default_speed.clamp(SPEED_MIN, SPEED_MAX);
        return source_duration_secs / spd;
    }
    let dt = 1.0 / fps.max(1.0);
    let safety_cap = source_duration_secs * 20.0;
    let mut source_offset = 0.0f64;
    let mut timeline_t = 0.0f64;
    while source_offset < source_duration_secs && timeline_t < safety_cap {
        let spd = speed_at_keyframes(keyframes, timeline_t, default_speed).clamp(SPEED_MIN, SPEED_MAX);
        let step_source = spd * dt;
        if source_offset + step_source >= source_duration_secs {
            // Partial final step: only use the fraction needed to hit the target.
            let remaining = source_duration_secs - source_offset;
            timeline_t += remaining / spd;
            break;
        }
        source_offset += step_source;
        timeline_t += dt;
    }
    timeline_t
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::MediaId;

    const RATE: u32 = 24;

    fn tc(frames: i64) -> Timecode {
        RationalTime::from_frames(frames, RATE)
    }

    fn make_clip(timeline_in: i64, timeline_out: i64, src_in: i64, src_out: i64) -> Clip {
        Clip::media(MediaId::new(), tc(timeline_in), tc(timeline_out), tc(src_in), tc(src_out))
    }

    fn make_track(clips: Vec<Clip>) -> Track {
        let mut t = Track::new("V1", TrackKind::Video);
        t.clips = clips;
        t
    }

    // ═════════════════════════════════════════════════════════════════
    // Track-matte data model (Phase 1)
    // ═════════════════════════════════════════════════════════════════

    #[test]
    fn matte_binding_default_constructor_uses_safe_defaults() {
        // The "easy path" UX produces a binding with these defaults; if
        // they shift the right-click "Use clip above as matte" gesture
        // changes meaning silently. Pin them.
        let src = ClipId::new();
        let b = MatteBinding::new(src);
        assert_eq!(b.source_clip_id, src);
        assert_eq!(b.channel, klipp_nodes::params::MatteChannel::Luminance);
        assert!(!b.invert);
        assert!((b.feather_px - 0.0).abs() < f32::EPSILON);
        assert!((b.choke_px - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn matte_binding_round_trips_through_serde() {
        let src = ClipId::new();
        let b = MatteBinding {
            source_clip_id: src,
            channel: klipp_nodes::params::MatteChannel::Alpha,
            invert: true,
            feather_px: 4.5,
            choke_px: 1.25,
        };
        let json = serde_json::to_string(&b).expect("serialise MatteBinding");
        let back: MatteBinding = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, b);
    }

    #[test]
    fn transform_link_default_constructor_uses_safe_defaults() {
        // `TransformLink::new(target)` is what the "easy path" UX
        // produces — pin the defaults so a casual edit can't shift
        // their meaning. All four inherit flags on, Absolute mode.
        let target = ClipId::new();
        let link = TransformLink::new(target);
        assert_eq!(link.target, TransformLinkTarget::Clip(target));
        assert_eq!(link.mode, LinkMode::Absolute);
        assert!(link.inherit_scale);
        assert!(link.inherit_position);
        assert!(link.inherit_rotation);
        assert!(link.inherit_anchor);
    }

    #[test]
    fn transform_link_round_trips_through_serde() {
        let target = ClipId::new();
        let link = TransformLink {
            target: TransformLinkTarget::Clip(target),
            mode: LinkMode::Additive,
            inherit_scale: true,
            inherit_position: false,
            inherit_rotation: true,
            inherit_anchor: false,
        };
        let json = serde_json::to_string(&link).expect("serialise TransformLink");
        let back: TransformLink = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, link);
    }

    // ─── HZ-MATTE-NEXT-TRACKER-DRIVER: TransformLinkTarget migration ───
    //
    // Focused tests for the schema migration shape itself. Pre-migration
    // the wire format was a bare uuid string at `target`; post-migration
    // the field is `TransformLinkTarget` with `#[serde(untagged)]`. The
    // round-trip + legacy-load tests below pin that the migration is
    // bidirectionally byte-compatible — every existing `.klipp` file in
    // the wild still loads, and re-saving doesn't change the on-disk
    // shape for the dominant `Clip` variant.

    #[test]
    fn transform_link_target_clip_serialises_as_bare_uuid_string() {
        // Pin: the `Clip(uuid)` variant of `TransformLinkTarget`
        // serialises as the SAME bare-uuid string the legacy
        // `target: ClipId` field used. New files writing this
        // variant produce on-disk shapes byte-identical to old
        // files — so a project saved with this build can be opened
        // by the previous build (forward-compat for the dominant
        // case).
        let target = ClipId::new();
        let t = TransformLinkTarget::Clip(target);
        let json = serde_json::to_string(&t).expect("serialise");
        // ClipId is a tuple-newtype around Uuid; serde serialises
        // it as a bare uuid string in JSON.
        let expected = format!("\"{}\"", target.0);
        assert_eq!(json, expected, "Clip variant must serialise as bare uuid string");
    }

    #[test]
    fn transform_link_target_tracker_serialises_as_tagged_object() {
        // The `Tracker` variant doesn't have a back-compat constraint
        // (no legacy form exists); it serialises as the natural
        // untagged-struct shape.
        let clip_id = ClipId::new();
        let t = TransformLinkTarget::Tracker { clip_id, point_index: 7 };
        let json = serde_json::to_string(&t).expect("serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object(), "Tracker variant serialises as object");
        assert_eq!(parsed["point_index"].as_u64(), Some(7));
        assert_eq!(parsed["clip_id"].as_str(), Some(clip_id.0.to_string().as_str()));
    }

    #[test]
    fn transform_link_target_legacy_bare_uuid_loads_as_clip_variant() {
        // The load-bearing migration test. A pre-migration save file
        // wrote `"target": "<uuid>"` directly; the new
        // `TransformLinkTarget` enum's `#[serde(untagged)]` must
        // accept this and deserialise it as `Clip(<uuid>)`. Without
        // this, every existing `.klipp` file with a transform_link
        // would fail to load.
        let target = ClipId::new();
        let bare_uuid_json = format!("\"{}\"", target.0);
        let back: TransformLinkTarget =
            serde_json::from_str(&bare_uuid_json).expect("legacy bare uuid loads");
        assert_eq!(back, TransformLinkTarget::Clip(target));
    }

    #[test]
    fn transform_link_target_tracker_object_loads_as_tracker_variant() {
        let clip_id = ClipId::new();
        let json = format!(
            r#"{{"clip_id":"{}","point_index":3}}"#,
            clip_id.0,
        );
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("tracker object loads");
        assert_eq!(back, TransformLinkTarget::Tracker { clip_id, point_index: 3 });
    }

    #[test]
    fn transform_link_target_round_trip_clip_variant() {
        let target = ClipId::new();
        let t = TransformLinkTarget::Clip(target);
        let json = serde_json::to_string(&t).expect("serialise");
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, t, "round-trip stable for Clip variant");
    }

    #[test]
    fn transform_link_target_round_trip_tracker_variant() {
        let clip_id = ClipId::new();
        let t = TransformLinkTarget::Tracker { clip_id, point_index: 12 };
        let json = serde_json::to_string(&t).expect("serialise");
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, t, "round-trip stable for Tracker variant");
    }

    #[test]
    fn transform_link_legacy_json_defaults_inherit_flags_to_true() {
        // Older save format may have only written `target`. Loading
        // such JSON should fill the inherit flags + mode with their
        // safe defaults so a roundtrip through an older project file
        // doesn't silently disable inheritance on every link.
        let target = ClipId::new();
        let json = format!(r#"{{"target":"{}"}}"#, target.0);
        let back: TransformLink = serde_json::from_str(&json).expect("deserialise minimal");
        assert_eq!(back.target, TransformLinkTarget::Clip(target));
        assert_eq!(back.mode, LinkMode::Absolute);
        assert!(back.inherit_scale);
        assert!(back.inherit_position);
        assert!(back.inherit_rotation);
        assert!(back.inherit_anchor);
    }

    #[test]
    fn clip_default_blend_mode_is_normal_and_no_matte() {
        let c = Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24));
        assert_eq!(c.blend_mode, klipp_core::BlendMode::Normal);
        assert!(c.matte_source.is_none());
        assert!(!c.consumed_as_matte);
    }

    #[test]
    fn blade_at_propagates_blend_and_matte_to_both_halves() {
        // Splitting a clip should preserve its blend_mode + matte
        // binding + consumed_as_matte on both halves — anything else
        // would silently break compositing at every blade. Hit this
        // before someone special-cases one of the new fields and not
        // the others.
        let mut c = make_clip(0, 24, 0, 24);
        c.blend_mode = klipp_core::BlendMode::Multiply;
        let src = ClipId::new();
        c.matte_source = Some(MatteBinding {
            source_clip_id: src,
            channel: klipp_nodes::params::MatteChannel::Alpha,
            invert: true,
            feather_px: 3.0,
            choke_px: 0.5,
        });
        c.consumed_as_matte = true;
        let id = c.id;
        let mut track = make_track(vec![c]);

        let (a, b) = track.blade_at(id, tc(12)).expect("split");
        for half in [&a, &b] {
            assert_eq!(half.blend_mode, klipp_core::BlendMode::Multiply);
            assert!(half.consumed_as_matte);
            let binding = half.matte_source.as_ref().expect("binding survived split");
            assert_eq!(binding.source_clip_id, src);
            assert_eq!(binding.channel, klipp_nodes::params::MatteChannel::Alpha);
            assert!(binding.invert);
            assert!((binding.feather_px - 3.0).abs() < f32::EPSILON);
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_trim_left
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_trim_left_shrinks_clip_shifts_downstream() {
        // Three adjacent clips: [0..24], [24..48], [48..72]
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1, c2]);

        // Trim clip 0's left edge inward by 6 frames (shrink it)
        track.ripple_trim_left(id0, tc(6), tc(6));

        // Clip 0: timeline_in=6, source_in=6 (smaller by 6 frames)
        assert_eq!(track.clips[0].timeline_in, tc(6));
        assert_eq!(track.clips[0].source_in, tc(6));
        // timeline_out unchanged for clip 0
        assert_eq!(track.clips[0].timeline_out, tc(24));

        // Downstream clips shift by +6
        assert_eq!(track.clips[1].timeline_in, tc(30));
        assert_eq!(track.clips[1].timeline_out, tc(54));
        assert_eq!(track.clips[2].timeline_in, tc(54));
        assert_eq!(track.clips[2].timeline_out, tc(78));
    }

    #[test]
    fn ripple_trim_left_expands_clip_shifts_downstream() {
        // Clip with source headroom: source starts at frame 12
        let c0 = make_clip(12, 36, 12, 36);
        let c1 = make_clip(36, 60, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1]);

        // Expand clip 0's left edge out by 12 frames
        track.ripple_trim_left(id0, tc(0), tc(0));

        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].source_in, tc(0));
        // Downstream shifts by -12
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    #[test]
    fn ripple_trim_left_nonexistent_clip_is_noop() {
        let c0 = make_clip(0, 24, 0, 24);
        let mut track = make_track(vec![c0]);
        let fake_id = ClipId::new();
        track.ripple_trim_left(fake_id, tc(6), tc(6));
        // Nothing changed
        assert_eq!(track.clips[0].timeline_in, tc(0));
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_trim_right
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_trim_right_shrinks_clip_shifts_downstream() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1, c2]);

        // Trim clip 0's right edge inward by 6 frames
        track.ripple_trim_right(id0, tc(18), tc(18));

        assert_eq!(track.clips[0].timeline_out, tc(18));
        assert_eq!(track.clips[0].source_out, tc(18));
        // Downstream clips shift left by 6
        assert_eq!(track.clips[1].timeline_in, tc(18));
        assert_eq!(track.clips[1].timeline_out, tc(42));
        assert_eq!(track.clips[2].timeline_in, tc(42));
        assert_eq!(track.clips[2].timeline_out, tc(66));
    }

    #[test]
    fn ripple_trim_right_expands_clip_shifts_downstream() {
        // Clip with source tail room
        let c0 = make_clip(0, 24, 0, 24); // source has more (we allow setting source_out > original)
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1]);

        // Extend clip 0 by 12 frames
        track.ripple_trim_right(id0, tc(36), tc(36));

        assert_eq!(track.clips[0].timeline_out, tc(36));
        assert_eq!(track.clips[0].source_out, tc(36));
        // Downstream shifts right by 12
        assert_eq!(track.clips[1].timeline_in, tc(36));
        assert_eq!(track.clips[1].timeline_out, tc(60));
    }

    #[test]
    fn ripple_trim_right_no_downstream_clips() {
        let c0 = make_clip(0, 24, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        track.ripple_trim_right(id0, tc(12), tc(12));

        assert_eq!(track.clips[0].timeline_out, tc(12));
        assert_eq!(track.clips[0].source_out, tc(12));
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_remove
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_remove_shifts_downstream() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id1 = c1.id;
        let mut track = make_track(vec![c0, c1, c2]);

        let removed = track.ripple_remove(id1).unwrap();
        assert_eq!(removed.timeline_in, tc(24));

        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_out, tc(24));
        // c2 shifted left by 24 frames
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    // ═══════════════════════════════════════════════════════════════
    // blade_at
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn blade_splits_clip_correctly() {
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        let result = track.blade_at(id0, tc(24));
        assert!(result.is_some());

        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[0].source_out, tc(24));
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].source_in, tc(24));
    }

    #[test]
    fn blade_at_edge_returns_none() {
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        // Cut at 1 frame from start (within 1-frame guard)
        assert!(track.blade_at(id0, tc(1)).is_none());
        // Cut at 1 frame from end
        assert!(track.blade_at(id0, tc(47)).is_none());
    }

    // ═══════════════════════════════════════════════════════════════
    // audio_effect_keyframes split round-trip — Phase 1 of audio
    // keyframe support. Mirrors the existing video `effect_keyframes`
    // split logic at sequence.rs:618-626. The whole point of Option B
    // (separate field) is that audio kfs travel through every clip
    // operation just like video kfs do — blade is the most rigorous
    // exercise because it has to split the kf track at the cut and
    // shift the right half's times back to the new clip's source_in.
    // ═══════════════════════════════════════════════════════════════
    #[test]
    fn blade_splits_audio_effect_keyframes() {
        use klipp_core::{Interpolation, Keyframe};
        let mut c0 = make_clip(0, 48, 0, 48);
        // Three keyframes on `0:gain` at clip-relative times 0.25s, 0.75s,
        // 1.5s. Cut is at frame 24 (= 1.0s @ 24fps), so the first two
        // belong to clip_a and the third to clip_b (with time shifted to
        // 0.5s relative to the new clip).
        let mk = |t: f64, v: f32| Keyframe {
            time_secs: t,
            value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
        };
        c0.audio_effect_keyframes.insert(
            "0:gain".to_string(),
            vec![mk(0.25, 0.5), mk(0.75, 0.7), mk(1.5, 0.9)],
        );
        // Bypass kf on the second-half side only (at 1.25s = 0.25s into clip_b).
        c0.audio_effect_keyframes.insert(
            "0:__bypass".to_string(),
            vec![mk(1.25, 1.0)],
        );
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        let result = track.blade_at(id0, tc(24));
        assert!(result.is_some(), "blade should succeed at frame 24");
        assert_eq!(track.clips.len(), 2);

        // clip_a: keeps the first two `gain` kfs, no bypass kf.
        let a_gain = track.clips[0].audio_effect_keyframes.get("0:gain")
            .expect("clip_a must keep the gain track");
        assert_eq!(a_gain.len(), 2, "clip_a gets the two pre-cut kfs");
        assert!((a_gain[0].time_secs - 0.25).abs() < 1e-9);
        assert!((a_gain[1].time_secs - 0.75).abs() < 1e-9);
        assert!(track.clips[0].audio_effect_keyframes.get("0:__bypass").is_none(),
            "clip_a must not carry the bypass kf (it sits past the cut)");

        // clip_b: the third gain kf, with time shifted back by 1.0s
        // (the cut point), and the bypass kf shifted likewise.
        let b_gain = track.clips[1].audio_effect_keyframes.get("0:gain")
            .expect("clip_b must inherit the post-cut gain kf");
        assert_eq!(b_gain.len(), 1);
        assert!((b_gain[0].time_secs - 0.5).abs() < 1e-9,
            "clip_b's kf time must be shifted: 1.5s - 1.0s = 0.5s, got {}", b_gain[0].time_secs);
        let b_bypass = track.clips[1].audio_effect_keyframes.get("0:__bypass")
            .expect("clip_b must carry the bypass kf");
        assert!((b_bypass[0].time_secs - 0.25).abs() < 1e-9,
            "bypass kf time must be 1.25s - 1.0s = 0.25s, got {}", b_bypass[0].time_secs);
    }

    #[test]
    fn blade_with_no_audio_effect_keyframes_leaves_both_halves_empty() {
        // Sanity: clips without audio kfs split into two clips that also
        // have empty maps — no spurious keys, no panics.
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);
        track.blade_at(id0, tc(24));
        assert!(track.clips[0].audio_effect_keyframes.is_empty());
        assert!(track.clips[1].audio_effect_keyframes.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════
    // Slip (source in/out shift, timeline fixed)
    // ═══════════════════════════════════════════════════════════════
    // Slip is UI-level only (no Track method), but we test the invariants:
    // - timeline_in and timeline_out must NOT change
    // - source_in and source_out shift by the same delta
    // - source_in >= 0, source_out <= media_duration

    #[test]
    fn slip_preserves_timeline_position() {
        // Clip at timeline [24..48], showing source [12..36] of a 72-frame media
        let mut clip = make_clip(24, 48, 12, 36);

        // Slip forward by 6 frames
        let delta = tc(6);
        clip.source_in += delta;
        clip.source_out += delta;

        assert_eq!(clip.timeline_in, tc(24)); // unchanged
        assert_eq!(clip.timeline_out, tc(48)); // unchanged
        assert_eq!(clip.source_in, tc(18));
        assert_eq!(clip.source_out, tc(42));
        assert_eq!(clip.duration(), tc(24)); // same duration
        assert_eq!(clip.source_duration(), tc(24)); // same source duration
    }

    #[test]
    fn slip_clamps_at_source_boundaries() {
        let mut clip = make_clip(0, 24, 0, 24);
        let media_duration = tc(48);

        // Try to slip backward (source_in would go negative)
        let delta = tc(-6);
        let clamped_delta = if clip.source_in + delta < tc(0) {
            tc(0) - clip.source_in
        } else {
            delta
        };
        clip.source_in += clamped_delta;
        clip.source_out += clamped_delta;
        // Should clamp at 0
        assert_eq!(clip.source_in, tc(0));
        assert_eq!(clip.source_out, tc(24));

        // Slip forward to the end
        let delta = tc(30);
        let clamped_delta = if clip.source_out + delta > media_duration {
            media_duration - clip.source_out
        } else {
            delta
        };
        clip.source_in += clamped_delta;
        clip.source_out += clamped_delta;
        assert_eq!(clip.source_in, tc(24));
        assert_eq!(clip.source_out, tc(48));
    }

    // ═══════════════════════════════════════════════════════════════
    // Roll (boundary between two adjacent clips shifts)
    // ═══════════════════════════════════════════════════════════════
    // Roll is UI-level, but we test the invariants:
    // - Clip A's out and Clip B's in move by same delta
    // - Total span A+B is preserved
    // - Neither clip goes below minimum duration

    #[test]
    fn roll_moves_boundary_preserves_total_span() {
        // Two adjacent clips: A=[0..24], B=[24..48]
        // Source: A shows [0..24] of 48-frame media, B shows [0..24] of 48-frame media
        let mut clip_a = make_clip(0, 24, 0, 24);
        let mut clip_b = make_clip(24, 48, 0, 24);

        let total_span = clip_b.timeline_out - clip_a.timeline_in;

        // Roll right by 6 frames: A gets longer, B gets shorter
        let delta = tc(6);
        clip_a.source_out += delta;
        clip_a.timeline_out += delta;
        clip_b.source_in += delta;
        clip_b.timeline_in += delta;

        assert_eq!(clip_a.timeline_out, tc(30));
        assert_eq!(clip_b.timeline_in, tc(30));
        assert_eq!(clip_a.duration(), tc(30));
        assert_eq!(clip_b.duration(), tc(18));
        // Total span preserved
        assert_eq!(clip_b.timeline_out - clip_a.timeline_in, total_span);
    }

    #[test]
    fn roll_clamped_to_minimum_duration() {
        let mut clip_a = make_clip(0, 24, 0, 24);
        let mut clip_b = make_clip(24, 48, 0, 24);

        let min_dur = tc(1); // 1 frame minimum

        // Try to roll right by 30 frames (would make B negative duration)
        let delta = tc(30);
        let max_delta = clip_b.duration() - min_dur; // 23 frames max
        let clamped = if delta > max_delta { max_delta } else { delta };

        clip_a.source_out += clamped;
        clip_a.timeline_out += clamped;
        clip_b.source_in += clamped;
        clip_b.timeline_in += clamped;

        assert_eq!(clip_b.duration(), min_dur); // B is at minimum
        assert_eq!(clip_a.duration(), tc(47)); // A got the rest
    }

    // ═══════════════════════════════════════════════════════════════
    // Slide (clip moves, neighbors absorb the shift)
    // ═══════════════════════════════════════════════════════════════
    // Slide is UI-level. Invariants:
    // - Center clip's source_in/out unchanged, timeline shifts
    // - Left neighbor's out expands/shrinks by delta
    // - Right neighbor's in expands/shrinks by -delta
    // - No gaps between any of the three clips

    #[test]
    fn slide_moves_clip_adjusts_neighbors() {
        // Three adjacent clips: L=[0..24], C=[24..48], R=[48..72]
        // All have source headroom (48 frame sources)
        let mut left = make_clip(0, 24, 0, 24);
        let mut center = make_clip(24, 48, 0, 24);
        let mut right = make_clip(48, 72, 0, 24);

        let orig_center_src_in = center.source_in;
        let orig_center_src_out = center.source_out;

        // Slide center right by 6 frames
        let delta = tc(6);
        // Center moves on timeline
        center.timeline_in += delta;
        center.timeline_out += delta;
        // Left neighbor extends to fill
        left.timeline_out += delta;
        left.source_out += delta;
        // Right neighbor shrinks from the left
        right.timeline_in += delta;
        right.source_in += delta;

        // No gaps
        assert_eq!(left.timeline_out, center.timeline_in);
        assert_eq!(center.timeline_out, right.timeline_in);

        // Center source unchanged
        assert_eq!(center.source_in, orig_center_src_in);
        assert_eq!(center.source_out, orig_center_src_out);

        // Verify positions
        assert_eq!(left.timeline_out, tc(30));
        assert_eq!(center.timeline_in, tc(30));
        assert_eq!(center.timeline_out, tc(54));
        assert_eq!(right.timeline_in, tc(54));
    }

    #[test]
    fn slide_clamped_by_neighbor_minimum_duration() {
        let left = make_clip(0, 24, 0, 48);  // 48-frame source
        let _center = make_clip(24, 48, 0, 24);
        let right = make_clip(48, 72, 0, 48); // 48-frame source

        let min_dur = tc(1);

        // Slide right: limited by right neighbor's minimum duration
        let delta = tc(30);
        let max_right = right.duration() - min_dur; // 23
        // Slide left: limited by left neighbor's minimum duration
        let max_left = left.duration() - min_dur; // 23

        let clamped = delta.min(max_right).min(max_left);
        // Should be 23 frames
        assert_eq!(clamped, tc(23));
    }

    // ═══════════════════════════════════════════════════════════════
    // find_clip / find_clip_mut
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn find_clip_returns_correct_clip() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let id1 = c1.id;
        let track = make_track(vec![c0, c1]);

        assert_eq!(track.find_clip(id0).unwrap().timeline_in, tc(0));
        assert_eq!(track.find_clip(id1).unwrap().timeline_in, tc(24));
        assert!(track.find_clip(ClipId::new()).is_none());
    }

    #[test]
    fn clip_at_timecode_finds_correct_clip() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let id1 = c1.id;
        let track = make_track(vec![c0, c1]);

        assert_eq!(track.clip_at_timecode(tc(0)).unwrap().1.id, id0);
        assert_eq!(track.clip_at_timecode(tc(12)).unwrap().1.id, id0);
        assert_eq!(track.clip_at_timecode(tc(24)).unwrap().1.id, id1);
        assert!(track.clip_at_timecode(tc(48)).is_none()); // past end
    }

    // ─── Time Remap helpers ────────────────────────────────────────

    fn kf(t: f64, v: f32) -> klipp_core::Keyframe {
        klipp_core::Keyframe {
            time_secs: t,
            value: v,
            interpolation: klipp_core::keyframe::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        }
    }

    #[test]
    fn constant_speed_no_keyframes_returns_base_speed() {
        // No keyframes → speed_at_keyframes returns the default.
        assert_eq!(speed_at_keyframes(&[], 0.0, 1.0), 1.0);
        assert_eq!(speed_at_keyframes(&[], 5.0, 1.0), 1.0);
        assert_eq!(speed_at_keyframes(&[], 5.0, 2.5), 2.5);

        // And the integral is just speed * duration.
        let src = speed_integral_keyframes(&[], 4.0, 60.0, 2.0);
        assert!((src - 8.0).abs() < 1e-9, "got {src}");
    }

    #[test]
    fn ramp_up_integrates_correctly() {
        // Linear ramp from 0.5× at t=0 to 2.0× at t=10.
        // Integral from 0 to 5 must be between the min-constant and max-constant
        // bounds (0.5*5 = 2.5 and 2.0*5 = 10) and close to the exact value for
        // a linear ramp: mean speed over [0,5] is (0.5 + 1.25)/2 = 0.875,
        // giving an integral of 4.375.
        let kfs = vec![kf(0.0, 0.5), kf(10.0, 2.0)];
        let src = speed_integral_keyframes(&kfs, 5.0, 60.0, 1.0);
        assert!(src > 2.5, "expected > min-constant; got {src}");
        assert!(src < 10.0, "expected < max-constant; got {src}");
        assert!((src - 4.375).abs() < 0.05, "expected ~4.375, got {src}");
    }

    #[test]
    fn speed_clamped_to_minimum() {
        // A keyframe at 0.0× would stall the clock; the integrator must clamp to SPEED_MIN.
        let kfs = vec![kf(0.0, 0.0), kf(10.0, 0.0)];
        let src = speed_integral_keyframes(&kfs, 10.0, 60.0, 1.0);
        // Expected: SPEED_MIN * 10.0
        let expected = SPEED_MIN * 10.0;
        assert!((src - expected).abs() < 1e-6, "expected {expected}, got {src}");
    }

    #[test]
    fn speed_clamped_to_maximum() {
        // A keyframe at 999× must clamp to SPEED_MAX.
        let kfs = vec![kf(0.0, 999.0), kf(10.0, 999.0)];
        let src = speed_integral_keyframes(&kfs, 1.0, 60.0, 1.0);
        let expected = SPEED_MAX * 1.0;
        assert!((src - expected).abs() < 1e-6, "expected {expected}, got {src}");
    }

    #[test]
    fn effective_timeline_duration_constant_speed() {
        // 10s of source at 2× plays back in 5s of timeline.
        let t = effective_timeline_duration_for_keyframes(&[], 10.0, 60.0, 2.0);
        assert!((t - 5.0).abs() < 1e-6, "got {t}");

        // 10s of source at 0.5× takes 20s of timeline.
        let t = effective_timeline_duration_for_keyframes(&[], 10.0, 60.0, 0.5);
        assert!((t - 20.0).abs() < 1e-6, "got {t}");
    }

    #[test]
    fn effective_timeline_duration_roundtrips_integral() {
        // For any curve: integrating over the computed timeline duration should
        // recover the source duration we started with (within integrator error).
        let kfs = vec![kf(0.0, 0.5), kf(10.0, 2.0)];
        let source_dur = 12.0;
        let t = effective_timeline_duration_for_keyframes(&kfs, source_dur, 60.0, 1.0);
        let back = speed_integral_keyframes(&kfs, t, 60.0, 1.0);
        assert!((back - source_dur).abs() < 0.05, "roundtrip {source_dur} → t={t} → src={back}");
    }

    // ═══════════════════════════════════════════════════════════════
    // Playhead-proximity clip lookup (Q/W trim-to-playhead helpers)
    // ═══════════════════════════════════════════════════════════════

    fn qw_track() -> Track {
        // Three clips with a gap: [0..24], [24..48], gap, [72..96]
        make_track(vec![
            make_clip(0, 24, 0, 24),
            make_clip(24, 48, 0, 24),
            make_clip(72, 96, 0, 24),
        ])
    }

    #[test]
    fn clip_with_out_at_or_before_inside_clip() {
        let t = qw_track();
        // Playhead at frame 12 is inside clip 0 → returns clip 0.
        let (idx, c) = t.clip_with_out_at_or_before(&tc(12)).expect("some");
        assert_eq!(idx, 0);
        assert_eq!(c.timeline_in, tc(0));
    }

    #[test]
    fn clip_with_out_at_or_before_on_gap() {
        let t = qw_track();
        // Playhead at frame 60 is in the gap between clip 1 (ends at 48)
        // and clip 2 (starts at 72) → returns clip 1 (the one before).
        let (idx, c) = t.clip_with_out_at_or_before(&tc(60)).expect("some");
        assert_eq!(idx, 1);
        assert_eq!(c.timeline_out, tc(48));
    }

    #[test]
    fn clip_with_out_at_or_before_before_first_clip() {
        // Single-clip track starting at frame 24; nothing before.
        let t = make_track(vec![make_clip(24, 48, 0, 24)]);
        assert!(t.clip_with_out_at_or_before(&tc(0)).is_none());
    }

    #[test]
    fn clip_with_in_at_or_after_inside_clip() {
        let t = qw_track();
        // Playhead at frame 30 is inside clip 1 → returns clip 1.
        let (idx, c) = t.clip_with_in_at_or_after(&tc(30)).expect("some");
        assert_eq!(idx, 1);
        assert_eq!(c.timeline_in, tc(24));
    }

    #[test]
    fn clip_with_in_at_or_after_on_gap() {
        let t = qw_track();
        // Playhead at frame 60 is in the gap between clip 1 and clip 2 →
        // returns clip 2 (the one after).
        let (idx, c) = t.clip_with_in_at_or_after(&tc(60)).expect("some");
        assert_eq!(idx, 2);
        assert_eq!(c.timeline_in, tc(72));
    }

    #[test]
    fn clip_with_in_at_or_after_after_last_clip() {
        let t = qw_track();
        // Playhead well past the last clip (which ends at 96).
        assert!(t.clip_with_in_at_or_after(&tc(200)).is_none());
    }

    // ─── source_time_for_sequence_time / sequence_time_for_source_time ──

    /// Convert a Timecode → frame count at RATE for ergonomic
    /// assertions. Production code can use `to_secs_f64()` directly.
    fn tc_to_frames(t: Timecode) -> i64 {
        (t.to_secs_f64() * RATE as f64).round() as i64
    }

    #[test]
    fn match_frame_constant_speed_unity_round_trips() {
        // Clip on timeline frames [24, 72), source [0, 48), speed 1.0.
        // Mid-clip (frame 48 sequence ↔ frame 24 source) round-trips.
        let clip = make_clip(24, 72, 0, 48);
        let src = clip.source_time_for_sequence_time(tc(48), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 24, "1×: seq frame 48 → src frame 24");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 48, "round-trip preserves seq frame 48");
    }

    #[test]
    fn match_frame_constant_speed_2x_doubles_source_offset() {
        // Clip on timeline [24, 48), source [10, 58), speed 2.0
        // (24-frame timeline span covers 48 source frames).
        let mut clip = make_clip(24, 48, 10, 58);
        clip.speed = 2.0;
        // Sequence frame 36 (= 12 frames into the clip on timeline) →
        // source frame 10 + 24 = 34 at 2× speed.
        let src = clip.source_time_for_sequence_time(tc(36), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 34, "2×: seq frame 36 → src frame 34");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 36, "2× round-trip preserves seq frame 36");
    }

    #[test]
    fn match_frame_constant_speed_half_halves_source_offset() {
        // Speed 0.5 — clip plays 24 source frames over 48 timeline
        // frames. Sequence frame 48 (= 24 frames into clip) → source
        // frame 0 + 12 = 12.
        let mut clip = make_clip(24, 72, 0, 24);
        clip.speed = 0.5;
        let src = clip.source_time_for_sequence_time(tc(48), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 12, "0.5×: seq frame 48 → src frame 12");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 48, "0.5× round-trip preserves seq frame 48");
    }

    #[test]
    fn match_frame_outside_clip_returns_none() {
        // Both directions must reject out-of-range times. Match Frame's
        // caller relies on this to skip non-matching tracks instead of
        // wrapping silently to a clamped value.
        let clip = make_clip(24, 72, 0, 48);
        assert!(clip.source_time_for_sequence_time(tc(0), 24.0).is_none(),
            "before timeline_in must be None");
        assert!(clip.source_time_for_sequence_time(tc(72), 24.0).is_none(),
            "at timeline_out (exclusive) must be None");
        assert!(clip.source_time_for_sequence_time(tc(120), 24.0).is_none(),
            "after timeline_out must be None");
        assert!(clip.sequence_time_for_source_time(tc(48), 24.0).is_none(),
            "at source_out (exclusive) must be None");
        assert!(clip.sequence_time_for_source_time(tc(60), 24.0).is_none(),
            "after source_out must be None");
    }

    #[test]
    fn match_frame_variable_speed_round_trips_within_one_frame() {
        // A 2-frame ramp from 0.5× to 2.0× over a 48-frame clip.
        // Round-trip drift is bounded by the integration step at the
        // project fps (~1/24 s) — well within one frame.
        let mut clip = make_clip(0, 48, 0, 48);
        clip.speed_keyframes = vec![kf(0.0, 0.5), kf(2.0, 2.0)];
        for seq_frame in [6, 12, 24, 36] {
            let src = clip.source_time_for_sequence_time(tc(seq_frame), 24.0)
                .expect("in-range");
            let back = clip.sequence_time_for_source_time(src, 24.0)
                .expect("inverse in-range");
            let drift = (tc_to_frames(back) - seq_frame).abs();
            assert!(
                drift <= 1,
                "seq frame {seq_frame} drifted by {drift} frames in round-trip \
                 ({} → {} → {})",
                seq_frame, tc_to_frames(src), tc_to_frames(back),
            );
        }
    }

    #[test]
    fn match_frame_at_clip_start_lands_on_source_in() {
        // The clip's first sequence frame must map exactly to source_in
        // — that's the contract that lets Match Frame land on a
        // discoverable head frame in the source viewer.
        let mut clip = make_clip(120, 168, 36, 84);
        clip.speed = 1.5;
        let src = clip.source_time_for_sequence_time(tc(120), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 36, "first seq frame must hit source_in");
    }
}
