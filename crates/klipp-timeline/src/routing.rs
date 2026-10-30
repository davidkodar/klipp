//! Aux buses and sends (#2861): a track sends some of its sound to a bus,
//! taken before its fader or after it, and the bus mixes what it is sent
//! through its own effects back to master, or on to another bus. One
//! reverb on a bus serves every track that sends to it.
//!
//! The routing is the sequence's: buses on the [`Sequence`], each
//! track's sends on its [`Track`]. Playback and export read it the same
//! way, a send's level through [`AuxSend::gain_at`].

use klipp_core::{AudioEffectInstance, BusId, Keyframe};
use serde::{Deserialize, Serialize};

use crate::track_automation::{gain_at, AutomationMode};
use crate::{Sequence, Track};

/// At most this many buses in a sequence.
pub const MAX_BUSES: usize = 32;
/// At most this many sends on a track.
pub const MAX_SENDS: usize = 16;
/// At most this many effects on a bus.
pub const MAX_BUS_EFFECTS: usize = 16;
/// The loudest a send or a bus may be set, as gain: +12 dB.
pub const MAX_LEVEL: f32 = 3.981_072;
/// A bus name is cut to this many characters.
pub const MAX_BUS_NAME: usize = 64;

fn unity() -> f32 {
    1.0
}

fn is_unity(v: &f32) -> bool {
    *v == 1.0
}

/// An aux bus: the sum of the sends to it, through its effects, at its
/// level and pan, into master or another bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuxBus {
    pub id: BusId,
    pub name: String,
    /// Its fader, as gain.
    #[serde(default = "unity", skip_serializing_if = "is_unity")]
    pub volume: f32,
    /// -1 (left) to 1 (right).
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub muted: bool,
    /// Its effects, run on its sum before its fader.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<AudioEffectInstance>,
    /// The bus it feeds; `None` is master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<BusId>,
}

impl AuxBus {
    pub fn new(name: &str) -> Self {
        Self { id: BusId::new(), name: name.to_string(), volume: 1.0, pan: 0.0, muted: false, effects: Vec::new(), output: None }
    }
}

/// A track's send to a bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuxSend {
    pub bus: BusId,
    /// Its level, as gain.
    #[serde(default = "unity", skip_serializing_if = "is_unity")]
    pub level: f32,
    /// Taken before the track's fader and pan, so it ignores them; after
    /// them otherwise, and follows them.
    #[serde(default)]
    pub pre_fader: bool,
    #[serde(default)]
    pub muted: bool,
    /// Its level's keys, in gain at sequence seconds, played as the
    /// track's volume keys are. Kept on the send, so they go with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Keyframe>,
}

impl AuxSend {
    /// A new send: after the fader, at 0 dB.
    pub fn new(bus: BusId) -> Self {
        Self { bus, level: 1.0, pre_fader: false, muted: false, keys: Vec::new() }
    }

    /// Its level at `secs`: from its keys when it has some and the track's
    /// automation isn't Off, else as set.
    pub fn gain_at(&self, secs: f64, mode: AutomationMode) -> f32 {
        if mode == AutomationMode::Off || self.keys.is_empty() {
            self.level
        } else {
            gain_at(&self.keys, secs)
        }
    }
}

/// A gain from a file: a number in `0..=MAX_LEVEL`, unity when it is not
/// one at all.
fn clamp_level(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, MAX_LEVEL) } else { 1.0 }
}

fn clamp_pan(v: f32) -> f32 {
    if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 }
}

impl Sequence {
    /// The index of bus `id`.
    pub fn bus_index(&self, id: BusId) -> Option<usize> {
        self.buses.iter().position(|b| b.id == id)
    }

    /// Whether routing bus `from` into bus `to` would make a loop: `to`
    /// already feeds `from`, or is it.
    pub fn bus_route_loops(&self, from: BusId, to: BusId) -> bool {
        let mut at = Some(to);
        for _ in 0..=self.buses.len() {
            match at {
                None => return false,
                Some(b) if b == from => return true,
                Some(b) => at = self.buses.iter().find(|x| x.id == b).and_then(|x| x.output),
            }
        }
        true
    }

    /// The buses in the order they are mixed: each before the bus it
    /// feeds. Their outputs must not loop ([`Self::mend_routing`]).
    pub fn bus_order(&self) -> Vec<usize> {
        let n = self.buses.len();
        let mut order = Vec::with_capacity(n);
        let mut placed = vec![false; n];
        // A bus goes once every bus feeding it has gone.
        while order.len() < n {
            let before = order.len();
            for i in 0..n {
                let fed_by_unplaced = (0..n).any(|j| !placed[j] && j != i && self.buses[j].output == Some(self.buses[i].id));
                if !placed[i] && !fed_by_unplaced {
                    placed[i] = true;
                    order.push(i);
                }
            }
            if order.len() == before {
                // A loop: the rest in the order they are listed.
                order.extend((0..n).filter(|&i| !placed[i]));
                break;
            }
        }
        order
    }

    /// The routing made sound, as a file is read: past [`MAX_BUSES`] buses
    /// and [`MAX_SENDS`] sends a track, the rest are dropped; a second bus
    /// with an id already used is dropped; levels, pans and keys are held
    /// in range; a send to no bus is dropped; a bus fed to no bus, or into
    /// a loop, feeds master. What was changed is said, one line each.
    pub fn mend_routing(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.buses.len() > MAX_BUSES {
            notes.push(format!("{} buses: kept the first {MAX_BUSES}", self.buses.len()));
            self.buses.truncate(MAX_BUSES);
        }
        let mut seen = std::collections::HashSet::new();
        let before = self.buses.len();
        self.buses.retain(|b| seen.insert(b.id));
        if self.buses.len() < before {
            notes.push(format!("{} bus(es) sharing an id dropped", before - self.buses.len()));
        }
        for bus in &mut self.buses {
            let (volume, pan) = (clamp_level(bus.volume), clamp_pan(bus.pan));
            if volume.to_bits() != bus.volume.to_bits() || pan.to_bits() != bus.pan.to_bits() {
                notes.push(format!("bus \"{}\": level or pan out of range, held in range", bus.name));
            }
            (bus.volume, bus.pan) = (volume, pan);
            if bus.name.chars().count() > MAX_BUS_NAME {
                bus.name = bus.name.chars().take(MAX_BUS_NAME).collect();
                notes.push(format!("bus \"{}\": name shortened", bus.name));
            }
            if bus.effects.len() > MAX_BUS_EFFECTS {
                notes.push(format!("bus \"{}\": {} effects, kept the first {MAX_BUS_EFFECTS}", bus.name, bus.effects.len()));
                bus.effects.truncate(MAX_BUS_EFFECTS);
            }
        }
        let ids: std::collections::HashSet<BusId> = self.buses.iter().map(|b| b.id).collect();
        for i in 0..self.buses.len() {
            let Some(to) = self.buses[i].output else { continue };
            let from = self.buses[i].id;
            if !ids.contains(&to) || self.bus_route_loops(from, to) {
                notes.push(format!("bus \"{}\": its output was no bus or made a loop, now master", self.buses[i].name));
                self.buses[i].output = None;
            }
        }
        for track in &mut self.tracks {
            notes.extend(mend_sends(track, &ids));
        }
        notes
    }
}

fn mend_sends(track: &mut Track, ids: &std::collections::HashSet<BusId>) -> Vec<String> {
    let mut notes = Vec::new();
    if track.sends.len() > MAX_SENDS {
        notes.push(format!("track \"{}\": {} sends, kept the first {MAX_SENDS}", track.name, track.sends.len()));
        track.sends.truncate(MAX_SENDS);
    }
    let before = track.sends.len();
    track.sends.retain(|s| ids.contains(&s.bus));
    if track.sends.len() < before {
        notes.push(format!("track \"{}\": {} send(s) to no bus dropped", track.name, before - track.sends.len()));
    }
    for send in &mut track.sends {
        let level = clamp_level(send.level);
        let keys_before = send.keys.len();
        send.keys.retain(|k| k.time_secs.is_finite() && k.value.is_finite());
        let mut changed = level.to_bits() != send.level.to_bits() || send.keys.len() < keys_before;
        send.level = level;
        for k in &mut send.keys {
            let v = clamp_level(k.value);
            changed |= v.to_bits() != k.value.to_bits();
            k.value = v;
        }
        if changed {
            notes.push(format!("track \"{}\": a send's level out of range, held in range", track.name));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq_with(buses: Vec<AuxBus>) -> Sequence {
        let mut seq = Sequence::new_default();
        seq.buses = buses;
        seq
    }

    /// A bus is mixed before the bus it feeds, whatever order they are
    /// listed in.
    #[test]
    fn a_bus_is_mixed_before_the_bus_it_feeds() {
        let (mut a, mut b, c) = (AuxBus::new("a"), AuxBus::new("b"), AuxBus::new("c"));
        a.output = Some(c.id);
        b.output = Some(a.id);
        let seq = seq_with(vec![a, b, c]);
        assert_eq!(seq.bus_order(), vec![1, 0, 2]);
        assert!(seq.bus_route_loops(seq.buses[2].id, seq.buses[1].id), "c into b loops: b feeds a feeds c");
        assert!(!seq.bus_route_loops(seq.buses[1].id, seq.buses[2].id));
        assert!(seq.bus_route_loops(seq.buses[0].id, seq.buses[0].id), "a bus into itself");
    }

    /// A file's routing is held to what can be mixed, and every change
    /// is said: a loop, a send to no bus, levels out of range, too many.
    #[test]
    fn a_hostile_files_routing_is_mended_and_said() {
        let (mut a, mut b) = (AuxBus::new("a"), AuxBus::new("b"));
        a.output = Some(b.id);
        b.output = Some(a.id);
        b.volume = f32::NAN;
        a.pan = 9.0;
        let (a_id, b_id) = (a.id, b.id);
        let mut buses = vec![a, b.clone(), b];
        buses.extend((0..40).map(|i| AuxBus::new(&format!("x{i}"))));
        let mut seq = seq_with(buses);
        let t = &mut seq.tracks[0];
        t.sends = (0..20).map(|_| AuxSend::new(a_id)).collect();
        t.sends[0].level = 1e9;
        t.sends[1].bus = BusId::new();
        t.sends[2].keys = vec![Keyframe { time_secs: f64::NAN, value: 1.0, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
        let notes = seq.mend_routing();
        assert_eq!(seq.buses.len(), MAX_BUSES - 1, "cut to 32, then the duplicate dropped");
        assert!(seq.buses.iter().all(|x| x.output.is_none_or(|o| !seq.bus_route_loops(x.id, o))), "no loop left");
        assert!(seq.buses[0].output.is_none() || seq.buses[1].output.is_none());
        let (a, b) = (&seq.buses[seq.bus_index(a_id).unwrap()], &seq.buses[seq.bus_index(b_id).unwrap()]);
        assert_eq!((a.pan, b.volume), (1.0, 1.0));
        let t = &seq.tracks[0];
        assert_eq!(t.sends.len(), MAX_SENDS - 1, "cut to 16, then the one to no bus dropped");
        assert_eq!(t.sends[0].level, MAX_LEVEL);
        assert!(t.sends.iter().all(|s| s.keys.is_empty()));
        for said in ["buses: kept the first 32", "sharing an id", "made a loop", "sends, kept the first 16", "to no bus dropped", "held in range"] {
            assert!(notes.iter().any(|n| n.contains(said)), "{said:?} not in {notes:?}");
        }
        assert!(seq.mend_routing().is_empty(), "mended once is sound");
    }

    /// A send's level from its keys is in dB as a track's volume is, and
    /// as set with none or with automation Off.
    #[test]
    fn a_sends_keys_play_as_a_volume_does() {
        let key = |t: f64, v: f32| Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut send = AuxSend::new(BusId::new());
        send.level = 0.5;
        assert_eq!(send.gain_at(1.0, AutomationMode::Read), 0.5);
        send.keys = vec![key(0.0, 1.0), key(2.0, 10f32.powf(-12.0 / 20.0))];
        let mid = send.gain_at(1.0, AutomationMode::Read);
        assert!((20.0 * mid.log10() + 6.0).abs() < 1e-3, "halfway is -6 dB: {mid}");
        assert_eq!(send.gain_at(1.0, AutomationMode::Off), 0.5);
    }

    /// A project from before buses reads with none and no sends, and
    /// writes nothing new.
    #[test]
    fn a_sequence_without_routing_saves_none() {
        let seq = Sequence::new_default();
        assert!(seq.buses.is_empty() && seq.tracks.iter().all(|t| t.sends.is_empty()));
        let json = serde_json::to_string(&seq.tracks[0]).unwrap();
        assert!(!json.contains("sends"), "{json}");
    }
}
