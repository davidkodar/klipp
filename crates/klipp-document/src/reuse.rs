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
//!
//! Two different files can also look the same: a re-export, a duplicate
//! download, the same stock shot from two sources. Given each file's
//! picture samples (`klipp_analysis::visual`, a CLIP embedding every 2 s),
//! [`find_with`] matches the parts of two files the timeline uses where
//! samples score [`LOOKS_MIN`] or more at one steady offset for
//! [`LOOKS_SECS`] or more, and marks those spans as it marks reuse, as
//! the other kind.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use klipp_core::{ClipId, MediaId};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// Compound clips are opened this deep at most, as the renderer does.
const MAX_DEPTH: usize = 8;

/// Samples this alike (cosine) or more look the same.
pub const LOOKS_MIN: f32 = 0.97;
/// For this long or more, s: two samples in a row.
pub const LOOKS_SECS: f64 = 4.0;

/// What a reuse is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The same frames of one file.
    SameFrames,
    /// Two files whose pictures look the same.
    LooksSame,
}

/// A file's picture samples: when (s), and a unit embedding times 127.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    pub every_secs: f64,
    pub samples: Vec<(f64, Vec<i8>)>,
}

/// The files' samples [`find_with`] compares, by media item; an item
/// left out hasn't been looked at.
#[derive(Clone, Debug, Default)]
pub struct Looks {
    pub of: HashMap<MediaId, Arc<Look>>,
}

/// The samples of the picture files `seq` and `others` use, from `load`
/// (given each file's path; `None` for one not looked at).
pub fn looks_of(seq: &Sequence, others: &[Sequence], pool: &MediaPool, load: impl Fn(&std::path::Path) -> Option<Look>) -> Looks {
    let used: std::collections::HashSet<MediaId> = std::iter::once(seq).chain(others).flat_map(|s| s.all_clips().filter_map(|c| c.media_id())).collect();
    let mut looks = Looks::default();
    for id in used {
        let Some(item) = pool.get(&id).filter(|i| !i.is_still && !i.video_codec.is_empty()) else { continue };
        if let Some(look) = load(&item.path) {
            looks.of.insert(id, Arc::new(look));
        }
    }
    looks
}

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

/// Two uses showing the same frames, or frames that look the same.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Pair {
    pub kind: Kind,
    pub a: Side,
    pub b: Side,
    /// How long the shared part is, s.
    pub secs: f64,
    /// How alike they look, at the least (cosine): `LooksSame` only.
    pub score: Option<f32>,
}

/// A clip's reused span, in the sequence's frames: `from..to`, and the
/// first other use of those frames, with the timeline frame there that
/// shows `from`'s source frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub kind: Kind,
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
    /// Why files that look the same weren't all compared, when they
    /// weren't: no samples were given, or some files have none.
    pub looks_unchecked: Option<String>,
}

impl Report {
    /// In words: "nothing is used twice", or the pairs, the first few.
    pub fn text(&self) -> String {
        let mut out = if self.pairs.is_empty() {
            "Nothing is used twice.".to_string()
        } else {
            let mut out = format!("{} reuse{}:", self.pairs.len(), if self.pairs.len() == 1 { "" } else { "s" });
            for p in self.pairs.iter().take(10) {
                out.push_str(&format!("\n- {}", p.what()));
            }
            if self.pairs.len() > 10 {
                out.push_str(&format!("\n- and {} more", self.pairs.len() - 10));
            }
            out
        };
        if let Some(why) = &self.looks_unchecked {
            out.push_str(&format!("\nShots that look the same in different files were not all compared: {why}."));
        }
        out
    }
}

impl Pair {
    /// "<a> at <time> shows the same frames as <b> at <time> (<n> s)", or
    /// "looks the same as".
    pub fn what(&self) -> String {
        let tc = crate::review::clock;
        let how = match self.kind {
            Kind::SameFrames => "shows the same frames as",
            Kind::LooksSame => "looks the same as",
        };
        format!("{} at {} {how} {} at {} ({:.1} s)", self.a.name, tc(self.a.timeline.0), self.b.name, tc(self.b.timeline.0), self.secs)
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
    name: Arc<str>,
    identity: u64,
}

/// A use's path as keys that order.
fn ids(path: &[ClipId]) -> Vec<u128> {
    path.iter().map(|c| c.0.as_u128()).collect()
}

/// Where `seq` (with `sequences`, the ones its compound clips open) shows
/// the same footage twice, with the files' identities from `pool`.
pub fn find(seq: &Sequence, sequences: &[Sequence], pool: &MediaPool) -> Report {
    find_with(seq, sequences, pool, None)
}

/// [`find`], and with `looks`, the spans of two files that look the same.
pub fn find_with(seq: &Sequence, sequences: &[Sequence], pool: &MediaPool, looks: Option<&Looks>) -> Report {
    let fps = seq.fps.as_f64().max(1.0);
    let mut by_frame: HashMap<(u64, i64), Vec<Seen>> = HashMap::new();
    let mut media = Media { pool, known: HashMap::new() };
    for (ti, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible) {
        for clip in track.clips.iter().filter(|c| c.enabled) {
            let first = (clip.timeline_in.to_secs_f64() * fps - 1e-6).ceil() as i64;
            let end = (clip.timeline_out.to_secs_f64() * fps - 1e-6).ceil() as i64;
            for f in first.max(0)..end {
                let elapsed = f as f64 / fps - clip.timeline_in.to_secs_f64();
                visit(clip, elapsed, fps, sequences, &mut media, &mut vec![clip.id], ti, f, 0, &mut by_frame);
            }
        }
    }
    // The files used, each once: its frame rate, and its samples if any.
    let mut used: BTreeMap<u64, (f64, Option<Arc<Look>>)> = BTreeMap::new();
    for (id, known) in &media.known {
        let Some((identity, media_fps, _)) = known else { continue };
        let entry = used.entry(*identity).or_insert((if *media_fps > 0.0 { *media_fps } else { fps }, None));
        if entry.1.is_none() {
            entry.1 = looks.and_then(|l| l.of.get(id).cloned());
        }
    }
    used.retain(|identity, _| by_frame.keys().any(|(i, _)| i == identity));
    let unlooked = used.values().filter(|u| u.1.is_none()).count();
    let looks_unchecked = match looks {
        _ if used.len() < 2 => None,
        None => Some("they weren't asked for".into()),
        Some(_) if unlooked == used.len() => Some("no file used has been looked at for picture search yet".into()),
        Some(_) if unlooked > 0 => Some(format!("{unlooked} of the {} files used haven't been looked at for picture search yet", used.len())),
        Some(_) => None,
    };
    let groups = look_groups(&by_frame, &used);
    let mut out = report(by_frame.into_values().map(|g| (g, None)), fps, Kind::SameFrames);
    let alike = report(groups.into_iter().map(|(g, score)| (g, Some(score))), fps, Kind::LooksSame);
    for (clip, marks) in alike.marks {
        out.marks.entry(clip).or_default().extend(marks);
    }
    out.pairs.extend(alike.pairs);
    out.pairs.sort_by(|p, q| p.a.timeline.0.total_cmp(&q.a.timeline.0));
    out.looks_unchecked = looks_unchecked;
    out
}

/// Where two files' samples look the same: `(from, to)` in `a`'s time,
/// the steady offset to `b`'s (`b = a + offset`), and the least score
/// over the run. Only the samples within `a_used` and `b_used` (the
/// source spans the timeline shows, s) are compared.
fn look_matches(a: &Look, b: &Look, a_used: &[(f64, f64)], b_used: &[(f64, f64)]) -> Vec<((f64, f64), f64, f32)> {
    let (sa, sb) = (picked(a, a_used), picked(b, b_used));
    if sa.is_empty() || sb.is_empty() {
        return Vec::new();
    }
    // Samples of different lengths (another model's) aren't alike at all.
    let cos = |i: usize, j: usize| {
        if sa[i].1.len() != sb[j].1.len() {
            return 0.0;
        }
        sa[i].1.iter().zip(sb[j].1).map(|(x, y)| *x as i32 * *y as i32).sum::<i32>() as f32 / (sa[i].2 * sb[j].2)
    };
    let nearest = |t: f64| -> usize {
        let k = sb.partition_point(|s| s.0 < t);
        match (k.checked_sub(1), (k < sb.len()).then_some(k)) {
            (Some(p), Some(n)) if (t - sb[p].0).abs() <= (sb[n].0 - t).abs() => p,
            (_, Some(n)) => n,
            (Some(p), None) => p,
            (None, None) => 0,
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < sa.len() {
        let mut starts: Vec<(usize, f32)> = (0..sb.len()).map(|j| (j, cos(i, j))).filter(|(_, c)| *c >= LOOKS_MIN).collect();
        starts.sort_by(|x, y| y.1.total_cmp(&x.1));
        let mut found = None;
        for (j, c) in starts {
            let offset = sb[j].0 - sa[i].0;
            let (mut last, mut least) = (i, c);
            while last + 1 < sa.len() && sa[last + 1].0 - sa[last].0 <= a.every_secs * 1.5 {
                let k = nearest(sa[last + 1].0 + offset);
                let score = cos(last + 1, k);
                if (sb[k].0 - sa[last + 1].0 - offset).abs() > b.every_secs * 0.5 || score < LOOKS_MIN {
                    break;
                }
                (last, least) = (last + 1, least.min(score));
            }
            if sa[last].0 + a.every_secs - sa[i].0 >= LOOKS_SECS - 1e-6 {
                found = Some((last, offset, least));
                break;
            }
        }
        match found {
            Some((last, offset, least)) => {
                out.push(((sa[i].0, sa[last].0 + a.every_secs), offset, least));
                i = last + 1;
            }
            None => i += 1,
        }
    }
    out
}

/// A file's samples within `used` (its source spans the timeline shows,
/// s, a sample counting for the 2 s after it): time, embedding, length.
fn picked<'a>(l: &'a Look, used: &[(f64, f64)]) -> Vec<(f64, &'a [i8], f32)> {
    l.samples
        .iter()
        .filter(|(t, _)| used.iter().any(|(s, e)| t + l.every_secs > *s && t < e))
        .map(|(t, e)| (*t, e.as_slice(), (e.iter().map(|v| (*v as i32 * *v as i32) as f32).sum::<f32>()).sqrt().max(1e-6)))
        .collect()
}

/// For each two files that look the same in parts, each source frame of
/// the first in such a part, with the second's uses near the frame it
/// matches there (within a second: the samples are 2 s apart), as one
/// group of sightings, with the match's score.
fn look_groups(by_frame: &HashMap<(u64, i64), Vec<Seen>>, used: &BTreeMap<u64, (f64, Option<Arc<Look>>)>) -> Vec<(Vec<Seen>, f32)> {
    let mut frames: HashMap<u64, BTreeMap<i64, &Vec<Seen>>> = HashMap::new();
    for ((identity, frame), seen) in by_frame {
        frames.entry(*identity).or_default().insert(*frame, seen);
    }
    // The source spans each file shows, s, its frames joined.
    let spans = |identity: u64, fps: f64| -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for f in frames.get(&identity).into_iter().flat_map(|m| m.keys()) {
            let (s, e) = (*f as f64 / fps, (*f + 1) as f64 / fps);
            match out.last_mut() {
                Some(last) if s <= last.1 + 1e-9 => last.1 = e,
                _ => out.push((s, e)),
            }
        }
        out
    };
    let looked: Vec<(u64, f64, &Look)> = used.iter().filter_map(|(id, (fps, look))| look.as_deref().map(|l| (*id, *fps, l))).collect();
    let mut groups = Vec::new();
    for (n, (ia, fa, la)) in looked.iter().enumerate() {
        for (ib, fb, lb) in &looked[n + 1..] {
            for ((from, to), offset, score) in look_matches(la, lb, &spans(*ia, *fa), &spans(*ib, *fb)) {
                let Some(b_frames) = frames.get(ib) else { continue };
                let reach = fb.ceil() as i64;
                let first = (from * fa - 1e-6).ceil() as i64;
                let end = (to * fa - 1e-6).ceil() as i64;
                for (fa_frame, a_seen) in frames.get(ia).into_iter().flat_map(|m| m.range(first..end)) {
                    let centre = ((*fa_frame as f64 / fa + offset) * fb).round() as i64;
                    // Each of the second file's uses once, at its nearest frame.
                    let mut near: Vec<(i64, &Seen)> = Vec::new();
                    for (fb_frame, b_seen) in b_frames.range(centre - reach..=centre + reach) {
                        for s in b_seen.iter() {
                            let d = (fb_frame - centre).abs();
                            match near.iter_mut().find(|(_, n)| n.path == s.path) {
                                Some(slot) if d < slot.0 => *slot = (d, s),
                                Some(_) => {}
                                None => near.push((d, s)),
                            }
                        }
                    }
                    if near.is_empty() {
                        continue;
                    }
                    let mut group: Vec<Seen> = a_seen.to_vec();
                    group.extend(near.into_iter().map(|(_, s)| s.clone()));
                    groups.push((group, score));
                }
            }
        }
    }
    groups
}

#[allow(clippy::too_many_arguments)]
/// Each media item's identity, worked out once a look: a path is
/// canonicalised once, not once a frame.
struct Media<'a> {
    pool: &'a MediaPool,
    /// `None` for an item that isn't footage (a still, no picture).
    known: HashMap<MediaId, Option<(u64, f64, std::sync::Arc<str>)>>,
}

impl Media<'_> {
    /// The item's identity, frame rate and file name.
    fn get(&mut self, id: &MediaId) -> Option<(u64, f64, std::sync::Arc<str>)> {
        let pool = self.pool;
        self.known
            .entry(*id)
            .or_insert_with(|| {
                let item = pool.get(id).filter(|i| !i.is_still && !i.video_codec.is_empty())?;
                let identity = item.fingerprint.unwrap_or_else(|| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone()).hash(&mut h);
                    h.finish()
                });
                Some((identity, item.fps.as_f64(), item.filename.as_str().into()))
            })
            .clone()
    }
}

fn visit(clip: &Clip, elapsed: f64, fps: f64, sequences: &[Sequence], media: &mut Media, path: &mut Vec<ClipId>, track: usize, frame: i64, depth: usize, out: &mut HashMap<(u64, i64), Vec<Seen>>) {
    match &clip.kind {
        ClipKind::Media { media_id } => {
            let Some((identity, media_fps, name)) = media.get(media_id) else { return };
            let source_secs = clip.source_secs_at(elapsed, fps);
            let media_fps = if media_fps > 0.0 { media_fps } else { fps };
            let source_frame = (source_secs * media_fps + 1e-6).floor() as i64;
            out.entry((identity, source_frame)).or_default().push(Seen { path: path.clone(), track, frame, source_secs, name, identity });
        }
        ClipKind::Sequence { sequence_id } | ClipKind::Precomposition { sequence_id } if depth < MAX_DEPTH => {
            let Some(inner) = sequences.iter().find(|s| s.id == *sequence_id) else { return };
            let at = clip.source_secs_at(elapsed, fps);
            for t in inner.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.visible) {
                for c in t.clips.iter().filter(|c| c.enabled && c.timeline_in.to_secs_f64() <= at + 1e-9 && at + 1e-9 < c.timeline_out.to_secs_f64()) {
                    path.push(c.id);
                    visit(c, at - c.timeline_in.to_secs_f64(), fps, sequences, media, path, track, frame, depth + 1, out);
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

/// Each group of sightings that show one picture (one source frame, or
/// frames that look the same, with the score) as each clip's marks and
/// the pairs of uses, of `kind`. Looking the same pairs only uses of
/// different files.
fn report(groups: impl Iterator<Item = (Vec<Seen>, Option<f32>)>, fps: f64, kind: Kind) -> Report {
    // Per use (its path), each shared frame: its own sighting, the first
    // other use there, how many more, and the score.
    let mut hits: BTreeMap<Vec<u128>, Vec<(Seen, Seen, usize, Option<f32>)>> = BTreeMap::new();
    for (seen, score) in groups {
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
            let mut others = uses.iter().filter(|u| u.path != s.path && (kind == Kind::SameFrames || u.identity != s.identity));
            let Some(first) = others.next() else { continue };
            hits.entry(ids(&s.path)).or_default().push((s.clone(), (*first).clone(), others.count(), score));
        }
    }
    let mut out = Report::default();
    let mut paired = std::collections::BTreeSet::new();
    for (key_path, mut list) in hits {
        let path = list[0].0.path.clone();
        list.sort_by_key(|h| h.0.frame);
        list.dedup_by_key(|h| h.0.frame);
        let mut runs: Vec<Vec<(Seen, Seen, usize, Option<f32>)>> = Vec::new();
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
                kind,
                from: own.frame,
                to,
                partner: partner.path[0],
                partner_track: partner.track,
                partner_frame: partner.frame,
                partner_name: partner.name.to_string(),
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
            let source = |v: &mut dyn Iterator<Item = f64>| {
                let s = span(v);
                (s.0, s.1 + 1.0 / fps)
            };
            let (ours_source, theirs_source) = (source(&mut run.iter().map(|h| h.0.source_secs)), source(&mut run.iter().map(|h| h.1.source_secs)));
            let theirs = span(&mut run.iter().map(|h| h.1.frame as f64));
            let side = |s: &Seen, timeline: (f64, f64), source: (f64, f64)| Side {
                clip: s.path[0],
                track: s.track,
                inner: *s.path.last().unwrap_or(&s.path[0]),
                inside: s.path[..s.path.len() - 1].to_vec(),
                name: s.name.to_string(),
                timeline,
                source,
            };
            out.pairs.push(Pair {
                kind,
                a: side(own, (own.frame as f64 / fps, to as f64 / fps), ours_source),
                b: side(partner, (theirs.0 / fps, (theirs.1 + 1.0) / fps), theirs_source),
                secs: (to - own.frame) as f64 / fps,
                score: run.iter().filter_map(|h| h.3).reduce(f32::min),
            });
        }
    }
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
        let (s, mut p) = (seq(tracks), pool(&spec));
        let t = std::time::Instant::now();
        let r = find(&s, &[], &p);
        println!("one hour, 3 tracks, 2160 clips: {:.0} ms, {} pairs", t.elapsed().as_secs_f64() * 1000.0, r.pairs.len());
        // Without fingerprints, the files' paths are what tells them apart.
        let dir = std::env::temp_dir().join(format!("klipp-reuse-measure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, f) in files.iter().enumerate() {
            let path = dir.join(format!("{i}.mov"));
            std::fs::write(&path, b"").unwrap();
            let item = p.get_mut(f).unwrap();
            (item.fingerprint, item.path) = (None, path);
        }
        let t = std::time::Instant::now();
        let paths = find(&s, &[], &p);
        println!("the same without fingerprints: {:.0} ms, {} pairs", t.elapsed().as_secs_f64() * 1000.0, paths.pairs.len());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(paths.pairs.len(), r.pairs.len());
        // Every file looked at: 512 values a sample every 2 s over 2000 s
        // (the files' parts used here), none alike.
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 24) as i8
        };
        let looks = Looks { of: files.iter().map(|f| (*f, Arc::new(Look { every_secs: 2.0, samples: (0..1000).map(|k| (k as f64 * 2.0, (0..512).map(|_| next()).collect())).collect() }))).collect() };
        let t = std::time::Instant::now();
        let alike = find_with(&s, &[], &p, Some(&looks));
        println!("the same, every file looked at: {:.0} ms, {} pairs", t.elapsed().as_secs_f64() * 1000.0, alike.pairs.len());
    }

    /// A sample "k": the unit vector along axis `k` (of 16), with `off`
    /// of axis 15 mixed in (so `off` 30 scores 0.973 against it, 35 0.964).
    fn e(k: usize, off: i8) -> Vec<i8> {
        let mut v = vec![0i8; 16];
        v[k] = 127;
        v[15] = v[15].saturating_add(off);
        v
    }

    /// Samples every 2 s from 0, the `k`th showing `which(k)`.
    fn look(n: usize, which: impl Fn(usize) -> Vec<i8>) -> Arc<Look> {
        Arc::new(Look { every_secs: 2.0, samples: (0..n).map(|k| (k as f64 * 2.0, which(k))).collect() })
    }

    /// Two 20 s files, the second showing the first's pictures 6 s later
    /// (its samples 3.. are the first's 0..), each used once.
    fn two_files(b_sample: impl Fn(usize) -> Vec<i8>) -> (Sequence, MediaPool, Looks, Clip, Clip) {
        let (fa, fb) = (MediaId::new(), MediaId::new());
        let a = Clip::media(fa, tc(0), tc(240), tc(0), tc(240));
        let b = Clip::media(fb, tc(480), tc(720), tc(144), tc(384));
        let looks = Looks { of: HashMap::from([(fa, look(10, |k| e(k, 0))), (fb, look(10, b_sample))]) };
        (seq(vec![vec![a.clone(), b.clone()]]), pool(&[(fa, 1, false), (fb, 2, false)]), looks, a, b)
    }

    fn looks_marks(r: &Report, c: &Clip) -> Vec<(i64, i64, ClipId, i64)> {
        r.marks.get(&c.id).into_iter().flatten().filter(|m| m.kind == Kind::LooksSame).map(|m| (m.from, m.to, m.partner, m.partner_frame)).collect()
    }

    /// The second file's 6–16 s looks like the first's 0–10 s: each clip is
    /// marked over what it shows of that, against the other, and the pair
    /// is "looks the same as" with its score; nothing is "same frames".
    #[test]
    fn two_files_that_look_the_same_at_an_offset_are_marked() {
        let (s, p, looks, a, b) = two_files(|k| if k >= 3 { e(k - 3, 0) } else { e(14, 0) });
        let r = find_with(&s, &[], &p, Some(&looks));
        assert_eq!(looks_marks(&r, &a), [(0, 240, b.id, 480)], "all of A: B shows its 0 s at 20 s");
        assert_eq!(looks_marks(&r, &b), [(480, 720, a.id, 0)]);
        assert_eq!(r.pairs.len(), 1);
        let pair = &r.pairs[0];
        let round = |s: (f64, f64)| ((s.0 * 1e6).round() / 1e6, (s.1 * 1e6).round() / 1e6);
        let (x, y) = if pair.a.clip == a.id { (pair.a.source, pair.b.source) } else { (pair.b.source, pair.a.source) };
        assert_eq!((pair.kind, pair.secs, round(x), round(y)), (Kind::LooksSame, 10.0, (0.0, 10.0), (6.0, 16.0)), "each side its own file's span");
        assert!(pair.score.is_some_and(|v| v > 0.999), "{:?}", pair.score);
        assert!(r.text().contains("looks the same as") && r.looks_unchecked.is_none(), "{}", r.text());
        assert!(r.marks.values().flatten().all(|m| m.kind == Kind::LooksSame));
    }

    /// One sample alike (2 s) is too short to say; at 0.964 they don't look
    /// the same, at 0.973 they do.
    #[test]
    fn a_short_or_weak_likeness_is_not_marked() {
        let (s, p, looks, a, _) = two_files(|k| if k == 5 { e(2, 0) } else { e(14, 0) });
        assert!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).is_empty(), "one sample alike");
        let (s, p, looks, a, _) = two_files(|k| if k >= 3 { e(k - 3, 35) } else { e(14, 0) });
        assert!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).is_empty(), "0.964 alike");
        let (s, p, looks, a, _) = two_files(|k| if k >= 3 { e(k - 3, 30) } else { e(14, 0) });
        assert_eq!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).len(), 1, "0.973 alike");
    }

    /// One strong sample followed by weak ones is too short: the run
    /// stops where the likeness falls under 0.97.
    #[test]
    fn a_run_ends_where_the_likeness_weakens() {
        let (s, p, looks, a, _) = two_files(|k| match k {
            3 => e(0, 0),
            k if k > 3 => e(k - 3, 35),
            _ => e(14, 0),
        });
        assert!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).is_empty());
    }

    /// A run ends where the second file does: its last sample isn't
    /// matched again for the first file's next one, however alike.
    #[test]
    fn a_run_ends_with_the_second_files_samples() {
        let (fa, fb) = (MediaId::new(), MediaId::new());
        let a = Clip::media(fa, tc(0), tc(480), tc(0), tc(480));
        let b = Clip::media(fb, tc(600), tc(744), tc(336), tc(480));
        // The first file holds still from 2 s to 6 s; the second ends
        // with its first 4 s.
        let looks = Looks { of: HashMap::from([(fa, look(10, |k| e(if k == 2 { 1 } else { k }, 0))), (fb, look(10, |k| e(if k >= 8 { k - 8 } else { 14 }, 0)))]) };
        let r = find_with(&seq(vec![vec![a.clone(), b]]), &[], &pool(&[(fa, 1, false), (fb, 2, false)]), Some(&looks));
        assert_eq!(looks_marks(&r, &a).iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(), [(0, 96)]);
    }

    /// Samples of another length (another model's) are never alike, not
    /// compared over the shorter one.
    #[test]
    fn samples_of_another_length_are_not_compared() {
        let (s, p, mut looks, a, b) = two_files(|k| if k >= 3 { e(k - 3, 0) } else { e(14, 0) });
        let longer: Look = Look { every_secs: 2.0, samples: looks.of[&b.media_id().unwrap()].samples.iter().map(|(t, v)| (*t, v.iter().copied().chain([0i8; 16]).collect())).collect() };
        looks.of.insert(b.media_id().unwrap(), Arc::new(longer));
        assert!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).is_empty());
    }

    /// Only what the timeline shows is compared: the second clip moved to
    /// the second file's first 6 s, which look like nothing in the first.
    #[test]
    fn only_the_parts_used_are_compared() {
        let (mut s, p, looks, a, _) = two_files(|k| if k >= 3 { e(k - 3, 0) } else { e(14, 0) });
        s.tracks[0].clips[1] = Clip::media(s.tracks[0].clips[1].media_id().unwrap(), tc(480), tc(624), tc(0), tc(144));
        assert!(looks_marks(&find_with(&s, &[], &p, Some(&looks)), &a).is_empty());
    }

    /// What wasn't compared is said: nothing asked, a file not looked at;
    /// one file alone has nothing to compare with.
    #[test]
    fn what_was_not_compared_is_said() {
        let (s, p, mut looks, a, _) = two_files(|k| e(k, 0));
        assert_eq!(find(&s, &[], &p).looks_unchecked.as_deref(), Some("they weren't asked for"));
        looks.of.retain(|id, _| Some(*id) == a.media_id());
        let r = find_with(&s, &[], &p, Some(&looks));
        assert_eq!(r.looks_unchecked.as_deref(), Some("1 of the 2 files used haven't been looked at for picture search yet"));
        assert!(r.text().ends_with("were not all compared: 1 of the 2 files used haven't been looked at for picture search yet."), "{}", r.text());
        looks.of.clear();
        assert_eq!(find_with(&s, &[], &p, Some(&looks)).looks_unchecked.as_deref(), Some("no file used has been looked at for picture search yet"));
        let one = seq(vec![vec![a.clone()]]);
        assert_eq!(find(&one, &[], &p).looks_unchecked, None);
    }

    /// The first file used twice over the same frames, and the second
    /// looking like it: each use of the first looks the same as the
    /// second, and is the same frames as the other, never "looks the
    /// same" as its own file.
    #[test]
    fn a_file_used_twice_looks_the_same_only_as_the_other_file() {
        let (mut s, p, looks, a, b) = two_files(|k| if k >= 3 { e(k - 3, 0) } else { e(14, 0) });
        let again = Clip::media(a.media_id().unwrap(), tc(800), tc(1040), tc(0), tc(240));
        s.tracks[0].clips.push(again.clone());
        let r = find_with(&s, &[], &p, Some(&looks));
        for c in [&a, &again] {
            let partners: Vec<ClipId> = looks_marks(&r, c).iter().map(|m| m.2).collect();
            assert_eq!(partners, [b.id], "{c:?}");
        }
        assert_eq!(marks(&r, &a).len(), 2, "one span the same frames as the other use, one looking like the second file");
    }

    /// One file under two media items is the same frames, never "looks
    /// the same" as itself.
    #[test]
    fn one_file_never_looks_the_same_as_itself() {
        let (fa, fb) = (MediaId::new(), MediaId::new());
        let a = Clip::media(fa, tc(0), tc(96), tc(0), tc(96));
        let b = Clip::media(fb, tc(100), tc(196), tc(0), tc(96));
        let looks = Looks { of: HashMap::from([(fa, look(10, |k| e(k, 0))), (fb, look(10, |k| e(k, 0)))]) };
        let r = find_with(&seq(vec![vec![a.clone(), b]]), &[], &pool(&[(fa, 7, false), (fb, 7, false)]), Some(&looks));
        assert!(looks_marks(&r, &a).is_empty());
        assert_eq!(r.pairs.iter().map(|p| p.kind).collect::<Vec<_>>(), [Kind::SameFrames]);
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
