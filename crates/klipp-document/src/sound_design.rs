//! Sound design from the cut itself: a whoosh on every cut, a click on
//! every reveal the beat sheet found, a riser into the last cut. The
//! effects are the ones the caller brings (search_sfx finds them); the
//! ops go on an "SFX" track, marked as effects and levelled.

use crate::ops::{Anchor, ClipRef, Op, TrackRef};
use klipp_core::MediaId;
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, TrackKind};

/// The effects to use, by media id.
#[derive(Debug, Clone, Copy, Default)]
pub struct SfxKit {
    pub whoosh: Option<MediaId>,
    pub click: Option<MediaId>,
    pub riser: Option<MediaId>,
}

/// One placement the pass decided on.
#[derive(Debug, Clone, PartialEq)]
pub struct SfxPlacement {
    pub what: &'static str,
    pub media_id: MediaId,
    pub at_secs: f64,
    pub source_in_secs: f64,
    pub secs: f64,
    pub volume_db: f32,
}

/// Where the effects go: the cuts of the top picture track (a whoosh
/// whose loudest moment lands on the cut), the reveals inside each clip
/// (a click; none within half a second of a cut, at most one every
/// second and a half), and the last cut (a riser ending on it). Nothing
/// is placed where the SFX track already has a clip. `reveals_of` gives
/// a file's reveal times, `duration_of` and `loudest_of` an effect's
/// length and loudest moment.
pub fn placements(
    seq: &Sequence,
    pool: &MediaPool,
    kit: SfxKit,
    reveals_of: &dyn Fn(MediaId) -> Vec<f64>,
    loudest_of: &dyn Fn(MediaId) -> Option<f64>,
) -> Vec<SfxPlacement> {
    let Some(video) = seq.tracks.iter().find(|t| t.kind == TrackKind::Video) else { return Vec::new() };
    let file_len = |id: MediaId| pool.get(&id).map(|it| it.duration.to_secs_f64()).unwrap_or(0.0);
    let taken: Vec<(f64, f64)> = seq
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.name.eq_ignore_ascii_case("sfx"))
        .flat_map(|t| t.clips.iter().map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())))
        .collect();
    let free = |a: f64, b: f64| !taken.iter().any(|(x, y)| a < *y && b > *x);
    let mut out = Vec::new();
    // The cuts: a boundary that is not the same file carrying on.
    let clips: Vec<_> = video.clips.iter().filter(|c| c.enabled).collect();
    let mut cuts: Vec<f64> = Vec::new();
    for w in clips.windows(2) {
        let (a, b) = (w[0], w[1]);
        let adjacent = (b.timeline_in.to_secs_f64() - a.timeline_out.to_secs_f64()).abs() < 0.02;
        let carries_on = a.media_id() == b.media_id() && (a.source_out.to_secs_f64() - b.source_in.to_secs_f64()).abs() < 0.02;
        if adjacent && !carries_on {
            cuts.push(b.timeline_in.to_secs_f64());
        }
    }
    if let Some(id) = kit.whoosh {
        let len = file_len(id);
        for cut in &cuts {
            let loud = loudest_of(id).unwrap_or(0.3).min(len);
            let lead = loud.min(0.3);
            let source_in = (loud - lead).max(0.0);
            let secs = (len - source_in).min(1.5);
            let at = (cut - lead).max(0.0);
            if secs > 0.05 && free(at, at + secs) {
                out.push(SfxPlacement { what: "whoosh", media_id: id, at_secs: at, source_in_secs: source_in, secs, volume_db: -6.0 });
            }
        }
    }
    if let Some(id) = kit.click {
        let len = file_len(id);
        let loud = loudest_of(id).unwrap_or(0.05).min(len);
        let lead = loud.min(0.05);
        let source_in = (loud - lead).max(0.0);
        let secs = (len - source_in).min(0.6);
        let mut last: Option<f64> = None;
        for c in &clips {
            let Some(media) = c.media_id() else { continue };
            let (s_in, s_out) = (c.source_in.to_secs_f64(), c.source_out.to_secs_f64());
            let speed = c.speed.max(0.001);
            for r in reveals_of(media) {
                if r <= s_in || r >= s_out {
                    continue;
                }
                let t = c.timeline_in.to_secs_f64() + (r - s_in) / speed;
                if cuts.iter().any(|cut| (cut - t).abs() < 0.5) || last.is_some_and(|l| t - l < 1.5) {
                    continue;
                }
                let at = (t - lead).max(0.0);
                if secs > 0.05 && free(at, at + secs) {
                    out.push(SfxPlacement { what: "click", media_id: id, at_secs: at, source_in_secs: source_in, secs, volume_db: -8.0 });
                    last = Some(t);
                }
            }
        }
    }
    if let (Some(id), Some(cut)) = (kit.riser, cuts.last().copied()) {
        let len = file_len(id);
        let secs = len.min(3.0);
        let loud = loudest_of(id).unwrap_or(secs).min(len);
        let source_in = (loud - secs).max(0.0);
        let at = (cut - (loud - source_in)).max(0.0);
        if secs > 0.05 && free(at, at + secs) {
            out.push(SfxPlacement { what: "riser", media_id: id, at_secs: at, source_in_secs: source_in, secs, volume_db: -6.0 });
        }
    }
    out.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs));
    out
}

/// The placements as ops: an SFX track made when there is none, each
/// effect placed, marked sfx and levelled.
pub fn ops(seq: &Sequence, placements: &[SfxPlacement]) -> Vec<Op> {
    let mut ops = Vec::new();
    if placements.is_empty() {
        return ops;
    }
    let track = match seq.tracks.iter().position(|t| t.kind == TrackKind::Audio && t.name.eq_ignore_ascii_case("sfx")) {
        Some(ti) => ti,
        None => {
            ops.push(Op::AddTrack { kind: "audio".into(), name: Some("SFX".into()) });
            seq.tracks.len()
        }
    };
    for p in placements {
        ops.push(Op::Place {
            media_id: p.media_id,
            track: TrackRef::Index(track),
            at: Anchor::Secs(p.at_secs),
            source_in: Some(p.source_in_secs),
            source_out: Some(p.source_in_secs + p.secs),
            insert: false,
            with_audio: true,
        });
        let mid = ClipRef::At { track, secs: p.at_secs + p.secs / 2.0 };
        ops.push(Op::Role { clip_id: mid.clone(), role: "sfx".into() });
        ops.push(Op::Audio { clip_id: mid, volume_db: Some(p.volume_db), gain_db: None, pan: None, fade_in: None, fade_out: None });
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Timecode;
    use klipp_timeline::{Clip, Track};

    fn tc(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 25)
    }

    #[test]
    fn a_whoosh_on_each_cut_a_click_on_each_reveal_a_riser_into_the_last() {
        let mut pool = MediaPool::new();
        let mut screen = klipp_media::MediaItem::synthetic_for_test("/tmp/screen.mp4".into(), None);
        screen.duration = tc(60.0);
        let screen = pool.add(screen);
        let mut fx = klipp_media::MediaItem::synthetic_for_test("/tmp/whoosh.wav".into(), None);
        fx.duration = tc(2.0);
        fx.has_audio = true;
        let whoosh = pool.add(fx.clone());
        let click = pool.add(fx.clone());
        fx.duration = tc(5.0);
        let riser = pool.add(fx);
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips.push(Clip::media(screen, tc(0.0), tc(4.0), tc(0.0), tc(4.0)));
        v.clips.push(Clip::media(screen, tc(4.0), tc(8.0), tc(4.0), tc(8.0))); // carries on: no cut
        v.clips.push(Clip::media(screen, tc(8.0), tc(12.0), tc(20.0), tc(24.0))); // a cut at 8
        v.clips.push(Clip::media(screen, tc(12.0), tc(16.0), tc(40.0), tc(44.0))); // a cut at 12
        seq.tracks.push(v);
        let reveals = |id: MediaId| if id == screen { vec![2.0, 21.0, 23.9, 42.0] } else { vec![] };
        let loudest = |id: MediaId| if id == riser { Some(4.5) } else { Some(0.4) };
        let kit = SfxKit { whoosh: Some(whoosh), click: Some(click), riser: Some(riser) };
        let got = placements(&seq, &pool, kit, &reveals, &loudest);
        let whooshes: Vec<f64> = got.iter().filter(|p| p.what == "whoosh").map(|p| p.at_secs).collect();
        assert_eq!(whooshes, vec![7.7, 11.7], "a whoosh leads each cut by its loudest moment: {got:?}");
        let clicks: Vec<f64> = got.iter().filter(|p| p.what == "click").map(|p| (p.at_secs * 100.0).round() / 100.0).collect();
        // Reveals at 2.0, 21.0 (timeline 9.0) and 42.0 (timeline 14.0); 23.9 lands on the cut at 12 and is left out.
        assert_eq!(clicks, vec![1.95, 8.95, 13.95], "{got:?}");
        let riser_at = got.iter().find(|p| p.what == "riser").map(|p| (p.at_secs, p.secs)).unwrap();
        assert!((riser_at.0 - 9.0).abs() < 1e-6 && (riser_at.1 - 3.0).abs() < 1e-6, "three seconds ending on the last cut: {riser_at:?}");
        let ops = ops(&seq, &got);
        assert!(matches!(&ops[0], Op::AddTrack { name: Some(n), .. } if n == "SFX"));
        assert_eq!(ops.len(), 1 + got.len() * 3);
        // A second pass places nothing where the first did.
        let mut sfx = Track::new("SFX", TrackKind::Audio);
        for p in &got {
            sfx.clips.push(Clip::media(p.media_id, tc(p.at_secs), tc(p.at_secs + p.secs), tc(p.source_in_secs), tc(p.source_in_secs + p.secs)));
        }
        seq.tracks.push(sfx);
        assert!(placements(&seq, &pool, kit, &reveals, &loudest).is_empty());
    }
}
