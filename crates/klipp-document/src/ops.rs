//! Intent in, geometry out: an edit said in editorial terms, resolved
//! against the document into the recorded commands that make it, with a
//! reason when it cannot be made.
//!
//! An [`Op`] names clips and media by id and places by an [`Anchor`]: a
//! time, a frame, a clip's edge, a marker, an offset from one of those.
//! [`resolve`] runs the ops in order over a copy of the document — each
//! op sees what the ones before it did — snapping every time to a frame,
//! and returns a [`Plan`]: one [`Step`] per op with the [`EditCommand`]
//! that does it and a sentence saying what. A host applies the plan's
//! commands through [`crate::apply::DocumentView::apply_forward`] inside
//! one history batch, so a plan is one undo step; or shows the plan and
//! applies nothing, which is the dry run. Because the same commands are
//! replayed, what the dry run said is what the apply does.
//!
//! A [`Refusal`] says which step, why, and what would make it possible.

use crate::history::{EditCommand, LinkedMovePartner};
use klipp_core::{ClipId, Fps, MediaId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};
use serde::{Deserialize, Serialize};

/// A point on the timeline, said the way an editor says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Seconds from the start of the timeline.
    Secs(f64),
    /// A frame number at the sequence's rate.
    Frame(i64),
    /// Where a clip starts.
    ClipStart(ClipId),
    /// Where a clip ends.
    ClipEnd(ClipId),
    /// A timeline marker, by its label (case-insensitive).
    Marker(String),
    /// Another anchor moved by some seconds, negative for earlier.
    Offset { from: Box<Anchor>, secs: f64 },
}

/// Which track a clip goes on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TrackRef {
    /// A track's index in the sequence, top first, as the views list them.
    Index(usize),
    /// The first track of a kind: "video", "audio", "text".
    Kind(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    In,
    Out,
}

/// One editorial operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A media item onto a track at an anchor, the whole file unless
    /// `source_in` and `source_out` (seconds in the file) say otherwise.
    /// Refused where it would overlap a clip already on the track —
    /// unless `insert`, which makes room: every unlocked track is
    /// rippled later by the clip's length from the anchor, and a clip
    /// straddling the anchor is split there.
    Place {
        media_id: MediaId,
        track: TrackRef,
        at: Anchor,
        #[serde(default)]
        source_in: Option<f64>,
        #[serde(default)]
        source_out: Option<f64>,
        #[serde(default)]
        insert: bool,
    },
    /// One edge of a clip to an anchor. The clip's other edge stays;
    /// the source moves with the edge, so the picture at the other edge
    /// does not change. Refused past the media's start or end, or into
    /// a neighbour — unless `ripple`, which moves the later clips on the
    /// track by the change in the clip's length (a head trim then keeps
    /// the clip where it starts and shortens it from the front).
    Trim {
        clip_id: ClipId,
        edge: Edge,
        to: Anchor,
        #[serde(default)]
        ripple: bool,
    },
    /// A J-cut: the sound of `incoming_clip` starts `frames` before its
    /// picture, over the end of the clip before it. Both clips need
    /// linked audio, adjacent on one audio track: the outgoing audio is
    /// shortened by `frames`, the incoming audio starts that much earlier.
    JCut { incoming_clip: ClipId, frames: i64 },
    /// An L-cut: the sound of `outgoing_clip` stays `frames` past its
    /// picture, under the start of the clip after it. The outgoing audio
    /// runs on by `frames`, the incoming audio starts that much later.
    LCut { outgoing_clip: ClipId, frames: i64 },
    /// A clip into two at an anchor strictly inside it.
    Split { clip_id: ClipId, at: Anchor },
    /// A clip off its track; with `ripple`, the clips after it on that
    /// track close the gap.
    Remove {
        clip_id: ClipId,
        #[serde(default)]
        ripple: bool,
    },
    /// A clip's start to an anchor, its length unchanged; a linked audio
    /// or video partner goes with it. Refused into another clip.
    Move { clip_id: ClipId, to: Anchor },
}

/// A clip as a plan reports it, after a step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipAfter {
    pub clip_id: ClipId,
    pub track: usize,
    pub timeline_in_secs: f64,
    pub timeline_out_secs: f64,
    pub source_in_secs: f64,
    pub source_out_secs: f64,
}

/// One op resolved: what it does, in a sentence and as the command
/// that does it.
#[derive(Debug, Clone)]
pub struct Step {
    pub op: Op,
    pub what: String,
    pub edit: EditCommand,
    /// The clips the step touched or made, as they are after it.
    pub after: Vec<ClipAfter>,
}

/// A plan as a client reads it: the steps without their commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub op: Op,
    pub what: String,
    pub after: Vec<ClipAfter>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// The timeline's length once the plan has run, seconds.
    pub duration_secs: f64,
}

impl Plan {
    pub fn report(&self) -> Vec<StepReport> {
        self.steps.iter().map(|s| StepReport { op: s.op.clone(), what: s.what.clone(), after: s.after.clone() }).collect()
    }

    /// The plan in a few lines, for a person or a model to read.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.steps.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, s.what));
        }
        out.push_str(&format!("Timeline afterwards: {:.2} s\n", self.duration_secs));
        out
    }
}

/// Why a plan could not be made: the step (0-based), the reason, and
/// what would make it possible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub step: usize,
    pub why: String,
    pub hint: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step {}: {}. {}", self.step + 1, self.why, self.hint)
    }
}

fn refuse(step: usize, why: impl Into<String>, hint: impl Into<String>) -> Refusal {
    Refusal { step, why: why.into(), hint: hint.into() }
}

/// `secs` on the nearest frame at `fps`, as ticks at the sequence's rate.
fn snap(secs: f64, fps: Fps) -> Timecode {
    let frames = (secs * fps.as_f64()).round() as i64;
    frame_tc(frames, fps)
}

fn frame_tc(frames: i64, fps: Fps) -> Timecode {
    // `rate` is the fps numerator, so a frame is `den` ticks: one for
    // whole rates, 1001 for the NTSC ones.
    Timecode { value: frames * fps.den.max(1) as i64, rate: fps.rate() }
}

fn one_frame(fps: Fps) -> Timecode {
    frame_tc(1, fps)
}

fn secs(t: Timecode) -> f64 {
    t.to_secs_f64()
}

fn find_clip(seq: &Sequence, id: ClipId) -> Option<(usize, usize, &Clip)> {
    seq.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|c| c.id == id).map(|ci| (ti, ci, &t.clips[ci])))
}

fn clip_name(seq: &Sequence, pool: &MediaPool, clip: &Clip) -> String {
    match &clip.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "media".into()),
        ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("").chars().take(24).collect::<String>()),
        ClipKind::Generator(_) => "generator".into(),
        ClipKind::AdjustmentLayer => "adjustment layer".into(),
        ClipKind::Gap => "gap".into(),
        ClipKind::Sequence { .. } => "nested sequence".into(),
    }
    .to_string()
        + &seq.tracks.iter().enumerate().find(|(_, t)| t.clips.iter().any(|c| c.id == clip.id)).map(|(i, _)| format!(" on track {i}")).unwrap_or_default()
}

fn resolve_anchor(seq: &Sequence, fps: Fps, a: &Anchor, step: usize) -> Result<Timecode, Refusal> {
    Ok(match a {
        Anchor::Secs(s) => {
            if !s.is_finite() || *s < 0.0 {
                return Err(refuse(step, format!("{s} s is not a time on the timeline"), "give seconds from the start, zero or more"));
            }
            snap(*s, fps)
        }
        Anchor::Frame(f) => {
            if *f < 0 {
                return Err(refuse(step, format!("frame {f} is before the start"), "give a frame number, zero or more"));
            }
            frame_tc(*f, fps)
        }
        Anchor::ClipStart(id) => find_clip(seq, *id).ok_or_else(|| refuse(step, format!("no clip {}", id.0), "clip ids are in the timeline view"))?.2.timeline_in,
        Anchor::ClipEnd(id) => find_clip(seq, *id).ok_or_else(|| refuse(step, format!("no clip {}", id.0), "clip ids are in the timeline view"))?.2.timeline_out,
        Anchor::Marker(label) => {
            let want = label.to_lowercase();
            seq.markers
                .iter()
                .find(|m| m.label.to_lowercase() == want)
                .map(|m| m.position)
                .ok_or_else(|| refuse(step, format!("no marker \"{label}\""), "markers are listed at the end of the timeline view"))?
        }
        Anchor::Offset { from, secs: off } => {
            let base = resolve_anchor(seq, fps, from, step)?;
            let t = base.to_secs_f64() + off;
            if t < 0.0 {
                return Err(refuse(step, "the offset lands before the start", "use a smaller offset"));
            }
            snap(t, fps)
        }
    })
}

/// A clip on `track` overlapping `[a, b)`, other than `except`.
fn overlapping(seq: &Sequence, track: usize, a: Timecode, b: Timecode, except: Option<ClipId>) -> Option<&Clip> {
    seq.tracks.get(track)?.clips.iter().find(|c| Some(c.id) != except && c.timeline_out > a && c.timeline_in < b)
}

fn after_of(seq: &Sequence, ids: &[ClipId]) -> Vec<ClipAfter> {
    ids.iter()
        .filter_map(|id| find_clip(seq, *id))
        .map(|(ti, _, c)| ClipAfter {
            clip_id: c.id,
            track: ti,
            timeline_in_secs: secs(c.timeline_in),
            timeline_out_secs: secs(c.timeline_out),
            source_in_secs: secs(c.source_in),
            source_out_secs: secs(c.source_out),
        })
        .collect()
}

/// Resolve `ops` against `seq` and `pool`, in order, each seeing the
/// ones before it. Nothing given is changed.
pub fn resolve(seq: &Sequence, pool: &MediaPool, ops: &[Op]) -> Result<Plan, Refusal> {
    let fps = seq.fps;
    let mut sims = vec![seq.clone()];
    let mut sim_pool = pool.clone();
    let mut sim_fps = fps;
    let mut steps = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let sim = &sims[0];
        let (what, edit, touched) = resolve_one(sim, &sim_pool, fps, op, i)?;
        let mut view = crate::apply::DocumentView { sequences: &mut sims, active: 0, media_pool: &mut sim_pool, fps: &mut sim_fps };
        view.apply_forward(&edit);
        let after = after_of(&sims[0], &touched);
        steps.push(Step { op: op.clone(), what, edit, after });
    }
    Ok(Plan { steps, duration_secs: secs(sims[0].total_duration()) })
}

fn resolve_one(seq: &Sequence, pool: &MediaPool, fps: Fps, op: &Op, i: usize) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    match op {
        Op::Place { media_id, track, at, source_in, source_out, insert } => {
            let item = pool.get(media_id).ok_or_else(|| refuse(i, format!("no media item {}", media_id.0), "media ids are in list_media"))?;
            if item.offline {
                return Err(refuse(i, format!("{} is offline", item.filename), "relink the media first"));
            }
            let ti = match track {
                TrackRef::Index(n) => {
                    if *n >= seq.tracks.len() {
                        return Err(refuse(i, format!("no track {n}"), format!("the sequence has tracks 0 to {}", seq.tracks.len().saturating_sub(1))));
                    }
                    *n
                }
                TrackRef::Kind(kind) => {
                    let want = match kind.to_lowercase().as_str() {
                        "video" => TrackKind::Video,
                        "audio" => TrackKind::Audio,
                        "text" | "title" => TrackKind::Text,
                        other => return Err(refuse(i, format!("\"{other}\" is not a track kind"), "use video, audio or text, or a track index")),
                    };
                    seq.tracks.iter().position(|t| t.kind == want).ok_or_else(|| refuse(i, format!("no {} track", kind.to_lowercase()), "add one in the app, or give a track index"))?
                }
            };
            let t = &seq.tracks[ti];
            if t.locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it, or use another track"));
            }
            let media_dur = secs(item.duration);
            let s_in = source_in.unwrap_or(0.0);
            let s_out = source_out.unwrap_or(media_dur);
            if !(0.0..media_dur).contains(&s_in) || s_out <= s_in || s_out > media_dur + 1e-6 {
                return Err(refuse(i, format!("source {s_in:.2}-{s_out:.2} s is not inside {} ({media_dur:.2} s)", item.filename), "give source_in and source_out within the file, in order"));
            }
            let src_in = snap(s_in, fps);
            let src_out = snap(s_out, fps);
            if src_out <= src_in {
                return Err(refuse(i, "the source range is shorter than a frame", "make it at least one frame"));
            }
            let tl_in = resolve_anchor(seq, fps, at, i)?;
            let tl_out = tl_in + (src_out - src_in);
            if *insert {
                let clip = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
                let id = clip.id;
                let mut scratch = seq.clone();
                let edit = scratch
                    .insert_edit(ti, clip, tl_in)
                    .ok_or_else(|| refuse(i, format!("track {ti} would not take an insert"), "unlock the track, or place without insert"))?;
                let what = format!(
                    "insert {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s; {} clip(s) split, {} moved {:.2} s later",
                    item.filename,
                    secs(tl_in),
                    secs(src_in),
                    secs(src_out),
                    secs(tl_out),
                    edit.splits.len(),
                    edit.shifted.len(),
                    secs(tl_out - tl_in)
                );
                return Ok((what, EditCommand::LegacyInsertEdit { edit, audio: None, created_audio_track_at: None }, vec![id]));
            }
            if let Some(c) = overlapping(seq, ti, tl_in, tl_out, None) {
                return Err(refuse(
                    i,
                    format!("it would overlap {} at {:.2}-{:.2} s", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)),
                    format!("place it at clip_end of {}, remove that clip first, or place with insert", c.id.0),
                ));
            }
            let clip = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
            let what = format!(
                "place {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s",
                item.filename,
                secs(tl_in),
                secs(src_in),
                secs(src_out),
                secs(tl_out)
            );
            let id = clip.id;
            Ok((what, EditCommand::LegacyAppendClip { clip, track_index: ti }, vec![id]))
        }

        Op::Trim { clip_id, edge, to, ripple } => {
            let (ti, ci, clip) = find_clip(seq, *clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let target = resolve_anchor(seq, fps, to, i)?;
            let media_dur = clip.media_id().and_then(|m| pool.get(&m)).map(|m| m.duration);
            let (new_tl_in, new_tl_out, new_src_in, new_src_out) = match edge {
                Edge::In => {
                    if target >= clip.timeline_out - one_frame(fps) {
                        return Err(refuse(i, "the in point would pass the out point", "trim to a time before the clip's end"));
                    }
                    let delta = target - clip.timeline_in;
                    let src_in = clip.source_in + delta;
                    if src_in.value < 0 {
                        return Err(refuse(i, format!("only {:.2} s of the file lies before this clip's in point", secs(clip.source_in)), "trim less, or move the clip instead"));
                    }
                    if *ripple {
                        // The clip stays where it starts and is shorter from
                        // the front: the later clips close up by the change.
                        (clip.timeline_in, clip.timeline_out - delta, src_in, clip.source_out)
                    } else {
                        (target, clip.timeline_out, src_in, clip.source_out)
                    }
                }
                Edge::Out => {
                    if target <= clip.timeline_in + one_frame(fps) {
                        return Err(refuse(i, "the out point would pass the in point", "trim to a time after the clip's start"));
                    }
                    let delta = target - clip.timeline_out;
                    let src_out = clip.source_out + delta;
                    if let Some(d) = media_dur {
                        if src_out > d {
                            return Err(refuse(i, format!("only {:.2} s of the file lies after this clip's out point", secs(d - clip.source_out)), "trim less, or place more of the file"));
                        }
                    }
                    (clip.timeline_in, target, clip.source_in, src_out)
                }
            };
            if *ripple {
                let shifted: Vec<(ClipId, Timecode, Timecode)> =
                    seq.tracks[ti].clips[ci + 1..].iter().map(|c| (c.id, c.timeline_in, c.timeline_out)).collect();
                let delta = new_tl_out - clip.timeline_out;
                // The earlier neighbour must not be run into by a head trim
                // that makes the clip longer (it cannot: the start stays).
                let what = format!(
                    "ripple trim the {} of {} to {:.2} s (now {:.2}-{:.2} s); {} later clip(s) move {:.2} s",
                    match edge { Edge::In => "in", Edge::Out => "out" },
                    clip_name(seq, pool, clip),
                    secs(target),
                    secs(new_tl_in),
                    secs(new_tl_out),
                    shifted.len(),
                    secs(delta)
                );
                let ids: Vec<ClipId> = std::iter::once(*clip_id).chain(shifted.iter().map(|s| s.0)).collect();
                let edit = EditCommand::LegacyRippleTrimClip {
                    clip_id: *clip_id,
                    old_source_in: clip.source_in,
                    old_source_out: clip.source_out,
                    old_timeline_in: clip.timeline_in,
                    old_timeline_out: clip.timeline_out,
                    new_source_in: new_src_in,
                    new_source_out: new_src_out,
                    new_timeline_in: new_tl_in,
                    new_timeline_out: new_tl_out,
                    shifted_clips: shifted,
                };
                return Ok((what, edit, ids));
            }
            if let Some(c) = overlapping(seq, ti, new_tl_in, new_tl_out, Some(*clip_id)) {
                return Err(refuse(i, format!("it would run into {} at {:.2} s", clip_name(seq, pool, c), secs(c.timeline_in)), "trim less, move that clip, or trim with ripple"));
            }
            let what = format!(
                "trim the {} of {} to {:.2} s (now {:.2}-{:.2} s)",
                match edge { Edge::In => "in", Edge::Out => "out" },
                clip_name(seq, pool, clip),
                secs(target),
                secs(new_tl_in),
                secs(new_tl_out)
            );
            let edit = EditCommand::LegacyTrimClip {
                clip_id: *clip_id,
                old_source_in: clip.source_in,
                old_source_out: clip.source_out,
                old_timeline_in: clip.timeline_in,
                old_timeline_out: clip.timeline_out,
                new_source_in: new_src_in,
                new_source_out: new_src_out,
                new_timeline_in: new_tl_in,
                new_timeline_out: new_tl_out,
            };
            Ok((what, edit, vec![*clip_id]))
        }

        Op::JCut { incoming_clip, frames } => split_edit(seq, pool, fps, i, *incoming_clip, *frames, true),
        Op::LCut { outgoing_clip, frames } => split_edit(seq, pool, fps, i, *outgoing_clip, *frames, false),

        Op::Split { clip_id, at } => {
            let (ti, ci, clip) = find_clip(seq, *clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let cut = resolve_anchor(seq, fps, at, i)?;
            let mut scratch = seq.tracks[ti].clone();
            let Some((a, b)) = scratch.blade_at(*clip_id, cut) else {
                return Err(refuse(
                    i,
                    format!("{:.2} s is not inside {} ({:.2}-{:.2} s) by at least a frame", secs(cut), clip_name(seq, pool, clip), secs(clip.timeline_in), secs(clip.timeline_out)),
                    "split at a time strictly inside the clip",
                ));
            };
            let what = format!("split {} at {:.2} s into {:.2}-{:.2} s and {:.2}-{:.2} s", clip_name(seq, pool, clip), secs(cut), secs(a.timeline_in), secs(a.timeline_out), secs(b.timeline_in), secs(b.timeline_out));
            let ids = vec![a.id, b.id];
            Ok((what, EditCommand::LegacyBladeClip { track_index: ti, original_clip: clip.clone(), clip_a: a, clip_b: b, position: ci }, ids))
        }

        Op::Remove { clip_id, ripple } => {
            let (ti, ci, clip) = find_clip(seq, *clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let name = clip_name(seq, pool, clip);
            if *ripple {
                let gap = clip.timeline_out - clip.timeline_in;
                let shifted: Vec<(ClipId, Timecode, Timecode)> =
                    seq.tracks[ti].clips[ci + 1..].iter().map(|c| (c.id, c.timeline_in, c.timeline_out)).collect();
                let what = format!("remove {} ({:.2}-{:.2} s) and close the gap: {} clips move {:.2} s earlier", name, secs(clip.timeline_in), secs(clip.timeline_out), shifted.len(), secs(gap));
                let ids: Vec<ClipId> = shifted.iter().map(|s| s.0).collect();
                Ok((what, EditCommand::LegacyRippleRemove { removed_clip: clip.clone(), track_index: ti, position: ci, shifted_clips: shifted }, ids))
            } else {
                let what = format!("remove {} ({:.2}-{:.2} s), leaving the gap", name, secs(clip.timeline_in), secs(clip.timeline_out));
                Ok((what, EditCommand::LegacyRemoveClip { clip: clip.clone(), track_index: ti, position: ci }, Vec::new()))
            }
        }

        Op::Move { clip_id, to } => {
            let (ti, _, clip) = find_clip(seq, *clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let new_in = resolve_anchor(seq, fps, to, i)?;
            let len = clip.timeline_out - clip.timeline_in;
            let new_out = new_in + len;
            if let Some(c) = overlapping(seq, ti, new_in, new_out, Some(*clip_id)) {
                return Err(refuse(i, format!("it would overlap {} at {:.2}-{:.2} s", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)), format!("move it to clip_end of {}, or remove that clip first", c.id.0)));
            }
            let delta = new_in - clip.timeline_in;
            let mut touched = vec![*clip_id];
            let linked_partner = clip.linked_clip_id.and_then(|pid| {
                let (pti, _, p) = find_clip(seq, pid)?;
                let p_in = p.timeline_in + delta;
                let p_out = p.timeline_out + delta;
                if seq.tracks[pti].locked || overlapping(seq, pti, p_in, p_out, Some(pid)).is_some() {
                    return None;
                }
                touched.push(pid);
                Some(LinkedMovePartner { partner_id: pid, old_timeline_in: p.timeline_in, old_timeline_out: p.timeline_out, new_timeline_in: p_in, new_timeline_out: p_out })
            });
            let what = format!(
                "move {} from {:.2} s to {:.2} s (now {:.2}-{:.2} s){}",
                clip_name(seq, pool, clip),
                secs(clip.timeline_in),
                secs(new_in),
                secs(new_in),
                secs(new_out),
                if linked_partner.is_some() { ", its linked partner with it" } else { "" }
            );
            let edit = EditCommand::LegacyMoveClip {
                clip_id: *clip_id,
                old_timeline_in: clip.timeline_in,
                old_timeline_out: clip.timeline_out,
                new_timeline_in: new_in,
                new_timeline_out: new_out,
                linked_partner,
            };
            Ok((what, edit, touched))
        }
    }
}

/// A J-cut (`j`) on the cut into `clip` or an L-cut out of it, `frames`
/// long: the two clips' linked audio, adjacent on one audio track, get
/// their shared edge moved — earlier for a J-cut, later for an L-cut —
/// as two trims recorded in one batch.
fn split_edit(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, clip_id: ClipId, frames: i64, j: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if frames <= 0 {
        return Err(refuse(i, "the lead has to be at least one frame", "give frames above zero"));
    }
    let (ti, ci, clip) = find_clip(seq, clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
    // The picture's neighbour: before it for a J-cut, after it for an L-cut.
    let track = &seq.tracks[ti];
    let neighbour = if j { ci.checked_sub(1).map(|p| &track.clips[p]) } else { track.clips.get(ci + 1) };
    let Some(neighbour) = neighbour.filter(|n| if j { n.timeline_out == clip.timeline_in } else { n.timeline_in == clip.timeline_out }) else {
        return Err(refuse(i, format!("{} has no clip cut {} it on its track", clip_name(seq, pool, clip), if j { "before" } else { "after" }), "a J-cut or L-cut needs two clips meeting at a cut"));
    };
    let (outgoing, incoming) = if j { (neighbour, clip) } else { (clip, neighbour) };
    let audio_of = |c: &Clip| -> Result<(usize, Clip), Refusal> {
        let aid = c.linked_clip_id.ok_or_else(|| refuse(i, format!("{} has no linked audio", clip_name(seq, pool, c)), "attach its audio first (attach_audio)"))?;
        let (ati, _, a) = find_clip(seq, aid).ok_or_else(|| refuse(i, format!("the audio linked to {} is not on the timeline", clip_name(seq, pool, c)), "attach its audio again"))?;
        Ok((ati, a.clone()))
    };
    let (out_ti, out_a) = audio_of(outgoing)?;
    let (in_ti, in_a) = audio_of(incoming)?;
    if out_ti != in_ti {
        return Err(refuse(i, "the two clips' audio is on different tracks", "put both audio clips on one track"));
    }
    if seq.tracks[out_ti].locked {
        return Err(refuse(i, format!("audio track {out_ti} is locked"), "unlock it first"));
    }
    if out_a.timeline_out != in_a.timeline_in {
        return Err(refuse(i, format!("the audio clips do not meet: {:.2} s and {:.2} s", secs(out_a.timeline_out), secs(in_a.timeline_in)), "line the audio up with the picture first"));
    }
    let lead = frame_tc(frames, fps);
    let (new_edge, out_src_out, in_src_in) = if j {
        (out_a.timeline_out - lead, out_a.source_out - lead, in_a.source_in - lead)
    } else {
        (out_a.timeline_out + lead, out_a.source_out + lead, in_a.source_in + lead)
    };
    if new_edge <= out_a.timeline_in + one_frame(fps) {
        return Err(refuse(i, "the outgoing audio would be shorter than a frame", "use fewer frames"));
    }
    if new_edge >= in_a.timeline_out - one_frame(fps) {
        return Err(refuse(i, "the incoming audio would be shorter than a frame", "use fewer frames"));
    }
    if in_src_in.value < 0 {
        return Err(refuse(i, format!("only {:.2} s of sound lies before the incoming clip's in point", secs(in_a.source_in)), "use fewer frames"));
    }
    if let Some(d) = outgoing.media_id().and_then(|m| pool.get(&m)).map(|m| m.duration) {
        if out_src_out > d {
            return Err(refuse(i, format!("only {:.2} s of sound lies after the outgoing clip's out point", secs(d - out_a.source_out)), "use fewer frames"));
        }
    }
    let trim = |a: &Clip, src_in: Timecode, src_out: Timecode, tl_in: Timecode, tl_out: Timecode| EditCommand::LegacyTrimClip {
        clip_id: a.id,
        old_source_in: a.source_in,
        old_source_out: a.source_out,
        old_timeline_in: a.timeline_in,
        old_timeline_out: a.timeline_out,
        new_source_in: src_in,
        new_source_out: src_out,
        new_timeline_in: tl_in,
        new_timeline_out: tl_out,
    };
    let edits = vec![
        trim(&out_a, out_a.source_in, out_src_out, out_a.timeline_in, new_edge),
        trim(&in_a, in_src_in, in_a.source_out, new_edge, in_a.timeline_out),
    ];
    let what = format!(
        "{} of {} frames at the cut between {} and {}: the audio now changes at {:.2} s, the picture at {:.2} s",
        if j { "J-cut" } else { "L-cut" },
        frames,
        clip_name(seq, pool, outgoing),
        clip_name(seq, pool, incoming),
        secs(new_edge),
        secs(incoming.timeline_in)
    );
    Ok((what, EditCommand::Batch(edits), vec![out_a.id, in_a.id]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::Track;

    fn tc(secs: f64) -> Timecode {
        RationalTime::from_secs_f64(secs, 25)
    }

    /// A 25 fps sequence with a video track holding two clips of a
    /// 10-second media item, at 0-4 s and 4-8 s, and an audio track.
    fn fixture() -> (Sequence, MediaPool, MediaId, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/a.mov".into(), None);
        item.duration = tc(10.0);
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media_id = pool.add(item);
        let mut v = Track::new("V1", TrackKind::Video);
        let mut a = Clip::media(media_id, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut b = Clip::media(media_id, tc(4.0), tc(8.0), tc(4.0), tc(8.0));
        let (ia, ib) = (a.id, b.id);
        // Linked audio under each, meeting at the same cut.
        let mut audio = Track::new("A1", TrackKind::Audio);
        let mut aa = Clip::media(media_id, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut ab = Clip::media(media_id, tc(4.0), tc(8.0), tc(4.0), tc(8.0));
        a.linked_clip_id = Some(aa.id);
        aa.linked_clip_id = Some(ia);
        b.linked_clip_id = Some(ab.id);
        ab.linked_clip_id = Some(ib);
        audio.clips = vec![aa, ab];
        v.clips = vec![a, b];
        seq.tracks = vec![v, audio];
        seq.markers.push(klipp_timeline::Marker {
            id: klipp_core::MarkerId::new(),
            position: tc(6.0),
            label: "Intro".into(),
            color: klipp_core::MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });
        (seq, pool, media_id, ia, ib)
    }

    #[test]
    fn a_plan_runs_in_order_and_reads_as_text() {
        let (seq, pool, media, a, b) = fixture();
        let ops = vec![
            Op::Split { clip_id: b, at: Anchor::Marker("intro".into()) },
            Op::Place { media_id: media, track: TrackRef::Kind("video".into()), at: Anchor::ClipEnd(b), source_in: Some(8.0), source_out: Some(9.0), insert: false },
            Op::Trim { clip_id: a, edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false },
        ];
        // The split replaces b, so its end no longer exists as `b`:
        // the plan must refuse the place with a reason naming that.
        let err = resolve(&seq, &pool, &ops).unwrap_err();
        assert_eq!(err.step, 1);
        assert!(err.why.contains("no clip"), "{err}");

        let ops = vec![
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(b), source_in: Some(8.0), source_out: Some(9.0), insert: false },
            Op::Split { clip_id: b, at: Anchor::Marker("intro".into()) },
            Op::Trim { clip_id: a, edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false },
        ];
        let plan = resolve(&seq, &pool, &ops).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!((plan.duration_secs - 9.0).abs() < 1e-9, "{}", plan.text());
        assert_eq!(plan.steps[1].after.len(), 2, "a split reports both halves");
        assert!((plan.steps[1].after[0].timeline_out_secs - 6.0).abs() < 1e-9);
        assert!((plan.steps[2].after[0].timeline_out_secs - 3.0).abs() < 1e-9);
        assert!((plan.steps[2].after[0].source_out_secs - 3.0).abs() < 1e-9, "the source moves with the edge");
        let text = plan.text();
        assert!(text.contains("1. place a.mov on track 0 at 8.00 s"), "{text}");
        assert!(text.contains("2. split"), "{text}");
        // The given sequence is untouched.
        assert_eq!(seq.tracks[0].clips.len(), 2);
    }

    #[test]
    fn refusals_say_why_and_what_would_help() {
        let (seq, pool, media, a, b) = fixture();
        let over = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: None, source_out: Some(1.0), insert: false }]).unwrap_err();
        assert!(over.why.contains("overlap"), "{over}");
        assert!(over.hint.contains("clip_end"), "{over}");
        let past = resolve(&seq, &pool, &[Op::Trim { clip_id: b, edge: Edge::Out, to: Anchor::Secs(11.0), ripple: false }]).unwrap_err();
        assert!(past.why.contains("after this clip's out point"), "{past}");
        let into = resolve(&seq, &pool, &[Op::Move { clip_id: a, to: Anchor::Secs(1.0) }]).unwrap_err();
        assert!(into.why.contains("overlap"), "{into}");
        let outside = resolve(&seq, &pool, &[Op::Split { clip_id: a, at: Anchor::Secs(4.0) }]).unwrap_err();
        assert!(outside.why.contains("not inside"), "{outside}");
        let no_marker = resolve(&seq, &pool, &[Op::Move { clip_id: a, to: Anchor::Marker("outro".into()) }]).unwrap_err();
        assert!(no_marker.why.contains("no marker"), "{no_marker}");
    }

    #[test]
    fn a_ripple_remove_closes_the_gap_and_the_commands_replay_on_the_document() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::Remove { clip_id: a, ripple: true }]).unwrap();
        assert_eq!(plan.steps[0].after[0].clip_id, b);
        assert!((plan.steps[0].after[0].timeline_in_secs).abs() < 1e-9, "b moved to the start");
        // Applying the plan's commands to the real document gives the
        // state the plan reported; the inverse takes it back.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for s in &plan.steps {
            view.apply_forward(&s.edit);
        }
        assert_eq!(seqs[0].tracks[0].clips.len(), 1);
        assert_eq!(seqs[0].tracks[0].clips[0].timeline_in, tc(0.0));
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for s in plan.steps.iter().rev() {
            view.apply_inverse(&s.edit);
        }
        assert_eq!(seqs[0].tracks[0].clips.len(), 2);
        assert_eq!(seqs[0].tracks[0].clips[1].timeline_in, tc(4.0));
    }

    #[test]
    fn times_snap_to_frames_including_ntsc() {
        let fps = Fps::new(30000, 1001);
        let t = snap(1.0, fps);
        assert_eq!(t.value, 30 * 1001, "thirty frames of 1001 ticks");
        assert!((t.to_secs_f64() - 1.001).abs() < 1e-9);
        let ops_json = r#"[{"op":"move","clip_id":"00000000-0000-0000-0000-000000000000","to":{"offset":{"from":{"clip_end":"00000000-0000-0000-0000-000000000001"},"secs":-0.5}}}]"#;
        let ops: Vec<Op> = serde_json::from_str(ops_json).unwrap();
        assert!(matches!(&ops[0], Op::Move { to: Anchor::Offset { .. }, .. }));
    }

    #[test]
    fn an_insert_makes_room_and_a_ripple_trim_closes_up() {
        let (seq, pool, media, a, b) = fixture();
        // Insert one second of the file at 2 s: a is split there and
        // everything after moves a second later, on every track.
        let plan = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: Some(9.0), source_out: Some(10.0), insert: true }]).unwrap();
        assert!(plan.steps[0].what.contains("2 clip(s) split"), "the picture and its audio: {}", plan.steps[0].what);
        assert!((plan.duration_secs - 9.0).abs() < 1e-9, "{}", plan.text());
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let v = &seqs[0].tracks[0].clips;
        assert_eq!(v.len(), 4, "a's two halves, the insert, b");
        assert_eq!(v[1].timeline_in, tc(2.0));
        assert_eq!(v[3].timeline_in, tc(5.0), "b moved a second later");
        assert_eq!(seqs[0].tracks[1].clips[2].timeline_in, tc(5.0), "and its audio with it (its own audio was split too)");

        // A ripple out trim of a by a second: b closes up.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: a, edge: Edge::Out, to: Anchor::Secs(3.0), ripple: true }]).unwrap();
        let after = &plan.steps[0].after;
        assert_eq!(after.len(), 2);
        assert!((after[1].timeline_in_secs - 3.0).abs() < 1e-9, "{after:?}");
        // A ripple head trim keeps the start and shortens from the front.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: b, edge: Edge::In, to: Anchor::Secs(5.0), ripple: true }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_in_secs - 4.0).abs() < 1e-9 && (after[0].timeline_out_secs - 7.0).abs() < 1e-9, "{after:?}");
        assert!((after[0].source_in_secs - 5.0).abs() < 1e-9, "{after:?}");
    }

    #[test]
    fn j_and_l_cuts_move_the_audio_edge_and_leave_the_picture() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::JCut { incoming_clip: b, frames: 10 }]).unwrap();
        assert!(plan.steps[0].what.starts_with("J-cut of 10 frames"), "{}", plan.steps[0].what);
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_out_secs - 3.6).abs() < 1e-9, "outgoing audio ends 10 frames early: {after:?}");
        assert!((after[1].timeline_in_secs - 3.6).abs() < 1e-9 && (after[1].source_in_secs - 3.6).abs() < 1e-9, "{after:?}");
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert_eq!(seqs[0].tracks[0].clips[1].timeline_in, tc(4.0), "the picture cut has not moved");
        assert_eq!(seqs[0].tracks[1].clips[1].timeline_in, tc(3.6));

        let plan = resolve(&seq, &pool, &[Op::LCut { outgoing_clip: a, frames: 5 }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_out_secs - 4.2).abs() < 1e-9 && (after[0].source_out_secs - 4.2).abs() < 1e-9, "{after:?}");
        assert!((after[1].timeline_in_secs - 4.2).abs() < 1e-9, "{after:?}");

        let too_far = resolve(&seq, &pool, &[Op::JCut { incoming_clip: b, frames: 120 }]).unwrap_err();
        assert!(too_far.why.contains("shorter than a frame"), "{too_far}");
        let no_cut = resolve(&seq, &pool, &[Op::JCut { incoming_clip: a, frames: 5 }]).unwrap_err();
        assert!(no_cut.why.contains("no clip cut before"), "{no_cut}");
    }
}
