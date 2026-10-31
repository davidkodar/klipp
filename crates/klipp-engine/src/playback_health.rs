//! Whether playback keeps up, and if it doesn't, why and what would help
//! (#3234): a track to freeze, proxies to make or play from, or a smaller
//! playback size.
//!
//! The engine records, a second at a time while it plays at 1×, how many
//! frames were due and how many were shown ([`Recorder`]); trouble is a
//! share dropped over the last seconds that is both large and steady, so
//! one hiccup is never it. [`diagnose`] names the cause from the frame's
//! measured phases and each track's cost, and the fix that fits. Nothing
//! here acts: the person chooses.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use klipp_core::{MediaId, TrackId};

/// The seconds trouble is judged over.
pub const WINDOW: usize = 10;
/// The share of due frames dropped over [`WINDOW`] that is trouble.
pub const DROP_SHARE: f32 = 0.05;
/// Of [`WINDOW`]'s seconds, how many must each have dropped a frame: a
/// steady shortfall, not one bad second.
pub const STEADY_SECONDS: usize = 8;
/// A track whose cost is over this share of the frame's time, and over
/// half of all tracks' together, is the one to freeze.
pub const HEAVY_TRACK_SHARE: f32 = 0.5;
/// Decoding over this share of the frame's time, and over the drawing,
/// is decode-bound.
pub const DECODE_SHARE: f32 = 0.6;

/// One second of playback: frames due, and shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Second {
    pub due: f32,
    pub shown: f32,
}

/// The seconds of playback at 1× since it last started, the first left
/// out (warm-up), at most 30 kept.
#[derive(Debug, Default)]
pub struct Recorder {
    seconds: VecDeque<Second>,
    started: Option<Instant>,
    last: Option<Instant>,
}

impl Recorder {
    /// Not playing at 1×, or a seek: what was recorded no longer says
    /// how this playback goes.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A tick while playing at 1×: a second's record taken each second,
    /// `shown` the rate presented over the last second, `due` the rate
    /// the sequence plays at.
    pub fn tick(&mut self, now: Instant, shown: Option<f64>, due: f64) {
        let started = *self.started.get_or_insert(now);
        if now.duration_since(started) < Duration::from_secs(1) {
            self.last = Some(now);
            return;
        }
        if self.last.is_some_and(|l| now.duration_since(l) < Duration::from_secs(1)) {
            return;
        }
        self.last = Some(now);
        let shown = shown.unwrap_or(0.0).min(due) as f32;
        self.seconds.push_back(Second { due: due as f32, shown });
        while self.seconds.len() > 30 {
            self.seconds.pop_front();
        }
    }

    pub fn health(&self) -> Health {
        if FORCED.load(std::sync::atomic::Ordering::Relaxed) {
            return Health { seconds: WINDOW, dropping: 0.5, sustained: true };
        }
        health_of(self.seconds.iter().rev().take(WINDOW).copied())
    }
}

static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Trouble reported whatever playback does: for a test of what follows
/// it without playing ten seconds short.
#[doc(hidden)]
pub fn force_trouble(on: bool) {
    FORCED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// How playback has gone over the last [`WINDOW`] seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct Health {
    /// Seconds recorded, up to [`WINDOW`].
    pub seconds: usize,
    /// The share of due frames not shown.
    pub dropping: f32,
    /// Trouble: [`WINDOW`] seconds recorded, over [`DROP_SHARE`] dropped,
    /// and a frame dropped in [`STEADY_SECONDS`] of them.
    pub sustained: bool,
}

fn health_of(seconds: impl Iterator<Item = Second>) -> Health {
    let seconds: Vec<Second> = seconds.collect();
    let due: f32 = seconds.iter().map(|s| s.due).sum();
    let shown: f32 = seconds.iter().map(|s| s.shown).sum();
    let dropping = if due > 0.0 { ((due - shown) / due).max(0.0) } else { 0.0 };
    let steady = seconds.iter().filter(|s| s.due - s.shown >= 1.0).count();
    Health { seconds: seconds.len(), dropping, sustained: seconds.len() >= WINDOW && dropping > DROP_SHARE && steady >= STEADY_SECONDS }
}

/// A track's load, as [`diagnose`] weighs it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackLoad {
    pub id: TrackId,
    pub name: String,
    /// Its GPU time a frame, ms, from timestamps.
    pub gpu_ms: Option<f32>,
    /// Its frame drawn alone once, ms, on a device without timestamps.
    pub measured_once_ms: Option<f32>,
}

/// A file playing now, and whether it has a proxy.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaLoad {
    pub id: MediaId,
    pub name: String,
    pub has_proxy: bool,
}

/// What [`diagnose`] weighs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inputs {
    pub health: Health,
    /// The frame's time at the sequence's rate, ms.
    pub budget_ms: f32,
    /// Over the same seconds, mean: the primary decoder's time, the wait
    /// for streams, and the drawing.
    pub decode_ms: f32,
    pub sync_ms: f32,
    pub render_ms: f32,
    pub tracks: Vec<TrackLoad>,
    pub media: Vec<MediaLoad>,
    pub proxies_on: bool,
    /// Playback quality's divisor: 1 full, 2 half, 4 quarter.
    pub quality: u8,
}

/// The fix to offer.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "fix", rename_all = "snake_case")]
pub enum Fix {
    FreezeTrack { track: TrackId, name: String },
    MakeProxies { media: Vec<MediaId> },
    UseProxies,
    PlaybackQuality { divisor: u8 },
}

impl Fix {
    /// The kind of fix, a word: what a note shown once per project is
    /// remembered by.
    pub fn kind(&self) -> &'static str {
        match self {
            Fix::FreezeTrack { .. } => "freeze_track",
            Fix::MakeProxies { .. } => "make_proxies",
            Fix::UseProxies => "use_proxies",
            Fix::PlaybackQuality { .. } => "playback_quality",
        }
    }
}

/// How much worse than the note last shown a new one must be to show
/// again for the same cause: twice the share dropped, and over this.
pub const CLEARLY_WORSE: f32 = 0.2;

/// Whether `d` is shown as a note, given the one last shown for this
/// project (its fix's kind and share dropped): one with a fix, for a
/// project that had none, for another fix, or clearly worse.
pub fn should_show(last: Option<(&str, f32)>, d: &Diagnosis) -> bool {
    let Some(fix) = &d.fix else { return false };
    match last {
        None => true,
        Some((kind, _)) if kind != fix.kind() => true,
        Some((_, dropping)) => d.health.dropping >= 2.0 * dropping && d.health.dropping > CLEARLY_WORSE,
    }
}

/// Why playback drops frames, and what would help.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Diagnosis {
    pub health: Health,
    /// The cause in words, with what was measured.
    pub cause: String,
    pub fix: Option<Fix>,
}

/// The cause of trouble, and the fix that fits: the first that holds of
/// a heavy track, decoding, then the frame as a whole. `None` without
/// trouble.
pub fn diagnose(i: &Inputs) -> Option<Diagnosis> {
    if !i.health.sustained {
        return None;
    }
    let budget = i.budget_ms.max(1e-3);
    // 1. One track taking most of the frame: by its timestamps, or where
    // there are none, its frame drawn alone once. Never guessed.
    let cost = |t: &TrackLoad| t.gpu_ms.map(|ms| (ms, false)).or(t.measured_once_ms.map(|ms| (ms, true)));
    let total: f32 = i.tracks.iter().filter_map(|t| cost(t).map(|(ms, _)| ms)).sum();
    if let Some((t, (ms, once))) = i.tracks.iter().filter_map(|t| cost(t).map(|c| (t, c))).max_by(|a, b| a.1 .0.total_cmp(&b.1 .0)) {
        if ms > budget * HEAVY_TRACK_SHARE && ms > total * 0.5 {
            let how = if once { "measured once, drawn alone" } else { "on the GPU" };
            return Some(Diagnosis {
                health: i.health,
                cause: format!("{} takes {ms:.0} ms a frame ({how}), of the {budget:.0} ms a frame has", t.name),
                fix: Some(Fix::FreezeTrack { track: t.id, name: t.name.clone() }),
            });
        }
    }
    // 2. Decoding can't keep up.
    let decode = i.decode_ms + i.sync_ms;
    if decode > budget * DECODE_SHARE && decode > i.render_ms {
        let missing: Vec<&MediaLoad> = i.media.iter().filter(|m| !m.has_proxy).collect();
        let named = |ms: &[&MediaLoad]| {
            let mut said: Vec<String> = ms.iter().take(3).map(|m| m.name.clone()).collect();
            if ms.len() > 3 {
                said.push(format!("{} more", ms.len() - 3));
            }
            said.join(", ")
        };
        if !missing.is_empty() {
            return Some(Diagnosis {
                health: i.health,
                cause: format!("decoding {} takes {decode:.0} ms a frame, of the {budget:.0} ms a frame has", named(&missing)),
                fix: Some(Fix::MakeProxies { media: missing.iter().map(|m| m.id).collect() }),
            });
        }
        if !i.proxies_on && !i.media.is_empty() {
            return Some(Diagnosis { health: i.health, cause: format!("decoding takes {decode:.0} ms a frame, of {budget:.0} ms, and the proxies made aren't played"), fix: Some(Fix::UseProxies) });
        }
    }
    // 3. No one cause: a smaller frame, while there is one smaller.
    let frame = decode.max(i.render_ms);
    let fix = match i.quality {
        0 | 1 => Some(Fix::PlaybackQuality { divisor: 2 }),
        2 | 3 => Some(Fix::PlaybackQuality { divisor: 4 }),
        _ => None,
    };
    Some(Diagnosis { health: i.health, cause: format!("frames take {frame:.0} ms, of the {budget:.0} ms a frame has, with no one cause"), fix })
}

/// What the engine last recorded, for the UI and the control socket.
#[derive(Clone, Debug, Default)]
pub struct Shared {
    pub health: Health,
    /// Each track's frame drawn alone once, ms (see [`TrackLoad`]).
    pub measured_once: HashMap<TrackId, f32>,
    /// Whether proxies are played.
    pub proxies_on: bool,
    /// The sequence time the record was last taken at, s.
    pub at_secs: f64,
}

static SHARED: std::sync::LazyLock<Mutex<Shared>> = std::sync::LazyLock::new(Default::default);

pub fn set_shared(f: impl FnOnce(&mut Shared)) {
    f(&mut SHARED.lock().unwrap_or_else(|e| e.into_inner()));
}

pub fn shared() -> Shared {
    SHARED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The diagnosis now, from what the engine recorded, each track's cost,
/// the perf ring's last `samples` frames, and what plays at the recorded
/// time in `seq`: for the control socket and the note. `None` without
/// trouble.
pub fn diagnose_now(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, ring: &crate::perf::PerfRing, samples: usize) -> Option<Diagnosis> {
    let s = shared();
    if !s.health.sustained {
        return None;
    }
    let avg = ring.averages(samples);
    let at = klipp_core::RationalTime::from_secs_f64(s.at_secs, seq.fps.rate().max(1));
    let visible = || seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible);
    let tracks = visible()
        .map(|t| {
            let cost = crate::track_cost::cost(t.id);
            TrackLoad { id: t.id, name: t.name.clone(), gpu_ms: cost.and_then(|c| c.gpu_ms), measured_once_ms: s.measured_once.get(&t.id).copied() }
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let media = visible()
        .flat_map(|t| t.clips.iter().filter(|c| c.timeline_in <= at && at < c.timeline_out))
        .filter_map(|c| c.media_id())
        .filter(|id| seen.insert(*id))
        .filter_map(|id| pool.get(&id))
        .filter(|item| !item.video_codec.is_empty())
        .map(|item| MediaLoad { id: item.id, name: item.filename.clone(), has_proxy: item.proxy_ready })
        .collect();
    diagnose(&Inputs {
        health: s.health,
        budget_ms: (1000.0 / seq.fps.as_f64().max(1.0)) as f32,
        decode_ms: avg.decode_ms,
        sync_ms: avg.phases.sync_ms,
        render_ms: avg.phases.render_ms,
        tracks,
        media,
        proxies_on: s.proxies_on,
        quality: crate::video_decoder::playback_quality_divisor(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(per: &[(f32, f32)]) -> Health {
        health_of(per.iter().map(|&(due, shown)| Second { due, shown }))
    }

    /// 7% dropped, a frame or two every second: trouble. One bad second
    /// among clean ones: not, however much it dropped. Too few seconds:
    /// not yet.
    #[test]
    fn trouble_is_steady_not_one_bad_second() {
        let steady = seconds(&[(24.0, 22.3); 10]);
        assert!(steady.sustained && (steady.dropping - 0.0708).abs() < 1e-3, "{steady:?}");
        let mut spike = vec![(24.0, 24.0); 9];
        spike.push((24.0, 6.0));
        let spike = seconds(&spike);
        assert!(!spike.sustained && spike.dropping > DROP_SHARE, "one second at 25 %: not steady: {spike:?}");
        assert!(!seconds(&[(24.0, 20.0); 9]).sustained, "nine seconds aren't enough");
        assert!(!seconds(&[(24.0, 23.5); 10]).sustained, "under a frame a second");
    }

    /// The first second isn't counted, a second is taken a second at a
    /// time, and a reset forgets it all.
    #[test]
    fn the_recorder_leaves_out_the_first_second_and_forgets_on_reset() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.tick(t0, Some(10.0), 24.0);
        r.tick(t0 + Duration::from_millis(500), Some(10.0), 24.0);
        assert_eq!(r.health().seconds, 0, "warm-up");
        for k in 1..=12 {
            r.tick(t0 + Duration::from_millis(1000 * k), Some(20.0), 24.0);
            r.tick(t0 + Duration::from_millis(1000 * k + 300), Some(20.0), 24.0);
        }
        let h = r.health();
        assert_eq!(h.seconds, WINDOW);
        assert!(h.sustained, "{h:?}");
        r.reset();
        assert_eq!(r.health(), Health::default());
    }

    fn trouble() -> Inputs {
        Inputs { health: seconds(&[(24.0, 20.0); 10]), budget_ms: 41.7, decode_ms: 5.0, sync_ms: 1.0, render_ms: 30.0, quality: 1, proxies_on: false, ..Default::default() }
    }

    fn track(name: &str, gpu: Option<f32>, once: Option<f32>) -> TrackLoad {
        TrackLoad { id: TrackId::new(), name: name.into(), gpu_ms: gpu, measured_once_ms: once }
    }

    #[test]
    fn a_heavy_track_is_the_one_to_freeze() {
        let mut i = trouble();
        i.tracks = vec![track("V2", Some(95.0), None), track("V1", Some(3.0), None)];
        let d = diagnose(&i).unwrap();
        assert!(matches!(&d.fix, Some(Fix::FreezeTrack { name, .. }) if name == "V2"), "{d:?}");
        assert!(d.cause.contains("V2 takes 95 ms") && d.cause.contains("on the GPU"), "{}", d.cause);
        // Without timestamps, its frame drawn alone once, said so.
        i.tracks = vec![track("V2", None, Some(80.0)), track("V1", None, Some(4.0))];
        let d = diagnose(&i).unwrap();
        assert!(matches!(&d.fix, Some(Fix::FreezeTrack { name, .. }) if name == "V2"));
        assert!(d.cause.contains("measured once"), "{}", d.cause);
        // Neither: never a track guessed.
        i.tracks = vec![track("V2", None, None), track("V1", None, None)];
        assert!(!matches!(diagnose(&i).unwrap().fix, Some(Fix::FreezeTrack { .. })));
        // Two tracks alike: neither is the one.
        i.tracks = vec![track("V2", Some(30.0), None), track("V1", Some(30.0), None)];
        assert!(!matches!(diagnose(&i).unwrap().fix, Some(Fix::FreezeTrack { .. })));
    }

    #[test]
    fn decode_bound_media_want_proxies_made_or_played() {
        let mut i = trouble();
        i.decode_ms = 30.0;
        i.render_ms = 8.0;
        let m = |n: &str, has| MediaLoad { id: MediaId::new(), name: n.into(), has_proxy: has };
        i.media = vec![m("A001.mov", false), m("A002.mov", true), m("B001.mov", false)];
        let d = diagnose(&i).unwrap();
        assert!(matches!(&d.fix, Some(Fix::MakeProxies { media }) if media.len() == 2), "{d:?}");
        assert!(d.cause.contains("A001.mov") && d.cause.contains("B001.mov") && !d.cause.contains("A002"), "{}", d.cause);
        i.media = vec![m("A002.mov", true)];
        assert_eq!(diagnose(&i).unwrap().fix, Some(Fix::UseProxies));
        i.proxies_on = true;
        assert!(matches!(diagnose(&i).unwrap().fix, Some(Fix::PlaybackQuality { .. })), "proxies played already: the frame as a whole");
    }

    /// A note shows once per project: again for another fix, or when the
    /// share dropped at least doubled and is over 20 %; never without a
    /// fix to offer.
    #[test]
    fn a_note_shows_once_unless_another_fix_or_clearly_worse() {
        let d = |dropping: f32, fix: Option<Fix>| Diagnosis { health: Health { seconds: WINDOW, dropping, sustained: true }, cause: String::new(), fix };
        let half = Some(Fix::PlaybackQuality { divisor: 2 });
        assert!(should_show(None, &d(0.08, half.clone())), "the first");
        assert!(!should_show(Some(("playback_quality", 0.08)), &d(0.12, half.clone())), "the same, a little worse");
        assert!(!should_show(Some(("playback_quality", 0.08)), &d(0.18, half.clone())), "doubled but under 20 %");
        assert!(should_show(Some(("playback_quality", 0.12)), &d(0.30, half.clone())), "doubled and over 20 %");
        assert!(should_show(Some(("playback_quality", 0.5)), &d(0.06, Some(Fix::UseProxies))), "another fix");
        assert!(!should_show(None, &d(0.5, None)), "nothing to offer");
    }

    #[test]
    fn with_no_one_cause_a_smaller_frame_while_there_is_one() {
        let mut i = trouble();
        assert_eq!(diagnose(&i).unwrap().fix, Some(Fix::PlaybackQuality { divisor: 2 }));
        i.quality = 2;
        assert_eq!(diagnose(&i).unwrap().fix, Some(Fix::PlaybackQuality { divisor: 4 }));
        i.quality = 4;
        let d = diagnose(&i).unwrap();
        assert_eq!(d.fix, None, "quarter already: nothing to offer");
        assert!(d.cause.contains("no one cause"));
        i.health = seconds(&[(24.0, 24.0); 10]);
        assert!(diagnose(&i).is_none(), "no trouble, no diagnosis");
    }
}
