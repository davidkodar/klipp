//! Sound design from the cut itself: a whoosh on every cut, a click on
//! every reveal the beat sheet found, a riser into the last cut. The
//! effects are the ones the caller brings (search_sfx finds them); the
//! ops go on an "SFX" track, marked as effects and levelled.

use crate::ops::{Anchor, ClipRef, Op, TrackRef};
use klipp_core::MediaId;
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, TrackKind};

/// The effects to use, by media id, and which cuts take a whoosh.
#[derive(Debug, Clone, Copy, Default)]
pub struct SfxKit {
    pub whoosh: Option<MediaId>,
    pub click: Option<MediaId>,
    pub riser: Option<MediaId>,
    pub on: SfxOn,
}

/// Which cuts a whoosh goes on (#791): every cut, the cuts carrying a
/// transition, or only the stylised ones (a zoom, a whip, a glitch, a
/// film burn), never a dissolve or a dip, which want quiet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SfxOn {
    #[default]
    Cuts,
    Transitions,
    Stylised,
}

impl SfxOn {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.to_lowercase().as_str() {
            "cuts" | "cut" => Ok(Self::Cuts),
            "transitions" | "transition" => Ok(Self::Transitions),
            "stylised" | "stylized" => Ok(Self::Stylised),
            other => Err(format!("on {other:?} is not one of cuts, transitions or stylised")),
        }
    }
}

/// A transition that marks time or place quietly: no whoosh on it.
fn is_calm(kind: klipp_core::TransitionKind) -> bool {
    use klipp_core::TransitionKind as K;
    matches!(kind, K::CrossDissolve | K::FilmDissolve | K::BlurDissolve | K::LuminanceDissolve | K::AdditiveDissolve | K::DipToBlack | K::DipToWhite | K::FadeToColor)
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
    placements_with_lengths(seq, kit, reveals_of, loudest_of, &|id| pool.get(&id).map(|it| it.duration.to_secs_f64()).unwrap_or(0.0))
}

/// Preview a generated whoosh without creating a file or importing media.
/// The placeholder media ID is local to this preview and must not be applied.
pub fn preview_whoosh(seq: &Sequence, duration_secs: f64, on: SfxOn) -> Vec<SfxPlacement> {
    placements_with_lengths(seq, SfxKit { whoosh: Some(MediaId::new()), on, ..Default::default() }, &|_| Vec::new(), &|_| Some(duration_secs / 2.0), &|_| duration_secs)
}

fn placements_with_lengths(
    seq: &Sequence,
    kit: SfxKit,
    reveals_of: &dyn Fn(MediaId) -> Vec<f64>,
    loudest_of: &dyn Fn(MediaId) -> Option<f64>,
    file_len: &dyn Fn(MediaId) -> f64,
) -> Vec<SfxPlacement> {
    let Some(video) = seq.tracks.iter().find(|t| t.kind == TrackKind::Video) else { return Vec::new() };
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
    // Every cut (the riser and the clicks read these), and those `on`
    // asks a whoosh for (#810 review).
    let mut cuts: Vec<f64> = Vec::new();
    let mut whoosh_cuts: Vec<f64> = Vec::new();
    for w in clips.windows(2) {
        let (a, b) = (w[0], w[1]);
        let adjacent = (b.timeline_in.to_secs_f64() - a.timeline_out.to_secs_f64()).abs() < 0.02;
        let carries_on = a.media_id() == b.media_id() && (a.source_out.to_secs_f64() - b.source_in.to_secs_f64()).abs() < 0.02;
        let transition = video.transitions.iter().find(|t| t.clip_a_id == a.id && t.clip_b_id == b.id).map(|t| t.kind);
        let wanted = match kit.on {
            SfxOn::Cuts => true,
            SfxOn::Transitions => transition.is_some(),
            SfxOn::Stylised => transition.is_some_and(|k| !is_calm(k)),
        };
        if adjacent && !carries_on {
            cuts.push(b.timeline_in.to_secs_f64());
            if wanted {
                whoosh_cuts.push(b.timeline_in.to_secs_f64());
            }
        }
    }
    if let Some(id) = kit.whoosh {
        let len = file_len(id);
        for cut in &whoosh_cuts {
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
            for r in reveals_of(media) {
                if r <= s_in || r >= s_out {
                    continue;
                }
                let t = c.timeline_in.to_secs_f64() + c.elapsed_for_source_secs(r, seq.fps.as_f64());
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
    // Reserve each accepted placement as well as the existing clips.
    // Match Place's frame rounding: separately snapped source endpoints
    // determine duration, rather than snapping at + duration as one value.
    let fps = seq.fps.as_f64();
    let mut reserved: Vec<_> = taken.iter().map(|(a, b)| (a * fps, b * fps)).collect();
    out.retain(|p| {
        let start = (p.at_secs * fps).round();
        let end = start + ((p.source_in_secs + p.secs) * fps).round() - (p.source_in_secs * fps).round();
        if end <= start || reserved.iter().any(|(a, b)| start < *b && end > *a) {
            return false;
        }
        reserved.push((start, end));
        true
    });
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
            sync_source_secs: None,
        });
        let mid = ClipRef::At { track, secs: p.at_secs + p.secs / 2.0 };
        ops.push(Op::Role { clip_id: mid.clone(), role: "sfx".into() });
        ops.push(Op::Audio { clip_id: Some(mid), track: None, from: None, to: None, volume_db: Some(p.volume_db), gain_db: None, pan: None, fade_in: None, fade_out: None });
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
    fn dense_montage_keeps_a_resolvable_sound_pass() {
        for duration in [0.3, 1.5, 3.0] {
            let mut pool = MediaPool::new();
            let screen = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/montage.mp4".into(), None));
            let mut fx = klipp_media::MediaItem::synthetic_for_test("/tmp/whoosh.wav".into(), None);
            fx.duration = tc(duration);
            fx.has_audio = true;
            let whoosh = pool.add(fx);
            let mut seq = Sequence::new_default();
            seq.tracks.clear();
            let mut video = Track::new("V1", TrackKind::Video);
            for n in 0..8 {
                // Each shot restarts the source, so these are real cuts.
                video.clips.push(Clip::media(screen, Timecode::from_secs_f64(n as f64 * 0.85, 1000), Timecode::from_secs_f64((n + 1) as f64 * 0.85, 1000), tc(0.0), tc(0.85)));
            }
            seq.tracks.push(video);
            let placements = placements(&seq, &pool, SfxKit { whoosh: Some(whoosh), click: None, riser: Some(whoosh), on: SfxOn::Cuts }, &|_| Vec::new(), &|_| Some(duration / 2.0));
            assert!(!placements.is_empty());
            let plan = crate::ops::resolve(&seq, &pool, &ops(&seq, &placements));
            assert!(plan.is_ok(), "duration {duration}: {plan:?}");
        }
    }

    /// #791: four cuts, one bare, one dissolved, one zoomed, one whipped:
    /// on cuts all four take a whoosh, on transitions the three carrying
    /// one, on stylised only the zoom and the whip.
    #[test]
    fn whooshes_go_on_the_cuts_asked_for() {
        let mut pool = MediaPool::new();
        let mut footage = klipp_media::MediaItem::synthetic_for_test("/tmp/walk.mp4".into(), None);
        footage.duration = tc(60.0);
        let footage = pool.add(footage);
        let mut fx = klipp_media::MediaItem::synthetic_for_test("/tmp/whoosh.wav".into(), None);
        fx.duration = tc(1.0);
        fx.has_audio = true;
        let whoosh = pool.add(fx);
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut video = Track::new("V1", TrackKind::Video);
        for n in 0..5 {
            // A different part of the file each time: real cuts.
            video.clips.push(Clip::media(footage, tc(n as f64 * 3.0), tc((n + 1) as f64 * 3.0), tc(n as f64 * 10.0), tc(n as f64 * 10.0 + 3.0)));
        }
        let ids: Vec<_> = video.clips.iter().map(|c| c.id).collect();
        for (i, kind) in [(1, klipp_core::TransitionKind::CrossDissolve), (2, klipp_core::TransitionKind::ZoomInDissolve), (3, klipp_core::TransitionKind::WhipPan)] {
            video.transitions.push(klipp_timeline::Transition {
                id: klipp_core::TransitionId::new(),
                kind,
                duration: tc(0.4),
                clip_a_id: ids[i],
                clip_b_id: ids[i + 1],
                user_params: Default::default(),
                alignment: klipp_core::TransitionAlignment::Center,
            });
        }
        seq.tracks.push(video);
        let at = |on: SfxOn| -> Vec<f64> {
            placements(&seq, &pool, SfxKit { whoosh: Some(whoosh), on, ..Default::default() }, &|_| Vec::new(), &|_| Some(0.3))
                .iter()
                .map(|p| (p.at_secs + 0.3).round())
                .collect()
        };
        assert_eq!(at(SfxOn::Cuts), vec![3.0, 6.0, 9.0, 12.0]);
        assert_eq!(at(SfxOn::Transitions), vec![6.0, 9.0, 12.0]);
        assert_eq!(at(SfxOn::Stylised), vec![9.0, 12.0], "no whoosh on the dissolve");
        // The riser rises into the film's last cut whatever on says: a
        // bare cut after the last stylised one is still the last (#810 review).
        let mut bare_last = seq.clone();
        bare_last.tracks[0].transitions.retain(|t| t.kind != klipp_core::TransitionKind::WhipPan);
        let riser = placements(&bare_last, &pool, SfxKit { riser: Some(whoosh), on: SfxOn::Stylised, ..Default::default() }, &|_| Vec::new(), &|_| Some(1.0));
        assert_eq!(riser.len(), 1);
        assert!((riser[0].at_secs + 1.0 - 12.0).abs() < 1e-6, "the riser ends on the last cut at 12 s: {:?}", riser[0]);
        assert!(SfxOn::parse("whips").is_err());
    }

    #[test]
    fn a_sound_pass_skips_conflicting_reveals_and_risers() {
        let mut pool = MediaPool::new();
        let mut screen = klipp_media::MediaItem::synthetic_for_test("/tmp/screen.mp4".into(), None);
        screen.duration = tc(60.0);
        let screen = pool.add(screen);
        let mut fx = klipp_media::MediaItem::synthetic_for_test("/tmp/whoosh.wav".into(), None);
        fx.duration = tc(2.0);
        fx.has_audio = true;
        let whoosh = pool.add(fx.clone());
        fx.id = MediaId::new();
        let click = pool.add(fx.clone());
        fx.id = MediaId::new();
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
        let kit = SfxKit { whoosh: Some(whoosh), click: Some(click), riser: Some(riser), on: SfxOn::Cuts };
        let got = placements(&seq, &pool, kit, &reveals, &loudest);
        let whooshes: Vec<f64> = got.iter().filter(|p| p.what == "whoosh").map(|p| p.at_secs).collect();
        assert_eq!(whooshes, vec![7.7, 11.7], "a whoosh leads each cut by its loudest moment: {got:?}");
        let clicks: Vec<f64> = got.iter().filter(|p| p.what == "click").map(|p| (p.at_secs * 100.0).round() / 100.0).collect();
        // The reveal at timeline 9 overlaps the whoosh; 23.9 lands on the cut at 12. Both are left out.
        assert_eq!(clicks, vec![1.95, 13.95], "{got:?}");
        assert!(!got.iter().any(|p| p.what == "riser"), "the riser overlaps the first whoosh and is skipped");
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
