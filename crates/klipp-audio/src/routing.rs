//! The sums aux sends and buses make (#2861), shared by the playback
//! mixer and the export mixdown so the two mix them alike.

/// `src` into `dest` at a gain moving from `from` to `to` across the
/// block, frame by frame, as a track's fader ramps: the last frame at
/// `to`. Held, the gain is `to` throughout.
pub fn add_ramped(src: &[f32], from: f32, to: f32, dest: &mut [f32]) {
    if from == to {
        for (d, s) in dest.iter_mut().zip(src) {
            *d += s * to;
        }
        return;
    }
    let frames = (src.len() / 2).max(1) as f32;
    for (j, (d, s)) in dest.chunks_mut(2).zip(src.chunks(2)).enumerate() {
        let g = from + (to - from) * ((j + 1) as f32 / frames);
        for (d, s) in d.iter_mut().zip(s) {
            *d += s * g;
        }
    }
}

/// A bus's sum into `dest` at its fader and its pan's gains; its peaks,
/// left and right, after them.
pub fn add_bus(bus: &[f32], volume: f32, (pan_l, pan_r): (f32, f32), dest: &mut [f32]) -> (f32, f32) {
    let (mut peak_l, mut peak_r) = (0.0f32, 0.0f32);
    for (d, s) in dest.chunks_mut(2).zip(bus.chunks(2)) {
        let out_l = s[0] * volume * pan_l;
        peak_l = peak_l.max(out_l.abs());
        d[0] += out_l;
        if let (Some(d1), Some(s1)) = (d.get_mut(1), s.get(1)) {
            let out_r = s1 * volume * pan_r;
            peak_r = peak_r.max(out_r.abs());
            *d1 += out_r;
        }
    }
    (peak_l, peak_r)
}

/// The most a way through the buses is made up for, in seconds: each
/// bus's effects may take up to 1 s, and 32 buses one after another
/// would hold the picture back half a minute.
pub const MAX_ROUTE_SECS: u32 = 2;

/// The latency of each bus's way to master: its own effects' and that of
/// every bus after it, in stereo frames, at most `cap`. `output[b]` is the
/// bus `b` feeds, `None` master; a way that loops is cut where it comes
/// round. A way held at `cap` comes back that much early.
pub fn bus_routes(latency: &[u32], output: &[Option<usize>], cap: u32) -> Vec<u32> {
    (0..latency.len())
        .map(|b| {
            let (mut at, mut sum, mut steps) = (Some(b), 0u32, 0);
            while let Some(i) = at.filter(|&i| i < latency.len() && steps <= latency.len()) {
                sum = sum.saturating_add(latency[i]);
                at = output.get(i).copied().flatten();
                steps += 1;
            }
            sum.min(cap)
        })
        .collect()
}

/// How long each way through the mix is held back so every way reaches
/// master together (#2861): a track's sound, through its own way to
/// master or its bus, and through each of its sends and the buses after.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdcPlan {
    /// The latency every way is brought to, in stereo frames: what the
    /// picture is held back by.
    pub total: u32,
    /// Each track's delay before its fader, on all its ways at once.
    pub pre: Vec<u32>,
    /// Each track's further delay on its own way, after its fader.
    pub direct: Vec<u32>,
    /// Each track's further delay on each send, by its sends' order.
    pub sends: Vec<Vec<u32>>,
}

/// The plan for tracks of latency `own` (their sources' and effects'),
/// each going to master or bus `out`, and sending to the buses `sends`,
/// over buses whose ways to master take `route` ([`bus_routes`]). With
/// no buses it is each track held to the slowest, as it always was.
pub fn pdc_plan(own: &[u32], out: &[Option<usize>], sends: &[Vec<usize>], route: &[u32]) -> PdcPlan {
    let way = |b: Option<usize>| b.and_then(|b| route.get(b).copied()).unwrap_or(0);
    let longest: Vec<u32> = (0..own.len())
        .map(|t| {
            let direct = way(out.get(t).copied().flatten());
            sends.get(t).map_or(direct, |s| s.iter().map(|&b| way(Some(b))).fold(direct, u32::max))
        })
        .collect();
    let total = own.iter().zip(&longest).map(|(o, l)| o.saturating_add(*l)).max().unwrap_or(0);
    PdcPlan {
        total,
        pre: own.iter().zip(&longest).map(|(o, l)| total - o.saturating_add(*l)).collect(),
        direct: (0..own.len()).map(|t| longest[t] - way(out.get(t).copied().flatten())).collect(),
        sends: (0..own.len())
            .map(|t| sends.get(t).map_or_else(Vec::new, |s| s.iter().map(|&b| longest[t] - way(Some(b))).collect()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bus's way to master adds the latency of every bus after it; a
    /// loop is cut.
    #[test]
    fn a_buses_way_adds_the_buses_after_it() {
        assert_eq!(bus_routes(&[100, 30, 5], &[Some(1), None, Some(0)], u32::MAX), vec![130, 30, 135]);
        let looped = bus_routes(&[10, 20], &[Some(1), Some(0)], u32::MAX);
        assert!(looped.iter().all(|&l| l < 1000), "{looped:?}");
        // 32 buses of 1 s each, one after another: held at 2 s.
        let rate = 48_000;
        let chained: Vec<Option<usize>> = (0..32).map(|b| (b < 31).then_some(b + 1)).collect();
        let routes = bus_routes(&[rate; 32], &chained, MAX_ROUTE_SECS * rate);
        assert_eq!(routes[0], 2 * rate);
        assert_eq!(routes[31], rate);
    }

    /// Every way reaches master together: a track's own latency, plus its
    /// pre delay, plus a way's further delay, plus that way's buses, is
    /// the total for each way.
    #[test]
    fn every_way_reaches_master_together() {
        let route = bus_routes(&[512, 0], &[None, None], u32::MAX);
        // Track 0: 64 of its own, sends to the 512 bus and the 0 bus.
        // Track 1: 200 of its own, to master only.
        let plan = pdc_plan(&[64, 200], &[None, None], &[vec![0, 1], vec![]], &route);
        assert_eq!(plan.total, 576);
        for (t, own) in [64u32, 200].into_iter().enumerate() {
            assert_eq!(own + plan.pre[t] + plan.direct[t], plan.total, "track {t} to master");
        }
        for (s, &b) in [0usize, 1].iter().enumerate() {
            assert_eq!(64 + plan.pre[0] + plan.sends[0][s] + route[b], plan.total, "send {s}");
        }
        let plain = pdc_plan(&[10, 30, 0], &[None; 3], &[vec![], vec![], vec![]], &[]);
        assert_eq!((plain.total, plain.pre, plain.direct), (30, vec![20, 0, 30], vec![0, 0, 0]), "no buses: each held to the slowest");
    }
}
