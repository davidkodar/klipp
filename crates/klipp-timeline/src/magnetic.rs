//! Magnetic Timeline — core editing operations.
//!
//! This module implements FCP-parity magnetic timeline behaviour.
//! All operations return a `MagneticResult` containing an `EditCommand`
//! that can be pushed to the undo stack.

use crate::magnetic_commands::{InsertSplit, MagneticEditCommand};
use crate::sequence::{AnchorAlignment, Clip, MagneticAnchor, Sequence, TrackRole};
use klipp_core::{ClipId, RationalTime};

// ═══════════════════════════════════════════════════════════════════
// Edit Mode
// ═══════════════════════════════════════════════════════════════════

/// Insert vs Overwrite mode for timeline edits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditMode {
    /// Placing a clip ripples everything downstream (magnetic default).
    #[default]
    Insert,
    /// Placing a clip replaces whatever is underneath, no ripple.
    Overwrite,
}

// ═══════════════════════════════════════════════════════════════════
// Supporting Types
// ═══════════════════════════════════════════════════════════════════

/// Result of a magnetic edit operation — contains the undoable command.
pub struct MagneticResult {
    pub command: crate::magnetic_commands::MagneticEditCommand,
}

/// Record of a clip that was rippled (moved) as a side effect of an edit.
#[derive(Debug, Clone)]
pub struct RippleRecord {
    pub clip_id: ClipId,
    pub old_in: RationalTime,
    pub old_out: RationalTime,
    pub new_in: RationalTime,
    pub new_out: RationalTime,
}

/// Record of a connected clip that was cascaded (followed its anchor).
#[derive(Debug, Clone)]
pub struct CascadeRecord {
    pub clip_id: ClipId,
    pub track_index: usize,
    pub old_in: RationalTime,
    pub old_out: RationalTime,
    pub new_in: RationalTime,
    pub new_out: RationalTime,
    pub collision: Option<CollisionInfo>,
}

/// Information about a clip overlap/collision on a connected track.
#[derive(Debug, Clone)]
pub struct CollisionInfo {
    pub colliding_clip_id: ClipId,
    pub overlap_start: RationalTime,
    pub overlap_end: RationalTime,
    pub resolution: CollisionResolution,
}

/// How to handle a collision between overlapping clips.
#[derive(Debug, Clone, PartialEq)]
pub enum CollisionResolution {
    /// Show amber warning indicator, allow overlap temporarily.
    Warn,
    /// Push colliding clip forward automatically.
    Push,
}

/// A snap target during drag operations.
#[derive(Debug, Clone)]
pub struct SnapPoint {
    pub time: RationalTime,
    pub kind: SnapKind,
}

/// What the snap target represents.
#[derive(Debug, Clone)]
pub enum SnapKind {
    ClipBoundary,
    Marker,
    Playhead,
    /// Snap back to where clip started (original position).
    OriginalPosition,
    /// Snap to a tick on the user-configured time grid.
    Grid,
}

/// Auto-connect/disconnect thresholds.
pub const AUTO_CONNECT_TRACK_DISTANCE: f32 = 1.5;
pub const AUTO_DISCONNECT_TRACK_DISTANCE: f32 = 3.0;
pub const AUTO_CONNECT_SNAP_FRAMES: i64 = 12;

// ═══════════════════════════════════════════════════════════════════
// Snap threshold (in frames)
// ═══════════════════════════════════════════════════════════════════

const SNAP_THRESHOLD_FRAMES: i64 = 8;

// ═══════════════════════════════════════════════════════════════════
// MagneticEditor — stateless editor with class methods
// ═══════════════════════════════════════════════════════════════════

pub struct MagneticEditor;

/// Undo an insert's split: take out the two pieces and put the clip
/// back whole where it was.
fn unsplit(sequence: &mut Sequence, track_index: usize, split: &Option<InsertSplit>) {
    let Some(s) = split else { return };
    let track = &mut sequence.tracks[track_index];
    track.remove_clip(s.left.id);
    track.remove_clip(s.right.id);
    let pos = s.position.min(track.clips.len());
    track.clips.insert(pos, s.original.clone());
}

/// Redo an insert: cut the clip it landed inside into the recorded
/// pieces with the new clip between them, or put the new clip in at its
/// time.
fn insert_or_split(sequence: &mut Sequence, track_index: usize, clip: &Clip, split: &Option<InsertSplit>) {
    let track = &mut sequence.tracks[track_index];
    if let Some(s) = split {
        track.remove_clip(s.original.id);
        let pos = s.position.min(track.clips.len());
        track.clips.splice(pos..pos, [s.left.clone(), clip.clone(), s.right.clone()]);
        return;
    }
    let pos = track
        .clips
        .iter()
        .position(|c| c.timeline_in >= clip.timeline_in)
        .unwrap_or(track.clips.len());
    track.clips.insert(pos, clip.clone());
}

/// Repack all clip positions on the primary track so they are gapless,
/// starting from frame 0. Returns RippleRecords for any clips that moved.
fn repack_primary(sequence: &mut Sequence) -> Vec<RippleRecord> {
    let primary_idx = match sequence.primary_track_index() {
        Some(i) => i,
        None => return Vec::new(),
    };
    let track = &mut sequence.tracks[primary_idx];
    let mut records = Vec::new();
    let rate = if let Some(first) = track.clips.first() {
        first.timeline_in.rate
    } else {
        return Vec::new();
    };
    let mut cursor = RationalTime::from_frames(0, rate);
    for clip in &mut track.clips {
        let old_in = clip.timeline_in;
        let old_out = clip.timeline_out;
        let dur = old_out - old_in;
        let new_in = cursor.rescale_to(old_in.rate);
        let new_out = new_in + dur;
        if new_in != old_in || new_out != old_out {
            records.push(RippleRecord {
                clip_id: clip.id,
                old_in,
                old_out,
                new_in,
                new_out,
            });
            clip.timeline_in = new_in;
            clip.timeline_out = new_out;
        }
        cursor = new_out.rescale_to(rate);
    }
    records
}

impl MagneticEditor {
    // ───────────────────────────────────────────────────────────────
    // 1. insert_primary
    // ───────────────────────────────────────────────────────────────
    /// `grid_interval_secs` and `snap_config`: live snap inputs from
    /// the UI layer. `None` for both = no per-target filtering, no
    /// grid post-pass — equivalent to pre-feature snap behaviour.
    pub fn insert_primary(
        sequence: &mut Sequence,
        mut new_clip: Clip,
        at_time: RationalTime,
        grid_interval_secs: Option<f64>,
        snap_config: Option<&klipp_core::SnapConfig>,
    ) -> MagneticResult {
        let primary_idx = sequence
            .primary_track_index()
            .expect("insert_primary requires a primary track");

        // Master gate: when snap is off (or no config supplied with
        // explicit-off intent), skip the boundary check entirely. The
        // is_none_or branch keeps None-callers (tests + back-compat)
        // running the existing behaviour.
        let master_on = snap_config.is_none_or(|c| c.master);
        let snapped = if master_on {
            Self::snap_to_boundary(sequence, at_time, Some(new_clip.id), grid_interval_secs, snap_config)
        } else {
            None
        };
        let at_time = snapped.unwrap_or(at_time);

        let clip_dur = new_clip.timeline_out - new_clip.timeline_in;
        new_clip.timeline_in = at_time;
        new_clip.timeline_out = at_time + clip_dur;

        // Find insert position: first clip whose timeline_in >= at_time
        let track = &sequence.tracks[primary_idx];
        let insert_pos = track
            .clips
            .iter()
            .position(|c| c.timeline_in >= at_time)
            .unwrap_or(track.clips.len());

        // Check if we need to split a clip that straddles at_time
        let split_clip_idx = track.clips.iter().position(|c| {
            c.timeline_in < at_time && c.timeline_out > at_time
        });

        let mut split = None;
        if let Some(split_idx) = split_clip_idx {
            // Split the clip at at_time
            let original = sequence.tracks[primary_idx].clips[split_idx].clone();
            let offset = at_time - original.timeline_in;
            let mut left = original.clone();
            left.id = ClipId::new();
            left.timeline_out = at_time;
            left.source_out = original.source_in + offset;
            left.magnetic_anchor = original.magnetic_anchor.clone();
            let (at, whole) = (offset.to_secs_f64(), (original.timeline_out - original.timeline_in).to_secs_f64());
            left.keep_angle_cuts_on_piece(0.0, at);

            let mut right = original.clone();
            right.id = ClipId::new();
            right.timeline_in = at_time;
            right.source_in = original.source_in + offset;
            right.magnetic_anchor = None;
            right.keep_angle_cuts_on_piece(at, whole);

            // Remove original, insert left + new_clip + right
            sequence.tracks[primary_idx].clips.remove(split_idx);
            sequence.tracks[primary_idx].clips.insert(split_idx, right.clone());
            sequence.tracks[primary_idx].clips.insert(split_idx, new_clip.clone());
            sequence.tracks[primary_idx].clips.insert(split_idx, left.clone());
            split = Some(InsertSplit { original, left, right, position: split_idx });
        } else {
            sequence.tracks[primary_idx].clips.insert(insert_pos, new_clip.clone());
        }

        // Repack to enforce gapless and get ripple records
        let rippled_clips = repack_primary(sequence);

        // Cascade connected clips
        let cascaded_clips = Self::cascade_all_connected(sequence, &rippled_clips);

        #[cfg(debug_assertions)]
        sequence.assert_primary_gapless();

        MagneticResult {
            command: MagneticEditCommand::InsertPrimary {
                inserted_clip: new_clip,
                track_index: primary_idx,
                rippled_clips,
                cascaded_clips,
                split,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 2. overwrite_primary
    // ───────────────────────────────────────────────────────────────
    pub fn overwrite_primary(
        sequence: &mut Sequence,
        mut new_clip: Clip,
        at_time: RationalTime,
    ) -> MagneticResult {
        let primary_idx = sequence
            .primary_track_index()
            .expect("overwrite_primary requires a primary track");

        let clip_dur = new_clip.timeline_out - new_clip.timeline_in;
        new_clip.timeline_in = at_time;
        new_clip.timeline_out = at_time + clip_dur;
        let region_in = new_clip.timeline_in;
        let region_out = new_clip.timeline_out;

        let mut overwritten_clips = Vec::new();
        let mut trimmed_clips = Vec::new();
        // T-8: full Clip values for what the overwrite leaves in place of
        // the clips it cut, so redo and undo can rebuild both states: the
        // split-case remnants, and the overlap-left/right trimmed clips
        // too. Those keep their original ClipId, so a position record
        // alone left undo with the trimmed copy beside the restored
        // original, and redo with nothing to move (#919).
        let mut split_remnants: Vec<Clip> = Vec::new();
        let mut new_clips_list: Vec<Clip> = Vec::new();

        let track = &sequence.tracks[primary_idx];
        for clip in &track.clips {
            let cin = clip.timeline_in;
            let cout = clip.timeline_out;

            if cout <= region_in || cin >= region_out {
                // Completely outside — keep
                new_clips_list.push(clip.clone());
            } else if cin >= region_in && cout <= region_out {
                // Completely inside — remove
                overwritten_clips.push(clip.clone());
            } else if cin < region_in && cout > region_out {
                // Straddles entire region — split into left + right
                overwritten_clips.push(clip.clone());
                let left_dur = region_in - cin;
                let mut left = clip.clone();
                left.id = ClipId::new();
                left.timeline_out = region_in;
                left.source_out = clip.source_in + left_dur;
                left.keep_angle_cuts_on_piece(0.0, left_dur.to_secs_f64());
                trimmed_clips.push(RippleRecord {
                    clip_id: left.id,
                    old_in: cin,
                    old_out: cout,
                    new_in: left.timeline_in,
                    new_out: left.timeline_out,
                });
                split_remnants.push(left.clone());
                new_clips_list.push(left);

                // The new clip goes in the middle
                // (will be inserted after this loop)

                let right_offset = region_out - cin;
                let mut right = clip.clone();
                right.id = ClipId::new();
                right.timeline_in = region_out;
                right.source_in = clip.source_in + right_offset;
                right.magnetic_anchor = None;
                right.keep_angle_cuts_on_piece(right_offset.to_secs_f64(), (cout - cin).to_secs_f64());
                trimmed_clips.push(RippleRecord {
                    clip_id: right.id,
                    old_in: cin,
                    old_out: cout,
                    new_in: right.timeline_in,
                    new_out: right.timeline_out,
                });
                split_remnants.push(right.clone());
                new_clips_list.push(right);
            } else if cin < region_in && cout > region_in {
                // Overlaps left — trim tail
                overwritten_clips.push(clip.clone());
                let mut trimmed = clip.clone();
                let trim_amount = cout - region_in;
                trimmed.timeline_out = region_in;
                trimmed.source_out -= trim_amount;
                trimmed_clips.push(RippleRecord {
                    clip_id: trimmed.id,
                    old_in: cin,
                    old_out: cout,
                    new_in: trimmed.timeline_in,
                    new_out: trimmed.timeline_out,
                });
                split_remnants.push(trimmed.clone());
                new_clips_list.push(trimmed);
            } else if cin < region_out && cout > region_out {
                // Overlaps right — trim head
                overwritten_clips.push(clip.clone());
                let mut trimmed = clip.clone();
                let trim_amount = region_out - cin;
                trimmed.timeline_in = region_out;
                trimmed.source_in += trim_amount;
                trimmed.keep_angle_cuts_on_picture(trim_amount.to_secs_f64());
                trimmed_clips.push(RippleRecord {
                    clip_id: trimmed.id,
                    old_in: cin,
                    old_out: cout,
                    new_in: trimmed.timeline_in,
                    new_out: trimmed.timeline_out,
                });
                split_remnants.push(trimmed.clone());
                new_clips_list.push(trimmed);
            }
        }

        // Insert the new clip at the right position
        let insert_pos = new_clips_list
            .iter()
            .position(|c| c.timeline_in >= region_in)
            .unwrap_or(new_clips_list.len());
        new_clips_list.insert(insert_pos, new_clip.clone());

        sequence.tracks[primary_idx].clips = new_clips_list;

        // Repack to enforce gapless (overwrite shouldn't move things, but ensure consistency)
        let repack_records = repack_primary(sequence);
        // Merge repack records into trimmed_clips
        for r in repack_records {
            trimmed_clips.push(r);
        }

        #[cfg(debug_assertions)]
        sequence.assert_primary_gapless();

        MagneticResult {
            command: MagneticEditCommand::OverwritePrimary {
                inserted_clip: new_clip,
                track_index: primary_idx,
                overwritten_clips,
                trimmed_clips,
                split_remnants,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 3. ripple_delete
    // ───────────────────────────────────────────────────────────────
    pub fn ripple_delete(sequence: &mut Sequence, clip_id: ClipId) -> MagneticResult {
        let (track_idx, clip_idx) = sequence
            .find_clip(clip_id)
            .expect("ripple_delete: clip not found");

        let removed_clip = sequence.tracks[track_idx].clips[clip_idx].clone();
        let position = clip_idx;

        // Check for linked clip to also remove
        let removed_linked_clip = if let Some(linked_id) = removed_clip.linked_clip_id {
            if let Some((lt_idx, _lc_idx)) = sequence.find_clip(linked_id) {
                let linked = sequence.tracks[lt_idx]
                    .clips
                    .iter()
                    .find(|c| c.id == linked_id)
                    .cloned();
                if let Some(lc) = linked {
                    sequence.tracks[lt_idx].remove_clip(linked_id);
                    Some((lc, lt_idx))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Disconnect connected clips whose anchor was the deleted clip (Gap 11)
        let mut disconnected_clips = Vec::new();
        let mut disconnected_storylines = Vec::new();
        for (ti, track) in sequence.tracks.iter_mut().enumerate() {
            // Check storyline anchor
            if track.magnetic_role == TrackRole::ConnectedStoryline {
                if let Some(ref anchor) = track.storyline_anchor {
                    if anchor.anchor_clip_id == clip_id {
                        let old_anchor = anchor.clone();
                        disconnected_storylines.push((ti, old_anchor));
                        track.storyline_anchor = None;
                        track.magnetic_role = TrackRole::Connected;
                    }
                }
            }
            // Check individual clip anchors
            for clip in &mut track.clips {
                if let Some(ref anchor) = clip.magnetic_anchor {
                    if anchor.anchor_clip_id == clip_id {
                        let old_anchor = anchor.clone();
                        disconnected_clips.push((clip.id, ti, old_anchor));
                        clip.magnetic_anchor = None;
                    }
                }
            }
        }

        // Remove the clip
        sequence.tracks[track_idx].clips.remove(clip_idx);

        // Clear any third-party clips with `linked_clip_id` pointing
        // at the deleted clip (or its just-removed linked partner).
        // The explicit pair-removal above handles symmetric A↔B
        // links, but a stray clip C with C.linked_clip_id == A.id
        // would otherwise be left dangling.
        let mut purged_ids = vec![removed_clip.id];
        if let Some((ref lc, _)) = removed_linked_clip {
            purged_ids.push(lc.id);
        }
        sequence.purge_dangling_linked_clip_ids(&purged_ids);

        // Repack primary track
        let mut rippled_clips = if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            repack_primary(sequence)
        } else {
            // For non-primary, ripple downstream clips on same track
            let clip_dur = removed_clip.timeline_out - removed_clip.timeline_in;
            let mut records = Vec::new();
            for c in &mut sequence.tracks[track_idx].clips[clip_idx..] {
                let old_in = c.timeline_in;
                let old_out = c.timeline_out;
                c.timeline_in -= clip_dur;
                c.timeline_out -= clip_dur;
                records.push(RippleRecord {
                    clip_id: c.id,
                    old_in,
                    old_out,
                    new_in: c.timeline_in,
                    new_out: c.timeline_out,
                });
            }
            records
        };

        // The linked partner's track closes up too, by the same amount, so
        // everything after the cut stays in sync with the track the clip
        // was deleted from. A primary partner track is repacked before the
        // cascade so clips connected to it follow.
        let partner_track = removed_linked_clip
            .as_ref()
            .map(|(lc, lt_idx)| (lc.timeline_out, *lt_idx))
            .filter(|&(_, lt_idx)| lt_idx != track_idx);
        let partner_is_primary = partner_track
            .is_some_and(|(_, lt_idx)| sequence.tracks[lt_idx].magnetic_role == TrackRole::Primary);
        if partner_is_primary {
            rippled_clips.extend(repack_primary(sequence));
        }

        // Cascade connected clips
        let cascaded_clips = Self::cascade_all_connected(sequence, &rippled_clips);

        if let (Some((partner_out, lt_idx)), false) = (partner_track, partner_is_primary) {
            let shift = removed_clip.timeline_out - removed_clip.timeline_in;
            for c in &mut sequence.tracks[lt_idx].clips {
                // A clip the cascade already moved with its anchor has
                // closed up once; moving it again would overshoot.
                if c.timeline_in < partner_out || cascaded_clips.iter().any(|cr| cr.clip_id == c.id) {
                    continue;
                }
                let (old_in, old_out) = (c.timeline_in, c.timeline_out);
                c.timeline_in -= shift;
                c.timeline_out -= shift;
                rippled_clips.push(RippleRecord {
                    clip_id: c.id,
                    old_in,
                    old_out,
                    new_in: c.timeline_in,
                    new_out: c.timeline_out,
                });
            }
        }

        #[cfg(debug_assertions)]
        if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary || partner_is_primary {
            sequence.assert_primary_gapless();
        }

        MagneticResult {
            command: MagneticEditCommand::RippleDelete {
                removed_clip,
                track_index: track_idx,
                position,
                rippled_clips,
                cascaded_clips,
                disconnected_clips,
                disconnected_storylines,
                removed_linked_clip,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 4. lift
    // ───────────────────────────────────────────────────────────────
    pub fn lift(sequence: &mut Sequence, clip_id: ClipId) -> MagneticResult {
        let (track_idx, clip_idx) = sequence
            .find_clip(clip_id)
            .expect("lift: clip not found");

        let removed_clip = sequence.tracks[track_idx].clips[clip_idx].clone();
        let gap = Clip::gap(removed_clip.timeline_in, removed_clip.timeline_out);
        let gap_clone = gap.clone();
        let position = clip_idx;

        // T-1 + T-3: before replacing the clip with a gap, scan the
        // sequence for any dangling references to the lifted clip id:
        //   • Individual clip anchors (magnetic_anchor) on any track
        //   • Connected-storyline anchors on other tracks
        //   • `linked_clip_id` partners (e.g. the audio half of a V+A pair)
        //
        // For each, clear the reference and record enough state to
        // restore it on undo. Mirrors the ripple_delete pattern — lift
        // is "soft delete replaced by a gap" so external references
        // should be treated the same way as a hard delete.
        let mut disconnected_clips: Vec<(ClipId, usize, MagneticAnchor)> = Vec::new();
        let mut disconnected_storylines: Vec<(usize, MagneticAnchor)> = Vec::new();
        let mut unlinked_partners: Vec<(ClipId, usize)> = Vec::new();
        for (ti, track) in sequence.tracks.iter_mut().enumerate() {
            if track.magnetic_role == TrackRole::ConnectedStoryline {
                if let Some(ref anchor) = track.storyline_anchor {
                    if anchor.anchor_clip_id == clip_id {
                        disconnected_storylines.push((ti, anchor.clone()));
                        track.storyline_anchor = None;
                        track.magnetic_role = TrackRole::Connected;
                    }
                }
            }
            for clip in &mut track.clips {
                if clip.id == clip_id {
                    // This is the clip about to be replaced by the
                    // gap — skip here, the gap replace happens below.
                    continue;
                }
                if let Some(ref anchor) = clip.magnetic_anchor {
                    if anchor.anchor_clip_id == clip_id {
                        disconnected_clips.push((clip.id, ti, anchor.clone()));
                        clip.magnetic_anchor = None;
                    }
                }
                if clip.linked_clip_id == Some(clip_id) {
                    unlinked_partners.push((clip.id, ti));
                    clip.linked_clip_id = None;
                }
            }
        }

        // Replace with gap
        sequence.tracks[track_idx].clips[clip_idx] = gap;

        #[cfg(debug_assertions)]
        if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            sequence.assert_primary_gapless();
        }

        MagneticResult {
            command: MagneticEditCommand::Lift {
                removed_clip,
                gap_clip: gap_clone,
                track_index: track_idx,
                position,
                disconnected_clips,
                disconnected_storylines,
                unlinked_partners,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 5. append_primary
    // ───────────────────────────────────────────────────────────────
    pub fn append_primary(sequence: &mut Sequence, mut new_clip: Clip) -> MagneticResult {
        let primary_idx = sequence
            .primary_track_index()
            .expect("append_primary requires a primary track");

        // Duration comes from source range, not timeline (caller may pass placeholder timeline values)
        let clip_dur = new_clip.source_out - new_clip.source_in;
        let source_rate = clip_dur.rate;

        let track = &sequence.tracks[primary_idx];
        let end = track
            .clips
            .last()
            .map(|c| c.timeline_out.rescale_to(source_rate))
            .unwrap_or(RationalTime::from_frames(0, source_rate));

        new_clip.timeline_in = end;
        new_clip.timeline_out = end + clip_dur;

        sequence.tracks[primary_idx].clips.push(new_clip.clone());

        #[cfg(debug_assertions)]
        sequence.assert_primary_gapless();

        MagneticResult {
            command: MagneticEditCommand::AppendPrimary {
                appended_clip: new_clip,
                track_index: primary_idx,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 6. move_primary (Gap 18 fix: index-based)
    // ───────────────────────────────────────────────────────────────
    pub fn move_primary(
        sequence: &mut Sequence,
        clip_id: ClipId,
        to_time: RationalTime,
    ) -> MagneticResult {
        let primary_idx = sequence
            .primary_track_index()
            .expect("move_primary requires a primary track");

        let (track_idx, src_idx) = sequence
            .find_clip(clip_id)
            .expect("move_primary: clip not found");
        assert_eq!(
            track_idx, primary_idx,
            "move_primary: clip must be on primary track"
        );

        // Find target index before removal
        let target_idx = sequence.tracks[primary_idx]
            .clips
            .iter()
            .position(|c| c.timeline_in >= to_time)
            .unwrap_or(sequence.tracks[primary_idx].clips.len());

        // Remove clip
        let clip = sequence.tracks[primary_idx].clips.remove(src_idx);

        // Adjust target index if the source was before target
        let adjusted_idx = if src_idx < target_idx {
            target_idx - 1
        } else {
            target_idx
        };
        let insert_idx = adjusted_idx.min(sequence.tracks[primary_idx].clips.len());

        // Re-insert
        sequence.tracks[primary_idx].clips.insert(insert_idx, clip);

        // Repack
        let rippled_clips = repack_primary(sequence);
        let cascaded_clips = Self::cascade_all_connected(sequence, &rippled_clips);

        #[cfg(debug_assertions)]
        sequence.assert_primary_gapless();

        // Build remove + insert sub-commands for undo
        let remove_cmd = MagneticEditCommand::RippleDelete {
            removed_clip: sequence.tracks[primary_idx].clips[insert_idx].clone(),
            track_index: primary_idx,
            position: src_idx,
            rippled_clips: Vec::new(),
            cascaded_clips: Vec::new(),
            disconnected_clips: Vec::new(),
            disconnected_storylines: Vec::new(),
            removed_linked_clip: None,
        };
        let insert_cmd = MagneticEditCommand::InsertPrimary {
            inserted_clip: sequence.tracks[primary_idx].clips[insert_idx].clone(),
            track_index: primary_idx,
            rippled_clips,
            cascaded_clips,
            split: None,
        };

        MagneticResult {
            command: MagneticEditCommand::MovePrimary {
                clip_id,
                remove_command: Box::new(remove_cmd),
                insert_command: Box::new(insert_cmd),
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 7. insert_gap
    // ───────────────────────────────────────────────────────────────
    pub fn insert_gap(
        sequence: &mut Sequence,
        duration: RationalTime,
        at_time: RationalTime,
        grid_interval_secs: Option<f64>,
        snap_config: Option<&klipp_core::SnapConfig>,
    ) -> MagneticResult {
        let gap = Clip::gap(at_time, at_time + duration);
        let result = Self::insert_primary(sequence, gap.clone(), at_time, grid_interval_secs, snap_config);

        // Re-wrap the command as InsertGap
        let (rippled_clips, cascaded_clips, track_index, split) = match result.command {
            MagneticEditCommand::InsertPrimary {
                rippled_clips,
                cascaded_clips,
                track_index,
                split,
                ..
            } => (rippled_clips, cascaded_clips, track_index, split),
            _ => unreachable!(),
        };

        MagneticResult {
            command: MagneticEditCommand::InsertGap {
                gap_clip: gap,
                track_index,
                rippled_clips,
                cascaded_clips,
                split,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 8. blade
    // ───────────────────────────────────────────────────────────────
    /// Returns `None`, changing nothing, when the clip is gone or the cut
    /// is not strictly inside it. Callers queue blades per track, and a
    /// clip bladed through its linked partner ends exactly on the cut by
    /// the time its own blade runs: that is a no-op, not a crash.
    pub fn blade(
        sequence: &mut Sequence,
        clip_id: ClipId,
        at_time: RationalTime,
    ) -> Option<MagneticResult> {
        let (track_idx, clip_idx) = sequence.find_clip(clip_id)?;

        let original = sequence.tracks[track_idx].clips[clip_idx].clone();

        if at_time <= original.timeline_in || at_time >= original.timeline_out {
            return None;
        }

        let offset = at_time - original.timeline_in;

        // T-2: left half INHERITS the original ClipId. This keeps any
        // external clip/storyline anchored to the original id anchored
        // to the visually-continuous "first half" of the clip — without
        // this change blade was turning every external anchor into a
        // dangling reference (both halves previously got fresh ids).
        // Linked-clip partners on other tracks also continue to resolve
        // because linked_clip_id on those partners still points at the
        // original id, which is now the left half.
        let mut left_clip = original.clone();
        left_clip.timeline_out = at_time;
        left_clip.source_out = original.source_in + offset;
        // The fades belong to the clip's own ends, not to the cut: the
        // left keeps its fade in and loses the fade out, or the join
        // would dip to nothing on a cut that should pass unheard. A fade
        // longer than the half it is left on is shortened to it. The
        // legacy `Track::split_at` does the same, so the two blades
        // agree.
        left_clip.fade_out_secs = 0.0;
        left_clip.fade_in_secs = left_clip.fade_in_secs.min(offset.to_secs_f64() as f32);
        // Left keeps magnetic_anchor (and its id).
        let (at, whole) = (offset.to_secs_f64(), (original.timeline_out - original.timeline_in).to_secs_f64());
        left_clip.keep_angle_cuts_on_piece(0.0, at);

        let mut right_clip = original.clone();
        right_clip.id = ClipId::new();
        right_clip.timeline_in = at_time;
        right_clip.source_in = original.source_in + offset;
        right_clip.fade_in_secs = 0.0;
        right_clip.fade_out_secs = right_clip.fade_out_secs.min((original.timeline_out - at_time).to_secs_f64() as f32);
        right_clip.keep_angle_cuts_on_piece(at, whole);
        // Right keeps magnetic_anchor too (the anchor is a relationship
        // TO some other clip — both halves sit at the anchored location,
        // so both should stay connected). Dropping it on the right half
        // was a stale T-2 design choice that made connected-clip edits
        // silently lose the connection after every blade.

        // Replace original with left + right
        sequence.tracks[track_idx].clips.remove(clip_idx);
        sequence.tracks[track_idx]
            .clips
            .insert(clip_idx, right_clip.clone());
        sequence.tracks[track_idx]
            .clips
            .insert(clip_idx, left_clip.clone());

        #[cfg(debug_assertions)]
        if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            sequence.assert_primary_gapless();
        }

        Some(MagneticResult {
            command: MagneticEditCommand::Blade {
                original_clip: original,
                left_clip,
                right_clip,
                track_index: track_idx,
                position: clip_idx,
            },
        })
    }

    // ───────────────────────────────────────────────────────────────
    // 9. trim_head (Gap 9 fix: repack for primary)
    // ───────────────────────────────────────────────────────────────
    /// Trim a clip's head to a new start point.
    ///
    /// Behaves differently on primary vs non-primary tracks:
    ///
    /// * **Non-primary track**: `new_timeline_in` becomes the clip's
    ///   new start position literally. `source_in` shifts by
    ///   `delta = new_timeline_in - old_timeline_in` so the visible
    ///   duration shrinks/grows accordingly. `timeline_out` is
    ///   unchanged.
    ///
    /// * **Primary track** (magnetic / gapless): `new_timeline_in` is
    ///   used to compute `delta` (for the source_in shift), but the
    ///   post-trim position is then **overwritten** by
    ///   `repack_primary` which re-packs all primary clips head-to-
    ///   tail from zero. The effective input is therefore the DELTA,
    ///   not the target position; two different `new_timeline_in`
    ///   values that produce the same delta on a primary track
    ///   produce identical final layouts.
    ///
    /// Callers that want to describe a primary-track trim in terms of
    /// duration change should still pass `old_timeline_in + delta` for
    /// `new_timeline_in` — the arithmetic inside this function only
    /// looks at `delta`, and the signature keeps symmetry with
    /// [`Self::trim_tail`].
    pub fn trim_head(
        sequence: &mut Sequence,
        clip_id: ClipId,
        new_timeline_in: RationalTime,
    ) -> MagneticResult {
        let (track_idx, clip_idx) = sequence
            .find_clip(clip_id)
            .expect("trim_head: clip not found");

        let clip = &sequence.tracks[track_idx].clips[clip_idx];
        let old_timeline_in = clip.timeline_in;
        let old_source_in = clip.source_in;
        let delta = new_timeline_in - old_timeline_in;
        let new_source_in = old_source_in + delta;

        // Apply the trim
        let trimmed = &mut sequence.tracks[track_idx].clips[clip_idx];
        let old_multicam_cuts = trimmed.multicam_cuts.clone();
        trimmed.timeline_in = new_timeline_in;
        trimmed.source_in = new_source_in;
        trimmed.keep_angle_cuts_on_picture(delta.to_secs_f64());
        let new_multicam_cuts = trimmed.multicam_cuts.clone();

        // For primary track: repack to enforce gapless
        let rippled_clips = if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            repack_primary(sequence)
        } else {
            Vec::new()
        };

        let cascaded_clips = Self::cascade_all_connected(sequence, &rippled_clips);

        #[cfg(debug_assertions)]
        if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            sequence.assert_primary_gapless();
        }

        MagneticResult {
            command: MagneticEditCommand::TrimHead {
                clip_id,
                track_index: track_idx,
                old_timeline_in,
                old_source_in,
                new_timeline_in,
                new_source_in,
                old_multicam_cuts,
                new_multicam_cuts,
                rippled_clips,
                cascaded_clips,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 10. trim_tail
    // ───────────────────────────────────────────────────────────────
    pub fn trim_tail(
        sequence: &mut Sequence,
        clip_id: ClipId,
        new_timeline_out: RationalTime,
    ) -> MagneticResult {
        let (track_idx, clip_idx) = sequence
            .find_clip(clip_id)
            .expect("trim_tail: clip not found");

        let clip = &sequence.tracks[track_idx].clips[clip_idx];
        let old_timeline_out = clip.timeline_out;
        let old_source_out = clip.source_out;
        let delta = new_timeline_out - old_timeline_out;
        let new_source_out = old_source_out + delta;

        // Apply the trim
        sequence.tracks[track_idx].clips[clip_idx].timeline_out = new_timeline_out;
        sequence.tracks[track_idx].clips[clip_idx].source_out = new_source_out;

        // T-6: on primary tracks, defer ripple to repack_primary so the
        // gapless invariant is enforced consistently with trim_head. The
        // old ad-hoc ripple did happen to produce a gapless result when
        // the track was already gapless going in, but it silently drifted
        // when a pre-existing gap existed (e.g. leftover state from a
        // past inconsistency) and the debug_assert at the bottom would
        // then fire. Non-primary tracks keep the ad-hoc ripple because
        // they're allowed to have gaps and expect position continuity.
        let rippled_clips = if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            repack_primary(sequence)
        } else {
            let mut records = Vec::new();
            let track = &mut sequence.tracks[track_idx];
            for c in &mut track.clips[(clip_idx + 1)..] {
                let old_in = c.timeline_in;
                let old_out = c.timeline_out;
                c.timeline_in += delta;
                c.timeline_out += delta;
                records.push(RippleRecord {
                    clip_id: c.id,
                    old_in,
                    old_out,
                    new_in: c.timeline_in,
                    new_out: c.timeline_out,
                });
            }
            records
        };

        let cascaded_clips = Self::cascade_all_connected(sequence, &rippled_clips);

        #[cfg(debug_assertions)]
        if sequence.tracks[track_idx].magnetic_role == TrackRole::Primary {
            sequence.assert_primary_gapless();
        }

        MagneticResult {
            command: MagneticEditCommand::TrimTail {
                clip_id,
                track_index: track_idx,
                old_timeline_out,
                old_source_out,
                new_timeline_out,
                new_source_out,
                rippled_clips,
                cascaded_clips,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 11. connect_clip
    // ───────────────────────────────────────────────────────────────
    pub fn connect_clip(
        sequence: &mut Sequence,
        clip_id: ClipId,
        anchor_clip_id: ClipId,
        alignment: AnchorAlignment,
    ) -> MagneticResult {
        let (clip_ti, clip_ci) = sequence
            .find_clip(clip_id)
            .expect("connect_clip: clip not found");
        let (anchor_ti, anchor_ci) = sequence
            .find_clip(anchor_clip_id)
            .expect("connect_clip: anchor clip not found");

        let clip_in = sequence.tracks[clip_ti].clips[clip_ci].timeline_in;
        let clip_out = sequence.tracks[clip_ti].clips[clip_ci].timeline_out;
        let anchor_in = sequence.tracks[anchor_ti].clips[anchor_ci].timeline_in;

        // Compute offset based on alignment
        let offset = match alignment {
            AnchorAlignment::StartAligned => clip_in - anchor_in,
            AnchorAlignment::EndAligned => clip_out - anchor_in,
        };

        let old_anchor = sequence.tracks[clip_ti].clips[clip_ci]
            .magnetic_anchor
            .clone();

        let new_anchor = MagneticAnchor {
            anchor_clip_id,
            offset,
            alignment,
        };

        // Position doesn't change on connect
        sequence.tracks[clip_ti].clips[clip_ci].magnetic_anchor = Some(new_anchor.clone());

        MagneticResult {
            command: MagneticEditCommand::ConnectClip {
                clip_id,
                old_anchor,
                new_anchor,
                old_position: clip_in,
                new_position: clip_in,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 12. disconnect_clip
    // ───────────────────────────────────────────────────────────────
    pub fn disconnect_clip(sequence: &mut Sequence, clip_id: ClipId) -> MagneticResult {
        let (track_idx, clip_idx) = sequence
            .find_clip(clip_id)
            .expect("disconnect_clip: clip not found");

        let clip = &mut sequence.tracks[track_idx].clips[clip_idx];
        let old_anchor = clip
            .magnetic_anchor
            .take()
            .expect("disconnect_clip: clip has no anchor");
        let position = clip.timeline_in;

        MagneticResult {
            command: MagneticEditCommand::DisconnectClip {
                clip_id,
                old_anchor,
                position,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 13. cascade_all_connected
    // ───────────────────────────────────────────────────────────────
    pub fn cascade_all_connected(
        sequence: &mut Sequence,
        moved_primary_clips: &[RippleRecord],
    ) -> Vec<CascadeRecord> {
        if moved_primary_clips.is_empty() {
            return Vec::new();
        }

        // Build delta map: clip_id -> delta
        let mut delta_map = std::collections::HashMap::new();
        for r in moved_primary_clips {
            let delta = r.new_in - r.old_in;
            delta_map.insert(r.clip_id, delta);
        }

        let mut cascade_records = Vec::new();

        // For each track, cascade connected clips
        for ti in 0..sequence.tracks.len() {
            let role = sequence.tracks[ti].magnetic_role.clone();
            match role {
                TrackRole::Connected => {
                    // Move individual clips whose anchor moved
                    for ci in 0..sequence.tracks[ti].clips.len() {
                        let clip = &sequence.tracks[ti].clips[ci];
                        if let Some(ref anchor) = clip.magnetic_anchor {
                            if let Some(&delta) = delta_map.get(&anchor.anchor_clip_id) {
                                if delta.value != 0 {
                                    let old_in = clip.timeline_in;
                                    let old_out = clip.timeline_out;
                                    let new_in = old_in + delta;
                                    let new_out = old_out + delta;

                                    let clip_id = clip.id;
                                    sequence.tracks[ti].clips[ci].timeline_in = new_in;
                                    sequence.tracks[ti].clips[ci].timeline_out = new_out;

                                    // Detect collision after move
                                    let collision =
                                        Self::detect_collision_internal(sequence, ti, ci);

                                    cascade_records.push(CascadeRecord {
                                        clip_id,
                                        track_index: ti,
                                        old_in,
                                        old_out,
                                        new_in,
                                        new_out,
                                        collision,
                                    });
                                }
                            }
                        }
                    }
                }
                TrackRole::ConnectedStoryline => {
                    // Move all clips by the storyline anchor's delta
                    if let Some(ref anchor) = sequence.tracks[ti].storyline_anchor {
                        if let Some(&delta) = delta_map.get(&anchor.anchor_clip_id) {
                            if delta.value != 0 {
                                for ci in 0..sequence.tracks[ti].clips.len() {
                                    let clip = &sequence.tracks[ti].clips[ci];
                                    let old_in = clip.timeline_in;
                                    let old_out = clip.timeline_out;
                                    let new_in = old_in + delta;
                                    let new_out = old_out + delta;
                                    let clip_id = clip.id;

                                    sequence.tracks[ti].clips[ci].timeline_in = new_in;
                                    sequence.tracks[ti].clips[ci].timeline_out = new_out;

                                    cascade_records.push(CascadeRecord {
                                        clip_id,
                                        track_index: ti,
                                        old_in,
                                        old_out,
                                        new_in,
                                        new_out,
                                        collision: None,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        cascade_records
    }

    // ───────────────────────────────────────────────────────────────
    // 14. promote_to_storyline (Gap 17: keep absolute positions)
    // ───────────────────────────────────────────────────────────────
    pub fn promote_to_storyline(
        sequence: &mut Sequence,
        track_index: usize,
        anchor_clip_id: ClipId,
    ) -> MagneticResult {
        // Resolve the anchor clip first so we know the rate to use for the
        // empty-track fallback below: the storyline's offset is kept at the
        // anchor's rate even when the track being promoted is empty.
        let anchor_in = sequence
            .find_clip(anchor_clip_id)
            .map(|(ti, ci)| sequence.tracks[ti].clips[ci].timeline_in)
            .expect("promote_to_storyline: anchor clip not found");

        let track = &mut sequence.tracks[track_index];
        let old_role = track.magnetic_role.clone();

        // Save first clip's absolute position (zero at the anchor's rate when
        // the track is empty — see comment above).
        let first_clip_absolute_pos = track
            .clips
            .first()
            .map(|c| c.timeline_in)
            .unwrap_or_else(|| RationalTime::from_frames(0, anchor_in.rate));

        // Save and clear individual clip anchors
        let old_clip_anchors: Vec<Option<MagneticAnchor>> = track
            .clips
            .iter()
            .map(|c| c.magnetic_anchor.clone())
            .collect();
        for clip in &mut track.clips {
            clip.magnetic_anchor = None;
        }

        let offset = first_clip_absolute_pos - anchor_in;

        let storyline_anchor = MagneticAnchor {
            anchor_clip_id,
            offset,
            alignment: AnchorAlignment::StartAligned,
        };

        // Set track role and anchor
        sequence.tracks[track_index].magnetic_role = TrackRole::ConnectedStoryline;
        sequence.tracks[track_index].storyline_anchor = Some(storyline_anchor.clone());

        // Enforce internal gaplessness: repack clips on this track
        let rate = sequence.tracks[track_index]
            .clips
            .first()
            .map(|c| c.timeline_in.rate)
            .unwrap_or(24);
        let mut cursor = first_clip_absolute_pos.rescale_to(rate);
        for clip in &mut sequence.tracks[track_index].clips {
            let dur = clip.timeline_out - clip.timeline_in;
            clip.timeline_in = cursor;
            clip.timeline_out = cursor + dur;
            cursor = clip.timeline_out;
        }

        MagneticResult {
            command: MagneticEditCommand::PromoteToStoryline {
                track_index,
                old_role,
                storyline_anchor,
                old_clip_anchors,
                first_clip_absolute_pos,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 15. break_apart_storyline
    // ───────────────────────────────────────────────────────────────
    pub fn break_apart_storyline(
        sequence: &mut Sequence,
        track_index: usize,
    ) -> MagneticResult {
        let track = &mut sequence.tracks[track_index];
        let old_anchor = track
            .storyline_anchor
            .take()
            .expect("break_apart_storyline: no storyline anchor");

        // Zero at the anchor's rate, so the stored start round-trips through
        // promote's undo path when the track is empty.
        let storyline_start = track
            .clips
            .first()
            .map(|c| c.timeline_in)
            .unwrap_or_else(|| RationalTime::from_frames(0, old_anchor.offset.rate));

        // Set role to Connected, positions stay absolute
        track.magnetic_role = TrackRole::Connected;

        MagneticResult {
            command: MagneticEditCommand::BreakApartStoryline {
                track_index,
                old_anchor,
                storyline_start,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 16. detect_collision
    // ───────────────────────────────────────────────────────────────
    pub fn detect_collision(
        sequence: &Sequence,
        track_index: usize,
        clip_index: usize,
    ) -> Option<CollisionInfo> {
        Self::detect_collision_internal(sequence, track_index, clip_index)
    }

    fn detect_collision_internal(
        sequence: &Sequence,
        track_index: usize,
        clip_index: usize,
    ) -> Option<CollisionInfo> {
        let track = &sequence.tracks[track_index];
        let clip = &track.clips[clip_index];
        let cin = clip.timeline_in;
        let cout = clip.timeline_out;

        for (i, other) in track.clips.iter().enumerate() {
            if i == clip_index || other.kind.is_gap() {
                continue;
            }
            let oin = other.timeline_in;
            let oout = other.timeline_out;
            // Check overlap
            if cin < oout && cout > oin {
                let overlap_start = if cin > oin { cin } else { oin };
                let overlap_end = if cout < oout { cout } else { oout };
                return Some(CollisionInfo {
                    colliding_clip_id: other.id,
                    overlap_start,
                    overlap_end,
                    resolution: CollisionResolution::Warn,
                });
            }
        }
        None
    }

    // ───────────────────────────────────────────────────────────────
    // 17. resolve_collision_push
    // ───────────────────────────────────────────────────────────────
    pub fn resolve_collision_push(
        sequence: &mut Sequence,
        clip_id: ClipId,
        colliding_id: ClipId,
    ) -> MagneticResult {
        let (clip_ti, clip_ci) = sequence
            .find_clip(clip_id)
            .expect("resolve_collision_push: clip not found");
        let clip_out = sequence.tracks[clip_ti].clips[clip_ci].timeline_out;

        let (col_ti, col_ci) = sequence
            .find_clip(colliding_id)
            .expect("resolve_collision_push: colliding clip not found");

        let old_in = sequence.tracks[col_ti].clips[col_ci].timeline_in;
        let old_out = sequence.tracks[col_ti].clips[col_ci].timeline_out;
        let dur = old_out - old_in;
        let new_in = clip_out;
        let new_out = new_in + dur;

        sequence.tracks[col_ti].clips[col_ci].timeline_in = new_in;
        sequence.tracks[col_ti].clips[col_ci].timeline_out = new_out;

        MagneticResult {
            command: MagneticEditCommand::ResolveCollision {
                clip_id: colliding_id,
                track_index: col_ti,
                old_in,
                old_out,
                new_in,
                new_out,
            },
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 18. snap_to_boundary
    // ───────────────────────────────────────────────────────────────
    /// `grid_interval_secs`: optional grid spacing in seconds. When
    /// `Some(secs)` and `secs > 0`, a constant-time post-pass adds a
    /// grid-tick candidate at `round(raw / secs) * secs`.
    /// `snap_config`: optional per-target filter. When `Some`,
    /// candidates of disabled kinds are skipped (e.g. `clips=false`
    /// suppresses ClipBoundary). `None` disables per-target filtering
    /// (every kind is in scope) — used for back-compat when callers
    /// don't yet pass a config.
    pub fn snap_to_boundary(
        sequence: &Sequence,
        raw_time: RationalTime,
        exclude_clip_id: Option<ClipId>,
        grid_interval_secs: Option<f64>,
        snap_config: Option<&klipp_core::SnapConfig>,
    ) -> Option<RationalTime> {
        let points = Self::active_snap_points(sequence, raw_time, exclude_clip_id, grid_interval_secs, snap_config);
        points.into_iter().next().map(|sp| sp.time)
    }

    // ───────────────────────────────────────────────────────────────
    // 19. active_snap_points
    // ───────────────────────────────────────────────────────────────
    /// `grid_interval_secs`: see `snap_to_boundary`. When `Some` and
    /// the resulting grid tick lies within `SNAP_THRESHOLD_FRAMES` of
    /// `raw_time`, a `SnapKind::Grid` candidate is appended. The
    /// implementation is constant-time per call (no enumeration of
    /// grid points across the timeline) — see audit-finding (l).
    /// `snap_config` filters candidate emission by per-target kind;
    /// `None` lets every kind through.
    pub fn active_snap_points(
        sequence: &Sequence,
        raw_time: RationalTime,
        exclude_clip_id: Option<ClipId>,
        grid_interval_secs: Option<f64>,
        snap_config: Option<&klipp_core::SnapConfig>,
    ) -> Vec<SnapPoint> {
        let rate = sequence.fps.rate();
        let threshold = RationalTime::from_frames(SNAP_THRESHOLD_FRAMES, rate);
        let mut points = Vec::new();
        let clips_allowed = snap_config.is_none_or(|c| c.clips);
        let markers_allowed = snap_config.is_none_or(|c| c.markers);
        let grid_allowed = snap_config.is_none_or(|c| c.grid);

        // Check clip boundaries on all tracks. Gated on `clips`
        // per-target flag — flipping "Snap to clips" off in the
        // toolbar dropdown suppresses every emission site for the
        // ClipBoundary kind in this layer (trace-full-chain).
        if clips_allowed {
            for track in &sequence.tracks {
                for clip in &track.clips {
                    if Some(clip.id) == exclude_clip_id {
                        continue;
                    }
                    let diff_in = (raw_time - clip.timeline_in).abs();
                    if diff_in <= threshold {
                        points.push(SnapPoint {
                            time: clip.timeline_in,
                            kind: SnapKind::ClipBoundary,
                        });
                    }
                    let diff_out = (raw_time - clip.timeline_out).abs();
                    if diff_out <= threshold {
                        points.push(SnapPoint {
                            time: clip.timeline_out,
                            kind: SnapKind::ClipBoundary,
                        });
                    }
                }
            }
        }

        // Check markers — both the start position and, for range
        // markers, the end position become snap targets. Gated on
        // `markers`.
        if markers_allowed {
            for marker in &sequence.markers {
                let diff = (raw_time - marker.position).abs();
                if diff <= threshold {
                    points.push(SnapPoint {
                        time: marker.position,
                        kind: SnapKind::Marker,
                    });
                }
                if let Some(dur) = marker.duration {
                    let end = marker.position + dur;
                    let diff_end = (raw_time - end).abs();
                    if diff_end <= threshold {
                        points.push(SnapPoint {
                            time: end,
                            kind: SnapKind::Marker,
                        });
                    }
                }
            }
        }

        // Grid post-pass — constant-time. Snap to round(raw / interval)
        // * interval if within threshold. No enumeration of grid points
        // across the timeline (see audit-finding (l)).
        if grid_allowed {
            if let Some(interval) = grid_interval_secs {
                if interval > 0.0 {
                    let raw_secs = raw_time.to_secs_f64();
                    let nearest = (raw_secs / interval).round() * interval;
                    let grid_time = RationalTime::from_secs_f64(nearest, rate);
                    let diff = (raw_time - grid_time).abs();
                    if diff <= threshold {
                        points.push(SnapPoint {
                            time: grid_time,
                            kind: SnapKind::Grid,
                        });
                    }
                }
            }
        }

        // Sort by distance from raw_time (closest first)
        points.sort_by(|a, b| {
            let da = (raw_time - a.time).abs();
            let db = (raw_time - b.time).abs();
            da.cmp(&db)
        });

        points
    }

    // ───────────────────────────────────────────────────────────────
    // 20. apply_inverse
    // ───────────────────────────────────────────────────────────────
    pub fn apply_inverse(sequence: &mut Sequence, command: &MagneticEditCommand) {
        match command {
            MagneticEditCommand::InsertPrimary {
                inserted_clip,
                track_index,
                rippled_clips,
                cascaded_clips,
                split,
            } => {
                // Undo cascades first
                for cr in cascaded_clips.iter().rev() {
                    sequence.set_clip_position(cr.clip_id, cr.old_in, cr.old_out);
                }
                // Remove the inserted clip
                sequence.tracks[*track_index].remove_clip(inserted_clip.id);
                // Restore rippled clips
                for rr in rippled_clips.iter().rev() {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                unsplit(sequence, *track_index, split);
                repack_primary(sequence);
            }
            MagneticEditCommand::OverwritePrimary {
                inserted_clip,
                track_index,
                overwritten_clips,
                trimmed_clips,
                split_remnants,
            } => {
                // Take out the inserted clip and what the overwrite left in
                // place of the clips it cut (split pieces, and trimmed
                // clips under their own ids), then put the originals back.
                sequence.tracks[*track_index].remove_clip(inserted_clip.id);
                for r in split_remnants {
                    sequence.tracks[*track_index].remove_clip(r.id);
                }
                // Anything else the repack moved goes back where it was.
                let cut = |id: ClipId| overwritten_clips.iter().any(|oc| oc.id == id) || split_remnants.iter().any(|r| r.id == id);
                for rr in trimmed_clips.iter().rev().filter(|rr| !cut(rr.clip_id)) {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                for oc in overwritten_clips {
                    let track = &sequence.tracks[*track_index];
                    let pos = track
                        .clips
                        .iter()
                        .position(|c| c.timeline_in >= oc.timeline_in)
                        .unwrap_or(track.clips.len());
                    sequence.tracks[*track_index].clips.insert(pos, oc.clone());
                }
                repack_primary(sequence);
            }
            MagneticEditCommand::RippleDelete {
                removed_clip,
                track_index,
                position,
                rippled_clips,
                cascaded_clips,
                disconnected_clips,
                disconnected_storylines,
                removed_linked_clip,
            } => {
                // Undo cascades
                for cr in cascaded_clips.iter().rev() {
                    sequence.set_clip_position(cr.clip_id, cr.old_in, cr.old_out);
                }
                // Undo ripples
                for rr in rippled_clips.iter().rev() {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                // Re-insert removed clip
                let pos = (*position).min(sequence.tracks[*track_index].clips.len());
                sequence.tracks[*track_index]
                    .clips
                    .insert(pos, removed_clip.clone());
                // Re-insert linked clip
                if let Some((linked, lt_idx)) = removed_linked_clip {
                    sequence.tracks[*lt_idx].clips.push(linked.clone());
                    sequence.tracks[*lt_idx].sort_clips();
                }
                // Reconnect disconnected clips
                for (cid, _ti, anchor) in disconnected_clips {
                    if let Some((ti2, ci2)) = sequence.find_clip(*cid) {
                        sequence.tracks[ti2].clips[ci2].magnetic_anchor = Some(anchor.clone());
                    }
                }
                // Reconnect disconnected storylines
                for (ti2, anchor) in disconnected_storylines {
                    sequence.tracks[*ti2].magnetic_role = TrackRole::ConnectedStoryline;
                    sequence.tracks[*ti2].storyline_anchor = Some(anchor.clone());
                }
            }
            MagneticEditCommand::Lift {
                removed_clip,
                gap_clip,
                track_index,
                position,
                disconnected_clips,
                disconnected_storylines,
                unlinked_partners,
            } => {
                // Replace gap with original clip
                let pos = *position;
                if pos < sequence.tracks[*track_index].clips.len()
                    && sequence.tracks[*track_index].clips[pos].id == gap_clip.id
                {
                    sequence.tracks[*track_index].clips[pos] = removed_clip.clone();
                }
                // T-1: re-attach clips that were anchored to the lifted
                // clip. Find each by id since the containing track is
                // cached as a hint but the clip may have moved.
                for (cid, _ti, anchor) in disconnected_clips {
                    if let Some((ti2, ci2)) = sequence.find_clip(*cid) {
                        sequence.tracks[ti2].clips[ci2].magnetic_anchor =
                            Some(anchor.clone());
                    }
                }
                // T-1: re-promote storylines that were demoted.
                for (ti2, anchor) in disconnected_storylines {
                    sequence.tracks[*ti2].magnetic_role = TrackRole::ConnectedStoryline;
                    sequence.tracks[*ti2].storyline_anchor = Some(anchor.clone());
                }
                // T-3: restore linked_clip_id on partner clips. The
                // `linked_clip_id` always pointed at the lifted clip's
                // id (`removed_clip.id`), which is stable across undo
                // because we re-insert the same Clip instance.
                for (pid, _ti) in unlinked_partners {
                    if let Some((pti, pci)) = sequence.find_clip(*pid) {
                        sequence.tracks[pti].clips[pci].linked_clip_id =
                            Some(removed_clip.id);
                    }
                }
            }
            MagneticEditCommand::AppendPrimary {
                appended_clip,
                track_index,
            } => {
                sequence.tracks[*track_index].remove_clip(appended_clip.id);
            }
            MagneticEditCommand::MovePrimary {
                clip_id: _,
                remove_command,
                insert_command,
            } => {
                // Undo: reverse insert then reverse remove
                Self::apply_inverse(sequence, insert_command);
                Self::apply_inverse(sequence, remove_command);
                repack_primary(sequence);
            }
            MagneticEditCommand::InsertGap {
                gap_clip,
                track_index,
                rippled_clips,
                cascaded_clips,
                split,
            } => {
                // Same as undo InsertPrimary
                for cr in cascaded_clips.iter().rev() {
                    sequence.set_clip_position(cr.clip_id, cr.old_in, cr.old_out);
                }
                sequence.tracks[*track_index].remove_clip(gap_clip.id);
                for rr in rippled_clips.iter().rev() {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                unsplit(sequence, *track_index, split);
                repack_primary(sequence);
            }
            MagneticEditCommand::Blade {
                original_clip,
                left_clip,
                right_clip,
                track_index,
                position,
            } => {
                // Remove left and right, re-insert original
                sequence.tracks[*track_index].remove_clip(left_clip.id);
                sequence.tracks[*track_index].remove_clip(right_clip.id);
                let pos = (*position).min(sequence.tracks[*track_index].clips.len());
                sequence.tracks[*track_index]
                    .clips
                    .insert(pos, original_clip.clone());
            }
            MagneticEditCommand::TrimHead {
                clip_id,
                track_index: _,
                old_multicam_cuts,
                old_timeline_in,
                old_source_in,
                rippled_clips,
                cascaded_clips,
                ..
            } => {
                for cr in cascaded_clips.iter().rev() {
                    sequence.set_clip_position(cr.clip_id, cr.old_in, cr.old_out);
                }
                for rr in rippled_clips.iter().rev() {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].timeline_in = *old_timeline_in;
                    sequence.tracks[ti].clips[ci].source_in = *old_source_in;
                    sequence.tracks[ti].clips[ci].multicam_cuts = old_multicam_cuts.clone();
                }
            }
            MagneticEditCommand::TrimTail {
                clip_id,
                track_index: _,
                old_timeline_out,
                old_source_out,
                rippled_clips,
                cascaded_clips,
                ..
            } => {
                for cr in cascaded_clips.iter().rev() {
                    sequence.set_clip_position(cr.clip_id, cr.old_in, cr.old_out);
                }
                for rr in rippled_clips.iter().rev() {
                    sequence.set_clip_position(rr.clip_id, rr.old_in, rr.old_out);
                }
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].timeline_out = *old_timeline_out;
                    sequence.tracks[ti].clips[ci].source_out = *old_source_out;
                }
            }
            MagneticEditCommand::ConnectClip {
                clip_id,
                old_anchor,
                old_position,
                ..
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].magnetic_anchor = old_anchor.clone();
                    sequence.tracks[ti].clips[ci].timeline_in = *old_position;
                }
            }
            MagneticEditCommand::DisconnectClip {
                clip_id,
                old_anchor,
                position,
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].magnetic_anchor = Some(old_anchor.clone());
                    sequence.tracks[ti].clips[ci].timeline_in = *position;
                }
            }
            MagneticEditCommand::PromoteToStoryline {
                track_index,
                old_role,
                old_clip_anchors,
                ..
            } => {
                sequence.tracks[*track_index].magnetic_role = old_role.clone();
                sequence.tracks[*track_index].storyline_anchor = None;
                for (i, anchor) in old_clip_anchors.iter().enumerate() {
                    if i < sequence.tracks[*track_index].clips.len() {
                        sequence.tracks[*track_index].clips[i].magnetic_anchor = anchor.clone();
                    }
                }
            }
            MagneticEditCommand::BreakApartStoryline {
                track_index,
                old_anchor,
                storyline_start: _,
            } => {
                sequence.tracks[*track_index].magnetic_role = TrackRole::ConnectedStoryline;
                sequence.tracks[*track_index].storyline_anchor = Some(old_anchor.clone());
            }
            MagneticEditCommand::ResolveCollision {
                clip_id,
                track_index: _,
                old_in,
                old_out,
                ..
            } => {
                sequence.set_clip_position(*clip_id, *old_in, *old_out);
            }
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 21. apply_forward
    // ───────────────────────────────────────────────────────────────
    pub fn apply_forward(sequence: &mut Sequence, command: &MagneticEditCommand) {
        match command {
            MagneticEditCommand::InsertPrimary {
                inserted_clip,
                track_index,
                rippled_clips,
                cascaded_clips,
                split,
            } => {
                insert_or_split(sequence, *track_index, inserted_clip, split);
                // Apply ripples
                for rr in rippled_clips {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                // Apply cascades
                for cr in cascaded_clips {
                    sequence.set_clip_position(cr.clip_id, cr.new_in, cr.new_out);
                }
                repack_primary(sequence);
            }
            MagneticEditCommand::OverwritePrimary {
                inserted_clip,
                track_index,
                overwritten_clips,
                trimmed_clips,
                split_remnants,
            } => {
                // Remove overwritten clips
                for oc in overwritten_clips {
                    sequence.tracks[*track_index].remove_clip(oc.id);
                }
                // T-8: re-insert split remnants (straddle case). The
                // previous implementation relied on the trimmed_clips
                // RippleRecord alone — but that only stores positions,
                // and set_clip_position silently no-opped because the
                // remnant clip_ids were synthesised fresh in
                // overwrite_primary and don't exist after apply_inverse
                // wiped them. Insert at each remnant's timeline_in so
                // repack_primary at the end enforces the gapless
                // invariant.
                for remnant in split_remnants {
                    let track = &sequence.tracks[*track_index];
                    let pos = track
                        .clips
                        .iter()
                        .position(|c| c.timeline_in >= remnant.timeline_in)
                        .unwrap_or(track.clips.len());
                    sequence.tracks[*track_index]
                        .clips
                        .insert(pos, remnant.clone());
                }
                // The trimmed clips came back whole above, as remnants;
                // what else the repack moved goes where it went.
                for rr in trimmed_clips.iter().filter(|rr| !split_remnants.iter().any(|r| r.id == rr.clip_id)) {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                // Insert the new clip
                let track = &sequence.tracks[*track_index];
                let pos = track
                    .clips
                    .iter()
                    .position(|c| c.timeline_in >= inserted_clip.timeline_in)
                    .unwrap_or(track.clips.len());
                sequence.tracks[*track_index]
                    .clips
                    .insert(pos, inserted_clip.clone());
                repack_primary(sequence);
            }
            MagneticEditCommand::RippleDelete {
                removed_clip,
                track_index,
                rippled_clips,
                cascaded_clips,
                disconnected_clips,
                disconnected_storylines,
                removed_linked_clip,
                ..
            } => {
                // Remove clip
                sequence.tracks[*track_index].remove_clip(removed_clip.id);
                // Remove linked
                if let Some((linked, lt_idx)) = removed_linked_clip {
                    sequence.tracks[*lt_idx].remove_clip(linked.id);
                }
                // Disconnect clips
                for (cid, _ti, _anchor) in disconnected_clips {
                    if let Some((ti2, ci2)) = sequence.find_clip(*cid) {
                        sequence.tracks[ti2].clips[ci2].magnetic_anchor = None;
                    }
                }
                // Disconnect storylines
                for (ti2, _anchor) in disconnected_storylines {
                    sequence.tracks[*ti2].storyline_anchor = None;
                    sequence.tracks[*ti2].magnetic_role = TrackRole::Connected;
                }
                // Apply ripples
                for rr in rippled_clips {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                // Apply cascades
                for cr in cascaded_clips {
                    sequence.set_clip_position(cr.clip_id, cr.new_in, cr.new_out);
                }
            }
            MagneticEditCommand::Lift {
                gap_clip,
                track_index,
                position,
                disconnected_clips,
                disconnected_storylines,
                unlinked_partners,
                ..
            } => {
                let pos = *position;
                if pos < sequence.tracks[*track_index].clips.len() {
                    sequence.tracks[*track_index].clips[pos] = gap_clip.clone();
                }
                // Re-apply the disconnection side-effects (T-1 + T-3).
                for (cid, _ti, _anchor) in disconnected_clips {
                    if let Some((ti2, ci2)) = sequence.find_clip(*cid) {
                        sequence.tracks[ti2].clips[ci2].magnetic_anchor = None;
                    }
                }
                for (ti2, _anchor) in disconnected_storylines {
                    sequence.tracks[*ti2].magnetic_role = TrackRole::Connected;
                    sequence.tracks[*ti2].storyline_anchor = None;
                }
                for (pid, _ti) in unlinked_partners {
                    if let Some((pti, pci)) = sequence.find_clip(*pid) {
                        sequence.tracks[pti].clips[pci].linked_clip_id = None;
                    }
                }
            }
            MagneticEditCommand::AppendPrimary {
                appended_clip,
                track_index,
            } => {
                sequence.tracks[*track_index]
                    .clips
                    .push(appended_clip.clone());
            }
            MagneticEditCommand::MovePrimary {
                clip_id: _,
                remove_command,
                insert_command,
            } => {
                Self::apply_forward(sequence, remove_command);
                Self::apply_forward(sequence, insert_command);
                repack_primary(sequence);
            }
            MagneticEditCommand::InsertGap {
                gap_clip,
                track_index,
                rippled_clips,
                cascaded_clips,
                split,
            } => {
                insert_or_split(sequence, *track_index, gap_clip, split);
                for rr in rippled_clips {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                for cr in cascaded_clips {
                    sequence.set_clip_position(cr.clip_id, cr.new_in, cr.new_out);
                }
                repack_primary(sequence);
            }
            MagneticEditCommand::Blade {
                original_clip,
                left_clip,
                right_clip,
                track_index,
                position,
            } => {
                sequence.tracks[*track_index].remove_clip(original_clip.id);
                let pos = (*position).min(sequence.tracks[*track_index].clips.len());
                sequence.tracks[*track_index]
                    .clips
                    .insert(pos, right_clip.clone());
                sequence.tracks[*track_index]
                    .clips
                    .insert(pos, left_clip.clone());
            }
            MagneticEditCommand::TrimHead {
                clip_id,
                new_timeline_in,
                new_source_in,
                new_multicam_cuts,
                rippled_clips,
                cascaded_clips,
                ..
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].timeline_in = *new_timeline_in;
                    sequence.tracks[ti].clips[ci].source_in = *new_source_in;
                    sequence.tracks[ti].clips[ci].multicam_cuts = new_multicam_cuts.clone();
                }
                for rr in rippled_clips {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                for cr in cascaded_clips {
                    sequence.set_clip_position(cr.clip_id, cr.new_in, cr.new_out);
                }
            }
            MagneticEditCommand::TrimTail {
                clip_id,
                new_timeline_out,
                new_source_out,
                rippled_clips,
                cascaded_clips,
                ..
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].timeline_out = *new_timeline_out;
                    sequence.tracks[ti].clips[ci].source_out = *new_source_out;
                }
                for rr in rippled_clips {
                    sequence.set_clip_position(rr.clip_id, rr.new_in, rr.new_out);
                }
                for cr in cascaded_clips {
                    sequence.set_clip_position(cr.clip_id, cr.new_in, cr.new_out);
                }
            }
            MagneticEditCommand::ConnectClip {
                clip_id,
                new_anchor,
                new_position,
                ..
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].magnetic_anchor = Some(new_anchor.clone());
                    sequence.tracks[ti].clips[ci].timeline_in = *new_position;
                }
            }
            MagneticEditCommand::DisconnectClip {
                clip_id,
                position,
                ..
            } => {
                if let Some((ti, ci)) = sequence.find_clip(*clip_id) {
                    sequence.tracks[ti].clips[ci].magnetic_anchor = None;
                    sequence.tracks[ti].clips[ci].timeline_in = *position;
                }
            }
            MagneticEditCommand::PromoteToStoryline {
                track_index,
                storyline_anchor,
                ..
            } => {
                sequence.tracks[*track_index].magnetic_role = TrackRole::ConnectedStoryline;
                sequence.tracks[*track_index].storyline_anchor = Some(storyline_anchor.clone());
                for clip in &mut sequence.tracks[*track_index].clips {
                    clip.magnetic_anchor = None;
                }
            }
            MagneticEditCommand::BreakApartStoryline {
                track_index, ..
            } => {
                sequence.tracks[*track_index].magnetic_role = TrackRole::Connected;
                sequence.tracks[*track_index].storyline_anchor = None;
            }
            MagneticEditCommand::ResolveCollision {
                clip_id,
                new_in,
                new_out,
                ..
            } => {
                sequence.set_clip_position(*clip_id, *new_in, *new_out);
            }
        }
    }

    // ───────────────────────────────────────────────────────────────
    // 22. three_point_edit
    // ───────────────────────────────────────────────────────────────
    pub fn three_point_edit(
        sequence: &mut Sequence,
        mut clip: Clip,
        timeline_point: RationalTime,
        point_is_in: bool,
        mode: EditMode,
        grid_interval_secs: Option<f64>,
        snap_config: Option<&klipp_core::SnapConfig>,
    ) -> MagneticResult {
        let at_time = if point_is_in {
            timeline_point
        } else {
            // Point is out — compute in from clip duration
            let dur = clip.timeline_out - clip.timeline_in;
            timeline_point - dur
        };

        // Set clip position
        let dur = clip.timeline_out - clip.timeline_in;
        clip.timeline_in = at_time;
        clip.timeline_out = at_time + dur;

        match mode {
            EditMode::Insert => Self::insert_primary(sequence, clip, at_time, grid_interval_secs, snap_config),
            EditMode::Overwrite => Self::overwrite_primary(sequence, clip, at_time),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::{Track, TrackKind};
    use klipp_core::{Fps, MediaId, MarkerId, MarkerColor, Resolution, SequenceId};

    const RATE: u32 = 24;

    fn tc(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, RATE)
    }

    fn media_clip(timeline_in: i64, timeline_out: i64) -> Clip {
        let mid = MediaId::new();
        Clip::media(mid, tc(timeline_in), tc(timeline_out), tc(0), tc(timeline_out - timeline_in))
    }

    fn media_clip_with_source(timeline_in: i64, timeline_out: i64, src_in: i64, src_out: i64) -> Clip {
        let mid = MediaId::new();
        Clip::media(mid, tc(timeline_in), tc(timeline_out), tc(src_in), tc(src_out))
    }

    /// Creates a sequence with a primary track containing the given clips.
    /// Clips are specified as (timeline_in_frames, timeline_out_frames).
    fn make_sequence(clip_ranges: &[(i64, i64)]) -> Sequence {
        let mut track = Track::new("Video 1", TrackKind::Video);
        track.magnetic_role = TrackRole::Primary;
        for &(i, o) in clip_ranges {
            track.clips.push(media_clip(i, o));
        }
        Sequence {
            title_nits: crate::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: "Test".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            first_clip_autoset: Default::default(),
            tracks: vec![track],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
        }
    }

    /// Creates a sequence with a primary track and a connected track.
    fn make_sequence_with_connected(
        primary_clips: &[(i64, i64)],
        connected_clips: &[(i64, i64)],
    ) -> Sequence {
        let mut primary = Track::new("Video 1", TrackKind::Video);
        primary.magnetic_role = TrackRole::Primary;
        for &(i, o) in primary_clips {
            primary.clips.push(media_clip(i, o));
        }
        let mut connected = Track::new("Video 2", TrackKind::Video);
        connected.magnetic_role = TrackRole::Connected;
        for &(i, o) in connected_clips {
            connected.clips.push(media_clip(i, o));
        }
        Sequence {
            title_nits: crate::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: "Test".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            first_clip_autoset: Default::default(),
            tracks: vec![connected, primary],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
        }
    }

    // ─────────────────────────────────────────────────────────────
    // insert_primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_insert_primary_middle() {
        // 3 clips: [0..24], [24..48], [48..72]
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let new_clip = media_clip(0, 12); // 12-frame clip

        // Insert at frame 24 (between clip 0 and clip 1)
        let result = MagneticEditor::insert_primary(&mut seq, new_clip, tc(24), None, None);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 4);
        // Clip 0 unchanged
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        // New clip at 24..36
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(36));
        // Old clip 1 shifted right by 12: 36..60
        assert_eq!(track.clips[2].timeline_in, tc(36));
        assert_eq!(track.clips[2].timeline_out, tc(60));
        // Old clip 2 shifted right by 12: 60..84
        assert_eq!(track.clips[3].timeline_in, tc(60));
        assert_eq!(track.clips[3].timeline_out, tc(84));

        // Verify ripple records
        if let MagneticEditCommand::InsertPrimary { rippled_clips, .. } = &result.command {
            // Clips 2 and 3 should have been rippled (the downstream ones)
            assert!(rippled_clips.len() >= 2);
        } else {
            panic!("Expected InsertPrimary command");
        }
    }

    #[test]
    fn test_insert_primary_at_beginning() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let new_clip = media_clip(0, 12);

        MagneticEditor::insert_primary(&mut seq, new_clip, tc(0), None, None);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(12));
        assert_eq!(track.clips[1].timeline_in, tc(12));
        assert_eq!(track.clips[1].timeline_out, tc(36));
        assert_eq!(track.clips[2].timeline_in, tc(36));
        assert_eq!(track.clips[2].timeline_out, tc(60));
    }

    #[test]
    fn test_insert_primary_at_end() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let new_clip = media_clip(0, 12);

        MagneticEditor::insert_primary(&mut seq, new_clip, tc(48), None, None);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[2].timeline_in, tc(48));
        assert_eq!(track.clips[2].timeline_out, tc(60));
    }

    // ─────────────────────────────────────────────────────────────
    // overwrite_primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_overwrite_primary_split() {
        // One 72-frame clip: [0..72]
        let mut seq = make_sequence(&[(0, 72)]);
        let new_clip = media_clip(0, 24);

        // Overwrite frames 24..48 in the middle of the clip
        MagneticEditor::overwrite_primary(&mut seq, new_clip, tc(24));

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        // Left remnant: [0..24]
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        // New clip: [24..48]
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
        // Right remnant: [48..72]
        assert_eq!(track.clips[2].timeline_in, tc(48));
        assert_eq!(track.clips[2].timeline_out, tc(72));
    }

    #[test]
    fn test_overwrite_primary_covers_entire_clip() {
        // Three clips: [0..24], [24..48], [48..72]
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let new_clip = media_clip(0, 24);

        // Overwrite exactly clip 1 at [24..48]
        MagneticEditor::overwrite_primary(&mut seq, new_clip, tc(24));

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
        assert_eq!(track.clips[2].timeline_in, tc(48));
    }

    // ─────────────────────────────────────────────────────────────
    // ripple_delete
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_ripple_delete_middle() {
        // [0..24], [24..48], [48..72]
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip1_id = seq.tracks[0].clips[1].id;

        MagneticEditor::ripple_delete(&mut seq, clip1_id);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        // Clip 2 shifted left by 24 frames
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    /// Issue #251: deleting the middle of a bladed V+A clip closed the
    /// picture track but only removed the audio, leaving every later
    /// sound late by the deleted length.
    #[test]
    fn test_ripple_delete_ripples_the_linked_partners_track() {
        // V (primary) and A (connected), three linked segments each.
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let mut audio = Track::new("Audio 1", TrackKind::Audio);
        audio.magnetic_role = TrackRole::Connected;
        for &(i, o) in &[(0, 24), (24, 48), (48, 72)] {
            audio.clips.push(media_clip(i, o));
        }
        for k in 0..3 {
            let (v, a) = (seq.tracks[0].clips[k].id, audio.clips[k].id);
            seq.tracks[0].clips[k].linked_clip_id = Some(a);
            audio.clips[k].linked_clip_id = Some(v);
        }
        seq.tracks.push(audio);
        let before = seq.clone();
        let middle = seq.tracks[0].clips[1].id;

        let result = MagneticEditor::ripple_delete(&mut seq, middle);

        let spans = |seq: &Sequence, ti: usize| -> Vec<(RationalTime, RationalTime)> {
            seq.tracks[ti].clips.iter().map(|c| (c.timeline_in, c.timeline_out)).collect()
        };
        assert_eq!(spans(&seq, 0), vec![(tc(0), tc(24)), (tc(24), tc(48))]);
        assert_eq!(spans(&seq, 1), spans(&seq, 0), "the audio did not ripple with its picture");

        // One undo puts both tracks back, and redo closes both again.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(spans(&seq, 0), spans(&before, 0));
        assert_eq!(spans(&seq, 1), spans(&before, 1));
        MagneticEditor::apply_forward(&mut seq, &result.command);
        assert_eq!(spans(&seq, 1), vec![(tc(0), tc(24)), (tc(24), tc(48))]);

        // Deleting the audio half closes the primary track too.
        let mut seq = before.clone();
        let middle_audio = seq.tracks[1].clips[1].id;
        MagneticEditor::ripple_delete(&mut seq, middle_audio);
        assert_eq!(spans(&seq, 0), vec![(tc(0), tc(24)), (tc(24), tc(48))]);
        assert_eq!(spans(&seq, 1), spans(&seq, 0));
    }

    #[test]
    fn test_ripple_delete_first() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip0_id = seq.tracks[0].clips[0].id;

        MagneticEditor::ripple_delete(&mut seq, clip0_id);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    #[test]
    fn ripple_delete_clears_third_party_linked_clip_ids() {
        // Setup: primary track [A][B][C]. A connected track holds D
        // anchored to A, with D.linked_clip_id pointing at A. After
        // ripple_delete(A), D's linked_clip_id was previously left
        // dangling (T-3): A is gone but D still claims to be linked
        // to it.
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let a_id = seq.tracks[0].clips[0].id;

        // Add a connected track with a clip D pointing at A.
        let mut connected = Track::new("Video 2", TrackKind::Video);
        connected.magnetic_role = TrackRole::Connected;
        let mut d_clip = media_clip(0, 12);
        d_clip.linked_clip_id = Some(a_id);
        connected.clips.push(d_clip);
        seq.tracks.push(connected);

        MagneticEditor::ripple_delete(&mut seq, a_id);

        // D survives but its dangling link to A is cleared.
        let surviving = &seq.tracks[1].clips[0];
        assert_eq!(
            surviving.linked_clip_id, None,
            "ripple_delete must purge linked_clip_id pointing at the removed clip"
        );
    }

    #[test]
    fn ripple_delete_clears_links_to_partner_clip_too() {
        // Symmetric pair A↔B: A.linked_clip_id = B, B.linked_clip_id = A.
        // ripple_delete(A) removes both A and B (existing behaviour).
        // A third clip C with C.linked_clip_id = B was previously
        // also left dangling because the cleanup only ran on A's id.
        // Closes — partner-id sweep.
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let a_id = seq.tracks[0].clips[0].id;

        // Audio track with B linked to A.
        let mut audio = Track::new("Audio 1", TrackKind::Audio);
        let mut b_clip = media_clip(0, 24);
        b_clip.linked_clip_id = Some(a_id);
        audio.clips.push(b_clip);
        let b_id = audio.clips[0].id;
        // Make A point back at B (symmetric link).
        seq.tracks[0].clips[0].linked_clip_id = Some(b_id);
        seq.tracks.push(audio);

        // Third clip C with linked_clip_id pointing at B.
        let mut connected = Track::new("Video 2", TrackKind::Video);
        connected.magnetic_role = TrackRole::Connected;
        let mut c_clip = media_clip(0, 12);
        c_clip.linked_clip_id = Some(b_id);
        connected.clips.push(c_clip);
        seq.tracks.push(connected);

        MagneticEditor::ripple_delete(&mut seq, a_id);

        // A and B are gone; C survives with link cleared.
        let c_surviving = &seq.tracks[2].clips[0];
        assert_eq!(
            c_surviving.linked_clip_id, None,
            "ripple_delete must also purge links pointing at the partner clip"
        );
    }

    #[test]
    fn purge_dangling_linked_clip_ids_only_clears_matched_ids() {
        // Direct unit test on Sequence::purge_dangling_linked_clip_ids:
        // unrelated linked_clip_id values must round-trip unchanged
        // (a future "let me simplify the contains check" must not
        // wipe links to surviving clips).
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let survivor_id = seq.tracks[0].clips[1].id;
        let removed_id = seq.tracks[0].clips[0].id;

        // Clip with link to a surviving clip — must not be cleared.
        seq.tracks[0].clips[1].linked_clip_id = Some(survivor_id);

        seq.purge_dangling_linked_clip_ids(&[removed_id]);

        assert_eq!(
            seq.tracks[0].clips[1].linked_clip_id, Some(survivor_id),
            "purge must not touch links to surviving clips"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // lift
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_lift_middle() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip1_id = seq.tracks[0].clips[1].id;

        MagneticEditor::lift(&mut seq, clip1_id);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        // Gap placeholder at position 1
        assert!(track.clips[1].kind.is_gap());
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
        // Nothing else moved
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[2].timeline_in, tc(48));
        assert_eq!(track.clips[2].timeline_out, tc(72));
    }

    // ─────────────────────────────────────────────────────────────
    // append_primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_append_primary() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let new_clip = media_clip(0, 36);

        MagneticEditor::append_primary(&mut seq, new_clip);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[2].timeline_in, tc(48));
        assert_eq!(track.clips[2].timeline_out, tc(84));
    }

    #[test]
    fn test_append_primary_empty() {
        let mut seq = make_sequence(&[]);
        let new_clip = media_clip(0, 24);

        MagneticEditor::append_primary(&mut seq, new_clip);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
    }

    // ─────────────────────────────────────────────────────────────
    // move_primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_move_primary_forward() {
        // [A: 0..24], [B: 24..48], [C: 48..72]
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip_a_id = seq.tracks[0].clips[0].id;

        // Move clip A to position after clip C (to_time=72 -> last position)
        MagneticEditor::move_primary(&mut seq, clip_a_id, tc(72));

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        // After repack: B at 0, C at 24, A at 48
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
        // A moved to end
        assert_eq!(track.clips[2].id, clip_a_id);
        assert_eq!(track.clips[2].timeline_in, tc(48));
        assert_eq!(track.clips[2].timeline_out, tc(72));
    }

    #[test]
    fn test_move_primary_backward() {
        // [A: 0..24], [B: 24..48], [C: 48..72]
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip_c_id = seq.tracks[0].clips[2].id;

        // Move clip C to position 0
        MagneticEditor::move_primary(&mut seq, clip_c_id, tc(0));

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        // After repack: C at 0, A at 24, B at 48
        assert_eq!(track.clips[0].id, clip_c_id);
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
    }

    // ─────────────────────────────────────────────────────────────
    // blade
    // ─────────────────────────────────────────────────────────────
    /// A 10 s multicam primary on the storyline, cut to angle 1 at 2 s and
    /// 6 s and back at 4 s and 8 s; its source is the timeline's.
    fn cut_primary() -> Sequence {
        let mut seq = make_sequence(&[]);
        let mut c = media_clip_with_source(0, 240, 0, 240);
        (c.multicam_group, c.multicam_angle) = (Some(klipp_core::GroupId::new()), 0);
        c.multicam_cuts = vec![(2.0, 1), (4.0, 0), (6.0, 1), (8.0, 0)];
        seq.tracks[0].clips = vec![c];
        seq
    }

    /// The angle a moment of the file is cut to, read through whichever
    /// piece of the primary shows it: `None` where none does.
    fn angle_of_picture(seq: &Sequence, s: f64) -> Option<u8> {
        seq.tracks[0].clips.iter().filter(|c| c.multicam_group.is_some()).find(|c| s >= c.source_in.to_secs_f64() && s < c.source_out.to_secs_f64()).map(|c| {
            let rel = s - c.source_in.to_secs_f64();
            c.multicam_cuts.iter().rfind(|(at, _)| *at <= rel + 1e-9).map_or(0, |(_, a)| *a)
        })
    }

    /// #2418 follow-up: the storyline's blade, insert, overwrite and head
    /// trim leave every moment of the picture cut to the angle it was;
    /// the head trim's undo puts the cut list back as it was.
    #[test]
    fn storyline_edits_keep_each_angle_on_its_picture() {
        let picture: Vec<f64> = (0..20).map(|k| k as f64 * 0.5 + 0.25).collect();
        let before = cut_primary();
        let check = |after: &Sequence, what: &str| {
            for &s in &picture {
                if let Some(now) = angle_of_picture(after, s) {
                    assert_eq!(Some(now), angle_of_picture(&before, s), "{what}: the picture at {s} s of the file");
                }
            }
        };
        let id = before.tracks[0].clips[0].id;

        let mut seq = before.clone();
        MagneticEditor::blade(&mut seq, id, tc(120)).unwrap();
        check(&seq, "blade");

        let mut seq = before.clone();
        MagneticEditor::insert_primary(&mut seq, media_clip(0, 24), tc(120), None, None);
        check(&seq, "insert");

        let mut seq = before.clone();
        MagneticEditor::overwrite_primary(&mut seq, media_clip(0, 48), tc(72));
        check(&seq, "overwrite");
        assert!(angle_of_picture(&seq, 3.5).is_none(), "what the overwrite covered is gone");

        let mut seq = before.clone();
        let trim = MagneticEditor::trim_head(&mut seq, id, tc(60));
        check(&seq, "head trim");
        MagneticEditor::apply_inverse(&mut seq, &trim.command);
        assert_eq!(seq.tracks[0].clips[0].multicam_cuts, before.tracks[0].clips[0].multicam_cuts, "undo puts the cuts back");
    }

    #[test]
    fn test_blade() {
        // One clip [0..48] with source [0..48]
        let mut seq = make_sequence(&[(0, 48)]);
        let clip_id = seq.tracks[0].clips[0].id;

        MagneticEditor::blade(&mut seq, clip_id, tc(24)).unwrap();

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 2);
        // Left half: [0..24], source [0..24]
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[0].source_in, tc(0));
        assert_eq!(track.clips[0].source_out, tc(24));
        // Right half: [24..48], source [24..48]
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
        assert_eq!(track.clips[1].source_in, tc(24));
        assert_eq!(track.clips[1].source_out, tc(48));
        // Right half has no magnetic anchor
        assert!(track.clips[1].magnetic_anchor.is_none());
    }

    /// #429: the magnetic blade leaves no fade at the cut either, so it
    /// and the legacy blade agree about what a split sounds like.
    #[test]
    fn test_blade_keeps_each_half_only_its_own_outer_fade() {
        let mut seq = make_sequence(&[(0, 96)]);
        seq.tracks[0].clips[0].fade_in_secs = 0.5;
        seq.tracks[0].clips[0].fade_out_secs = 0.75;
        let clip_id = seq.tracks[0].clips[0].id;

        let result = MagneticEditor::blade(&mut seq, clip_id, tc(48)).unwrap();
        let MagneticEditCommand::Blade { left_clip, right_clip, .. } = &result.command else { panic!("a blade") };
        assert_eq!((left_clip.fade_in_secs, left_clip.fade_out_secs), (0.5, 0.0), "the left half does not fade out at the cut");
        assert_eq!((right_clip.fade_in_secs, right_clip.fade_out_secs), (0.0, 0.75), "the right half does not fade in at the cut");

        let track = &seq.tracks[0];
        assert_eq!((track.clips[0].fade_in_secs, track.clips[0].fade_out_secs), (0.5, 0.0));
        assert_eq!((track.clips[1].fade_in_secs, track.clips[1].fade_out_secs), (0.0, 0.75));
    }

    /// The magnetic blade shortens an over-long fade to its half too, so
    /// it and the legacy blade still agree.
    #[test]
    fn test_blade_shortens_a_fade_that_outlasts_its_half() {
        let mut seq = make_sequence(&[(0, 96)]);
        seq.tracks[0].clips[0].fade_in_secs = 3.0;
        seq.tracks[0].clips[0].fade_out_secs = 3.5;
        let clip_id = seq.tracks[0].clips[0].id;

        MagneticEditor::blade(&mut seq, clip_id, tc(24)).unwrap();
        let track = &seq.tracks[0];
        assert_eq!((track.clips[0].fade_in_secs, track.clips[0].fade_out_secs), (1.0, 0.0), "the left half has a second to fade in over");
        assert_eq!((track.clips[1].fade_in_secs, track.clips[1].fade_out_secs), (0.0, 3.0), "the right half has three seconds to fade out over");
    }

    #[test]
    fn test_blade_on_edge_or_outside_is_a_noop() {
        let mut seq = make_sequence(&[(0, 48)]);
        let clip_id = seq.tracks[0].clips[0].id;

        assert!(MagneticEditor::blade(&mut seq, clip_id, tc(0)).is_none());
        assert!(MagneticEditor::blade(&mut seq, clip_id, tc(48)).is_none());
        assert!(MagneticEditor::blade(&mut seq, clip_id, tc(60)).is_none());
        assert!(MagneticEditor::blade(&mut seq, ClipId::new(), tc(24)).is_none());
        assert_eq!(seq.tracks[0].clips.len(), 1);
    }

    #[test]
    fn test_blade_preserves_source_offsets() {
        // Clip with non-zero source_in: timeline [0..48], source [12..60]
        let mut seq = make_sequence(&[]);
        let clip = media_clip_with_source(0, 48, 12, 60);
        seq.tracks[0].clips.push(clip);
        let clip_id = seq.tracks[0].clips[0].id;

        MagneticEditor::blade(&mut seq, clip_id, tc(24)).unwrap();

        let track = &seq.tracks[0];
        // Left: source [12..36]
        assert_eq!(track.clips[0].source_in, tc(12));
        assert_eq!(track.clips[0].source_out, tc(36));
        // Right: source [36..60]
        assert_eq!(track.clips[1].source_in, tc(36));
        assert_eq!(track.clips[1].source_out, tc(60));
    }

    // ─────────────────────────────────────────────────────────────
    // trim_head on primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_trim_head_primary() {
        // [0..48], [48..72]
        let mut seq = make_sequence(&[(0, 48), (48, 72)]);
        let clip0_id = seq.tracks[0].clips[0].id;

        // Trim head of clip 0 to frame 12 (shrink by 12 frames)
        MagneticEditor::trim_head(&mut seq, clip0_id, tc(12));

        let track = &seq.tracks[0];
        // After repack: clip 0 is now 36 frames [0..36], clip 1 is [36..60]
        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].timeline_out, tc(36));
        assert_eq!(track.clips[1].timeline_in, tc(36));
        assert_eq!(track.clips[1].timeline_out, tc(60));
    }

    #[test]
    fn test_trim_head_primary_gapless() {
        // Verify gapless is maintained
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip1_id = seq.tracks[0].clips[1].id;

        // Trim head of middle clip inward by 6 frames
        MagneticEditor::trim_head(&mut seq, clip1_id, tc(30));

        let track = &seq.tracks[0];
        // Clip 0 should still end where clip 1 starts (gapless)
        assert_eq!(track.clips[0].timeline_out, track.clips[1].timeline_in);
        assert_eq!(track.clips[1].timeline_out, track.clips[2].timeline_in);
    }

    // ─────────────────────────────────────────────────────────────
    // trim_tail on primary
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_trim_tail_primary() {
        // [0..48], [48..72]
        let mut seq = make_sequence(&[(0, 48), (48, 72)]);
        let clip0_id = seq.tracks[0].clips[0].id;

        // Trim tail of clip 0 to frame 36 (shrink by 12)
        MagneticEditor::trim_tail(&mut seq, clip0_id, tc(36));

        let track = &seq.tracks[0];
        assert_eq!(track.clips[0].timeline_out, tc(36));
        // Clip 1 rippled left by 12
        assert_eq!(track.clips[1].timeline_in, tc(36));
        assert_eq!(track.clips[1].timeline_out, tc(60));
    }

    #[test]
    fn test_trim_tail_expand() {
        // [0..24], [24..48]
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let clip0_id = seq.tracks[0].clips[0].id;

        // Expand tail of clip 0 by 12 frames
        MagneticEditor::trim_tail(&mut seq, clip0_id, tc(36));

        let track = &seq.tracks[0];
        assert_eq!(track.clips[0].timeline_out, tc(36));
        // Clip 1 rippled right by 12
        assert_eq!(track.clips[1].timeline_in, tc(36));
        assert_eq!(track.clips[1].timeline_out, tc(60));
    }

    // ─────────────────────────────────────────────────────────────
    // cascade
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_cascade_connected_clip_follows_anchor() {
        // Primary: [0..24], [24..48]
        // Connected: [0..24] anchored to primary clip 0
        let mut seq = make_sequence_with_connected(&[(0, 24), (24, 48)], &[(0, 24)]);
        let primary_clip0_id = seq.tracks[1].clips[0].id;
        let connected_clip_id = seq.tracks[0].clips[0].id;

        // Connect the clip to primary clip 0
        MagneticEditor::connect_clip(
            &mut seq,
            connected_clip_id,
            primary_clip0_id,
            AnchorAlignment::StartAligned,
        );

        // Now insert a clip at the beginning of primary, pushing everything right
        let new_clip = media_clip(0, 12);
        MagneticEditor::insert_primary(&mut seq, new_clip, tc(0), None, None);

        // Primary clip 0 moved from [0..24] to [12..36]
        // Connected clip should follow: from [0..24] to [12..36]
        let connected = &seq.tracks[0].clips[0];
        assert_eq!(connected.timeline_in, tc(12));
        assert_eq!(connected.timeline_out, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // connect / disconnect
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_connect_clip() {
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[(12, 36)]);
        let anchor_id = seq.tracks[1].clips[0].id;
        let clip_id = seq.tracks[0].clips[0].id;

        MagneticEditor::connect_clip(
            &mut seq,
            clip_id,
            anchor_id,
            AnchorAlignment::StartAligned,
        );

        let clip = &seq.tracks[0].clips[0];
        assert!(clip.magnetic_anchor.is_some());
        let anchor = clip.magnetic_anchor.as_ref().unwrap();
        assert_eq!(anchor.anchor_clip_id, anchor_id);
        // Offset = clip.timeline_in - anchor.timeline_in = 12 - 0 = 12
        assert_eq!(anchor.offset, tc(12));
        // Position unchanged
        assert_eq!(clip.timeline_in, tc(12));
        assert_eq!(clip.timeline_out, tc(36));
    }

    #[test]
    fn test_disconnect_clip() {
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[(12, 36)]);
        let anchor_id = seq.tracks[1].clips[0].id;
        let clip_id = seq.tracks[0].clips[0].id;

        // Connect first
        MagneticEditor::connect_clip(
            &mut seq,
            clip_id,
            anchor_id,
            AnchorAlignment::StartAligned,
        );
        assert!(seq.tracks[0].clips[0].magnetic_anchor.is_some());

        // Disconnect
        MagneticEditor::disconnect_clip(&mut seq, clip_id);

        assert!(seq.tracks[0].clips[0].magnetic_anchor.is_none());
        // Position unchanged
        assert_eq!(seq.tracks[0].clips[0].timeline_in, tc(12));
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // promote / break_apart storyline
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_promote_to_storyline() {
        let mut seq = make_sequence_with_connected(&[(0, 48)], &[(12, 36), (36, 60)]);
        let anchor_id = seq.tracks[1].clips[0].id; // primary clip

        // Promote track 0 (connected) to storyline
        MagneticEditor::promote_to_storyline(&mut seq, 0, anchor_id);

        assert_eq!(seq.tracks[0].magnetic_role, TrackRole::ConnectedStoryline);
        assert!(seq.tracks[0].storyline_anchor.is_some());
        // Individual clip anchors cleared
        for clip in &seq.tracks[0].clips {
            assert!(clip.magnetic_anchor.is_none());
        }
        // Positions stay absolute
        assert_eq!(seq.tracks[0].clips[0].timeline_in, tc(12));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(36));
    }

    #[test]
    fn test_break_apart_storyline() {
        let mut seq = make_sequence_with_connected(&[(0, 48)], &[(12, 36), (36, 60)]);
        let anchor_id = seq.tracks[1].clips[0].id;

        MagneticEditor::promote_to_storyline(&mut seq, 0, anchor_id);
        assert_eq!(seq.tracks[0].magnetic_role, TrackRole::ConnectedStoryline);

        MagneticEditor::break_apart_storyline(&mut seq, 0);

        assert_eq!(seq.tracks[0].magnetic_role, TrackRole::Connected);
        assert!(seq.tracks[0].storyline_anchor.is_none());
        // Positions stay absolute
        assert_eq!(seq.tracks[0].clips[0].timeline_in, tc(12));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // collision detection
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_detect_collision() {
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[]);
        // Add two overlapping clips on connected track
        let clip_a = media_clip(0, 24);
        let clip_b = media_clip(12, 36); // overlaps with clip_a at [12..24]
        seq.tracks[0].clips.push(clip_a);
        seq.tracks[0].clips.push(clip_b);

        let collision = MagneticEditor::detect_collision(&seq, 0, 0);
        assert!(collision.is_some());
        let col = collision.unwrap();
        assert_eq!(col.overlap_start, tc(12));
        assert_eq!(col.overlap_end, tc(24));
    }

    #[test]
    fn test_no_collision() {
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[]);
        let clip_a = media_clip(0, 24);
        let clip_b = media_clip(24, 48); // no overlap
        seq.tracks[0].clips.push(clip_a);
        seq.tracks[0].clips.push(clip_b);

        let collision = MagneticEditor::detect_collision(&seq, 0, 0);
        assert!(collision.is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // snap
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_snap_within_threshold() {
        let seq = make_sequence(&[(0, 24), (24, 48)]);
        // 3 frames away from boundary at 24 — should snap
        let snapped = MagneticEditor::snap_to_boundary(&seq, tc(27), None, None, None);
        assert_eq!(snapped, Some(tc(24)));
    }

    #[test]
    fn test_snap_beyond_threshold() {
        let seq = make_sequence(&[(0, 24), (24, 48)]);
        // 10 frames away — beyond 8 frame threshold
        let snapped = MagneticEditor::snap_to_boundary(&seq, tc(34), None, None, None);
        assert_eq!(snapped, None);
    }

    #[test]
    fn test_snap_excludes_clip() {
        let seq = make_sequence(&[(0, 24), (24, 48)]);
        let _clip0_id = seq.tracks[0].clips[0].id;
        // 3 frames from clip 0's out-point at 24 — but exclude clip 0
        // Clip 1's in-point is also at 24, so it should still snap
        let clip1_id = seq.tracks[0].clips[1].id;
        let snapped = MagneticEditor::snap_to_boundary(&seq, tc(27), Some(clip1_id), None, None);
        // Should snap to clip 0's boundary
        assert_eq!(snapped, Some(tc(24)));

        // Exclude both clips by creating a sequence with only one clip
        // and exclude that clip — no snap targets
        let seq2 = make_sequence(&[(0, 24)]);
        let only_clip_id = seq2.tracks[0].clips[0].id;
        // 3 frames from boundary at 0
        let snapped2 = MagneticEditor::snap_to_boundary(&seq2, tc(3), Some(only_clip_id), None, None);
        // No snap targets (the only clip is excluded)
        assert_eq!(snapped2, None);
    }

    #[test]
    fn test_snap_to_marker() {
        let mut seq = make_sequence(&[(0, 48)]);
        seq.markers.push(crate::marker::Marker { review: None,
            id: MarkerId::new(),
            position: tc(36),
            label: "Mark".to_string(),
            color: MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });

        let snapped = MagneticEditor::snap_to_boundary(&seq, tc(33), None, None, None);
        assert_eq!(snapped, Some(tc(36)));
    }

    // Pinned: passing `None` for grid_interval_secs preserves the
    // pre-grid-snap snap behaviour byte-for-byte. If a future refactor
    // accidentally enables grid snap by default, this test fails.
    #[test]
    fn test_grid_snap_off_is_back_compat() {
        let seq = make_sequence(&[(0, 24), (24, 48)]);
        // 5 frames past the boundary at 24 — beyond clip-edge threshold
        // (3 frame distance from 27 was within threshold above; 5
        // frames is still within the 8-frame threshold so this is
        // already a clip-edge hit). What we actually want to verify:
        // the result is identical to the pre-feature impl when
        // grid_interval_secs is None. So compare with a query that
        // has NO clip-edge hit.
        let snapped_no_grid = MagneticEditor::snap_to_boundary(&seq, tc(35), None, None, None);
        // 35 is 11 frames from 24 (out of threshold) and 13 from 48
        // (out of threshold). With None grid: no snap.
        assert_eq!(snapped_no_grid, None);
    }

    // Pinned: when grid_interval_secs is Some, grid candidates appear.
    // Sequence is 24 fps so 1 second = 24 frames. Querying at frame 35
    // (= 1.458 s) with a 1.0 s grid: nearest tick is 1.0 s = frame 24.
    // 35 - 24 = 11 frames > 8-frame threshold, so no grid hit.
    // Query at frame 26 (= 1.083 s) with a 1.0 s grid: nearest tick
    // is 1.0 s = frame 24. 26 - 24 = 2 frames < threshold → hit.
    // BUT clip-edge at 24 also hits (2 frames). Distance ties go to
    // first-pushed which is clip-edge, so the grid candidate is in
    // the points list but sorted second.
    #[test]
    fn test_grid_snap_on_emits_grid_candidate() {
        let seq = make_sequence(&[(0, 24)]); // single clip, ends at frame 24
        // Query at frame 50 (= ~2.083 s) — well past any clip edge.
        // With 1-second grid, nearest is 2.0s = frame 48. Distance
        // 50-48 = 2 frames, within threshold → grid hit.
        let snapped = MagneticEditor::snap_to_boundary(&seq, tc(50), None, Some(1.0), None);
        assert_eq!(snapped, Some(tc(48)));

        // And without the grid hint, no snap (clip ends at 24, far
        // beyond threshold).
        let snapped_off = MagneticEditor::snap_to_boundary(&seq, tc(50), None, None, None);
        assert_eq!(snapped_off, None);
    }

    // ─────────────────────────────────────────────────────────────
    // apply_inverse round-trip
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_apply_inverse_insert_primary() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);

        // Save original state
        let orig_clip0_in = seq.tracks[0].clips[0].timeline_in;
        let orig_clip0_out = seq.tracks[0].clips[0].timeline_out;
        let orig_clip1_in = seq.tracks[0].clips[1].timeline_in;
        let orig_clip1_out = seq.tracks[0].clips[1].timeline_out;
        let orig_clip2_in = seq.tracks[0].clips[2].timeline_in;
        let orig_clip2_out = seq.tracks[0].clips[2].timeline_out;
        let orig_clip0_id = seq.tracks[0].clips[0].id;
        let orig_clip1_id = seq.tracks[0].clips[1].id;
        let orig_clip2_id = seq.tracks[0].clips[2].id;

        let new_clip = media_clip(0, 12);
        let result = MagneticEditor::insert_primary(&mut seq, new_clip, tc(24), None, None);

        // Verify insertion happened
        assert_eq!(seq.tracks[0].clips.len(), 4);

        // Apply inverse
        MagneticEditor::apply_inverse(&mut seq, &result.command);

        // Should restore to 3 clips
        assert_eq!(seq.tracks[0].clips.len(), 3);

        // Find original clips and verify positions restored
        let c0 = seq.tracks[0].clips.iter().find(|c| c.id == orig_clip0_id).unwrap();
        assert_eq!(c0.timeline_in, orig_clip0_in);
        assert_eq!(c0.timeline_out, orig_clip0_out);

        let c1 = seq.tracks[0].clips.iter().find(|c| c.id == orig_clip1_id).unwrap();
        assert_eq!(c1.timeline_in, orig_clip1_in);
        assert_eq!(c1.timeline_out, orig_clip1_out);

        let c2 = seq.tracks[0].clips.iter().find(|c| c.id == orig_clip2_id).unwrap();
        assert_eq!(c2.timeline_in, orig_clip2_in);
        assert_eq!(c2.timeline_out, orig_clip2_out);
    }

    /// An insert inside a clip splits it; undo must leave the clip whole
    /// again, not its two pieces, and redo must split it the same way.
    #[test]
    fn undo_of_a_splitting_insert_puts_the_clip_back_whole() {
        let bounds = |seq: &Sequence| seq.tracks[0].clips.iter().map(|c| (c.id, c.timeline_in, c.timeline_out, c.source_in, c.source_out)).collect::<Vec<_>>();
        for gap in [false, true] {
            let mut seq = make_sequence(&[(0, 96)]);
            let before = bounds(&seq);
            let result = if gap {
                MagneticEditor::insert_gap(&mut seq, tc(24), tc(48), None, None)
            } else {
                MagneticEditor::insert_primary(&mut seq, media_clip(0, 24), tc(48), None, None)
            };
            assert_eq!(seq.tracks[0].clips.len(), 3);
            let after = bounds(&seq);

            MagneticEditor::apply_inverse(&mut seq, &result.command);
            assert_eq!(bounds(&seq), before, "gap {gap}: undo");
            MagneticEditor::apply_forward(&mut seq, &result.command);
            assert_eq!(bounds(&seq), after, "gap {gap}: redo");
            MagneticEditor::apply_inverse(&mut seq, &result.command);
            assert_eq!(bounds(&seq), before, "gap {gap}: undo after redo");
        }
    }

    #[test]
    fn test_apply_inverse_ripple_delete() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip1_id = seq.tracks[0].clips[1].id;

        let result = MagneticEditor::ripple_delete(&mut seq, clip1_id);

        // 2 clips after delete
        assert_eq!(seq.tracks[0].clips.len(), 2);

        // Apply inverse
        MagneticEditor::apply_inverse(&mut seq, &result.command);

        // Should restore to 3 clips
        assert_eq!(seq.tracks[0].clips.len(), 3);
        assert_eq!(seq.tracks[0].clips[1].id, clip1_id);
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(24));
        assert_eq!(seq.tracks[0].clips[1].timeline_out, tc(48));
    }

    // ─────────────────────────────────────────────────────────────
    // T-1: lift clears + restores magnetic_anchor on external clips
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_lift_clears_dangling_external_anchor_and_restores_on_undo() {
        // Primary [A, B], Connected [X anchored to A]. Lift A. X's
        // anchor must be cleared (otherwise it dangles at A's id).
        // Undo must reattach X to A.
        let mut seq = make_sequence_with_connected(&[(0, 24), (24, 48)], &[(0, 24)]);
        let primary_a_id = seq.tracks[1].clips[0].id;
        let connected_x_id = seq.tracks[0].clips[0].id;

        MagneticEditor::connect_clip(
            &mut seq,
            connected_x_id,
            primary_a_id,
            AnchorAlignment::StartAligned,
        );
        assert!(
            seq.tracks[0].clips[0].magnetic_anchor.is_some(),
            "pre-condition: X should be anchored"
        );

        let result = MagneticEditor::lift(&mut seq, primary_a_id);

        // X's anchor is cleared.
        assert!(
            seq.tracks[0].clips[0].magnetic_anchor.is_none(),
            "T-1: lift must clear dangling external anchor"
        );

        // Undo restores it.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert!(
            seq.tracks[0].clips[0].magnetic_anchor.is_some(),
            "T-1 undo: lift apply_inverse must restore external anchor"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // T-2: blade left half inherits id, both halves keep anchor
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_blade_left_half_inherits_original_id() {
        let mut seq = make_sequence(&[(0, 48)]);
        let original_id = seq.tracks[0].clips[0].id;

        MagneticEditor::blade(&mut seq, original_id, tc(24)).unwrap();

        // Left half (index 0) must keep the original id so external
        // anchors pointing at it continue to resolve. Right half gets
        // a fresh id.
        assert_eq!(
            seq.tracks[0].clips[0].id, original_id,
            "T-2: blade left half must inherit the original ClipId"
        );
        assert_ne!(
            seq.tracks[0].clips[1].id, original_id,
            "T-2: blade right half must have a fresh ClipId"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // T-3: lift clears + restores linked_clip_id on partner clips
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_lift_clears_linked_partner_and_restores_on_undo() {
        // Two tracks with a linked pair; lift the primary-track clip
        // and confirm the partner's linked_clip_id is cleared, then
        // restored on undo.
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[(0, 24)]);
        // link primary[0] ↔ connected[0] (V+A style)
        let primary_id = seq.tracks[1].clips[0].id;
        let connected_id = seq.tracks[0].clips[0].id;
        seq.tracks[1].clips[0].linked_clip_id = Some(connected_id);
        seq.tracks[0].clips[0].linked_clip_id = Some(primary_id);

        let result = MagneticEditor::lift(&mut seq, primary_id);

        // Partner's linked_clip_id is cleared.
        let partner = seq.tracks[0].clips.iter().find(|c| c.id == connected_id).unwrap();
        assert!(
            partner.linked_clip_id.is_none(),
            "T-3: lift must clear partner's linked_clip_id"
        );

        // Undo restores it to the original primary id.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        let partner = seq.tracks[0].clips.iter().find(|c| c.id == connected_id).unwrap();
        assert_eq!(
            partner.linked_clip_id, Some(primary_id),
            "T-3 undo: linked_clip_id must be restored"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // T-8: OverwritePrimary straddle-case round-trip via redo
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_apply_forward_overwrite_primary_straddle_roundtrip() {
        // Straddle: new clip 30..42 splits clip 24..48 into two
        // fresh-id remnants. Before T-8, apply_forward didn't
        // re-insert the remnants (the trimmed_clips ripple records
        // referenced ids that didn't exist in the sequence at replay
        // time, so set_clip_position no-opped). Now split_remnants
        // carries the full clip values.
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let new_clip = media_clip(0, 12);
        let result = MagneticEditor::overwrite_primary(&mut seq, new_clip, tc(30));

        let post_len = seq.tracks[0].clips.len();
        assert!(
            post_len >= 4,
            "straddle should produce at least [A, left_remnant, new_clip, right_remnant, …]"
        );

        // undo → forward → same state.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        MagneticEditor::apply_forward(&mut seq, &result.command);
        assert_eq!(
            seq.tracks[0].clips.len(),
            post_len,
            "T-8: apply_forward after undo must reconstruct the split remnants"
        );

        // undo again → back to original 3 clips.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(seq.tracks[0].clips.len(), 3);
    }

    /// #919: an overwrite across a cut trims the clip before it and the
    /// clip after it, which keep their ids. Undo put the originals back
    /// beside the trimmed copies (two clips with one id) and the repack
    /// pushed everything after along; redo lost the trimmed clips.
    #[test]
    fn test_overwrite_primary_across_a_cut_undoes_and_redoes_exactly() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let state = |seq: &Sequence| -> Vec<(ClipId, RationalTime, RationalTime, RationalTime, RationalTime)> {
            seq.tracks[0].clips.iter().map(|c| (c.id, c.timeline_in, c.timeline_out, c.source_in, c.source_out)).collect()
        };
        let before = state(&seq);
        let result = MagneticEditor::overwrite_primary(&mut seq, media_clip(0, 12), tc(18));
        let after = state(&seq);
        assert_eq!(after.len(), 4, "A trimmed, the new clip, B trimmed, C");
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(state(&seq), before, "undo gives back the clips as they were");
        MagneticEditor::apply_forward(&mut seq, &result.command);
        assert_eq!(state(&seq), after, "redo gives back the overwrite");
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(state(&seq), before);
    }

    #[test]
    fn test_apply_inverse_overwrite_primary_full_replace_roundtrip() {
        // Simplest OverwritePrimary shape: the new clip exactly replaces
        // an existing clip (no trim, no split). Round-trip cycle: save
        // state → overwrite_primary → apply_inverse → state equal →
        // apply_forward → state equal to post-overwrite → apply_inverse
        // again → still equal to original.
        //
        // The "completely inside" branch is the only shape of
        // overwrite_primary whose apply_forward is trivially correct —
        // overwritten clips are removed, no remnants are synthesised,
        // so `trimmed_clips` is empty and apply_forward has nothing to
        // bring back. The trim and split shapes produce remnant clips
        // with fresh ClipIds; apply_forward's
        // `set_clip_position(trimmed.clip_id, ...)` then no-ops because
        // the id doesn't exist in the sequence yet. See T-8 in the
        // audit tracker — "OverwritePrimary apply_inverse is heuristic
        // + untested" — for the broader regression this test partially
        // covers.
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);

        let orig: Vec<(ClipId, RationalTime, RationalTime)> = seq.tracks[0]
            .clips
            .iter()
            .map(|c| (c.id, c.timeline_in, c.timeline_out))
            .collect();

        // new_clip is exactly 24 frames long, overwrite at frame 24 →
        // region 24..48 exactly equals clip 1. clip 1 is "completely
        // inside" the region, so it's removed; no trims, no splits.
        let new_clip = media_clip(0, 24);
        let new_clip_id = new_clip.id;
        let result = MagneticEditor::overwrite_primary(&mut seq, new_clip, tc(24));
        assert!(
            matches!(result.command, MagneticEditCommand::OverwritePrimary { .. }),
            "overwrite_primary must emit OverwritePrimary command"
        );

        let post_len = seq.tracks[0].clips.len();
        assert!(
            seq.tracks[0].clips.iter().any(|c| c.id == new_clip_id),
            "inserted clip must be on the track after overwrite"
        );
        // Same number of clips — clip 1 removed, new clip inserted.
        assert_eq!(post_len, orig.len());

        // apply_inverse → restore every original clip at its original
        // position, inserted clip gone.
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(
            seq.tracks[0].clips.len(),
            orig.len(),
            "apply_inverse must restore original clip count"
        );
        assert!(
            !seq.tracks[0].clips.iter().any(|c| c.id == new_clip_id),
            "inserted clip must be gone after apply_inverse"
        );
        for (id, want_in, want_out) in &orig {
            let c = seq.tracks[0]
                .clips
                .iter()
                .find(|c| c.id == *id)
                .unwrap_or_else(|| panic!("original clip {:?} missing after inverse", id));
            assert_eq!(c.timeline_in, *want_in, "clip {:?} timeline_in drift", id);
            assert_eq!(c.timeline_out, *want_out, "clip {:?} timeline_out drift", id);
        }

        // apply_forward → state back to post-overwrite.
        MagneticEditor::apply_forward(&mut seq, &result.command);
        assert_eq!(seq.tracks[0].clips.len(), post_len);
        assert!(
            seq.tracks[0].clips.iter().any(|c| c.id == new_clip_id),
            "inserted clip must be back after apply_forward"
        );

        // Second apply_inverse → back to originals (confirms the
        // command is idempotent under an even number of toggles, the
        // invariant undo/redo cycling relies on).
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(seq.tracks[0].clips.len(), orig.len());
        for (id, want_in, want_out) in &orig {
            let c = seq.tracks[0].clips.iter().find(|c| c.id == *id).unwrap();
            assert_eq!(c.timeline_in, *want_in);
            assert_eq!(c.timeline_out, *want_out);
        }
    }

    #[test]
    fn test_apply_inverse_blade() {
        let mut seq = make_sequence(&[(0, 48)]);
        let clip_id = seq.tracks[0].clips[0].id;

        let result = MagneticEditor::blade(&mut seq, clip_id, tc(24)).unwrap();

        assert_eq!(seq.tracks[0].clips.len(), 2);

        MagneticEditor::apply_inverse(&mut seq, &result.command);

        assert_eq!(seq.tracks[0].clips.len(), 1);
        assert_eq!(seq.tracks[0].clips[0].id, clip_id);
        assert_eq!(seq.tracks[0].clips[0].timeline_in, tc(0));
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(48));
    }

    #[test]
    fn test_apply_inverse_lift() {
        let mut seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        let clip1_id = seq.tracks[0].clips[1].id;

        let result = MagneticEditor::lift(&mut seq, clip1_id);

        // Verify gap is there
        assert!(seq.tracks[0].clips[1].kind.is_gap());

        MagneticEditor::apply_inverse(&mut seq, &result.command);

        // Restored original clip
        assert_eq!(seq.tracks[0].clips[1].id, clip1_id);
        assert!(!seq.tracks[0].clips[1].kind.is_gap());
    }

    // ─────────────────────────────────────────────────────────────
    // insert_gap
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_insert_gap() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);

        MagneticEditor::insert_gap(&mut seq, tc(12), tc(24), None, None);

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert!(track.clips[1].kind.is_gap());
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(36));
        // Downstream shifted
        assert_eq!(track.clips[2].timeline_in, tc(36));
        assert_eq!(track.clips[2].timeline_out, tc(60));
    }

    // ─────────────────────────────────────────────────────────────
    // resolve_collision_push
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_resolve_collision_push() {
        let mut seq = make_sequence_with_connected(&[(0, 24)], &[]);
        let clip_a = media_clip(0, 24);
        let clip_b = media_clip(12, 36); // overlaps
        seq.tracks[0].clips.push(clip_a);
        seq.tracks[0].clips.push(clip_b);
        let a_id = seq.tracks[0].clips[0].id;
        let b_id = seq.tracks[0].clips[1].id;

        MagneticEditor::resolve_collision_push(&mut seq, a_id, b_id);

        // b should be pushed to start at a's end (24)
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(24));
        assert_eq!(seq.tracks[0].clips[1].timeline_out, tc(48));
    }

    // ─────────────────────────────────────────────────────────────
    // three_point_edit
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_three_point_edit_insert_in_point() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let clip = media_clip(0, 12);

        MagneticEditor::three_point_edit(
            &mut seq,
            clip,
            tc(24),
            true,
            EditMode::Insert,
            None,
            None,
        );

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(36));
    }

    #[test]
    fn test_three_point_edit_insert_out_point() {
        let mut seq = make_sequence(&[(0, 24), (24, 48)]);
        let clip = media_clip(0, 12);

        // out point at 36 means in point at 36 - 12 = 24
        MagneticEditor::three_point_edit(
            &mut seq,
            clip,
            tc(36),
            false,
            EditMode::Insert,
            None,
            None,
        );

        let track = &seq.tracks[0];
        assert_eq!(track.clips.len(), 3);
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // apply_forward / apply_inverse round trip for trim_tail
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_apply_forward_inverse_trim_tail() {
        let mut seq = make_sequence(&[(0, 48), (48, 72)]);
        let clip0_id = seq.tracks[0].clips[0].id;
        let _clip1_id = seq.tracks[0].clips[1].id;

        let result = MagneticEditor::trim_tail(&mut seq, clip0_id, tc(36));

        // Verify trim happened
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(36));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(36));

        // Undo
        MagneticEditor::apply_inverse(&mut seq, &result.command);
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(48));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(48));

        // Redo
        MagneticEditor::apply_forward(&mut seq, &result.command);
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(36));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // Cascade with ConnectedStoryline
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_cascade_connected_storyline() {
        let mut seq = make_sequence_with_connected(&[(0, 24), (24, 48)], &[(0, 12), (12, 24)]);
        let primary_clip0_id = seq.tracks[1].clips[0].id;

        // Promote connected track to storyline anchored to primary clip 0
        MagneticEditor::promote_to_storyline(&mut seq, 0, primary_clip0_id);

        // Insert at beginning of primary, shifting everything right by 12
        let new_clip = media_clip(0, 12);
        MagneticEditor::insert_primary(&mut seq, new_clip, tc(0), None, None);

        // All storyline clips should have moved right by 12
        assert_eq!(seq.tracks[0].clips[0].timeline_in, tc(12));
        assert_eq!(seq.tracks[0].clips[0].timeline_out, tc(24));
        assert_eq!(seq.tracks[0].clips[1].timeline_in, tc(24));
        assert_eq!(seq.tracks[0].clips[1].timeline_out, tc(36));
    }

    // ─────────────────────────────────────────────────────────────
    // Ripple delete disconnects anchored clips (Gap 11)
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_ripple_delete_disconnects_anchored_clips() {
        let mut seq = make_sequence_with_connected(&[(0, 24), (24, 48)], &[(0, 24)]);
        let primary_clip0_id = seq.tracks[1].clips[0].id;
        let connected_clip_id = seq.tracks[0].clips[0].id;

        // Connect to primary clip 0
        MagneticEditor::connect_clip(
            &mut seq,
            connected_clip_id,
            primary_clip0_id,
            AnchorAlignment::StartAligned,
        );
        assert!(seq.tracks[0].clips[0].magnetic_anchor.is_some());

        // Delete primary clip 0
        MagneticEditor::ripple_delete(&mut seq, primary_clip0_id);

        // Connected clip should be disconnected
        assert!(seq.tracks[0].clips[0].magnetic_anchor.is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // active_snap_points
    // ─────────────────────────────────────────────────────────────
    #[test]
    fn test_active_snap_points_sorted_by_distance() {
        let seq = make_sequence(&[(0, 24), (24, 48), (48, 72)]);
        // Query at frame 26 — closest to 24, then 48, then 0
        let points = MagneticEditor::active_snap_points(&seq, tc(26), None, None, None);
        // Should include 24 (2 frames away) but not 48 (22 frames) or 0 (26 frames)
        assert!(!points.is_empty());
        assert_eq!(points[0].time, tc(24));
    }
}
