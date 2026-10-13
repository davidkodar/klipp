//! What a tracked subject matte (#239) must cover: the source times a
//! render of the sequence asks a clip for, and how many of them a matte
//! sequence has a frame for. The export checks this before its first
//! frame, and the External Matte card says it.

use std::path::{Path, PathBuf};

use klipp_nodes::matte_sequence;
use klipp_nodes::NodeKind;
use klipp_timeline::{Clip, GradeAccess, Sequence};

/// The sequence frames a render draws `clip` at: its own span, and the
/// spans of the transitions it is in (whose handles it gives).
fn span(seq: &Sequence, clip: &Clip) -> (klipp_core::Timecode, klipp_core::Timecode) {
    let (mut start, mut end) = (clip.timeline_in, clip.timeline_out);
    for track in &seq.tracks {
        if track.find_clip(clip.id).is_none() {
            continue;
        }
        for tr in track.transitions.iter().filter(|t| t.clip_a_id == clip.id || t.clip_b_id == clip.id) {
            let Some(b) = track.find_clip(tr.clip_b_id) else { continue };
            let cut = b.timeline_in;
            let (s, e) = match tr.alignment {
                klipp_core::TransitionAlignment::Center => {
                    let half = klipp_core::RationalTime { value: tr.duration.value / 2, rate: tr.duration.rate };
                    (cut - half, cut + half)
                }
                klipp_core::TransitionAlignment::StartAtCut => (cut, cut + tr.duration),
                klipp_core::TransitionAlignment::EndAtCut => (cut - tr.duration, cut),
            };
            start = start.min(s);
            end = end.max(e);
        }
    }
    (start, end)
}

/// The source times (seconds) a render of `seq` asks `clip` for: one for
/// every sequence frame it is drawn at, through its speed, ramp and
/// direction, and the frames either side a blend reads, never before 0.
pub fn source_times(seq: &Sequence, clip: &Clip) -> Vec<f64> {
    let fps = seq.fps.as_f64();
    let (start, end) = span(seq, clip);
    let step = klipp_core::RationalTime::from_frames(1, seq.fps.rate());
    let mut out = Vec::new();
    let mut t = start;
    while t < end {
        out.push(crate::render::source_secs(clip, t, fps));
        t += step;
    }
    if let (Some(first), Some(last)) = (out.first().copied(), out.last().copied()) {
        let frame = 1.0 / fps.max(1.0);
        let (lo, hi) = (first.min(last), first.max(last));
        out.push((lo - frame).max(0.0));
        out.push(hi + frame);
    }
    out.retain(|s| *s >= 0.0);
    out
}

/// The matte sequence folders `clip`'s grade reads: its External Mattes
/// whose file is a matte sequence.
pub fn sequences_of(clip: &Clip) -> Vec<PathBuf> {
    clip.grade()
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ExternalMatte && n.enabled)
        .filter_map(|n| n.params.matte_image_path.clone())
        .filter(|p| matte_sequence::is_sequence(p))
        .collect()
}

/// How many of `times` the sequence in `dir` has a frame for (within half
/// a frame), of how many: `None` when `dir` isn't a sequence this build
/// reads.
pub fn coverage(dir: &Path, times: &[f64]) -> Option<(usize, usize)> {
    let manifest = matte_sequence::read_manifest(dir)?;
    let keys = matte_sequence::frames(dir);
    let half = manifest.half_frame_us();
    let covered = times
        .iter()
        .filter(|t| {
            let want = matte_sequence::key_of(**t);
            let at = keys.partition_point(|k| *k < want);
            [at.checked_sub(1), (at < keys.len()).then_some(at)].into_iter().flatten().any(|i| (keys[i] - want).abs() <= half)
        })
        .count();
    Some((covered, times.len()))
}

/// A clip whose tracked matte doesn't cover what the render will ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct Uncovered {
    pub clip: klipp_core::ClipId,
    pub folder: PathBuf,
    pub covered: usize,
    pub needed: usize,
    /// The source range the render reads, seconds.
    pub from_secs: f64,
    pub to_secs: f64,
}

/// Every clip of `seq` graded by a tracked matte that doesn't cover the
/// source times a render asks it for: what an export checks before its
/// first frame.
pub fn uncovered(seq: &Sequence) -> Vec<Uncovered> {
    let mut out = Vec::new();
    for track in &seq.tracks {
        for clip in track.clips.iter().filter(|c| c.enabled) {
            let folders = sequences_of(clip);
            if folders.is_empty() {
                continue;
            }
            let times = source_times(seq, clip);
            let (from_secs, to_secs) = times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
            for folder in folders {
                if let Some((covered, needed)) = coverage(&folder, &times) {
                    if covered < needed {
                        out.push(Uncovered { clip: clip.id, folder, covered, needed, from_secs, to_secs });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{ClipKind, Track, TrackKind};

    fn tc(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 25)
    }

    fn clip(from: f64, to: f64, source_in: f64) -> Clip {
        let mut c = Clip::gap(tc(from), tc(to));
        c.kind = ClipKind::Media { media_id: klipp_core::MediaId::new() };
        c.source_in = tc(source_in);
        c.source_out = tc(source_in + to - from);
        c
    }

    /// The times asked are the clip's frames through its source, plus the
    /// handles a transition reads, plus a frame either side; a sequence
    /// covering some of them says how many.
    #[test]
    fn the_times_a_render_asks_for_and_how_many_are_covered() {
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let (a, b) = (clip(0.0, 2.0, 10.0), clip(2.0, 4.0, 0.0));
        let mut v = Track::new("V1", TrackKind::Video);
        v.transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(0.4),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        v.clips = vec![a.clone(), b.clone()];
        seq.tracks = vec![v];
        let times = source_times(&seq, &a);
        // 2 s of clip plus 0.2 s of handle after its out, at 25 fps, and
        // a frame either side.
        assert_eq!(times.len(), 55 + 2, "{times:?}");
        let (lo, hi) = times.iter().fold((f64::MAX, f64::MIN), |(l, h), t| (l.min(*t), h.max(*t)));
        assert!((lo - 9.96).abs() < 1e-6 && (hi - 12.2).abs() < 1e-6, "{lo} {hi}");

        let dir = std::env::temp_dir().join(format!("klipp-matte-coverage-{}", klipp_core::MediaId::new().0));
        let m = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 2, height: 2, rotation: 0, model: "t".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        matte_sequence::create(&dir, &m).unwrap();
        for f in 249..=280 {
            matte_sequence::put(&dir, &m, f * 40_000, &[255; 4]).unwrap();
        }
        assert_eq!(coverage(&dir, &times), Some((32, 57)), "10.0 s to 11.2 s: frames 250..280, and 9.96");
        let _ = std::fs::remove_dir_all(dir);
    }
}
