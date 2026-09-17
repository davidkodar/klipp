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
    /// Where a clip starts: the clip by id, or by where it is, which is
    /// how a step names a clip an earlier step of the same plan placed.
    ClipStart(ClipRef),
    /// Where a clip ends.
    ClipEnd(ClipRef),
    /// Where the last clip on a track ends (the start, on an empty
    /// track): where the next select of an assembly goes.
    TrackEnd(TrackRef),
    /// A timeline marker, by its label (case-insensitive).
    Marker(String),
    /// Another anchor moved by some seconds, negative for earlier.
    Offset { from: Box<Anchor>, secs: f64 },
}

/// A clip, by its id or by where it is: the clip covering `secs` on
/// track `track`, as the document stands when the op runs. The latter
/// is how a step names the halves a split before it made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClipRef {
    Id(ClipId),
    At { track: usize, secs: f64 },
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
        /// The file's sound with it, as a linked clip on the first audio
        /// track (made if there is none), at the same place. On by
        /// default; off for B-roll over a voice.
        #[serde(default = "default_true")]
        with_audio: bool,
    },
    /// A track: a video or text track goes on top (it becomes track 0
    /// and every other index moves down by one), an audio track at the
    /// bottom.
    AddTrack {
        kind: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// One edge of a clip to an anchor. The clip's other edge stays;
    /// the source moves with the edge, so the picture at the other edge
    /// does not change. Refused past the media's start or end, or into
    /// a neighbour — unless `ripple`, which moves the later clips on the
    /// track by the change in the clip's length (a head trim then keeps
    /// the clip where it starts and shortens it from the front).
    Trim {
        clip_id: ClipRef,
        edge: Edge,
        to: Anchor,
        #[serde(default)]
        ripple: bool,
    },
    /// A J-cut: the sound of `incoming_clip` starts `frames` before its
    /// picture, over the end of the clip before it. Both clips need
    /// linked audio, adjacent on one audio track: the outgoing audio is
    /// shortened by `frames`, the incoming audio starts that much earlier.
    JCut { incoming_clip: ClipRef, frames: i64 },
    /// An L-cut: the sound of `outgoing_clip` stays `frames` past its
    /// picture, under the start of the clip after it. The outgoing audio
    /// runs on by `frames`, the incoming audio starts that much later.
    LCut { outgoing_clip: ClipRef, frames: i64 },
    /// A clip into two at an anchor strictly inside it.
    Split { clip_id: ClipRef, at: Anchor },
    /// A clip off its track; with `ripple`, the clips after it on that
    /// track close the gap.
    Remove {
        clip_id: ClipRef,
        #[serde(default)]
        ripple: bool,
    },
    /// A clip's start to an anchor, its length unchanged; a linked audio
    /// or video partner goes with it. Refused into another clip.
    Move { clip_id: ClipRef, to: Anchor },
    /// Captions for what is said on the timeline, from the transcripts of
    /// the clips whose sound is heard: each word lands where its clip
    /// puts it, words gather into cues of at most `max_lines` lines of
    /// `max_chars`, a cue ends at a sentence, a pause or a cut. They go
    /// on a subtitle track (the first, made at the bottom if there is
    /// none, or `track`), between `from` and `to` when given.
    Captions {
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        from: Option<Anchor>,
        #[serde(default)]
        to: Option<Anchor>,
        #[serde(default = "default_caption_chars")]
        max_chars: usize,
        #[serde(default = "default_caption_lines")]
        max_lines: usize,
    },
    /// A clip's sound: its volume and pre-fader gain in decibels, its
    /// pan (-1 left to 1 right), a fade in and out in seconds. What is
    /// left out stays as it is. For an audio clip linked to a video
    /// clip the settings live on the video clip, where the app and the
    /// render read them, and that is the clip changed.
    Audio {
        clip_id: ClipRef,
        #[serde(default)]
        volume_db: Option<f32>,
        #[serde(default)]
        gain_db: Option<f32>,
        #[serde(default)]
        pan: Option<f32>,
        #[serde(default)]
        fade_in: Option<f32>,
        #[serde(default)]
        fade_out: Option<f32>,
    },
    /// Music under speech: the volume of `clip_id` drops by `by_db`
    /// (9 by default) wherever another clip is heard over it, moving
    /// there and back over `fade_secs` (0.5 by default), never as a step.
    /// Written as volume keyframes on the clip; any it had are replaced.
    Duck {
        clip_id: ClipRef,
        #[serde(default = "default_duck_db")]
        by_db: f32,
        #[serde(default = "default_duck_fade")]
        fade_secs: f64,
    },
    /// A lower third: `name`, and `role` under it, bottom left inside
    /// title-safe, for `secs` from `at`, fading in and out. It goes on a
    /// text track where it fits (one is made on top if none does).
    /// Refused across a shot change. The name comes from the brief or
    /// the cast list, never from a guess.
    LowerThird {
        name: String,
        #[serde(default)]
        role: Option<String>,
        at: Anchor,
        #[serde(default = "default_lower_third_secs")]
        secs: f64,
        #[serde(default)]
        track: Option<TrackRef>,
    },
}

fn default_caption_chars() -> usize {
    42
}

fn default_caption_lines() -> usize {
    2
}

fn default_lower_third_secs() -> f64 {
    4.0
}

fn default_duck_db() -> f32 {
    9.0
}

fn default_duck_fade() -> f64 {
    0.5
}

/// A word of a media file's transcript, in the file's seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct SpokenWord {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// Where the resolver gets transcripts from. The document does not know
/// how speech is recognised; whoever asks for a plan does.
pub trait Transcripts {
    /// The words of `item`, in order, or `None` when it has no transcript.
    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>>;
}

/// No transcripts at all: `captions` is refused.
pub struct NoTranscripts;

impl Transcripts for NoTranscripts {
    fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
        None
    }
}

fn default_true() -> bool {
    true
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

/// The clip a step names, by id or by place, or a refusal saying which.
fn resolve_clip<'a>(seq: &'a Sequence, fps: Fps, r: &ClipRef, step: usize) -> Result<(usize, usize, &'a Clip), Refusal> {
    match r {
        ClipRef::Id(id) => find_clip(seq, *id).ok_or_else(|| refuse(step, format!("no clip {}", id.0), "clip ids are in the timeline view; a clip made by an earlier step can be named by {track, secs}")),
        ClipRef::At { track, secs: at } => {
            let t = seq.tracks.get(*track).ok_or_else(|| refuse(step, format!("no track {track}"), format!("the sequence has tracks 0 to {}", seq.tracks.len().saturating_sub(1))))?;
            let tc = snap(*at, fps);
            let ci = t
                .clips
                .iter()
                .position(|c| c.timeline_in <= tc && tc < c.timeline_out)
                .ok_or_else(|| refuse(step, format!("nothing on track {track} at {at:.2} s"), "give a time inside a clip on that track"))?;
            Ok((*track, ci, &t.clips[ci]))
        }
    }
}

fn clip_ref_id(seq: &Sequence, fps: Fps, r: &ClipRef, step: usize) -> Result<ClipId, Refusal> {
    resolve_clip(seq, fps, r, step).map(|(_, _, c)| c.id)
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

/// A track kind by the name the ops use for it.
fn track_kind(name: &str) -> Option<TrackKind> {
    match name.to_lowercase().as_str() {
        "video" => Some(TrackKind::Video),
        "audio" => Some(TrackKind::Audio),
        "text" | "title" => Some(TrackKind::Text),
        "subtitle" | "subtitles" | "captions" => Some(TrackKind::Subtitle),
        _ => None,
    }
}

/// A track's index: the one given, or the first of a kind.
fn resolve_track(seq: &Sequence, t: &TrackRef, step: usize) -> Result<usize, Refusal> {
    match t {
        TrackRef::Index(n) if *n < seq.tracks.len() => Ok(*n),
        TrackRef::Index(n) => Err(refuse(step, format!("no track {n}"), format!("the sequence has tracks 0 to {}", seq.tracks.len().saturating_sub(1)))),
        TrackRef::Kind(kind) => {
            let want = track_kind(kind).ok_or_else(|| refuse(step, format!("\"{kind}\" is not a track kind"), "use video, audio, text or subtitle, or a track index"))?;
            seq.tracks.iter().position(|t| t.kind == want).ok_or_else(|| refuse(step, format!("no {} track", kind.to_lowercase()), "add one with add_track, or give a track index"))
        }
    }
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
        Anchor::ClipStart(r) => resolve_clip(seq, fps, r, step)?.2.timeline_in,
        Anchor::ClipEnd(r) => resolve_clip(seq, fps, r, step)?.2.timeline_out,
        Anchor::TrackEnd(t) => {
            let ti = resolve_track(seq, t, step)?;
            seq.tracks[ti].clips.iter().map(|c| c.timeline_out).max().unwrap_or_else(|| frame_tc(0, fps))
        }
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
    resolve_with(seq, pool, ops, &NoTranscripts)
}

/// [`resolve`], with the transcripts `captions` reads.
pub fn resolve_with(seq: &Sequence, pool: &MediaPool, ops: &[Op], transcripts: &dyn Transcripts) -> Result<Plan, Refusal> {
    let mut sims = vec![seq.clone()];
    let mut sim_pool = pool.clone();
    let mut sim_fps = seq.fps;
    let mut steps = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let sim = &sims[0];
        // The rate as the plan stands at this step: a place that set an
        // empty sequence's format changed it for every step after.
        let (what, edit, touched) = resolve_one(sim, &sim_pool, sim.fps, op, i, transcripts)?;
        let mut view = crate::apply::DocumentView { sequences: &mut sims, active: 0, media_pool: &mut sim_pool, fps: &mut sim_fps };
        view.apply_forward(&edit);
        let after = after_of(&sims[0], &touched);
        steps.push(Step { op: op.clone(), what, edit, after });
    }
    Ok(Plan { steps, duration_secs: secs(sims[0].total_duration()) })
}

fn resolve_one(seq: &Sequence, pool: &MediaPool, fps: Fps, op: &Op, i: usize, transcripts: &dyn Transcripts) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    match op {
        Op::Captions { track, from, to, max_chars, max_lines } => {
            let from = from.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            let to = to.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            captions(seq, pool, fps, i, transcripts, track.as_ref(), from, to, (*max_chars).max(8), (*max_lines).max(1))
        }
        Op::Audio { clip_id, volume_db, gain_db, pan, fade_in, fade_out } => {
            let (_, _, given) = resolve_clip(seq, fps, clip_id, i)?;
            audio_settings(seq, pool, i, given, *volume_db, *gain_db, *pan, *fade_in, *fade_out)
        }
        Op::Duck { clip_id, by_db, fade_secs } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            duck(seq, pool, i, ti, clip, *by_db, *fade_secs)
        }
        Op::LowerThird { name, role, at, secs: hold, track } => {
            let at = resolve_anchor(seq, fps, at, i)?;
            lower_third(seq, fps, i, name, role.as_deref(), at, *hold, track.as_ref())
        }
        Op::AddTrack { kind, name } => {
            let want = track_kind(kind).ok_or_else(|| refuse(i, format!("\"{kind}\" is not a track kind"), "use video, audio, text or subtitle"))?;
            // Pictures and titles go on top; sound and subtitles at the bottom.
            let at_top = matches!(want, TrackKind::Video | TrackKind::Text);
            let count = seq.tracks.iter().filter(|t| t.kind == want).count();
            let name = name.clone().unwrap_or_else(|| {
                let label = match want {
                    TrackKind::Video => "Video",
                    TrackKind::Audio => "Audio",
                    TrackKind::Text => "Text",
                    TrackKind::Subtitle => "Subtitle",
                };
                format!("{label} {}", count + 1)
            });
            let track = klipp_timeline::Track::new(&name, want);
            let index = if at_top { 0 } else { seq.tracks.len() };
            let what = format!("add {} track \"{name}\" as track {index}{}", kind.to_lowercase(), if at_top && !seq.tracks.is_empty() { "; every other track's index moves down by one" } else { "" });
            Ok((what, EditCommand::AddTrack { index, track }, Vec::new()))
        }

        Op::Place { media_id, track, at, source_in, source_out, insert, with_audio } => {
            let item = pool.get(media_id).ok_or_else(|| refuse(i, format!("no media item {}", media_id.0), "media ids are in list_media"))?;
            if item.offline {
                return Err(refuse(i, format!("{} is offline", item.filename), "relink the media first"));
            }
            let ti = resolve_track(seq, track, i)?;
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
            // An empty sequence takes the first picture's size and rate,
            // as an append in the app does; that place is already snapped
            // at the rate the sequence is about to have.
            let no_video_yet = !seq.tracks.iter().any(|t| t.kind == TrackKind::Video && !t.clips.is_empty());
            let autoset = if no_video_yet && item.resolution.width > 0 && item.fps.as_f64() > 0.0 && (seq.resolution != item.resolution || seq.fps != item.fps) {
                Some(((seq.resolution, seq.fps), (item.resolution, item.fps)))
            } else {
                None
            };
            let fps = autoset.map(|(_, (_, f))| f).unwrap_or(fps);
            let src_in = snap(s_in, fps);
            let src_out = snap(s_out, fps);
            if src_out <= src_in {
                return Err(refuse(i, "the source range is shorter than a frame", "make it at least one frame"));
            }
            let tl_in = resolve_anchor(seq, fps, at, i)?;
            let tl_out = tl_in + (src_out - src_in);
            let mut clip = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
            // The sound with the picture, linked both ways, on the first
            // audio track — made at the bottom when there is none.
            let bring_audio = *with_audio && item.has_audio && t.kind != TrackKind::Audio;
            // A video clip with no linked audio clip plays the sound in
            // its file; left out means silent, so the clip is muted.
            let muted = !*with_audio && item.has_audio && t.kind != TrackKind::Audio;
            if muted {
                clip.volume = 0.0;
            }
            let mut audio: Option<(Clip, usize)> = None;
            let mut created_audio_track_at = None;
            if bring_audio {
                let ati = match seq.tracks.iter().position(|t| t.kind == TrackKind::Audio) {
                    Some(ati) => ati,
                    None => {
                        created_audio_track_at = Some(seq.tracks.len());
                        seq.tracks.len()
                    }
                };
                if created_audio_track_at.is_none() {
                    if seq.tracks[ati].locked {
                        return Err(refuse(i, format!("audio track {ati} is locked"), "unlock it, or place with with_audio false"));
                    }
                    if !*insert {
                        if let Some(c) = overlapping(seq, ati, tl_in, tl_out, None) {
                            return Err(refuse(i, format!("its sound would overlap {} at {:.2}-{:.2} s on track {ati}", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)), "place it later, or with with_audio false"));
                        }
                    }
                }
                let mut a = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
                a.linked_clip_id = Some(clip.id);
                clip.linked_clip_id = Some(a.id);
                audio = Some((a, ati));
            }
            let sound_note = match (&audio, created_audio_track_at) {
                (Some(_), Some(ati)) => format!(", its sound linked on a new audio track {ati}"),
                (Some((_, ati)), None) => format!(", its sound linked on track {ati}"),
                _ if muted => ", its sound muted".to_string(),
                _ => String::new(),
            };
            let format_note = autoset.map(|(_, (r, f))| format!("; the sequence becomes {}x{} at {:.3} fps", r.width, r.height, f.as_f64())).unwrap_or_default();
            let ids: Vec<ClipId> = std::iter::once(clip.id).chain(audio.iter().map(|(a, _)| a.id)).collect();
            if *insert {
                let mut scratch = seq.clone();
                let edit = scratch
                    .insert_edit(ti, clip, tl_in)
                    .ok_or_else(|| refuse(i, format!("track {ti} would not take an insert"), "unlock the track, or place without insert"))?;
                let what = format!(
                    "insert {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s; {} clip(s) split, {} moved {:.2} s later{sound_note}",
                    item.filename,
                    secs(tl_in),
                    secs(src_in),
                    secs(src_out),
                    secs(tl_out),
                    edit.splits.len(),
                    edit.shifted.len(),
                    secs(tl_out - tl_in)
                );
                return Ok((what, EditCommand::LegacyInsertEdit { edit, audio, created_audio_track_at }, ids));
            }
            if let Some(c) = overlapping(seq, ti, tl_in, tl_out, None) {
                return Err(refuse(
                    i,
                    format!("it would overlap {} at {:.2}-{:.2} s", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)),
                    format!("place it at clip_end of {}, remove that clip first, or place with insert", c.id.0),
                ));
            }
            let what = format!(
                "place {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s{sound_note}{format_note}",
                item.filename,
                secs(tl_in),
                secs(src_in),
                secs(src_out),
                secs(tl_out)
            );
            let edit = if audio.is_some() || autoset.is_some() {
                EditCommand::LinkedImport { video: Some((clip, ti)), audio, created_video_track_at: None, created_audio_track_at, autoset }
            } else {
                EditCommand::LegacyAppendClip { clip, track_index: ti }
            };
            Ok((what, edit, ids))
        }

        Op::Trim { clip_id: clip_ref, edge, to, ripple } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let clip_id = &clip.id;
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

        Op::JCut { incoming_clip, frames } => split_edit(seq, pool, fps, i, clip_ref_id(seq, fps, incoming_clip, i)?, *frames, true),
        Op::LCut { outgoing_clip, frames } => split_edit(seq, pool, fps, i, clip_ref_id(seq, fps, outgoing_clip, i)?, *frames, false),

        Op::Split { clip_id: clip_ref, at } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let clip_id = &clip.id;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let cut = resolve_anchor(seq, fps, at, i)?;
            let mut scratch = seq.tracks[ti].clone();
            let Some((mut a, mut b)) = scratch.blade_at(*clip_id, cut) else {
                return Err(refuse(
                    i,
                    format!("{:.2} s is not inside {} ({:.2}-{:.2} s) by at least a frame", secs(cut), clip_name(seq, pool, clip), secs(clip.timeline_in), secs(clip.timeline_out)),
                    "split at a time strictly inside the clip",
                ));
            };
            // The linked sound (or picture) is cut at the same time, and
            // the halves are linked crosswise, as one split in the app is.
            let mut partner_edit = None;
            let mut ids = vec![a.id, b.id];
            if let Some((pti, pci, partner)) = clip.linked_clip_id.and_then(|pid| find_clip(seq, pid)) {
                if !seq.tracks[pti].locked {
                    let mut pscratch = seq.tracks[pti].clone();
                    if let Some((mut pa, mut pb)) = pscratch.blade_at(partner.id, cut) {
                        a.linked_clip_id = Some(pa.id);
                        pa.linked_clip_id = Some(a.id);
                        b.linked_clip_id = Some(pb.id);
                        pb.linked_clip_id = Some(b.id);
                        ids.push(pa.id);
                        ids.push(pb.id);
                        partner_edit = Some(EditCommand::LegacyBladeClip { track_index: pti, original_clip: partner.clone(), clip_a: pa, clip_b: pb, position: pci });
                    }
                }
            }
            let what = format!(
                "split {} at {:.2} s into {:.2}-{:.2} s and {:.2}-{:.2} s{}",
                clip_name(seq, pool, clip),
                secs(cut),
                secs(a.timeline_in),
                secs(a.timeline_out),
                secs(b.timeline_in),
                secs(b.timeline_out),
                if partner_edit.is_some() { ", its linked sound with it" } else { "" }
            );
            let main = EditCommand::LegacyBladeClip { track_index: ti, original_clip: clip.clone(), clip_a: a, clip_b: b, position: ci };
            let edit = match partner_edit {
                Some(p) => EditCommand::Batch(vec![main, p]),
                None => main,
            };
            Ok((what, edit, ids))
        }

        Op::Remove { clip_id: clip_ref, ripple } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let _ = clip_ref;
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

        Op::Move { clip_id: clip_ref, to } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let clip_id = &clip.id;
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

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn linear_to_db(v: f32) -> f32 {
    if v <= 0.0 { f32::NEG_INFINITY } else { 20.0 * v.log10() }
}

/// The clip whose sound settings `clip` plays with: the video clip it
/// is linked to when it has one, else itself. Playback and the render
/// read them there.
fn sound_settings_clip<'a>(seq: &'a Sequence, clip: &'a Clip) -> &'a Clip {
    clip.linked_clip_id
        .and_then(|lid| seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).find(|c| c.id == lid))
        .unwrap_or(clip)
}

#[allow(clippy::too_many_arguments)]
fn audio_settings(
    seq: &Sequence,
    pool: &MediaPool,
    i: usize,
    given: &Clip,
    volume_db: Option<f32>,
    gain_db: Option<f32>,
    pan: Option<f32>,
    fade_in: Option<f32>,
    fade_out: Option<f32>,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let clip = sound_settings_clip(seq, given);
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) {
        return Err(refuse(i, format!("{} has no sound", clip_name(seq, pool, clip)), "name a clip whose media has audio"));
    }
    if volume_db.is_none() && gain_db.is_none() && pan.is_none() && fade_in.is_none() && fade_out.is_none() {
        return Err(refuse(i, "nothing to change", "give volume_db, gain_db, pan, fade_in or fade_out"));
    }
    for (name, v, lo, hi) in [("volume_db", volume_db, -96.0, 12.0), ("gain_db", gain_db, -96.0, 24.0), ("pan", pan, -1.0, 1.0)] {
        if let Some(v) = v {
            if !v.is_finite() || v < lo || v > hi {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between {lo} and {hi}")));
            }
        }
    }
    let length = secs(clip.timeline_out - clip.timeline_in);
    let (fi, fo) = (fade_in.unwrap_or(clip.fade_in_secs), fade_out.unwrap_or(clip.fade_out_secs));
    if fi < 0.0 || fo < 0.0 || !fi.is_finite() || !fo.is_finite() {
        return Err(refuse(i, "a fade cannot be negative", "give fades in seconds, zero or more"));
    }
    if (fi + fo) as f64 > length + 1e-6 {
        return Err(refuse(i, format!("fades of {fi:.2} and {fo:.2} s do not fit a clip of {length:.2} s"), "shorten the fades"));
    }
    let new_volume = volume_db.map(db_to_linear).unwrap_or(clip.volume);
    let new_gain = gain_db.map(db_to_linear).unwrap_or(clip.gain);
    let new_pan = pan.unwrap_or(clip.pan);
    let mut parts = Vec::new();
    if volume_db.is_some() {
        parts.push(format!("volume {:.1} dB (was {:.1})", linear_to_db(new_volume), linear_to_db(clip.volume)));
    }
    if gain_db.is_some() {
        parts.push(format!("gain {:.1} dB (was {:.1})", linear_to_db(new_gain), linear_to_db(clip.gain)));
    }
    if pan.is_some() {
        parts.push(format!("pan {new_pan:.2}"));
    }
    if fade_in.is_some() {
        parts.push(format!("fade in {fi:.2} s"));
    }
    if fade_out.is_some() {
        parts.push(format!("fade out {fo:.2} s"));
    }
    let on = if clip.id == given.id { String::new() } else { " (set on the video clip it is linked to)".to_string() };
    let what = format!("sound of {}: {}{on}", clip_name(seq, pool, given), parts.join(", "));
    let edit = EditCommand::EditClipAudio {
        clip_id: clip.id,
        old_gain: clip.gain,
        old_volume: clip.volume,
        old_pan: clip.pan,
        old_fade_in: clip.fade_in_secs,
        old_fade_out: clip.fade_out_secs,
        old_channel_config: clip.channel_config,
        new_gain,
        new_volume,
        new_pan,
        new_fade_in: fi,
        new_fade_out: fo,
        new_channel_config: clip.channel_config,
    };
    Ok((what, edit, vec![given.id]))
}

/// Whether `clip` on `track` is heard: enabled, with sound, not silent,
/// and — on a video track — not standing in for a linked audio clip.
fn is_heard(seq: &Sequence, pool: &MediaPool, track: &klipp_timeline::Track, clip: &Clip) -> bool {
    if track.muted || !clip.enabled || !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
        return false;
    }
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) {
        return false;
    }
    if track.kind == TrackKind::Video {
        let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid)));
        if partner_on_audio {
            return false;
        }
    }
    sound_settings_clip(seq, clip).volume > 0.0
}

fn duck(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, music: &Clip, by_db: f32, fade_secs: f64) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if !(1.0..=40.0).contains(&by_db) {
        return Err(refuse(i, format!("{by_db} dB is not a duck"), "music under speech comes down six to twelve decibels"));
    }
    if !(0.05..=5.0).contains(&fade_secs) {
        return Err(refuse(i, format!("a move of {fade_secs} s"), "give fade_secs between 0.05 and 5; a duck is a fade, never a step"));
    }
    if !is_heard(seq, pool, &seq.tracks[ti], music) {
        return Err(refuse(i, format!("{} is not heard", clip_name(seq, pool, music)), "name the music clip: one with sound, not muted"));
    }
    let settings = sound_settings_clip(seq, music);
    let (m_in, m_out) = (secs(music.timeline_in), secs(music.timeline_out));
    // Where anything else is heard over the music, merged where the
    // gaps are too short to come up and go down again.
    let mut over: Vec<(f64, f64)> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
        .filter(|(t, c)| c.id != music.id && Some(c.id) != music.linked_clip_id && is_heard(seq, pool, t, c))
        .map(|(_, c)| (secs(c.timeline_in).max(m_in), secs(c.timeline_out).min(m_out)))
        .filter(|(a, b)| b > a)
        .collect();
    if over.is_empty() {
        return Err(refuse(i, "nothing is heard over it", "place the speech first; a duck follows what is on the timeline"));
    }
    over.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for (a, b) in over {
        match spans.last_mut() {
            Some(last) if a - last.1 < fade_secs * 2.5 => last.1 = last.1.max(b),
            _ => spans.push((a, b)),
        }
    }
    let base = settings.volume;
    let low = base * db_to_linear(-by_db);
    // Keyframes in the clip's own time: down before the speech starts,
    // up after it ends.
    let mut kfs: Vec<klipp_core::Keyframe> = Vec::new();
    let mut put = |t: f64, v: f32| klipp_core::keyframe::upsert_keyframe(&mut kfs, (t - m_in).clamp(0.0, m_out - m_in), v);
    for (a, b) in &spans {
        if *a - fade_secs > m_in {
            put(a - fade_secs, base);
        }
        put(*a, low);
        put(*b, low);
        if *b + fade_secs < m_out {
            put(b + fade_secs, base);
        }
    }
    kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    let old_state = crate::history::KeyframeSnapshot::from_clip(settings);
    let mut new_state = old_state.clone();
    new_state.volume_keyframes = kfs;
    let on = if settings.id == music.id { String::new() } else { " (on the video clip it is linked to)".to_string() };
    let what = format!(
        "duck {} by {by_db:.0} dB under {} stretch{} of other sound ({}), moving over {fade_secs:.2} s{on}",
        clip_name(seq, pool, music),
        spans.len(),
        if spans.len() == 1 { "" } else { "es" },
        spans.iter().map(|(a, b)| format!("{a:.2}-{b:.2} s")).collect::<Vec<_>>().join(", "),
    );
    let edit = EditCommand::EditKeyframes {
        clip_id: settings.id,
        linked_clip_id: None,
        description: "Duck",
        old_state,
        new_state,
        old_linked_state: None,
        new_linked_state: None,
    };
    Ok((what, edit, vec![music.id]))
}

/// A word placed on the timeline, and whether a cut in the sound comes
/// before it.
struct HeardWord {
    start: f64,
    end: f64,
    text: String,
    after_cut: bool,
}

/// The words heard on the timeline, in order: every enabled clip whose
/// sound plays — audio-track clips, and video clips with no audio clip
/// linked to them — that is not muted, its transcript's words moved from
/// the file's time to where the clip puts them.
fn heard_words(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<HeardWord> {
    let mut cache: std::collections::HashMap<MediaId, Option<Vec<SpokenWord>>> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for track in &seq.tracks {
        if track.muted || !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
            continue;
        }
        for clip in &track.clips {
            let ClipKind::Media { media_id } = &clip.kind else { continue };
            if !clip.enabled || clip.volume == 0.0 {
                continue;
            }
            if track.kind == TrackKind::Video {
                let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| {
                    seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid))
                });
                if partner_on_audio {
                    continue;
                }
            }
            let Some(item) = pool.get(media_id) else { continue };
            let Some(words) = cache.entry(*media_id).or_insert_with(|| transcripts.words(item)).as_ref() else { continue };
            let (s_in, s_out) = (secs(clip.source_in), secs(clip.source_out));
            let (t_in, t_out) = (secs(clip.timeline_in), secs(clip.timeline_out));
            let speed = if clip.speed > 0.0 { clip.speed } else { 1.0 };
            let mut first = true;
            for w in words.iter().filter(|w| w.start_secs >= s_in - 0.05 && w.start_secs < s_out) {
                let text = w.text.trim();
                if text.is_empty() {
                    continue;
                }
                let start = (t_in + (w.start_secs - s_in) / speed).max(t_in);
                let end = (t_in + (w.end_secs - s_in) / speed).clamp(start, t_out);
                out.push(HeardWord { start, end, text: text.to_string(), after_cut: first });
                first = false;
            }
        }
    }
    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// A caption: its time on the timeline and its lines.
#[derive(Debug, Clone, PartialEq)]
struct Cue {
    start: f64,
    end: f64,
    text: String,
}

/// Gather words into cues: a cue closes at a cut in the sound, a pause,
/// the end of a sentence once it has some length, when it is full, or
/// when it has been up long enough.
fn cues_from_words(words: &[HeardWord], max_chars: usize, max_lines: usize, sound_ends: f64) -> Vec<Cue> {
    const PAUSE_SECS: f64 = 0.7;
    const LONGEST_SECS: f64 = 6.0;
    const SENTENCE_MIN_CHARS: usize = 18;
    let room = max_chars * max_lines;
    let mut cues: Vec<Cue> = Vec::new();
    let mut cur: Vec<&HeardWord> = Vec::new();
    let close = |cur: &mut Vec<&HeardWord>, cues: &mut Vec<Cue>| {
        if let (Some(a), Some(b)) = (cur.first(), cur.last()) {
            let text = cur.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            cues.push(Cue { start: a.start, end: b.end, text });
        }
        cur.clear();
    };
    for w in words {
        if let Some(last) = cur.last() {
            let len: usize = cur.iter().map(|x| x.text.chars().count() + 1).sum();
            let sentence_done = last.text.ends_with(['.', '?', '!']) && len >= SENTENCE_MIN_CHARS;
            let full = len + w.text.chars().count() > room;
            let long = w.end - cur[0].start > LONGEST_SECS;
            if w.after_cut || w.start - last.end > PAUSE_SECS || sentence_done || full || long {
                close(&mut cur, &mut cues);
            }
        }
        cur.push(w);
    }
    close(&mut cur, &mut cues);
    // Held a moment past the last word, at least long enough to read,
    // and never into the next cue.
    let n = cues.len();
    for k in 0..n {
        // The last cue ends with the sound: a caption held past it would
        // make the timeline longer than the piece.
        let next = if k + 1 < n { cues[k + 1].start } else { sound_ends };
        let want = (cues[k].end + 0.2).max(cues[k].start + 1.0);
        cues[k].end = want.min(next).max(cues[k].start);
        cues[k].text = wrap_lines(&cues[k].text, max_chars, max_lines);
    }
    cues
}

/// `text` on at most `max_lines` lines of about `max_chars`, the lines
/// as even as the words allow.
fn wrap_lines(text: &str, max_chars: usize, max_lines: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars || max_lines <= 1 {
        return text.to_string();
    }
    let lines_needed = total.div_ceil(max_chars).clamp(2, max_lines);
    let target = total.div_ceil(lines_needed);
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split_whitespace() {
        let more_lines = lines.len() < lines_needed;
        let cur = lines.last_mut().expect("one line at least");
        let len = cur.chars().count();
        if len > 0 && len + 1 + word.chars().count() > target.max(1) + 3 && more_lines {
            lines.push(word.to_string());
        } else {
            if len > 0 {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    lines.join("\n")
}

/// A caption's look: white, centred low in the frame, outlined so it
/// reads over any picture. The look a subtitle file imports with.
fn caption_title(text: &str) -> klipp_timeline::TitleData {
    let mut tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 36.0,
        font_name: String::new(),
        alignment: klipp_timeline::TextAlignment::Center,
        ..Default::default()
    };
    tb.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() });
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
    }
    td
}

fn title_clip(td: klipp_timeline::TitleData, tl_in: Timecode, tl_out: Timecode, fps: Fps) -> Clip {
    Clip {
        id: ClipId::new(),
        kind: ClipKind::Title(td),
        source_in: frame_tc(0, fps),
        source_out: tl_out - tl_in,
        ..Clip::gap(tl_in, tl_out)
    }
}

#[allow(clippy::too_many_arguments)]
fn captions(
    seq: &Sequence,
    pool: &MediaPool,
    fps: Fps,
    i: usize,
    transcripts: &dyn Transcripts,
    track: Option<&TrackRef>,
    from: Option<Timecode>,
    to: Option<Timecode>,
    max_chars: usize,
    max_lines: usize,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let (from_s, to_s) = (from.map(secs).unwrap_or(0.0), to.map(secs).unwrap_or(f64::MAX));
    if to_s <= from_s {
        return Err(refuse(i, "the range ends before it starts", "give from before to"));
    }
    let words: Vec<HeardWord> = heard_words(seq, pool, transcripts).into_iter().filter(|w| w.start >= from_s && w.start < to_s).collect();
    if words.is_empty() {
        return Err(refuse(i, "no transcribed speech is heard on the timeline there", "captions come from the transcripts: run analyze and wait for it, and place the speech first"));
    }
    let sound_ends = secs(seq.total_duration());
    let cues = cues_from_words(&words, max_chars, max_lines, sound_ends);
    // The track: the one named, else the first subtitle track, else a new one at the bottom.
    let existing = match track {
        Some(t) => Some(resolve_track(seq, t, i)?),
        None => seq.tracks.iter().position(|t| t.kind == TrackKind::Subtitle),
    };
    if let Some(ti) = existing {
        let t = &seq.tracks[ti];
        if !matches!(t.kind, TrackKind::Subtitle | TrackKind::Text) {
            return Err(refuse(i, format!("track {ti} is not a subtitle or text track"), "name a subtitle track, or leave the track out"));
        }
        if t.locked {
            return Err(refuse(i, format!("track {ti} is locked"), "unlock it, or use another track"));
        }
    }
    let ti = existing.unwrap_or(seq.tracks.len());
    let mut edits = Vec::with_capacity(cues.len() + 1);
    if existing.is_none() {
        let n = seq.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle).count() + 1;
        edits.push(EditCommand::AddTrack { index: ti, track: klipp_timeline::Track::new(&format!("Subtitle {n}"), TrackKind::Subtitle) });
    }
    let mut ids = Vec::new();
    let mut last_out: Option<Timecode> = None;
    for cue in &cues {
        let mut tl_in = snap(cue.start, fps);
        if let Some(prev) = last_out {
            if tl_in < prev {
                tl_in = prev;
            }
        }
        let tl_out = snap(cue.end, fps).max(tl_in + one_frame(fps));
        if existing.is_some() {
            if let Some(c) = overlapping(seq, ti, tl_in, tl_out, None) {
                return Err(refuse(i, format!("a caption at {:.2}-{:.2} s would overlap {} on track {ti}", secs(tl_in), secs(tl_out), clip_name(seq, pool, c)), "remove the captions that are there, give from and to around them, or name another track"));
            }
        }
        let clip = title_clip(caption_title(&cue.text), tl_in, tl_out, fps);
        ids.push(clip.id);
        last_out = Some(tl_out);
        edits.push(EditCommand::LegacyAppendClip { clip, track_index: ti });
    }
    let first = cues.first().map(|c| c.text.replace('\n', " / ")).unwrap_or_default();
    let what = format!(
        "captions: {} cues from {:.2} to {:.2} s on {}subtitle track {ti}, at most {max_lines} lines of {max_chars}; the first reads \"{first}\"",
        cues.len(),
        cues.first().map(|c| c.start).unwrap_or(0.0),
        cues.last().map(|c| c.end).unwrap_or(0.0),
        if existing.is_none() { "a new " } else { "" },
    );
    // The plan reports the first and last cue, not hundreds.
    let touched: Vec<ClipId> = ids.first().into_iter().chain(ids.last().filter(|_| ids.len() > 1)).copied().collect();
    Ok((what, EditCommand::Batch(edits), touched))
}

/// A two-line lower third, bottom left inside title-safe: the name, and
/// the role smaller and spaced under it, both lifted off the picture by
/// a soft shadow, fading in and out.
fn lower_third_title(name: &str, role: Option<&str>) -> klipp_timeline::TitleData {
    use klipp_timeline::{default_text_transform, Easing, FontStyle, Motion, MotionKind, ObjectTransform, TextAlignment, TextBox, TitleMotion, TitleObjectContent, TitlePosition};
    let lift = klipp_timeline::TitleShadow {
        color: [0, 0, 0, 120],
        offset_x: 0.0,
        offset_y: 3.0,
        blur: 12.0,
        enabled: true,
        offset_x_keyframes: Vec::new(),
        offset_y_keyframes: Vec::new(),
        blur_keyframes: Vec::new(),
    };
    let motion = |delay: f32| {
        TitleMotion::both(
            Motion::new(MotionKind::Fade).duration(0.4).easing(Easing::OutCubic).delay(delay),
            Motion::new(MotionKind::Fade).duration(0.4).easing(Easing::InCubic),
        )
    };
    let at = |y: f32| ObjectTransform { center: TitlePosition { x: 0.08, y }, anchor_norm: TitlePosition { x: 0.0, y: 0.5 }, ..default_text_transform() };
    let mut td = klipp_timeline::TitleData {
        objects: Vec::new(),
        next_object_id: 0,
        animation_type: Default::default(),
        background: klipp_timeline::TitleBackground::None,
    };
    let mut name_box = TextBox {
        text: name.to_string(),
        font_size: 46.0,
        font_name: "Inter".into(),
        font_style: FontStyle::Bold,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Left,
        ..Default::default()
    };
    name_box.shadows.push(lift.clone());
    name_box.motion = motion(0.0);
    td.add_object(TitleObjectContent::TextBox(name_box), at(if role.is_some() { 0.815 } else { 0.85 }));
    if let Some(role) = role {
        let mut role_box = TextBox {
            text: role.to_string(),
            font_size: 24.0,
            font_name: "Inter".into(),
            font_style: FontStyle::Regular,
            color: [205, 205, 212, 255],
            alignment: TextAlignment::Left,
            ..Default::default()
        };
        role_box.case_transform = klipp_timeline::TextCase::Uppercase;
        role_box.tracking = 3.0;
        role_box.shadows.push(lift);
        role_box.motion = motion(0.1);
        td.add_object(TitleObjectContent::TextBox(role_box), at(0.868));
    }
    td
}

#[allow(clippy::too_many_arguments)]
fn lower_third(
    seq: &Sequence,
    fps: Fps,
    i: usize,
    name: &str,
    role: Option<&str>,
    at: Timecode,
    hold_secs: f64,
    track: Option<&TrackRef>,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let name = name.trim();
    let role = role.map(str::trim).filter(|r| !r.is_empty());
    if name.is_empty() {
        return Err(refuse(i, "a lower third needs a name", "names come from the brief or the cast list"));
    }
    if !hold_secs.is_finite() || hold_secs < 1.5 {
        return Err(refuse(i, format!("{hold_secs} s is too short to read a name"), "hold it about four seconds, never under two"));
    }
    let tl_in = at;
    let tl_out = tl_in + snap(hold_secs, fps);
    if tl_out > seq.total_duration() {
        return Err(refuse(i, format!("it would run to {:.2} s, past the end of the timeline at {:.2} s", secs(tl_out), secs(seq.total_duration())), "start it earlier or hold it shorter"));
    }
    // Never across a shot change: a clip edge on a video track while it is up.
    let cut = seq
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && t.visible)
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| [c.timeline_in, c.timeline_out])
        .filter(|e| *e > tl_in && *e < tl_out)
        .min();
    if let Some(cut) = cut {
        return Err(refuse(i, format!("the picture changes at {:.2} s while it is up ({:.2}-{:.2} s)", secs(cut), secs(tl_in), secs(tl_out)), "a lower third does not run across a shot change: start it after the cut, or end it before"));
    }
    // The track: the one named, else a text track with room, else a new one on top.
    let fits = |ti: usize| overlapping(seq, ti, tl_in, tl_out, None).is_none() && !seq.tracks[ti].locked;
    let existing = match track {
        Some(t) => {
            let ti = resolve_track(seq, t, i)?;
            if !matches!(seq.tracks[ti].kind, TrackKind::Text | TrackKind::Video) {
                return Err(refuse(i, format!("track {ti} is not a text or video track"), "name a text track, or leave the track out"));
            }
            if !fits(ti) {
                return Err(refuse(i, format!("track {ti} is locked or has a clip there"), "leave the track out and one with room is found or made"));
            }
            Some(ti)
        }
        None => (0..seq.tracks.len()).find(|ti| seq.tracks[*ti].kind == TrackKind::Text && fits(*ti)),
    };
    let clip = title_clip(lower_third_title(name, role), tl_in, tl_out, fps);
    let id = clip.id;
    let what = format!(
        "lower third \"{name}\"{} at {:.2}-{:.2} s on {}",
        role.map(|r| format!(", \"{r}\"")).unwrap_or_default(),
        secs(tl_in),
        secs(tl_out),
        match existing {
            Some(ti) => format!("track {ti}"),
            None if seq.tracks.is_empty() => "a new text track 0".to_string(),
            None => "a new text track 0; every other track's index moves down by one".to_string(),
        },
    );
    let edit = EditCommand::AddTitleClip { clip, track_index: existing.unwrap_or(0), created_track: existing.is_none() };
    Ok((what, edit, vec![id]))
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
    // Compared as times, not as fields: a clip placed before the sequence
    // changed rate carries the old rate in its timecodes.
    let same = |a: Timecode, b: Timecode| a.cmp(&b) == std::cmp::Ordering::Equal;
    let Some(neighbour) = neighbour.filter(|n| if j { same(n.timeline_out, clip.timeline_in) } else { same(n.timeline_in, clip.timeline_out) }) else {
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
    if !same(out_a.timeline_out, in_a.timeline_in) {
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

    /// The fixture's file says a sentence in each clip and one the
    /// timeline never uses.
    struct Spoken;

    impl Transcripts for Spoken {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            let say = |at: f64, text: &str| -> Vec<SpokenWord> {
                text.split(' ').enumerate().map(|(k, w)| SpokenWord { start_secs: at + k as f64 * 0.2, end_secs: at + k as f64 * 0.2 + 0.18, text: format!(" {w}") }).collect()
            };
            Some([say(0.5, "Nobody is afraid of the machine."), say(4.4, "What changed it for me was agents, and that is the long and the short of it."), say(9.0, "Never used.")].concat())
        }
    }

    #[test]
    fn an_assembly_names_its_own_clips_and_the_end_of_a_track() {
        let (seq, pool, media, _, _) = fixture();
        let plan = resolve(&seq, &pool, &[
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::TrackEnd(TrackRef::Kind("video".into())), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: true },
            // The clip that place made, by where it is: the next goes at its end.
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::At { track: 0, secs: 8.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipStart(ClipRef::At { track: 0, secs: 9.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
        ]);
        // The first two land end to end; the third names the second's start, where the second is.
        let err = plan.unwrap_err();
        assert_eq!(err.step, 2);
        assert!(err.why.contains("overlap"), "{err}");
        let plan = resolve(&seq, &pool, &[
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::TrackEnd(TrackRef::Index(0)), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: true },
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::At { track: 0, secs: 8.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
        ]).unwrap();
        assert_eq!((plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (8.0, 9.0));
        assert_eq!((plan.steps[1].after[0].timeline_in_secs, plan.steps[1].after[0].timeline_out_secs), (9.0, 10.0));
    }

    #[test]
    fn captions_follow_the_sound_and_break_at_cuts_and_sentences() {
        let (seq, pool, _, _, _) = fixture();
        // Without transcripts there is nothing to caption, and it says what would help.
        let err = resolve(&seq, &pool, &[Op::Captions { track: None, from: None, to: None, max_chars: 42, max_lines: 2 }]).unwrap_err();
        assert!(err.hint.contains("analyze"), "{err}");

        let plan = resolve_with(&seq, &pool, &[Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2 }], &Spoken).unwrap();
        assert!(plan.steps[0].what.starts_with("captions: 3 cues"), "{}", plan.steps[0].what);
        assert_eq!(plan.duration_secs, 8.0, "captions do not lengthen the piece");
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let after = view.sequences[0].clone();
        let track = after.tracks.last().unwrap();
        assert_eq!(track.kind, TrackKind::Subtitle, "a subtitle track at the bottom");
        let texts: Vec<String> = track.clips.iter().map(|c| c.title_data().unwrap().text()).collect();
        // Thirty-two characters on lines of thirty: two even lines.
        assert_eq!(texts[0], "Nobody is afraid of\nthe machine.");
        // The second sentence is over sixty characters: two cues, each on two lines at most.
        assert!(texts[1].starts_with("What changed it for me") && texts[1].contains('\n'), "{texts:?}");
        assert!(texts.iter().all(|t| t.lines().count() <= 2 && !t.contains("Never")), "{texts:?}");
        // On the timeline where the clip puts the words, in order, never overlapping.
        assert!((secs(track.clips[0].timeline_in) - 0.52).abs() < 0.021, "{}", secs(track.clips[0].timeline_in));
        assert!((secs(track.clips[1].timeline_in) - 4.4).abs() < 0.021);
        assert!(track.clips.windows(2).all(|w| w[0].timeline_out <= w[1].timeline_in));
        // Undone, the track goes with them.
        view.apply_inverse(&plan.steps[0].edit);
        assert_eq!(view.sequences[0].tracks.len(), 2);

        // A second time onto the same track is refused where cues are already.
        let twice = [Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2 }, Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2 }];
        let err = resolve_with(&seq, &pool, &twice, &Spoken).unwrap_err();
        assert_eq!(err.step, 1);
        assert!(err.why.contains("overlap"), "{err}");
    }

    #[test]
    fn sound_settings_land_where_they_are_read_and_a_duck_follows_the_speech() {
        let (mut seq, pool, media, a, _) = fixture();
        // A music bed under the whole piece, on an audio track of its own.
        let mut music_track = Track::new("A2", TrackKind::Audio);
        let music = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        let music_id = music.id;
        music_track.clips = vec![music];
        seq.tracks.push(music_track);

        // Named by its audio clip, the speech's settings go on the video
        // clip it is linked to: that is where playback and the render read them.
        let plan = resolve(&seq, &pool, &[Op::Audio { clip_id: ClipRef::At { track: 1, secs: 1.0 }, volume_db: Some(-6.0), gain_db: None, pan: Some(-0.5), fade_in: Some(0.5), fade_out: None }]).unwrap();
        let EditCommand::EditClipAudio { clip_id, new_volume, new_pan, new_fade_in, new_fade_out, new_gain, .. } = &plan.steps[0].edit else { panic!("an audio edit") };
        assert_eq!(*clip_id, a, "{}", plan.steps[0].what);
        assert!((new_volume - 0.5012).abs() < 1e-3 && *new_pan == -0.5 && *new_fade_in == 0.5 && *new_fade_out == 0.0 && *new_gain == 1.0);
        assert!(plan.steps[0].what.contains("linked"), "{}", plan.steps[0].what);
        // Fades that do not fit, a level out of range, nothing to change: refused.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: None, gain_db: None, pan: None, fade_in: Some(3.0), fade_out: Some(2.0) }).why.contains("do not fit"));
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: Some(40.0), gain_db: None, pan: None, fade_in: None, fade_out: None }).why.contains("out of range"));
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: None, gain_db: None, pan: None, fade_in: None, fade_out: None }).why.contains("nothing"));

        // The duck: speech runs 0 to 8 s in two clips that meet, one stretch;
        // the music comes back up over half a second after it.
        let plan = resolve(&seq, &pool, &[
            Op::Audio { clip_id: ClipRef::Id(music_id), volume_db: Some(-6.0), gain_db: None, pan: None, fade_in: None, fade_out: Some(1.0) },
            Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 12.0, fade_secs: 0.5 },
        ]).unwrap();
        assert!(plan.steps[1].what.contains("1 stretch") && plan.steps[1].what.contains("0.00-8.00"), "{}", plan.steps[1].what);
        let EditCommand::EditKeyframes { clip_id, new_state, .. } = &plan.steps[1].edit else { panic!("keyframes") };
        assert_eq!(*clip_id, music_id);
        let at = |t: f64| klipp_core::keyframe::evaluate(&new_state.volume_keyframes, t, 1.0);
        let (base, low) = (0.5012f32, 0.5012 * 0.2512);
        assert!((at(4.0) - low).abs() < 1e-3, "under the speech: {}", at(4.0));
        assert!((at(9.5) - base).abs() < 1e-3, "after it: {}", at(9.5));
        assert!(at(8.25) > low && at(8.25) < base, "on the way up: {}", at(8.25));
        // A duck of nothing, a step, a clip that is not heard: refused.
        let alone = { let mut s2 = seq.clone(); s2.tracks = vec![seq.tracks[2].clone()]; s2 };
        assert!(resolve(&alone, &pool, &[Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 9.0, fade_secs: 0.5 }]).unwrap_err().why.contains("nothing is heard"));
        assert!(bad(Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 9.0, fade_secs: 0.0 }).hint.contains("never a step"));
        // As the tools send them.
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"audio","clip_id":{"track":2,"secs":1.0},"volume_db":-8,"fade_in":2},{"op":"duck","clip_id":{"track":2,"secs":1.0}}]"#).unwrap();
    }

    #[test]
    fn a_lower_third_sits_inside_a_shot() {
        let (seq, pool, _, _, _) = fixture();
        let lt = |at: f64, secs: f64| Op::LowerThird { name: "David Heinemeier Hansson".into(), role: Some("Creator of Omarchy".into()), at: Anchor::Secs(at), secs, track: None };
        let plan = resolve(&seq, &pool, &[lt(4.4, 3.0)]).unwrap();
        assert!(plan.steps[0].what.contains("a new text track 0"), "{}", plan.steps[0].what);
        assert_eq!((plan.steps[0].after[0].track, plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (0, 4.4, 7.4));
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        let td = clip.title_data().unwrap();
        assert_eq!(td.objects.len(), 2, "the name and the role");
        assert_eq!(td.text(), "David Heinemeier Hansson");

        // Across the cut at 4 s it is refused, with the cut named.
        let err = resolve(&seq, &pool, &[lt(2.0, 4.0)]).unwrap_err();
        assert!(err.why.contains("4.00 s") && err.hint.contains("shot change"), "{err}");
        // Too short to read, no name, past the end: refused.
        assert!(resolve(&seq, &pool, &[lt(4.5, 1.0)]).is_err());
        assert!(resolve(&seq, &pool, &[lt(6.0, 3.0)]).unwrap_err().why.contains("past the end"));
        let nameless = Op::LowerThird { name: " ".into(), role: None, at: Anchor::Secs(4.5), secs: 3.0, track: None };
        assert!(resolve(&seq, &pool, &[nameless]).unwrap_err().hint.contains("cast list"));

        // A second one finds the text track the first made; the ops read as JSON the way the tools send them.
        let ops: Vec<Op> = serde_json::from_str(r#"[
            {"op":"lower_third","name":"Brodie","at":{"secs":0.5},"secs":3},
            {"op":"lower_third","name":"DHH","role":"Omarchy","at":{"clip_start":{"track":1,"secs":5.0}}},
            {"op":"captions"},
            {"op":"place","media_id":"00000000-0000-0000-0000-000000000000","track":"video","at":{"track_end":"video"}}
        ]"#).unwrap();
        let plan = resolve_with(&seq, &pool, &ops[..3], &Spoken).unwrap();
        assert!(plan.steps[1].what.contains("on track 0") && plan.steps[1].what.contains("4.00-8.00"), "{}", plan.steps[1].what);
    }

    #[test]
    fn a_plan_runs_in_order_and_reads_as_text() {
        let (seq, pool, media, a, b) = fixture();
        let ops = vec![
            Op::Split { clip_id: ClipRef::Id(b), at: Anchor::Marker("intro".into()) },
            Op::Place { media_id: media, track: TrackRef::Kind("video".into()), at: Anchor::ClipEnd(ClipRef::Id(b)), source_in: Some(8.0), source_out: Some(9.0), insert: false, with_audio: false },
            Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false },
        ];
        // The split replaces b, so its end no longer exists as `b`:
        // the plan must refuse the place with a reason naming that.
        let err = resolve(&seq, &pool, &ops).unwrap_err();
        assert_eq!(err.step, 1);
        assert!(err.why.contains("no clip"), "{err}");

        let ops = vec![
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::Id(b)), source_in: Some(8.0), source_out: Some(9.0), insert: false, with_audio: false },
            Op::Split { clip_id: ClipRef::Id(b), at: Anchor::Marker("intro".into()) },
            Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false },
        ];
        let plan = resolve(&seq, &pool, &ops).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!((plan.duration_secs - 9.0).abs() < 1e-9, "{}", plan.text());
        assert_eq!(plan.steps[1].after.len(), 4, "a split reports both halves of the picture and of its linked sound");
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
        let over = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: None, source_out: Some(1.0), insert: false, with_audio: false }]).unwrap_err();
        assert!(over.why.contains("overlap"), "{over}");
        assert!(over.hint.contains("clip_end"), "{over}");
        let past = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(b), edge: Edge::Out, to: Anchor::Secs(11.0), ripple: false }]).unwrap_err();
        assert!(past.why.contains("after this clip's out point"), "{past}");
        let into = resolve(&seq, &pool, &[Op::Move { clip_id: ClipRef::Id(a), to: Anchor::Secs(1.0) }]).unwrap_err();
        assert!(into.why.contains("overlap"), "{into}");
        let outside = resolve(&seq, &pool, &[Op::Split { clip_id: ClipRef::Id(a), at: Anchor::Secs(4.0) }]).unwrap_err();
        assert!(outside.why.contains("not inside"), "{outside}");
        let no_marker = resolve(&seq, &pool, &[Op::Move { clip_id: ClipRef::Id(a), to: Anchor::Marker("outro".into()) }]).unwrap_err();
        assert!(no_marker.why.contains("no marker"), "{no_marker}");
    }

    #[test]
    fn a_ripple_remove_closes_the_gap_and_the_commands_replay_on_the_document() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::Remove { clip_id: ClipRef::Id(a), ripple: true }]).unwrap();
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
        let plan = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: Some(9.0), source_out: Some(10.0), insert: true, with_audio: false }]).unwrap();
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
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: true }]).unwrap();
        let after = &plan.steps[0].after;
        assert_eq!(after.len(), 2);
        assert!((after[1].timeline_in_secs - 3.0).abs() < 1e-9, "{after:?}");
        // A ripple head trim keeps the start and shortens from the front.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(b), edge: Edge::In, to: Anchor::Secs(5.0), ripple: true }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_in_secs - 4.0).abs() < 1e-9 && (after[0].timeline_out_secs - 7.0).abs() < 1e-9, "{after:?}");
        assert!((after[0].source_in_secs - 5.0).abs() < 1e-9, "{after:?}");
    }

    /// A clip placed before the sequence changed rate keeps the old rate
    /// in its timecodes; the cut between it and a clip at the new rate is
    /// still a cut.
    #[test]
    fn a_j_cut_meets_a_neighbour_at_another_rate() {
        let (mut seq, pool, _, _, b) = fixture();
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[0];
            c.timeline_in = c.timeline_in.rescale_to(50);
            c.timeline_out = c.timeline_out.rescale_to(50);
            assert_ne!(c.timeline_out, tc(4.0), "the fields differ");
        }
        let plan = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 10 }]).unwrap();
        assert!(plan.steps[0].what.starts_with("J-cut of 10 frames"), "{}", plan.steps[0].what);
    }

    #[test]
    fn j_and_l_cuts_move_the_audio_edge_and_leave_the_picture() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 10 }]).unwrap();
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

        let plan = resolve(&seq, &pool, &[Op::LCut { outgoing_clip: ClipRef::Id(a), frames: 5 }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_out_secs - 4.2).abs() < 1e-9 && (after[0].source_out_secs - 4.2).abs() < 1e-9, "{after:?}");
        assert!((after[1].timeline_in_secs - 4.2).abs() < 1e-9, "{after:?}");

        let too_far = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 120 }]).unwrap_err();
        assert!(too_far.why.contains("shorter than a frame"), "{too_far}");
        let no_cut = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(a), frames: 5 }]).unwrap_err();
        assert!(no_cut.why.contains("no clip cut before"), "{no_cut}");
    }

    #[test]
    fn a_place_brings_linked_sound_sets_an_empty_sequence_and_a_split_half_is_named_by_place() {
        let (seq, pool, media, _, _) = fixture();
        // An empty sequence at 25 fps with a 24 fps item to place.
        let mut empty = Sequence::new_default();
        empty.fps = Fps::new(25, 1);
        empty.tracks = vec![Track::new("V1", TrackKind::Video)];
        let mut pool2 = pool.clone();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/b.mov".into(), None);
        item.duration = RationalTime::from_secs_f64(10.0, 24);
        item.fps = Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 1280, height: 720 };
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media2 = pool2.add(item);
        let plan = resolve(&empty, &pool2, &[
            Op::Place { media_id: media2, track: TrackRef::Kind("video".into()), at: Anchor::Secs(0.0), source_in: Some(1.0), source_out: Some(3.0), insert: false, with_audio: true },
            Op::Split { clip_id: ClipRef::At { track: 0, secs: 0.5 }, at: Anchor::Secs(1.0) },
            Op::JCut { incoming_clip: ClipRef::At { track: 0, secs: 1.5 }, frames: 6 },
            // Frames count at the rate the sequence has by now, 24, not
            // the 25 it started with: 48 frames is 2.0 s, the first clip's
            // end, and the source range was snapped at 24 too.
            Op::Place { media_id: media2, track: TrackRef::Index(0), at: Anchor::Frame(48), source_in: Some(4.0), source_out: Some(5.0), insert: false, with_audio: true },
        ]).unwrap();
        let w = &plan.steps[0].what;
        assert!(w.contains("new audio track 1") && w.contains("1280x720 at 24.000 fps"), "{w}");
        assert_eq!((plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (0.0, 2.0));
        assert_eq!((plan.steps[3].after[0].timeline_in_secs, plan.steps[3].after[0].timeline_out_secs), (2.0, 3.0));
        assert_eq!(plan.steps[0].after.len(), 2, "picture and sound");
        assert_eq!(plan.steps[0].after[1].track, 1);
        assert!(plan.steps[2].what.starts_with("J-cut of 6 frames"), "{}", plan.steps[2].what);
        // Replay on a document: the audio track exists, the clips are linked, the rate is the item's.
        let mut seqs = vec![empty.clone()];
        let mut p = pool2.clone();
        let mut fps = empty.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for st in &plan.steps {
            view.apply_forward(&st.edit);
        }
        assert_eq!(seqs[0].tracks.len(), 2);
        assert_eq!(seqs[0].fps, Fps::new(24, 1));
        assert_eq!(seqs[0].tracks[0].clips.len(), 3, "two halves and the second place");
        let v0 = &seqs[0].tracks[0].clips[0];
        assert!(v0.linked_clip_id.is_some());

        // Without its sound, a video clip is muted: the engine would
        // otherwise play the sound in its file.
        let plan = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(20.0), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: false }]).unwrap();
        assert!(plan.steps[0].what.ends_with("its sound muted"), "{}", plan.steps[0].what);
        assert_eq!(plan.steps[0].after.len(), 1);
        match &plan.steps[0].edit {
            EditCommand::LegacyAppendClip { clip, .. } => assert_eq!(clip.volume, 0.0),
            other => panic!("expected an append, got {other:?}"),
        }

        // An add_track on top shifts the indices, and says so.
        let plan = resolve(&seq, &pool, &[Op::AddTrack { kind: "video".into(), name: None }]).unwrap();
        assert!(plan.steps[0].what.contains("as track 0") && plan.steps[0].what.contains("moves down"), "{}", plan.steps[0].what);
        let _ = media;
    }
}
