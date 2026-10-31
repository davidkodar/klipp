//! What each track costs while it plays (#3207): the decoders open for
//! it and the frames they hold, on the GPU and in memory. The engine
//! works it out as its streams change; the timeline text and the track
//! header read it, so a heavy track can be found and frozen.

use std::collections::HashMap;
use std::sync::Mutex;

use klipp_core::{ClipId, TrackId};
use klipp_timeline::Sequence;

/// One track's playback cost now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackCost {
    /// Decoded frames held on the GPU, and in memory, bytes.
    pub gpu_bytes: u64,
    pub cpu_bytes: u64,
    /// Decoders open for its clips.
    pub decoders: u32,
    /// The GPU's time over its work in a frame, ms, from the last timed
    /// frame: `None` before one, or on a device that doesn't time.
    pub gpu_ms: Option<f32>,
}

static GPU_TIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the device times the GPU's work, as the engine found.
pub fn set_gpu_timed(timed: bool) {
    GPU_TIMED.store(timed, std::sync::atomic::Ordering::Relaxed);
}

pub fn gpu_timed() -> bool {
    GPU_TIMED.load(std::sync::atomic::Ordering::Relaxed)
}

impl TrackCost {
    /// In a few words: "2 decoders, 1.2 GB on the GPU, 340 MB in memory,
    /// 4.1 ms of GPU time a frame", or "GPU time not measured" on a device
    /// that doesn't time.
    pub fn describe(&self) -> String {
        self.describe_timed(gpu_timed())
    }

    fn describe_timed(&self, timed: bool) -> String {
        let size = |b: u64| if b >= 1 << 30 { format!("{:.1} GB", b as f64 / f64::from(1u32 << 30)) } else { format!("{} MB", b >> 20) };
        let mut said = vec![format!("{} decoder{}", self.decoders, if self.decoders == 1 { "" } else { "s" })];
        if self.gpu_bytes > 0 {
            said.push(format!("{} on the GPU", size(self.gpu_bytes)));
        }
        if self.cpu_bytes > 0 {
            said.push(format!("{} in memory", size(self.cpu_bytes)));
        }
        match self.gpu_ms {
            Some(ms) => said.push(format!("{ms:.1} ms of GPU time a frame")),
            None if !timed => said.push("GPU time not measured".into()),
            None => {}
        }
        said.join(", ")
    }
}

/// Each clip's track in `seq`, with the clips a precomposition or a
/// template on it lays, at any depth, on the track it sits on.
fn tracks_of_clips(seq: &Sequence) -> HashMap<ClipId, TrackId> {
    fn walk(c: &klipp_timeline::Clip, track: TrackId, out: &mut HashMap<ClipId, TrackId>) {
        out.insert(c.id, track);
        if let Some(scene) = &c.precomp {
            for t in &scene.tracks {
                for inner in &t.clips {
                    walk(inner, track, out);
                }
            }
        }
    }
    let mut out = HashMap::new();
    for t in &seq.tracks {
        for c in &t.clips {
            walk(c, t.id, &mut out);
        }
    }
    out
}

/// Each track's cost from what each open decoder holds (its clip, bytes
/// on the GPU and in memory) and each track's GPU time, ms. A decoder for
/// a clip on no track is left out.
pub fn costs(seq: &Sequence, decoders: impl IntoIterator<Item = (ClipId, u64, u64)>, gpu_ms: &HashMap<TrackId, f64>) -> HashMap<TrackId, TrackCost> {
    let tracks = tracks_of_clips(seq);
    let mut out: HashMap<TrackId, TrackCost> = HashMap::new();
    for (clip, gpu, cpu) in decoders {
        let Some(track) = tracks.get(&clip) else { continue };
        let c = out.entry(*track).or_default();
        c.gpu_bytes += gpu;
        c.cpu_bytes += cpu;
        c.decoders += 1;
    }
    for (track, ms) in gpu_ms {
        if seq.tracks.iter().any(|t| t.id == *track) {
            out.entry(*track).or_default().gpu_ms = Some(*ms as f32);
        }
    }
    out
}

static COSTS: std::sync::LazyLock<Mutex<HashMap<TrackId, TrackCost>>> = std::sync::LazyLock::new(Default::default);

/// Every track's cost now; a track left out costs nothing.
pub fn set_costs(costs: HashMap<TrackId, TrackCost>) {
    *COSTS.lock().unwrap_or_else(|e| e.into_inner()) = costs;
}

/// Track `track`'s cost now, `None` while it costs nothing.
pub fn cost(track: TrackId) -> Option<TrackCost> {
    COSTS.lock().unwrap_or_else(|e| e.into_inner()).get(&track).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn at(f: i64) -> RationalTime {
        RationalTime { value: f, rate: 24 }
    }

    /// A track's decoders add up on it, a precomposition's inner clips on
    /// the track it sits on; a clip on no track counts nowhere.
    #[test]
    fn decoders_add_up_on_their_tracks() {
        let m = MediaId::new();
        let (a, b) = (Clip::media(m, at(0), at(24), at(0), at(24)), Clip::media(m, at(24), at(48), at(0), at(24)));
        let inner = Clip::media(m, at(0), at(24), at(0), at(24));
        let mut scene = Sequence::new_default();
        let mut st = Track::new("V1", TrackKind::Video);
        st.clips = vec![inner.clone()];
        scene.tracks = vec![st];
        let mut nest = Clip::media(m, at(0), at(48), at(0), at(48));
        nest.precomp = Some(std::sync::Arc::new(scene));
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![a.clone(), b.clone()];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![nest.clone()];
        let (t1, t2) = (v1.id, v2.id);
        let mut seq = Sequence::new_default();
        seq.tracks = vec![v2, v1];
        let timed = HashMap::from([(t2, 3.5), (TrackId::new(), 9.0)]);
        let got = costs(&seq, [(a.id, 100, 0), (b.id, 50, 10), (inner.id, 7, 0), (ClipId::new(), 999, 999)], &timed);
        assert_eq!(got[&t1], TrackCost { gpu_bytes: 150, cpu_bytes: 10, decoders: 2, gpu_ms: None });
        assert_eq!(got[&t2], TrackCost { gpu_bytes: 7, cpu_bytes: 0, decoders: 1, gpu_ms: Some(3.5) }, "a precomposition's inner clip on its track, and its GPU time");
        assert_eq!(got.len(), 2, "a clip or a time on no track counts nowhere");
    }

    #[test]
    fn a_cost_is_said_in_a_few_words() {
        let c = TrackCost { gpu_bytes: 3 << 29, cpu_bytes: 340 << 20, decoders: 2, gpu_ms: Some(4.06) };
        assert_eq!(c.describe_timed(true), "2 decoders, 1.5 GB on the GPU, 340 MB in memory, 4.1 ms of GPU time a frame");
        assert_eq!(TrackCost { decoders: 1, ..Default::default() }.describe_timed(true), "1 decoder", "not timed yet");
        assert_eq!(TrackCost { decoders: 1, ..Default::default() }.describe_timed(false), "1 decoder, GPU time not measured");
    }
}
