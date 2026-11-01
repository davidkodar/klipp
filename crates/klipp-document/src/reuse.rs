//! Where the timeline uses the same footage twice (#3272): picture clips,
//! wherever they sit (on a track, inside a compound clip or a
//! precomposition, at any depth), that show the same frames of one file.
//!
//! A use is one picture clip's frames: the file's identity (its
//! fingerprint, so one file under two media items is one; its path
//! without one), and the source frame each timeline frame shows, worked
//! out through the clip's own timing, holds, speed ramps and reverse
//! included. A source frame shown by two uses is reused; each clip is
//! marked over the timeline frames whose source frames another use shows
//! too, the overlap only. A clip that shows a frame twice itself (a hold,
//! a ramp back and forth) isn't marked against itself, and stills aren't
//! footage.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use klipp_core::ClipId;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// Compound clips are opened this deep at most, as the renderer does.
const MAX_DEPTH: usize = 8;

/// One side of a reuse: the clip on the sequence it is in, and the clip
/// inside it that shows the footage when they differ.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Side {
    /// The clip on the sequence's own track, and that track's index.
    pub clip: ClipId,
    pub track: usize,
    /// The clip showing the footage, and the compound clips between, the
    /// outermost first; both the clip itself when it is on the track.
    pub inner: ClipId,
    pub inside: Vec<ClipId>,
    /// Its file's name.
    pub name: String,
    /// The reused span on the timeline, and the source span it shows, s.
    pub timeline: (f64, f64),
    pub source: (f64, f64),
}

/// Two uses showing the same frames.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Pair {
    pub a: Side,
    pub b: Side,
    /// How long the shared part is, s.
    pub secs: f64,
}

/// A clip's reused span, in the sequence's frames: `from..to`, and the
/// first other use of those frames, with the timeline frame there that
/// shows `from`'s source frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub from: i64,
    pub to: i64,
    pub partner: ClipId,
    pub partner_track: usize,
    pub partner_frame: i64,
    pub partner_name: String,
    /// The partner sits inside a compound clip: its name in words.
    pub partner_inside: bool,
    /// Other uses of these frames past the first.
    pub more: usize,
}

/// What [`find`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub pairs: Vec<Pair>,
    /// By the clip on the sequence's own track.
    pub marks: HashMap<ClipId, Vec<Mark>>,
}

impl Report {
    /// In words: "nothing is used twice", or the pairs, the first few.
    pub fn text(&self) -> String {
        if self.pairs.is_empty() {
            return "Nothing is used twice.".into();
        }
        let tc = crate::review::clock;
        let mut out = format!("{} reuse{}:", self.pairs.len(), if self.pairs.len() == 1 { "" } else { "s" });
        for p in self.pairs.iter().take(10) {
            out.push_str(&format!("\n- {} at {} shows the same frames as {} at {} ({:.1} s)", p.a.name, tc(p.a.timeline.0), p.b.name, tc(p.b.timeline.0), p.secs));
        }
        if self.pairs.len() > 10 {
            out.push_str(&format!("\n- and {} more", self.pairs.len() - 10));
        }
        out
    }
}

/// One timeline frame of one use.
#[derive(Clone)]
struct Seen {
    /// Which use: the clips from the track down.
    path: Vec<ClipId>,
    track: usize,
    frame: i64,
    source_secs: f64,
    name: String,
}

/// A use's path as keys that order.
fn ids(path: &[ClipId]) -> Vec<u128> {
    path.iter().map(|c| c.0.as_u128()).collect()
}

/// Where `seq` (with `sequences`, the ones its compound clips open) shows
/// the same footage twice, with the files' identities from `pool`.
pub fn find(seq: &Sequence, sequences: &[Sequence], pool: &MediaPool) -> Report {
    let fps = seq.fps.as_f64().max(1.0);
    let mut by_frame: HashMap<(u64, i64), Vec<Seen>> = HashMap::new();
    for (ti, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible) {
        for clip in track.clips.iter().filter(|c| c.enabled) {
            let first = (clip.timeline_in.to_secs_f64() * fps - 1e-6).ceil() as i64;
            let end = (clip.timeline_out.to_secs_f64() * fps - 1e-6).ceil() as i64;
            for f in first.max(0)..end {
                let elapsed = f as f64 / fps - clip.timeline_in.to_secs_f64();
                visit(clip, elapsed, fps, sequences, pool, &mut vec![clip.id], ti, f, 0, &mut by_frame);
            }
        }
    }
    report(by_frame, fps)
}

#[allow(clippy::too_many_arguments)]
fn visit(clip: &Clip, elapsed: f64, fps: f64, sequences: &[Sequence], pool: &MediaPool, path: &mut Vec<ClipId>, track: usize, frame: i64, depth: usize, out: &mut HashMap<(u64, i64), Vec<Seen>>) {
    match &clip.kind {
        ClipKind::Media { media_id } => {
            let Some(item) = pool.get(media_id).filter(|i| !i.is_still && !i.video_codec.is_empty()) else { return };
            let identity = item.fingerprint.unwrap_or_else(|| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone()).hash(&mut h);
                h.finish()
            });
            let source_secs = clip.source_secs_at(elapsed, fps);
            let media_fps = item.fps.as_f64();
            let media_fps = if media_fps > 0.0 { media_fps } else { fps };
            let source_frame = (source_secs * media_fps + 1e-6).floor() as i64;
            out.entry((identity, source_frame)).or_default().push(Seen { path: path.clone(), track, frame, source_secs, name: item.filename.clone() });
        }
        ClipKind::Sequence { sequence_id } | ClipKind::Precomposition { sequence_id } if depth < MAX_DEPTH => {
            let Some(inner) = sequences.iter().find(|s| s.id == *sequence_id) else { return };
            let at = clip.source_secs_at(elapsed, fps);
            for t in inner.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.visible) {
                for c in t.clips.iter().filter(|c| c.enabled && c.timeline_in.to_secs_f64() <= at + 1e-9 && at + 1e-9 < c.timeline_out.to_secs_f64()) {
                    path.push(c.id);
                    visit(c, at - c.timeline_in.to_secs_f64(), fps, sequences, pool, path, track, frame, depth + 1, out);
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

/// The frames shown by more than one use, as each clip's marks and the
/// pairs of uses.
fn report(by_frame: HashMap<(u64, i64), Vec<Seen>>, fps: f64) -> Report {
    // Per use (its path), each shared frame: its own sighting, the first
    // other use there, and how many more.
    let mut hits: BTreeMap<Vec<u128>, Vec<(Seen, Seen, usize)>> = BTreeMap::new();
    for seen in by_frame.into_values() {
        // The first sighting of each use at this source frame.
        let mut uses: Vec<&Seen> = Vec::new();
        for s in &seen {
            if !uses.iter().any(|u| u.path == s.path) {
                uses.push(s);
            }
        }
        if uses.len() < 2 {
            continue;
        }
        uses.sort_by_key(|u| (u.track, u.frame));
        for s in &seen {
            let mut others = uses.iter().filter(|u| u.path != s.path);
            let Some(first) = others.next() else { continue };
            hits.entry(ids(&s.path)).or_default().push((s.clone(), (*first).clone(), others.count()));
        }
    }
    let mut out = Report::default();
    let mut paired = std::collections::BTreeSet::new();
    for (key_path, mut list) in hits {
        let path = list[0].0.path.clone();
        list.sort_by_key(|h| h.0.frame);
        list.dedup_by_key(|h| h.0.frame);
        let mut runs: Vec<Vec<(Seen, Seen, usize)>> = Vec::new();
        for h in list {
            match runs.last_mut() {
                Some(run) if run.last().is_some_and(|l| l.0.frame + 1 == h.0.frame && l.1.path == h.1.path) => run.push(h),
                _ => runs.push(vec![h]),
            }
        }
        for run in runs {
            let (own, partner) = (&run[0].0, &run[0].1);
            let to = run[run.len() - 1].0.frame + 1;
            out.marks.entry(path[0]).or_default().push(Mark {
                from: own.frame,
                to,
                partner: partner.path[0],
                partner_track: partner.track,
                partner_frame: partner.frame,
                partner_name: partner.name.clone(),
                partner_inside: partner.path.len() > 1,
                more: run.iter().map(|h| h.2).max().unwrap_or(0),
            });
            // Each pair once, from the side met first.
            let theirs_key = ids(&partner.path);
            let key = if key_path <= theirs_key { (key_path.clone(), theirs_key) } else { (theirs_key, key_path.clone()) };
            if !paired.insert(key) {
                continue;
            }
            let span = |v: &mut dyn Iterator<Item = f64>| v.fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(x), b.max(x)));
            let source = span(&mut run.iter().map(|h| h.0.source_secs));
            let source = (source.0, source.1 + 1.0 / fps);
            let theirs = span(&mut run.iter().map(|h| h.1.frame as f64));
            let side = |s: &Seen, timeline: (f64, f64)| Side {
                clip: s.path[0],
                track: s.track,
                inner: *s.path.last().unwrap_or(&s.path[0]),
                inside: s.path[..s.path.len() - 1].to_vec(),
                name: s.name.clone(),
                timeline,
                source,
            };
            out.pairs.push(Pair {
                a: side(own, (own.frame as f64 / fps, to as f64 / fps)),
                b: side(partner, (theirs.0 / fps, (theirs.1 + 1.0) / fps)),
                secs: (to - own.frame) as f64 / fps,
            });
        }
    }
    out.pairs.sort_by(|p, q| p.a.timeline.0.total_cmp(&q.a.timeline.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::Track;

    fn tc(f: i64) -> RationalTime {
        RationalTime::from_frames(f, 24)
    }

    /// A pool of video files at 24 fps, each with its own fingerprint
    /// unless `same` says which share one.
    fn pool(files: &[(MediaId, u64, bool)]) -> MediaPool {
        let mut pool = MediaPool::default();
        for (id, fingerprint, still) in files {
            let mut item = klipp_media::MediaItem::synthetic_for_test(format!("/footage/{fingerprint}.mov").into(), None);
            (item.id, item.fingerprint, item.is_still) = (*id, Some(*fingerprint), *still);
            item.fps = klipp_core::Fps::film_24();
            pool.add(item);
        }
        pool
    }

    fn seq(tracks: Vec<Vec<Clip>>) -> Sequence {
        let mut s = Sequence::new_default();
        s.fps = klipp_core::Fps::film_24();
        s.tracks = tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| {
                let mut t = Track::new(&format!("V{}", i + 1), TrackKind::Video);
                t.clips = clips;
                t
            })
            .collect();
        s
    }

    fn marks(r: &Report, c: &Clip) -> Vec<(i64, i64)> {
        r.marks.get(&c.id).map(|m| m.iter().map(|m| (m.from, m.to)).collect()).unwrap_or_default()
    }

    /// Two clips from overlapping ranges of one file are marked on exactly
    /// the overlap, each on its own side, and named to each other.
    #[test]
    fn overlapping_ranges_are_marked_on_exactly_the_overlap() {
        let f = MediaId::new();
        let a = Clip::media(f, tc(0), tc(48), tc(0), tc(48));
        let b = Clip::media(f, tc(100), tc(148), tc(24), tc(72));
        let s = seq(vec![vec![a.clone(), b.clone()]]);
        let r = find(&s, &[], &pool(&[(f, 1, false)]));
        assert_eq!(marks(&r, &a), [(24, 48)]);
        assert_eq!(marks(&r, &b), [(100, 124)]);
        let m = &r.marks[&a.id][0];
        assert_eq!((m.partner, m.partner_frame), (b.id, 100), "A's first shared frame is shown by B at frame 100");
        assert_eq!(r.pairs.len(), 1, "one pair");
        assert!((r.pairs[0].secs - 1.0).abs() < 1e-9 && r.text().contains("1 reuse"));
    }

    /// What a look costs on an hour at 24 fps: three tracks of 5 s clips
    /// from 20 files, each file's range used about twice.
    #[test]
    #[ignore = "a measurement: cargo test -p klipp-document --release measure_find_on_an_hour -- --ignored --nocapture"]
    fn measure_find_on_an_hour() {
        let files: Vec<MediaId> = (0..20).map(|_| MediaId::new()).collect();
        let spec: Vec<(MediaId, u64, bool)> = files.iter().enumerate().map(|(i, f)| (*f, i as u64 + 1, false)).collect();
        let tracks = (0..3)
            .map(|t| (0..720).map(|i| Clip::media(files[(i * 7 + t * 3) % 20], tc(i as i64 * 120), tc(i as i64 * 120 + 120), tc((i as i64 * 37) % 4000), tc((i as i64 * 37) % 4000 + 120))).collect())
            .collect();
        let (s, p) = (seq(tracks), pool(&spec));
        let t = std::time::Instant::now();
        let r = find(&s, &[], &p);
        println!("one hour, 3 tracks, 2160 clips: {:.0} ms, {} pairs", t.elapsed().as_secs_f64() * 1000.0, r.pairs.len());
    }

    #[test]
    fn separate_ranges_of_one_file_are_not_marked() {
        let f = MediaId::new();
        let a = Clip::media(f, tc(0), tc(24), tc(0), tc(24));
        let b = Clip::media(f, tc(50), tc(74), tc(24), tc(48));
        let r = find(&seq(vec![vec![a, b]]), &[], &pool(&[(f, 1, false)]));
        assert!(r.marks.is_empty() && r.pairs.is_empty());
        assert_eq!(r.text(), "Nothing is used twice.");
    }

    /// A held frame inside another clip's range is marked for its whole
    /// length, the other clip on that one frame; a hold alone, showing one
    /// frame many times, isn't marked against itself.
    #[test]
    fn a_held_frame_is_reuse_and_a_hold_alone_isnt() {
        let f = MediaId::new();
        let a = Clip::media(f, tc(0), tc(48), tc(0), tc(48));
        let mut hold = Clip::media(f, tc(60), tc(72), tc(30), tc(31));
        hold.hold = Some(tc(30));
        let r = find(&seq(vec![vec![a.clone(), hold.clone()]]), &[], &pool(&[(f, 1, false)]));
        assert_eq!(marks(&r, &hold), [(60, 72)]);
        assert_eq!(marks(&r, &a), [(30, 31)]);
        let alone = find(&seq(vec![vec![hold]]), &[], &pool(&[(f, 1, false)]));
        assert!(alone.marks.is_empty(), "a clip isn't marked against itself");
    }

    /// A reversed clip is marked on the frames it really shares: shown
    /// last first.
    #[test]
    fn a_reversed_clip_is_marked_on_the_frames_it_shares() {
        let f = MediaId::new();
        let a = Clip::media(f, tc(0), tc(12), tc(0), tc(12));
        let mut back = Clip::media(f, tc(100), tc(124), tc(0), tc(24));
        back.speed = -1.0;
        let r = find(&seq(vec![vec![a.clone(), back.clone()]]), &[], &pool(&[(f, 1, false)]));
        // Played back from frame 23: source frames 11..0 land on its
        // timeline frames 112..124.
        assert_eq!(marks(&r, &back), [(112, 124)]);
        assert_eq!(marks(&r, &a), [(0, 12)]);
    }

    /// One file under two media items (its fingerprint the same) is one.
    #[test]
    fn one_file_under_two_media_items_counts_once() {
        let (f, g) = (MediaId::new(), MediaId::new());
        let a = Clip::media(f, tc(0), tc(24), tc(0), tc(24));
        let b = Clip::media(g, tc(50), tc(74), tc(0), tc(24));
        let r = find(&seq(vec![vec![a.clone()], vec![b.clone()]]), &[], &pool(&[(f, 7, false), (g, 7, false)]));
        assert_eq!((marks(&r, &a), marks(&r, &b)), (vec![(0, 24)], vec![(50, 74)]));
        let other = find(&seq(vec![vec![a], vec![b]]), &[], &pool(&[(f, 7, false), (g, 8, false)]));
        assert!(other.marks.is_empty(), "two files");
    }

    /// A use inside a compound clip, and inside a precomposition, is
    /// counted and marked on the clip on the sequence's track, named as
    /// inside it.
    #[test]
    fn uses_inside_compounds_and_precompositions_are_counted() {
        let f = MediaId::new();
        let mut inner = Sequence::new_default();
        inner.fps = klipp_core::Fps::film_24();
        let inside = Clip::media(f, tc(0), tc(48), tc(0), tc(48));
        let mut it = Track::new("V1", TrackKind::Video);
        it.clips = vec![inside.clone()];
        inner.tracks = vec![it];
        let direct = Clip::media(f, tc(0), tc(24), tc(12), tc(36));
        for kind in [ClipKind::Sequence { sequence_id: inner.id }, ClipKind::Precomposition { sequence_id: inner.id }] {
            // The compound shows its inner frames 24..48 at 100..124.
            let compound = Clip { kind, ..Clip::media(f, tc(100), tc(124), tc(24), tc(48)) };
            let s = seq(vec![vec![direct.clone(), compound.clone()]]);
            let r = find(&s, std::slice::from_ref(&inner), &pool(&[(f, 1, false)]));
            assert_eq!(marks(&r, &compound), [(100, 112)], "its inner frames 24..36 are the direct clip's 12..36's end");
            assert_eq!(marks(&r, &direct), [(12, 24)]);
            let m = &r.marks[&direct.id][0];
            assert!(m.partner == compound.id && m.partner_inside, "named as inside the compound");
            let pair = &r.pairs[0];
            let nested = if pair.a.clip == compound.id { &pair.a } else { &pair.b };
            assert_eq!((nested.inner, nested.inside.clone()), (inside.id, vec![compound.id]));
        }
    }

    #[test]
    fn stills_are_not_footage() {
        let f = MediaId::new();
        let a = Clip::media(f, tc(0), tc(24), tc(0), tc(24));
        let b = Clip::media(f, tc(50), tc(74), tc(0), tc(24));
        assert!(find(&seq(vec![vec![a, b]]), &[], &pool(&[(f, 1, true)])).marks.is_empty());
    }
}
