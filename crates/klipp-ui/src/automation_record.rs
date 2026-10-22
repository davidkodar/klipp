//! Recording a track's control moves as its automation (#2306).
//!
//! While playback runs, each audio track's controls are written by its
//! mode: Write over the whole pass, Touch while the control is held, Latch
//! from its first touch until playback stops (its last value held after
//! it is let go). A control being written is sampled every frame at the
//! playhead; when playback stops, each run of samples is thinned to the
//! shape of the move and put over the track's keys in its span, every
//! track the pass wrote changed in one undo step.
//!
//! A control the user moves while it plays from keys, or while a pass
//! writes it, is not set: its value is live, here, until it is let go or
//! the pass ends, and the engine plays that value over the keys, so what is
//! heard is what is written and the control's value as set stays as it
//! was. Write starts from the value the control was playing.
//!
//! A pass belongs to its sequence: switched away from, or another project
//! opened, it is dropped.

use std::collections::{HashMap, HashSet};

use klipp_core::automation::{AutomationMode, TrackParam};
use klipp_core::{Keyframe, SequenceId, TrackId};
use klipp_timeline::{Sequence, Track, TrackAutomation, TrackKind};

/// The controls recorded.
const PARAMS: [TrackParam; 2] = [TrackParam::Volume, TrackParam::Pan];

/// How far a thinned move may stray from what was played: in dB for
/// volume, in pan units for pan.
const VOLUME_TOLERANCE_DB: f32 = 0.25;
const PAN_TOLERANCE: f32 = 0.01;

/// How long a written run takes to leave and return to the keys around
/// it: the old curve is kept up to this far before the run and from this
/// far after.
const GLIDE_SECS: f64 = 0.05;

/// The longest step between two frames that is still playing on: a longer
/// one, or one back (a loop, a seek, reverse play), ends the run, and the
/// next frame starts another.
const MAX_STEP_SECS: f64 = 0.25;

/// One control's recording in a pass: its runs of samples, (sequence
/// seconds, value), each moving forward in time.
#[derive(Default)]
struct Series {
    runs: Vec<Vec<(f64, f32)>>,
    latched: bool,
    writing: bool,
}

/// A pass being recorded, and the controls being moved.
#[derive(Default)]
pub struct Pass {
    sequence: Option<SequenceId>,
    series: HashMap<(TrackId, TrackParam), Series>,
    live: HashMap<(TrackId, TrackParam), f32>,
    /// The values the engine was last told to play, by track index.
    sent: HashMap<(usize, TrackParam), f32>,
}

impl Pass {
    /// Whether anything was written.
    pub fn has_samples(&self) -> bool {
        self.series.values().any(|s| s.runs.iter().any(|r| !r.is_empty()))
    }

    /// The value `param` of track `track` plays at while it is moved or
    /// written, if it is.
    pub fn live(&self, track: TrackId, param: TrackParam) -> Option<f32> {
        self.live.get(&(track, param)).copied()
    }

    /// The user moved `param` of track `track` to `value`, a control that
    /// plays from its keys or is being written.
    pub fn set_live(&mut self, track: TrackId, param: TrackParam, value: f32) {
        self.live.insert((track, param), value);
    }

    /// What the pass has written of `param` on track `track` so far, run
    /// by run, (sequence seconds, value).
    pub fn runs(&self, track: TrackId, param: TrackParam) -> impl Iterator<Item = &[(f64, f32)]> {
        self.series.get(&(track, param)).into_iter().flat_map(|s| s.runs.iter().map(Vec::as_slice))
    }

    /// One frame at `secs`, `held` the controls the user holds now by
    /// track index: while `playing`, each control written samples its
    /// value. The controls whose value the engine plays changed: `Some`
    /// to play that, `None` back to their keys.
    pub fn tick(&mut self, seq: &Sequence, held: &HashSet<(usize, TrackParam)>, playing: bool, secs: f64) -> Vec<(usize, TrackParam, Option<f32>)> {
        if self.sequence.is_some_and(|s| s != seq.id) {
            self.series.clear();
            self.live.clear();
        }
        self.sequence = Some(seq.id);
        for (i, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio) {
            for param in PARAMS {
                let key = (track.id, param);
                let touched = held.contains(&(i, param));
                let s = self.series.entry(key).or_default();
                s.latched = playing && (s.latched || touched && track.automation.mode == AutomationMode::Latch);
                let writing = playing
                    && match track.automation.mode {
                        AutomationMode::Write => true,
                        AutomationMode::Touch => touched,
                        AutomationMode::Latch => s.latched,
                        AutomationMode::Off | AutomationMode::Read => false,
                    };
                if writing {
                    let value = *self.live.entry(key).or_insert_with(|| track.automation.value_at(param, secs, as_set(track, param)));
                    let last = s.runs.last().and_then(|r| r.last()).map(|&(t, _)| t);
                    if !s.writing || last.is_none_or(|t| secs < t || secs - t > MAX_STEP_SECS) {
                        s.runs.push(Vec::new());
                    }
                    if let Some(run) = s.runs.last_mut() {
                        if run.last().is_none_or(|&(t, _)| secs > t) {
                            run.push((secs, value));
                        }
                    }
                } else if !touched {
                    self.live.remove(&key);
                }
                s.writing = writing;
            }
        }

        let index: HashMap<TrackId, usize> = seq.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        self.live.retain(|(id, _), _| index.contains_key(id));
        let playing_live: HashMap<(usize, TrackParam), f32> = self.live.iter().map(|((id, param), v)| ((index[id], *param), *v)).collect();
        let mut changed: Vec<(usize, TrackParam, Option<f32>)> = playing_live
            .iter()
            .filter(|(k, v)| self.sent.get(*k) != Some(*v))
            .map(|(&(i, param), &v)| (i, param, Some(v)))
            .chain(self.sent.keys().filter(|k| !playing_live.contains_key(*k)).map(|&(i, param)| (i, param, None)))
            .collect();
        changed.sort_by_key(|&(i, param, _)| (i, param));
        self.sent = playing_live;
        changed
    }

    /// Drop the pass and every control moved, another project opening: the
    /// controls the engine is to play from their keys again.
    pub fn drop_pass(&mut self) -> Vec<(usize, TrackParam, Option<f32>)> {
        self.series.clear();
        self.live.clear();
        self.sequence = None;
        let mut changed: Vec<_> = self.sent.drain().map(|((i, param), _)| (i, param, None)).collect();
        changed.sort_by_key(|&(i, param, _)| (i, param));
        changed
    }

    /// End the pass: each track of `seq` it wrote, as it is with the runs
    /// thinned and put over its keys in their spans, by index. The pass is
    /// empty after; one recorded in another sequence writes nothing.
    pub fn finish(&mut self, seq: &Sequence) -> Vec<(usize, Track)> {
        let series = std::mem::take(&mut self.series);
        if self.sequence != Some(seq.id) {
            return Vec::new();
        }
        let mut out: HashMap<usize, Track> = HashMap::new();
        for ((id, param), s) in series {
            let Some((i, track)) = seq.tracks.iter().enumerate().find(|(_, t)| t.id == id) else { continue };
            // A run of one sample has no span: reverse play leaves only these.
            for run in s.runs.iter().filter(|r| r.len() >= 2) {
                write_run(out.entry(i).or_insert_with(|| track.clone()), param, run);
            }
        }
        let mut out: Vec<(usize, Track)> = out.into_iter().collect();
        out.sort_by_key(|(i, _)| *i);
        out
    }
}

/// The control's value as set on `track`.
fn as_set(track: &Track, param: TrackParam) -> f32 {
    match param {
        TrackParam::Volume => track.volume,
        TrackParam::Pan => track.pan,
    }
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// `run` thinned and put in `track`'s keys of `param` in place of those
/// in its span. The old curve is kept outside it: a key holding its value
/// [`GLIDE_SECS`] before the run (unless the run starts at the sequence's
/// start) and one after.
fn write_run(track: &mut Track, param: TrackParam, run: &[(f64, f32)]) {
    let (from, to) = (run[0].0, run[run.len() - 1].0);
    let old = TrackAutomation { mode: AutomationMode::Read, lanes: track.automation.lanes.clone() };
    let set = as_set(track, param);
    let lead = (from > GLIDE_SECS).then(|| key(from - GLIDE_SECS, old.value_at(param, from - GLIDE_SECS, set)));
    let tail = key(to + GLIDE_SECS, old.value_at(param, to + GLIDE_SECS, set));
    let start = lead.as_ref().map_or(from, |k| k.time_secs);
    let keys = track.automation.lanes.entry(param).or_default();
    keys.retain(|k| k.time_secs < start - 1e-9 || k.time_secs > tail.time_secs + 1e-9);
    keys.extend(lead);
    keys.extend(thin(run, param).into_iter().map(|(t, v)| key(t, v)));
    keys.push(tail);
    keys.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
}

/// The samples a move needs to keep its shape (Ramer–Douglas–Peucker, by
/// the value's error between two kept samples): its ends, and each sample
/// that strays more than the tolerance from the line through its
/// neighbours kept; volume in dB.
pub fn thin(run: &[(f64, f32)], param: TrackParam) -> Vec<(f64, f32)> {
    if run.len() <= 2 {
        return run.to_vec();
    }
    let (level, tolerance): (fn(f32) -> f32, f32) = match param {
        TrackParam::Volume => (klipp_timeline::track_automation::to_db, VOLUME_TOLERANCE_DB),
        TrackParam::Pan => (|p: f32| p, PAN_TOLERANCE),
    };
    let mut keep = vec![false; run.len()];
    keep[0] = true;
    keep[run.len() - 1] = true;
    let mut spans = vec![(0, run.len() - 1)];
    while let Some((a, b)) = spans.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ta, ya, tb, yb) = (run[a].0, level(run[a].1), run[b].0, level(run[b].1));
        let (worst, err) = (a + 1..b)
            .map(|i| {
                let f = if tb > ta { ((run[i].0 - ta) / (tb - ta)) as f32 } else { 0.0 };
                (i, (level(run[i].1) - (ya + (yb - ya) * f)).abs())
            })
            .fold((a, 0.0_f32), |m, x| if x.1 > m.1 { x } else { m });
        if err > tolerance {
            keep[worst] = true;
            spans.push((a, worst));
            spans.push((worst, b));
        }
    }
    run.iter().zip(keep).filter(|(_, k)| *k).map(|(s, _)| *s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(mode: AutomationMode) -> Sequence {
        let mut s = Sequence::new_default();
        let mut t = Track::new("A", TrackKind::Audio);
        t.automation.mode = mode;
        s.tracks = vec![t];
        s
    }

    fn times(keys: &[Keyframe]) -> Vec<f64> {
        keys.iter().map(|k| (k.time_secs * 1000.0).round() / 1000.0).collect()
    }

    /// A steady fade thins to its ends; a move with a turn keeps the turn.
    #[test]
    fn a_move_thins_to_its_shape() {
        let fade: Vec<(f64, f32)> = (0..=60).map(|i| (i as f64 / 30.0, i as f32 / 60.0 * 0.8 - 0.4)).collect();
        assert_eq!(thin(&fade, TrackParam::Pan).len(), 2);
        let turn: Vec<(f64, f32)> = (0..=60).map(|i| (i as f64 / 30.0, if i <= 30 { i as f32 / 30.0 } else { (60 - i) as f32 / 30.0 })).collect();
        let kept = thin(&turn, TrackParam::Pan);
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert!((kept[1].0 - 1.0).abs() < 1e-9);
    }

    /// Touch writes while held, at the value the fader is moved to, which
    /// the engine plays meanwhile; the keys outside its span are kept, and
    /// the control's value as set is untouched. Read writes nothing.
    #[test]
    fn touch_writes_while_held() {
        let mut s = seq(AutomationMode::Touch);
        s.tracks[0].automation.lanes.insert(TrackParam::Volume, vec![key(0.5, 1.0), key(5.0, 1.0)]);
        let id = s.tracks[0].id;
        let mut pass = Pass::default();
        let held: HashSet<_> = [(0, TrackParam::Volume)].into_iter().collect();
        let none = HashSet::new();
        let mut changes = Vec::new();
        for f in 0..90 {
            if f == 30 {
                pass.set_live(id, TrackParam::Volume, 0.5);
            }
            changes.extend(pass.tick(&s, if (30..60).contains(&f) { &held } else { &none }, true, f as f64 / 30.0));
        }
        assert_eq!(changes, vec![(0, TrackParam::Volume, Some(0.5)), (0, TrackParam::Volume, None)], "played as moved, then from the keys");
        let done = pass.finish(&s);
        let track = &done[0].1;
        assert_eq!(times(track.automation.keys(TrackParam::Volume)), vec![0.5, 0.95, 1.0, 1.967, 2.017, 5.0], "the touch's span written, the keys outside kept");
        assert_eq!(track.volume, s.tracks[0].volume, "the value as set untouched");
        assert!(!pass.has_samples(), "the pass ended");

        let read = seq(AutomationMode::Read);
        let mut pass = Pass::default();
        pass.tick(&read, &held, true, 1.0);
        pass.tick(&read, &held, true, 1.1);
        assert!(!pass.has_samples());
    }

    /// The old curve is unchanged outside a run: a pan sweep touched in its
    /// middle still sweeps before and after the touch.
    #[test]
    fn the_curve_outside_a_run_is_kept() {
        let mut s = seq(AutomationMode::Touch);
        s.tracks[0].automation.lanes.insert(TrackParam::Pan, vec![key(0.0, -1.0), key(10.0, 1.0)]);
        let before = s.tracks[0].automation.clone();
        let id = s.tracks[0].id;
        let held: HashSet<_> = [(0, TrackParam::Pan)].into_iter().collect();
        let mut pass = Pass::default();
        pass.set_live(id, TrackParam::Pan, 0.5);
        for f in 120..=180 {
            pass.tick(&s, &held, true, f as f64 / 30.0);
        }
        pass.tick(&s, &HashSet::new(), false, 6.0);
        let after = &pass.finish(&s)[0].1.automation;
        for t in [0.0, 2.0, 3.9, 6.1, 8.0, 10.0] {
            let (a, b) = (after.value_at(TrackParam::Pan, t, 0.0), before.value_at(TrackParam::Pan, t, 0.0));
            assert!((a - b).abs() < 1e-5, "at {t}: {a} vs {b}");
        }
        assert_eq!(after.value_at(TrackParam::Pan, 5.0, 0.0), 0.5, "the touch in its span");
    }

    /// A loop writes each time round as its own run, the later over the
    /// earlier; reverse play, its frames going back, writes nothing.
    #[test]
    fn a_loop_or_reverse_play_writes_forward_runs() {
        let mut s = seq(AutomationMode::Write);
        let id = s.tracks[0].id;
        let none = HashSet::new();
        let mut pass = Pass::default();
        pass.set_live(id, TrackParam::Pan, -0.5);
        for f in 60..=150 {
            pass.tick(&s, &none, true, f as f64 / 30.0);
        }
        pass.set_live(id, TrackParam::Pan, 0.5);
        for f in 60..=120 {
            pass.tick(&s, &none, true, f as f64 / 30.0);
        }
        let after = pass.finish(&s).remove(0).1.automation;
        assert_eq!(after.value_at(TrackParam::Pan, 3.0, 0.0), 0.5, "the second time round");
        assert_eq!(after.value_at(TrackParam::Pan, 4.5, 0.0), -0.5, "the first, past where the second stopped");
        assert!(after.keys(TrackParam::Pan).windows(2).all(|w| w[0].time_secs < w[1].time_secs));

        s.tracks[0].automation = TrackAutomation { mode: AutomationMode::Write, ..Default::default() };
        let mut pass = Pass::default();
        for f in (60..=150).rev() {
            pass.tick(&s, &none, true, f as f64 / 30.0);
        }
        assert!(pass.finish(&s).is_empty(), "reverse play writes no keys");
    }

    /// Write plays what it writes: untouched, the value the control was
    /// playing from its keys, not its value as set; moved, the new value.
    #[test]
    fn write_plays_what_it_writes() {
        let mut s = seq(AutomationMode::Write);
        s.tracks[0].volume = 1.0;
        s.tracks[0].automation.lanes.insert(TrackParam::Volume, vec![key(0.0, 0.5)]);
        let id = s.tracks[0].id;
        let none = HashSet::new();
        let mut pass = Pass::default();
        let first = pass.tick(&s, &none, true, 1.0);
        assert!(first.iter().any(|&(i, p, v)| (i, p) == (0, TrackParam::Volume) && v.is_some_and(|v| (v - 0.5).abs() < 1e-5)), "{first:?}");
        pass.tick(&s, &none, true, 1.1);
        pass.set_live(id, TrackParam::Volume, 0.25);
        assert!(pass.tick(&s, &none, true, 1.2).contains(&(0, TrackParam::Volume, Some(0.25))));
        pass.tick(&s, &none, true, 1.3);
        let stop = pass.tick(&s, &none, false, 1.3);
        assert!(stop.contains(&(0, TrackParam::Volume, None)), "stopped, back to the keys: {stop:?}");
        let after = pass.finish(&s).remove(0).1.automation;
        let written: Vec<f32> = after.keys(TrackParam::Volume).iter().filter(|k| (1.0..=1.3).contains(&k.time_secs)).map(|k| (k.value * 1e4).round() / 1e4).collect();
        assert_eq!(written, vec![0.5, 0.5, 0.25, 0.25], "what was heard, not the value as set");
    }

    /// Latch writes from its first touch until the pass ends, the last
    /// value held after it is let go; Write writes the whole pass.
    #[test]
    fn latch_holds_and_write_writes_all() {
        let held: HashSet<_> = [(0, TrackParam::Pan)].into_iter().collect();
        let none = HashSet::new();
        for (mode, first) in [(AutomationMode::Latch, 1.0), (AutomationMode::Write, 0.0)] {
            let s = seq(mode);
            let id = s.tracks[0].id;
            let mut pass = Pass::default();
            for f in 0..60 {
                if (30..40).contains(&f) {
                    pass.set_live(id, TrackParam::Pan, -1.0 + (f - 30) as f32 / 10.0);
                }
                pass.tick(&s, if (30..40).contains(&f) { &held } else { &none }, true, f as f64 / 30.0);
            }
            let done = pass.finish(&s);
            let keys = done[0].1.automation.keys(TrackParam::Pan);
            let run: Vec<&Keyframe> = keys.iter().filter(|k| k.time_secs >= first - 1e-9 && k.time_secs <= 59.0 / 30.0 + 1e-9).collect();
            assert!((run[0].time_secs - first).abs() < 1e-9, "{mode:?}: from {}", run[0].time_secs);
            assert!((run.last().unwrap().time_secs - 59.0 / 30.0).abs() < 1e-9, "{mode:?}: to the end of the pass");
            assert!((run.last().unwrap().value - -0.1).abs() < 1e-6, "{mode:?}: the last value held");
        }
    }

    /// A pass belongs to its sequence: another one made active, it is
    /// dropped, and writes nothing into either.
    #[test]
    fn a_pass_is_dropped_with_its_sequence() {
        let s = seq(AutomationMode::Write);
        let other = seq(AutomationMode::Write);
        let mut pass = Pass::default();
        for f in 0..10 {
            pass.tick(&s, &HashSet::new(), true, f as f64 / 30.0);
        }
        assert!(pass.finish(&other).is_empty(), "not into another sequence");
        for f in 0..10 {
            pass.tick(&s, &HashSet::new(), true, f as f64 / 30.0);
        }
        pass.tick(&other, &HashSet::new(), false, 0.0);
        assert!(pass.finish(&s).is_empty() && !pass.has_samples(), "switched away from, dropped");
    }
}
