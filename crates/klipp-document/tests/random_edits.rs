//! Random edits against the editing core's invariants. From a seeded
//! generator, a small project gets long runs of real edits through the
//! paths the app and the ops use: the ops (place, insert and overwrite,
//! trims with and without ripple, split, move across tracks, remove with
//! and without ripple, retime, slip, slide, transitions) and the magnetic
//! primary's own edits the timeline makes (ripple trims, ripple delete,
//! move), with undo and redo at random. The timeline's blade is the
//! split op, so the magnetic blade, which cuts one clip alone, isn't one. After every step:
//!
//! 1. each linked picture and its sound are in sync: the same moment of
//!    their file under the same moment of the timeline, within half a
//!    frame of the file (their ends may differ, as a J or L cut's do);
//! 2. the primary track has no gap;
//! 3. an undo gives back exactly the state before its edit;
//! 4. a redo gives back exactly the state after it;
//! 5. the sequence saved and loaded back is the same, every time in it
//!    the same time (a time may come back written at another rate).
//!
//! A failure of a known shape (`KNOWN`) is noted, its step taken out and
//! the run goes on, so the steps after it are checked too. Any other is
//! shrunk to the shortest run of steps that still fails the same way,
//! printed as a repro, a list of the steps, and fails the test.
//!
//! Each step names its clip by its place among the clips there are and
//! its times as shares of what there is, so a shorter run replays the
//! steps left. Roll, captions and First Cut are not here: roll is the
//! timeline panel's own edit, the other two need a transcript.
//!
//! The usual run takes a few seeds. `KLIPP_RANDOM_EDITS_SEEDS=N` runs N
//! seeds and `KLIPP_RANDOM_EDITS_STEPS=M` M steps each;
//! `KLIPP_RANDOM_EDITS_SKIP=PrimaryTrimHead,Slide` leaves those kinds of
//! step out, to look past a failure already found.

use klipp_core::{ClipId, MediaId, RationalTime};
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_document::ops::{resolve, Op};
use klipp_media::MediaPool;
use klipp_timeline::magnetic::MagneticEditor;
use klipp_timeline::{Clip, Sequence};
use serde_json::{json, Value};

/// What is known to fail, each by its shape and the entry in the private
/// bug list that holds it: a failure of that shape is reported, not a
/// failure of this test, and any other shape is. Each goes when its fix
/// lands, and the test then proves the fix.
const KNOWN: &[(&str, fn(&Broken, &str) -> bool)] = &[
    // A link to a clip that isn't there.
    ("R5", |b, _| b.invariant == SYNC && b.detail.contains("isn't there")),
    // A transition saving drops: one left on a clip split or deleted.
    ("R4", |b, _| b.invariant == SAVED && b.detail.contains("/transitions")),
    // Undo or redo giving back a time at another rate, the same time.
    ("R8", |b, _| b.invariant.ends_with("(a time written at another rate)")),
    // Under a frame off: a sound placed to the sample against pictures
    // on frames; a pair's sync off by up to a frame of the file.
    ("R6", |b, _| b.invariant == GAPLESS && b.amount.abs() < FRAME),
    ("R7", |b, _| b.invariant == SYNC && b.amount <= 1.0 + 1e-6),
    // A primary clip starting a frame or more before the one before it
    // ends: two over each other.
    ("R9", |b, _| b.invariant == GAPLESS && b.amount <= -FRAME),
    // A slide leaving a pair out of sync.
    ("R10", |b, last| b.invariant == SYNC && last == "Slide"),
    // Undo or redo giving back other clip ids, on a clip or a transition.
    ("R11", |b, _| (b.invariant == "undo restores the state before" || b.invariant == "redo restores the state after") && b.detail.contains("id: ")),
    // A move leaving its partner.
    ("R2", |b, last| b.invariant == SYNC && last == "Move"),
    // A ripple remove closing up one track under linked clips.
    ("R3", |b, last| b.invariant == SYNC && last == "Remove"),
];

const SYNC: &str = "linked pairs in sync";
const GAPLESS: &str = "primary gapless";
const SAVED: &str = "save and load";
const FRAME: f64 = 1.0 / RATE as f64;

/// The known entry `broken` is, from the step it broke at in `steps`.
fn known(broken: &Broken, steps: &[Step]) -> Option<&'static str> {
    let last = steps.get(broken.step).map(kind).unwrap_or_default();
    KNOWN.iter().find(|(_, is)| is(broken, &last)).map(|(entry, _)| *entry)
}

const RATE: u32 = 25;

/// A small, fast generator: the same seed gives the same steps on every
/// machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn share(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

/// One step. A clip is its place among the clips there are at the time,
/// counted round; a time a share of the span it is in.
#[derive(Debug, Clone)]
enum Step {
    Place { media: usize, track: usize, at: f64, frames: i64, insert: bool },
    Trim { clip: usize, out: bool, to: f64, ripple: bool },
    Split { clip: usize, at: f64 },
    Move { clip: usize, by_frames: i64, track: Option<usize> },
    Remove { clip: usize, ripple: bool },
    Retime { clip: usize, speed: f64, ripple: bool },
    Slip { clip: usize, frames: i64 },
    Slide { clip: usize, frames: i64 },
    Transition { clip: usize, remove: bool },
    PrimaryTrimHead { clip: usize, frames: i64 },
    PrimaryTrimTail { clip: usize, frames: i64 },
    PrimaryRippleDelete { clip: usize },
    PrimaryMove { clip: usize, to: f64 },
    Undo,
    Redo,
}

fn random_step(r: &mut Rng) -> Step {
    let clip = r.below(64) as usize;
    let frames = |r: &mut Rng| r.below(60) as i64 - 30;
    match r.below(16) {
        0 | 1 => Step::Place { media: r.below(2) as usize, track: r.below(4) as usize, at: r.share() * 1.2, frames: 12 + r.below(100) as i64, insert: r.coin() },
        2 => Step::Trim { clip, out: r.coin(), to: r.share(), ripple: r.coin() },
        3 => Step::Split { clip, at: r.share() },
        4 => Step::Move { clip, by_frames: frames(r) * 4, track: r.coin().then(|| r.below(4) as usize) },
        5 => Step::Remove { clip, ripple: r.coin() },
        6 => Step::Retime { clip, speed: [0.5, 0.75, 1.5, 2.0][r.below(4) as usize], ripple: r.coin() },
        7 => Step::Slip { clip, frames: frames(r) },
        8 => Step::Slide { clip, frames: frames(r) },
        9 => Step::Transition { clip, remove: r.below(3) == 0 },
        10 => Step::PrimaryTrimHead { clip, frames: frames(r) },
        11 => Step::PrimaryTrimTail { clip, frames: frames(r) },
        12 => Step::PrimaryRippleDelete { clip },
        13 => Step::PrimaryMove { clip, to: r.share() },
        14 => Step::Undo,
        _ => Step::Redo,
    }
}

fn t(frames: i64) -> RationalTime {
    RationalTime::from_frames(frames, RATE)
}

fn secs(t: RationalTime) -> f64 {
    t.to_secs_f64()
}

fn frame_of(s: f64) -> i64 {
    (s * RATE as f64).round() as i64
}

/// The sequence as it is stored, to compare by.
fn state(seq: &Sequence) -> Value {
    serde_json::to_value(seq).unwrap()
}

/// The project every run starts from: a picture file with sound, a sound
/// file, and three of the picture's clips with their sound on V1 and A1.
fn fixture() -> (Sequence, MediaPool, [MediaId; 2]) {
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(RATE, 1);
    let mut pool = MediaPool::new();
    // Sixty seconds of each file.
    let mut picture = klipp_media::MediaItem::synthetic_for_test("/tmp/random-edits.mov".into(), None);
    picture.duration = t(RATE as i64 * 60);
    picture.video_codec = "h264".into();
    picture.has_audio = true;
    let mut sound = klipp_media::MediaItem::synthetic_for_test("/tmp/random-edits.wav".into(), None);
    sound.duration = t(RATE as i64 * 60);
    sound.has_audio = true;
    let media = [pool.add(picture), pool.add(sound)];
    let mut fps = seq.fps;
    let mut seqs = vec![seq];
    for k in 0..3 {
        let op: Op = serde_json::from_value(json!({"op": "place", "media_id": media[0].0.to_string(), "track": 0, "at": {"secs": 4.0 * k as f64}, "source_in": 2.0 * k as f64, "source_out": 2.0 * k as f64 + 4.0})).unwrap();
        let plan = resolve(&seqs[0], &pool, &[op]).unwrap_or_else(|r| panic!("the fixture: {}", r.why));
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for s in &plan.steps {
            view.apply_forward(&s.edit);
        }
    }
    (seqs.remove(0), pool, media)
}

/// What broke, at which step, and how.
#[derive(Debug, Clone)]
struct Broken {
    step: usize,
    invariant: &'static str,
    detail: String,
    /// How far out, where it is a distance: a pair's sync in frames (the
    /// file's or the sequence's, whichever is fewer), a gap in seconds
    /// (less than 0 for two over each other); infinite otherwise.
    amount: f64,
}

/// A broken invariant, what it was, and how far out.
type Fail = (&'static str, String, f64);

/// One undo step: its edits and the states either side of it.
struct Done {
    edits: Vec<EditCommand>,
    before: Value,
    after: Value,
}

struct Run {
    seq: Sequence,
    pool: MediaPool,
    media: [MediaId; 2],
    undo: Vec<Done>,
    redo: Vec<Done>,
    dir: std::path::PathBuf,
}

impl Run {
    fn new(dir: std::path::PathBuf) -> Self {
        let (seq, pool, media) = fixture();
        Run { seq, pool, media, undo: Vec::new(), redo: Vec::new(), dir }
    }

    fn forward(&mut self, edits: &[EditCommand]) {
        let mut fps = self.seq.fps;
        let mut seqs = vec![std::mem::replace(&mut self.seq, Sequence::new_default())];
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut self.pool, fps: &mut fps };
        for e in edits {
            view.apply_forward(e);
        }
        self.seq = seqs.remove(0);
    }

    fn inverse(&mut self, edits: &[EditCommand]) {
        let mut fps = self.seq.fps;
        let mut seqs = vec![std::mem::replace(&mut self.seq, Sequence::new_default())];
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut self.pool, fps: &mut fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        self.seq = seqs.remove(0);
    }

    /// The clips a step can name, in a fixed order: by track, then time.
    fn clips(&self, primary_only: bool) -> Vec<(usize, Clip)> {
        let p = self.seq.primary_track_index();
        let mut out: Vec<(usize, Clip)> = self.seq.tracks.iter().enumerate()
            .filter(|(ti, _)| !primary_only || Some(*ti) == p)
            .flat_map(|(ti, tr)| tr.clips.iter().filter(|c| !c.kind.is_gap()).map(move |c| (ti, c.clone())))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.timeline_in.cmp(&b.1.timeline_in)));
        out
    }

    fn pick(&self, k: usize, primary_only: bool) -> Option<(usize, Clip)> {
        let all = self.clips(primary_only);
        (!all.is_empty()).then(|| all[k % all.len()].clone())
    }

    /// The edits of an op, as the ops resolve them; `None` when refused,
    /// which is no step.
    fn op(&self, op: Value) -> Option<Vec<EditCommand>> {
        let op: Op = serde_json::from_value(op).ok()?;
        resolve(&self.seq, &self.pool, &[op]).ok().map(|p| p.steps.into_iter().map(|s| s.edit).collect())
    }

    /// A magnetic edit made on the sequence, as the timeline makes it: the
    /// edit in place, and its record.
    fn magnetic(&mut self, edit: impl FnOnce(&mut Sequence) -> Option<klipp_timeline::magnetic_commands::MagneticEditCommand>) -> Option<Vec<EditCommand>> {
        let mut seq = self.seq.clone();
        let cmd = edit(&mut seq)?;
        self.seq = seq;
        Some(vec![EditCommand::Magnetic(cmd)])
    }

    /// `step` made, as one undo step; `Ok(false)` when there was nothing
    /// to do or it was refused.
    fn take(&mut self, step: &Step) -> Result<bool, Fail> {
        let id = |c: &Clip| c.id.0.to_string();
        let before = state(&self.seq);
        let end = self.seq.total_duration();
        let made: Option<(Vec<EditCommand>, bool)> = match step {
            Step::Undo => {
                let Some(done) = self.undo.pop() else { return Ok(false) };
                self.inverse(&done.edits);
                let now = state(&self.seq);
                if now != done.before {
                    let only_rates = same_times(now.clone()) == same_times(done.before.clone());
                    return Err((if only_rates { "undo restores the state before (a time written at another rate)" } else { "undo restores the state before" }, first_difference(&done.before, &now), f64::INFINITY));
                }
                self.redo.push(done);
                return Ok(true);
            }
            Step::Redo => {
                let Some(done) = self.redo.pop() else { return Ok(false) };
                self.forward(&done.edits);
                let now = state(&self.seq);
                if now != done.after {
                    let only_rates = same_times(now.clone()) == same_times(done.after.clone());
                    return Err((if only_rates { "redo restores the state after (a time written at another rate)" } else { "redo restores the state after" }, first_difference(&done.after, &now), f64::INFINITY));
                }
                self.undo.push(done);
                return Ok(true);
            }
            Step::Place { media, track, at, frames, insert } => {
                let start = frame_of(secs(end) * at);
                let src = (*frames).min(RATE as i64 * 50);
                self.op(json!({"op": "place", "media_id": self.media[*media % 2].0.to_string(), "track": track % self.seq.tracks.len().max(1), "at": {"secs": secs(t(start))}, "source_in": 1.0, "source_out": 1.0 + secs(t(src)), "insert": insert})).map(|e| (e, true))
            }
            Step::Trim { clip, out, to, ripple } => self.pick(*clip, false).and_then(|(_, c)| {
                let (a, b) = (frame_of(secs(c.timeline_in)) - 20, frame_of(secs(c.timeline_out)) + 20);
                let at = a.max(0) + ((b - a.max(0)) as f64 * to) as i64;
                self.op(json!({"op": "trim", "clip_id": id(&c), "edge": if *out { "out" } else { "in" }, "to": {"secs": secs(t(at))}, "ripple": ripple}))
            }).map(|e| (e, true)),
            Step::Split { clip, at } => self.pick(*clip, false).and_then(|(_, c)| {
                let (a, b) = (frame_of(secs(c.timeline_in)), frame_of(secs(c.timeline_out)));
                self.op(json!({"op": "split", "clip_id": id(&c), "at": {"secs": secs(t(a + ((b - a) as f64 * at) as i64))}}))
            }).map(|e| (e, true)),
            Step::Move { clip, by_frames, track } => self.pick(*clip, false).and_then(|(_, c)| {
                let to = (frame_of(secs(c.timeline_in)) + by_frames).max(0);
                let mut op = json!({"op": "move", "clip_id": id(&c), "to": {"secs": secs(t(to))}});
                if let Some(tr) = track {
                    op["track"] = json!(tr % self.seq.tracks.len().max(1));
                }
                self.op(op)
            }).map(|e| (e, true)),
            Step::Remove { clip, ripple } => self.pick(*clip, false).and_then(|(_, c)| self.op(json!({"op": "remove", "clip_id": id(&c), "ripple": ripple}))).map(|e| (e, true)),
            Step::Retime { clip, speed, ripple } => self.pick(*clip, false).and_then(|(_, c)| self.op(json!({"op": "retime", "clip_id": id(&c), "speed": speed, "ripple": ripple}))).map(|e| (e, true)),
            Step::Slip { clip, frames } => self.pick(*clip, false).and_then(|(_, c)| self.op(json!({"op": "slip", "clip_id": id(&c), "secs": secs(t(*frames))}))).map(|e| (e, true)),
            Step::Slide { clip, frames } => self.pick(*clip, false).and_then(|(_, c)| self.op(json!({"op": "slide", "clip_id": id(&c), "secs": secs(t(*frames))}))).map(|e| (e, true)),
            Step::Transition { clip, remove } => self.pick(*clip, false).and_then(|(_, c)| self.op(json!({"op": "transition", "clip_id": id(&c), "kind": "cross_dissolve", "secs": 0.4, "remove": remove}))).map(|e| (e, true)),
            Step::PrimaryTrimHead { clip, frames } => self.pick(*clip, true).and_then(|(_, c)| {
                let to = c.timeline_in + t(*frames);
                (to < c.timeline_out && secs(c.source_in) + secs(t(*frames)) >= 0.0).then_some(())?;
                self.magnetic(|s| Some(MagneticEditor::trim_head(s, c.id, to).command))
            }).map(|e| (e, false)),
            Step::PrimaryTrimTail { clip, frames } => self.pick(*clip, true).and_then(|(_, c)| {
                let to = c.timeline_out + t(*frames);
                // Never past the file's end, as the timeline's trim stops.
                (to > c.timeline_in && secs(c.source_out) + secs(t(*frames)) <= 60.0).then_some(())?;
                self.magnetic(|s| Some(MagneticEditor::trim_tail(s, c.id, to).command))
            }).map(|e| (e, false)),
            Step::PrimaryRippleDelete { clip } => self.pick(*clip, true).and_then(|(_, c)| self.magnetic(|s| Some(MagneticEditor::ripple_delete(s, c.id).command))).map(|e| (e, false)),
            Step::PrimaryMove { clip, to } => self.pick(*clip, true).and_then(|(_, c)| {
                let at = t(frame_of(secs(end) * to));
                // As the timeline's drag does with Linked on (#2967): the
                // pictures' sound follows, one undo step.
                let before_move = self.seq.clone();
                let mut edits = self.magnetic(|s| Some(MagneticEditor::move_primary(s, c.id, at).command))?;
                for p in MagneticEditor::follow_linked_partners(&before_move, &mut self.seq) {
                    edits.push(EditCommand::LegacyMoveClip { clip_id: p.clip_id, old_timeline_in: p.old_in, old_timeline_out: p.old_out, new_timeline_in: p.new_in, new_timeline_out: p.new_out, linked_partner: None });
                }
                Some(edits)
            }).map(|e| (e, false)),
        };
        let Some((edits, through_ops)) = made else { return Ok(false) };
        if edits.is_empty() {
            return Ok(false);
        }
        if through_ops {
            self.forward(&edits);
        }
        let after = state(&self.seq);
        if after == before {
            return Ok(false);
        }
        self.redo.clear();
        self.undo.push(Done { edits, before, after });
        Ok(true)
    }

    /// The five invariants on the sequence as it is now.
    fn check(&self) -> Result<(), Fail> {
        for c in self.seq.all_clips() {
            let Some(p) = c.linked_clip_id else { continue };
            let Some(partner) = self.seq.all_clips().find(|x| x.id == p) else {
                return Err((SYNC, format!("{} links to {}, which isn't there", short(c.id), short(p)), f64::INFINITY));
            };
            // Where the file's start falls on the timeline, for each.
            let zero = |x: &Clip| secs(x.timeline_in) - secs(x.source_in) / x.speed;
            if c.media_id().is_none() || partner.media_id() != c.media_id() || c.is_reversed() || partner.is_reversed() {
                continue;
            }
            // How far apart in the file, in its seconds: within half a frame
            // of the file (24 fps, the coarser rate), as a picture's file
            // time falls on the file's frames and a sound's anywhere, so a
            // cut between them rounds.
            let apart = (zero(c) - zero(partner)).abs() * c.speed;
            // And in frames, the file's or the sequence's, whichever is
            // fewer: what a rounding to either gives.
            let frames = (apart * 24.0).min(apart / c.speed * RATE as f64);
            if (c.speed - partner.speed).abs() > 1e-9 || apart > 0.5 / 24.0 + 1e-6 {
                return Err((SYNC, format!(
                    "{} at {:.3}-{:.3} s from {:.3} s of its file at {}x, its partner {} at {:.3}-{:.3} s from {:.3} s at {}x",
                    short(c.id), secs(c.timeline_in), secs(c.timeline_out), secs(c.source_in), c.speed,
                    short(p), secs(partner.timeline_in), secs(partner.timeline_out), secs(partner.source_in), partner.speed
                ), if (c.speed - partner.speed).abs() > 1e-9 { f64::INFINITY } else { frames }));
            }
        }
        if let Some(p) = self.seq.primary_track() {
            let mut expected = RationalTime::ZERO;
            for c in &p.clips {
                if !c.timeline_in.cmp(&expected).is_eq() {
                    return Err((GAPLESS, format!("{} starts at {:?}, the clip before ends at {:?}", short(c.id), c.timeline_in, expected), secs(c.timeline_in) - secs(expected)));
                }
                expected = c.timeline_out;
            }
        }
        let built = state(&self.seq);
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::create_dir_all(&self.dir);
        klipp_project::save_project(&self.dir, std::slice::from_ref(&self.seq), &self.pool).map_err(|e| (SAVED, format!("save refused: {e}"), f64::INFINITY))?;
        let (loaded, ..) = klipp_project::load_project(&self.dir).map_err(|e| (SAVED, format!("load refused: {e}"), f64::INFINITY))?;
        let (built, back) = (same_times(built), same_times(state(&loaded[0])));
        if back != built {
            return Err((SAVED, first_difference(&built, &back), f64::INFINITY));
        }
        Ok(())
    }
}

/// `v` with every time (`{"value", "rate"}`) written in lowest terms, so
/// a time written at another rate compares as the same time.
fn same_times(v: Value) -> Value {
    fn gcd(a: i128, b: i128) -> i128 {
        if b == 0 { a.abs() } else { gcd(b, a % b) }
    }
    match v {
        Value::Object(m) if m.len() == 2 && m.get("value").is_some_and(Value::is_i64) && m.get("rate").is_some_and(Value::is_u64) => {
            let (n, d) = (i128::from(m["value"].as_i64().unwrap()), i128::from(m["rate"].as_u64().unwrap()));
            let g = gcd(n, d).max(1);
            json!({"value": (n / g) as i64, "rate": (d / g) as u64})
        }
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, x)| (k, same_times(x))).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(same_times).collect()),
        other => other,
    }
}

fn short(id: ClipId) -> String {
    id.0.to_string()[..8].to_string()
}

/// `steps` run from the fixture; the first broken invariant, if any. A
/// panic is a broken invariant too.
fn run(steps: &[Step], dir: &std::path::Path) -> Option<Broken> {
    let mut r = Run::new(dir.to_path_buf());
    for (k, step) in steps.iter().enumerate() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.take(step).and_then(|moved| if moved { r.check() } else { Ok(()) })));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err((invariant, detail, amount))) => return Some(Broken { step: k, invariant, detail, amount }),
            Err(panic) => {
                let detail = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "a panic".into());
                return Some(Broken { step: k, invariant: "no panic", detail: detail.lines().next().unwrap_or("").to_string(), amount: f64::INFINITY });
            }
        }
    }
    None
}

/// What a failure is, to keep while shrinking: its known entry, or the
/// invariant it broke.
fn label(broken: &Broken, steps: &[Step]) -> &'static str {
    known(broken, steps).unwrap_or(broken.invariant)
}

/// The shortest run of `steps` found that breaks the same way: steps
/// taken out, a chunk at a time and then one at a time, while it still
/// breaks as it did.
fn shrink(steps: Vec<Step>, broken: &Broken, dir: &std::path::Path) -> (Vec<Step>, Broken) {
    let mut best = steps[..=broken.step].to_vec();
    let mut why = broken.clone();
    let want = label(broken, &steps);
    let mut chunk = (best.len() / 2).max(1);
    loop {
        let mut shorter = false;
        let mut at = 0;
        while at < best.len() {
            let mut fewer = best.clone();
            fewer.drain(at..(at + chunk).min(fewer.len()));
            match run(&fewer, dir) {
                Some(b) if label(&b, &fewer) == want => {
                    best = fewer[..=b.step].to_vec();
                    why = b;
                    shorter = true;
                }
                _ => at += chunk,
            }
        }
        if !shorter {
            if chunk == 1 {
                break;
            }
            chunk = (chunk / 2).max(1);
        }
    }
    (best, why)
}

fn first_difference(want: &Value, got: &Value) -> String {
    fn walk(path: String, a: &Value, b: &Value) -> Option<String> {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for k in x.keys().chain(y.keys()) {
                    let (va, vb) = (x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
                    if va != vb {
                        return walk(format!("{path}/{k}"), va, vb);
                    }
                }
                None
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find(|(_, (p, q))| p != q).and_then(|(i, (p, q))| walk(format!("{path}/{i}"), p, q)),
            _ => {
                let short = |v: &Value| v.to_string().chars().take(80).collect::<String>();
                Some(format!("{path}: {} != {}", short(a), short(b)))
            }
        }
    }
    walk(String::new(), want, got).unwrap_or_else(|| "differs".into())
}

/// A step's kind, as its repro names it.
fn kind(step: &Step) -> String {
    format!("{step:?}").split([' ', '{']).next().unwrap_or("").to_string()
}

fn env(name: &str, or: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(or)
}

#[test]
fn random_edits_keep_the_editing_cores_invariants() {
    let seeds = env("KLIPP_RANDOM_EDITS_SEEDS", 8);
    let steps = env("KLIPP_RANDOM_EDITS_STEPS", 60) as usize;
    let skipped: Vec<String> = std::env::var("KLIPP_RANDOM_EDITS_SKIP").unwrap_or_default().split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    let dir = klipp_testing::scratch::scratch_dir("klipp-random-edits");
    // A panic is caught and reported as a repro, not printed each time
    // the shrink replays it.
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut report = Vec::new();
    let mut new = Vec::new();
    for seed in 1..=seeds {
        let mut rng = Rng(seed);
        let mut run_steps: Vec<Step> = (0..steps).map(|_| random_step(&mut rng)).filter(|s| !skipped.iter().any(|k| kind(s) == k.as_str())).collect();
        // A known failure doesn't end the seed: the step that broke is
        // taken out and the rest run, so what follows it is still checked.
        let mut known_here = Vec::new();
        while let Some(broken) = run(&run_steps, &dir) {
            if let Some(entry) = known(&broken, &run_steps) {
                known_here.push(entry);
                run_steps.remove(broken.step);
                continue;
            }
            let (repro, why) = shrink(run_steps, &broken, &dir);
            let text = format!(
                "seed {seed}: NEW: {} broken at step {} of {}: {}\n{}",
                why.invariant,
                why.step + 1,
                repro.len(),
                why.detail,
                repro.iter().enumerate().map(|(k, s)| format!("  {:>2}. {s:?}", k + 1)).collect::<Vec<_>>().join("\n")
            );
            new.push(text.clone());
            report.push(text);
            break;
        }
        if !known_here.is_empty() {
            report.push(format!("seed {seed}: known {}", known_here.join(", ")));
        }
    }
    std::panic::set_hook(quiet);
    let _ = std::fs::write(dir.join("report.txt"), report.join("\n\n"));
    for r in &report {
        eprintln!("{r}\n");
    }
    assert!(new.is_empty(), "{} seed(s) broke an invariant in a way not known; repros above and in {}", new.len(), dir.join("report.txt").display());
}
