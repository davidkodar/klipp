//! Heavy frames drawn ahead while the person is idle (#3212).
//!
//! Playback records what each frame cost to draw ([`CostLog`]); a frame
//! whose last playback missed the frame's time is heavy, and so is every
//! frame of a track whose measured cost is over half that time, before it
//! has ever been played. After a few seconds with nothing done (no input,
//! playback, scrub, edit, export or freeze), a background job draws the
//! heavy frames into the frame cache ([`crate::frame_cache`]), nearest the
//! playhead first ([`plan`]); any input stops it within a frame. It runs
//! on mains power only ([`on_battery`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// The idle time before drawing starts, unless set otherwise.
pub const DEFAULT_DELAY: Duration = Duration::from_secs(2);

/// What a frame cost the last time it played.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameCost {
    /// Its render and the wait for its pictures, ms.
    pub ms: f32,
    /// Over the frame's time at the sequence's rate.
    pub late: bool,
}

/// Frames' costs kept: the ones farthest from the last recorded go first.
const KEPT_COSTS: usize = 8192;

/// What frames cost as they played, by their tick on the timeline. Each
/// playback writes over what the last one found; a frame shown from the
/// frame cache costs nothing and isn't recorded.
#[derive(Default)]
pub struct CostLog {
    costs: BTreeMap<i64, FrameCost>,
    /// Bumped on every change that alters which frames are heavy.
    generation: u64,
}

impl CostLog {
    /// Record frame `tick`'s cost against `budget_ms`, the frame's time.
    pub fn record(&mut self, tick: i64, ms: f32, budget_ms: f32) {
        let cost = FrameCost { ms, late: ms > budget_ms };
        if self.costs.insert(tick, cost).map(|c| c.late) != Some(cost.late) {
            self.generation += 1;
        }
        if self.costs.len() > KEPT_COSTS {
            let mut by_distance: Vec<i64> = self.costs.keys().copied().collect();
            by_distance.sort_by_key(|t| std::cmp::Reverse((t - tick).abs()));
            for t in by_distance.into_iter().take(KEPT_COSTS / 4) {
                self.costs.remove(&t);
            }
            self.generation += 1;
        }
    }

    /// The frames whose last playback missed their time, in order.
    pub fn late(&self) -> impl Iterator<Item = i64> + '_ {
        self.costs.iter().filter(|(_, c)| c.late).map(|(t, _)| *t)
    }

    pub fn get(&self, tick: i64) -> Option<FrameCost> {
        self.costs.get(&tick).copied()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.costs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.costs.is_empty()
    }
}

/// Whether to show a kept frame rather than draw it live: unless its
/// last live draw (`live`) was as quick as loading a kept frame (`load`,
/// once measured). On a machine where reading a frame costs more than
/// drawing it, the frame cache only slows playback down (#3212).
pub fn prefer_kept(live: Option<FrameCost>, load: Option<f32>) -> bool {
    match (live, load) {
        (Some(live), Some(load)) => live.ms > load,
        _ => true,
    }
}

/// The order heavy frames are drawn in: `heavy` (ticks, sorted, a frame
/// `step` apart where they run on) grouped into ranges, the range nearest
/// `playhead` first (the one ahead when two are as near), each from its
/// start; the range under the playhead from the playhead on, then its
/// part before. Frames `skip` says need no drawing (frozen tracks'
/// frames, current already) are left out.
pub fn plan(heavy: &[i64], step: i64, playhead: i64, mut skip: impl FnMut(i64) -> bool) -> Vec<i64> {
    let step = step.max(1);
    let mut ranges: Vec<&[i64]> = Vec::new();
    let mut start = 0;
    for i in 1..=heavy.len() {
        if i == heavy.len() || heavy[i] - heavy[i - 1] > step {
            if start < i {
                ranges.push(&heavy[start..i]);
            }
            start = i;
        }
    }
    let distance = |r: &[i64]| {
        let (a, b) = (r[0], r[r.len() - 1]);
        if playhead < a {
            a - playhead
        } else if playhead > b {
            playhead - b
        } else {
            0
        }
    };
    // Equally near, the one ahead first: playback reaches it next.
    ranges.sort_by_key(|r| (distance(r), r[0] < playhead, r[0]));
    let mut out = Vec::new();
    for r in ranges {
        let from = r.partition_point(|&t| t < playhead);
        let order = if distance(r) == 0 { [&r[from..], &r[..from]] } else { [r, &r[..0]] };
        out.extend(order.into_iter().flatten().copied().filter(|&t| !skip(t)));
    }
    out
}

/// Whether the computer runs on its battery, from the power supplies
/// under `root` (`/sys/class/power_supply`): its own battery discharging,
/// and no mains or USB supply online. A device's battery (a wireless
/// keyboard's, a mouse's, a headset's: `scope` Device) is no battery of
/// the computer's; it reads discharging always, and USB-C ports read
/// offline with nothing in them, so a desktop with one is on mains.
/// Nothing to read (a desktop, a sandbox) means not.
pub fn on_battery(root: &Path) -> bool {
    let read = |p: &Path, f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
    let (mut plugged, mut discharging) = (false, false);
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = entry.path();
        match read(&p, "type").as_str() {
            "Mains" | "USB" | "USB_PD" | "USB_C" => plugged |= read(&p, "online") == "1",
            "Battery" if read(&p, "scope") != "Device" => discharging |= read(&p, "status") == "Discharging",
            _ => {}
        }
    }
    discharging && !plugged
}

/// What the engine knows about now, for whether to draw ahead.
#[derive(Clone, Copy, Debug)]
pub struct Moment {
    /// Drawing ahead turned on in Preferences, and the frame cache.
    pub on: bool,
    pub cache_on: bool,
    pub battery: bool,
    pub playing: bool,
    pub scrubbing: bool,
    pub exporting: bool,
    pub freezing: bool,
    /// A range being drawn on request (Render In to Out).
    pub rendering_range: bool,
    /// Since the last input, edit or seek.
    pub since_activity: Duration,
    pub delay: Duration,
}

/// Whether drawing ahead may run now, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Run,
    /// Not now: the person is working, or something else has the GPU.
    Busy,
    /// Waiting out the idle time.
    Waiting,
    Off,
    OnBattery,
}

pub fn decide(m: &Moment) -> Decision {
    if !m.on || !m.cache_on {
        Decision::Off
    } else if m.battery {
        Decision::OnBattery
    } else if m.playing || m.scrubbing || m.exporting || m.freezing || m.rendering_range {
        Decision::Busy
    } else if m.since_activity < m.delay {
        Decision::Waiting
    } else {
        Decision::Run
    }
}

/// How drawing ahead stands, for the timeline and the socket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Off,
    OnBattery,
    /// Waiting for the person to be idle.
    Waiting,
    /// Drawing: frames gone through of the frames planned.
    Drawing { done: usize, total: usize },
    /// Stopped by input; starts again after the idle time.
    Stopped,
    /// Every heavy frame is drawn.
    Done,
    /// The cache is full: nothing more is drawn until room is made.
    Full,
}

static STATE: Mutex<State> = Mutex::new(State::Off);

pub fn set_state(s: State) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

pub fn state() -> State {
    *STATE.lock().unwrap_or_else(|e| e.into_inner())
}

static FORCE_HEAVY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests: every frame with a picture counts as heavy.
pub fn force_heavy(on: bool) {
    FORCE_HEAVY.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn forced_heavy() -> bool {
    FORCE_HEAVY.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_ranges_are_drawn_nearest_the_playhead_first() {
        // Three ranges a frame (10 ticks) apart: 0..30, 100..130, 200..230.
        let heavy = [0, 10, 20, 100, 110, 120, 200, 210, 220];
        assert_eq!(plan(&heavy, 10, 150, |_| false), vec![100, 110, 120, 200, 210, 220, 0, 10, 20], "the nearer range first, each from its start");
        assert_eq!(plan(&heavy, 10, 160, |_| false), vec![200, 210, 220, 100, 110, 120, 0, 10, 20], "as near: the one ahead first");
        assert_eq!(plan(&heavy, 10, 110, |_| false), vec![110, 120, 100, 200, 210, 220, 0, 10, 20], "under the playhead: from it on, then before it");
        assert_eq!(plan(&heavy, 10, 500, |_| false), vec![200, 210, 220, 100, 110, 120, 0, 10, 20]);
        assert!(plan(&[], 10, 0, |_| false).is_empty());
    }

    #[test]
    fn frames_that_need_no_drawing_are_left_out_without_splitting_a_range() {
        let heavy = [0, 10, 20, 30];
        assert_eq!(plan(&heavy, 10, 0, |t| t == 10 || t == 20), vec![0, 30]);
        // A gap of more than a frame splits ranges; the far one waits.
        assert_eq!(plan(&[0, 10, 50, 60], 10, 55, |_| false), vec![60, 50, 0, 10]);
    }

    /// A kept frame is shown unless drawing it live was as quick as
    /// loading one; with either cost unknown, it is shown.
    #[test]
    fn a_kept_frame_is_shown_only_where_loading_beats_drawing_live() {
        let live = |ms| Some(FrameCost { ms, late: ms > 41.7 });
        assert!(prefer_kept(live(60.0), Some(12.0)), "live late, loading quicker");
        assert!(!prefer_kept(live(17.0), Some(27.0)), "live quicker than loading (the M1's 4K FFV1)");
        assert!(!prefer_kept(live(12.0), Some(12.0)), "as quick: live");
        assert!(prefer_kept(None, Some(27.0)) && prefer_kept(live(17.0), None), "unknown: shown");
    }

    #[test]
    fn a_late_frame_is_heavy_and_a_frame_on_time_is_not() {
        let mut log = CostLog::default();
        log.record(0, 50.0, 41.7);
        log.record(10, 20.0, 41.7);
        assert_eq!(log.late().collect::<Vec<_>>(), vec![0]);
        let g = log.generation();
        log.record(0, 30.0, 41.7);
        assert!(log.late().next().is_none(), "the last playback wins");
        assert!(log.generation() > g, "which frames are heavy changed");
        let g = log.generation();
        log.record(10, 25.0, 41.7);
        assert_eq!(log.generation(), g, "still on time: nothing changed");
    }

    #[test]
    fn the_cost_log_keeps_the_frames_near_the_last_recorded() {
        let mut log = CostLog::default();
        for t in 0..=KEPT_COSTS as i64 {
            log.record(t, 1.0, 41.7);
        }
        assert!(log.len() <= KEPT_COSTS);
        assert!(log.get(KEPT_COSTS as i64).is_some() && log.get(0).is_none(), "the farthest from the last went");
    }

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (f, v) in files {
            std::fs::write(d.join(f), format!("{v}\n")).unwrap();
        }
    }

    /// A fresh fake `/sys/class/power_supply` named `name`.
    fn supplies(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("klipp-power-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_laptop_unplugged_is_on_battery_and_plugged_in_is_not() {
        let root = supplies("laptop");
        supply(&root, "BAT0", &[("type", "Battery"), ("scope", "System"), ("status", "Discharging")]);
        supply(&root, "AC", &[("type", "Mains"), ("online", "0")]);
        assert!(on_battery(&root), "unplugged");
        supply(&root, "AC", &[("type", "Mains"), ("online", "1")]);
        supply(&root, "BAT0", &[("type", "Battery"), ("status", "Charging")]);
        assert!(!on_battery(&root), "plugged in");
        supply(&root, "BAT0", &[("type", "Battery"), ("status", "Discharging")]);
        assert!(!on_battery(&root), "plugged in though its battery reads discharging: a supply online wins");
        std::fs::remove_dir_all(root.join("AC")).unwrap();
        supply(&root, "ucsi-source-psy-USBC000:001", &[("type", "USB"), ("online", "1")]);
        assert!(!on_battery(&root), "charging over USB-C");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A desktop with a wireless keyboard: its battery is the keyboard's,
    /// always discharging, beside USB-C ports with nothing in them and no
    /// mains supply listed (an M1 Mac mini). Not on battery.
    #[test]
    fn a_desktop_with_a_keyboards_battery_is_on_mains() {
        let root = supplies("hid");
        supply(&root, "hid-0000:004c:029c.0003-battery", &[("type", "Battery"), ("scope", "Device"), ("status", "Discharging")]);
        supply(&root, "ucsi-source-psy-USBC000:001", &[("type", "USB"), ("online", "0")]);
        supply(&root, "ucsi-source-psy-USBC000:002", &[("type", "USB"), ("online", "0")]);
        assert!(!on_battery(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_desktop_with_no_supplies_is_on_mains() {
        let root = supplies("none");
        assert!(!on_battery(&root), "an empty folder");
        assert!(!on_battery(&root.join("missing")), "no folder at all");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn it_runs_only_when_idle_past_the_delay_on_mains() {
        let idle = Moment {
            on: true,
            cache_on: true,
            battery: false,
            playing: false,
            scrubbing: false,
            exporting: false,
            freezing: false,
            rendering_range: false,
            since_activity: Duration::from_secs(3),
            delay: DEFAULT_DELAY,
        };
        assert_eq!(decide(&idle), Decision::Run);
        assert_eq!(decide(&Moment { since_activity: Duration::from_secs(1), ..idle }), Decision::Waiting);
        for (name, m) in [
            ("playing", Moment { playing: true, ..idle }),
            ("scrubbing", Moment { scrubbing: true, ..idle }),
            ("exporting", Moment { exporting: true, ..idle }),
            ("freezing", Moment { freezing: true, ..idle }),
            ("a range rendering", Moment { rendering_range: true, ..idle }),
        ] {
            assert_eq!(decide(&m), Decision::Busy, "{name}");
        }
        assert_eq!(decide(&Moment { battery: true, ..idle }), Decision::OnBattery);
        assert_eq!(decide(&Moment { on: false, ..idle }), Decision::Off);
        assert_eq!(decide(&Moment { cache_on: false, ..idle }), Decision::Off);
    }
}
