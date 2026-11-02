//! Frames proposed for a cover (#3311): the sequence's markers first, a
//! Best Take or a chapter before the rest; then frames with a face, by the
//! face's size, how squarely it faces the camera and the detector's
//! confidence, from the framing analysis; then, for shots with nobody in
//! them, their stillest second. Never two in one shot. Each is then moved
//! to the sharpest frame near it. Whether eyes are open and what a face
//! expresses are not judged: the face model gives five points, which
//! can't tell.

use std::collections::HashMap;

use klipp_core::{MarkerKind, MediaId};
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// The most frames proposed.
pub(crate) const MAX_PROPOSALS: usize = 12;

/// How far either way a proposal is moved for a sharper frame, in seconds.
pub(crate) const SHARPEST_WITHIN: f64 = 0.5;

/// What a file's analysis gives the proposals.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    /// Where each shot after the first starts, seconds of the file.
    pub shot_starts: Vec<f64>,
    /// Sampled frames: seconds of the file, and each face as a box and
    /// landmarks (fractions of the frame) with the detector's confidence.
    pub samples: Vec<(f64, Vec<klipp_analysis::framing::FaceBox>)>,
    /// How much the picture moves over each second of the file.
    pub motion: Vec<Option<f32>>,
}

impl Seen {
    pub(crate) fn of(report: &klipp_analysis::report::MediaAnalysis) -> Seen {
        Seen {
            shot_starts: report.shots.as_ref().map(|s| s.boundaries.clone()).unwrap_or_default(),
            samples: report.framing.as_ref().map(|f| f.samples.iter().map(|s| (s.secs, s.faces.clone())).collect()).unwrap_or_default(),
            motion: report.motion.as_ref().map(|m| m.per_second.clone()).unwrap_or_default(),
        }
    }

    fn shot_of(&self, secs: f64) -> usize {
        self.shot_starts.iter().filter(|b| **b <= secs).count()
    }
}

/// A frame proposed: where, why, how good, and the shot it is from
/// (none for a marker's).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Proposal {
    pub at_secs: f64,
    pub why: String,
    pub score: f32,
    pub shot: Option<(MediaId, usize)>,
    /// The part of the clip's file it may be moved within for a sharper
    /// frame: sequence seconds, inside its clip and shot.
    pub room: (f64, f64),
}

/// How good a face is for a cover, 0 to 1: its confidence, its size (a
/// face a third of the frame's height or more is full marks) and how
/// squarely it faces the camera, from where the nose sits between the
/// eyes.
pub(crate) fn face_score(f: &klipp_analysis::framing::FaceBox) -> f32 {
    let size = ((f.w * f.h).max(0.0).sqrt() / 0.33).min(1.0);
    let frontal = match f.landmarks.as_slice() {
        [right, left, nose, ..] => {
            let half = ((left.0 - right.0).abs() / 2.0).max(1e-4);
            1.0 - ((nose.0 - (left.0 + right.0) / 2.0).abs() / half).min(1.0)
        }
        _ => 0.5,
    };
    f.score.clamp(0.0, 1.0) * size * (0.4 + 0.6 * frontal)
}

/// The sequence second that shows second `src` of `clip`'s file, for a
/// clip played forward at one speed; `None` for any other, or a second
/// outside it.
fn on_timeline(clip: &Clip, src: f64) -> Option<f64> {
    if clip.hold.is_some() || clip.is_reversed() || !clip.speed_keyframes.is_empty() || clip.speed <= 0.0 {
        return None;
    }
    let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    (src >= s_in && src < s_out).then(|| clip.timeline_in.to_secs_f64() + (src - s_in) / clip.speed)
}

/// Whether a picture on a video track above `track` covers `secs`.
fn covered(seq: &Sequence, track: usize, secs: f64) -> bool {
    seq.tracks[..track].iter().filter(|t| t.kind == TrackKind::Video && t.visible).flat_map(|t| &t.clips).any(|c| {
        c.enabled && !matches!(c.kind, ClipKind::Gap | ClipKind::AdjustmentLayer) && c.timeline_in.to_secs_f64() <= secs && secs < c.timeline_out.to_secs_f64()
    })
}

/// The frames proposed for `seq`'s cover, best first, before they are
/// moved for sharpness: at most [`MAX_PROPOSALS`], never two from one
/// shot, nor two within a second of each other.
pub(crate) fn propose(seq: &Sequence, seen: &HashMap<MediaId, Seen>) -> Vec<Proposal> {
    let end = seq.total_duration().to_secs_f64();
    let mut all: Vec<Proposal> = Vec::new();
    for m in &seq.markers {
        let at = m.position.to_secs_f64();
        let rank = match m.kind {
            MarkerKind::BestTake => 3.0,
            MarkerKind::Chapter => 2.0,
            MarkerKind::ToDo | MarkerKind::Review => continue,
            _ => 1.0,
        };
        if at < end {
            let label = if m.label.trim().is_empty() { String::new() } else { format!(" \"{}\"", m.label.trim()) };
            all.push(Proposal { at_secs: at, why: format!("marker{label}"), score: 2.0 + rank * 0.1, shot: None, room: (at, at) });
        }
    }
    for (ti, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible) {
        for clip in track.clips.iter().filter(|c| c.enabled) {
            let ClipKind::Media { media_id } = &clip.kind else { continue };
            let Some(s) = seen.get(media_id) else { continue };
            let room_in = |src: f64| -> (f64, f64) {
                let shot = s.shot_of(src);
                let start = if shot == 0 { 0.0 } else { s.shot_starts[shot - 1] };
                let stop = s.shot_starts.get(shot).copied().unwrap_or(f64::MAX);
                // Only clips played forward at one speed get here.
                let (t_in, t_out, s_in) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64(), clip.source_in.to_secs_f64());
                let at = |src: f64| (t_in + (src - s_in) / clip.speed).clamp(t_in, t_out);
                (at(start), at(stop))
            };
            let mut faced: std::collections::HashSet<usize> = Default::default();
            for (src, faces) in &s.samples {
                let Some(at) = on_timeline(clip, *src) else { continue };
                if covered(seq, ti, at) {
                    continue;
                }
                let Some(best) = faces.iter().map(face_score).max_by(f32::total_cmp).filter(|v| *v > 0.05) else { continue };
                faced.insert(s.shot_of(*src));
                let why = if faces.len() == 1 { "a face".to_string() } else { format!("{} faces", faces.len()) };
                all.push(Proposal { at_secs: at, why, score: best, shot: Some((*media_id, s.shot_of(*src))), room: room_in(*src) });
            }
            // The stillest second of each shot nobody is in.
            let top = s.motion.iter().flatten().copied().fold(0.0_f32, f32::max).max(1e-3);
            let mut stillest: HashMap<usize, (f64, f32)> = HashMap::new();
            for (k, m) in s.motion.iter().enumerate() {
                let (Some(m), src) = (m, k as f64 + 0.5) else { continue };
                let shot = s.shot_of(src);
                if faced.contains(&shot) || on_timeline(clip, src).is_none_or(|at| covered(seq, ti, at)) {
                    continue;
                }
                if stillest.get(&shot).is_none_or(|(_, b)| m < b) {
                    stillest.insert(shot, (src, *m));
                }
            }
            for (shot, (src, m)) in stillest {
                if let Some(at) = on_timeline(clip, src) {
                    all.push(Proposal { at_secs: at, why: "a still moment".into(), score: 0.2 * (1.0 - m / top), shot: Some((*media_id, shot)), room: room_in(src) });
                }
            }
        }
    }
    all.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.at_secs.total_cmp(&b.at_secs)));
    let mut kept: Vec<Proposal> = Vec::new();
    for p in all {
        let same_shot = p.shot.is_some() && kept.iter().any(|k| k.shot == p.shot);
        if kept.len() < MAX_PROPOSALS && !same_shot && !kept.iter().any(|k| (k.at_secs - p.at_secs).abs() < 1.0) {
            kept.push(p);
        }
    }
    kept
}

/// How sharp a picture is: the variance of its luma's Laplacian, over
/// `rgba` `width` × `height`. Higher is sharper; a blurred or flat frame
/// is near 0.
pub(crate) fn sharpness(rgba: &[u8], width: u32, height: u32) -> f32 {
    let (w, h) = (width as usize, height as usize);
    if w < 3 || h < 3 || rgba.len() < w * h * 4 {
        return 0.0;
    }
    let luma = |x: usize, y: usize| {
        let k = (y * w + x) * 4;
        0.2126 * rgba[k] as f32 + 0.7152 * rgba[k + 1] as f32 + 0.0722 * rgba[k + 2] as f32
    };
    let (mut sum, mut sq, mut n) = (0.0_f64, 0.0_f64, 0.0_f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = (luma(x - 1, y) + luma(x + 1, y) + luma(x, y - 1) + luma(x, y + 1) - 4.0 * luma(x, y)) as f64;
            sum += l;
            sq += l * l;
            n += 1.0;
        }
    }
    let mean = sum / n;
    (sq / n - mean * mean) as f32
}

/// The times tried for a sharper frame near `p`: itself, then a quarter
/// and a half second either way, inside its room.
pub(crate) fn tries(p: &Proposal) -> Vec<f64> {
    if p.shot.is_none() {
        return vec![p.at_secs];
    }
    let step = SHARPEST_WITHIN / 2.0;
    [0.0, -step, step, -2.0 * step, 2.0 * step].iter().map(|d| p.at_secs + d).filter(|t| *t >= p.room.0 && *t < p.room.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_analysis::framing::FaceBox;
    use klipp_core::RationalTime;

    fn face(size: f32, nose_off: f32) -> FaceBox {
        FaceBox { x: 0.4, y: 0.2, w: size, h: size, score: 0.9, landmarks: vec![(0.45, 0.3), (0.55, 0.3), (0.5 + nose_off, 0.35), (0.46, 0.4), (0.54, 0.4)] }
    }

    /// A large face looking at the camera beats a small one and one in
    /// profile.
    #[test]
    fn a_large_face_looking_at_the_camera_scores_best() {
        let (big, small, side) = (face_score(&face(0.4, 0.0)), face_score(&face(0.1, 0.0)), face_score(&face(0.4, 0.05)));
        assert!(big > 0.85 && big > small * 2.0 && big > side * 2.0, "big {big}, small {small}, profile {side}");
    }

    /// The proposals: markers first, a Best Take before a plain one, To-Do
    /// notes left out; then faces, best first, one per shot; then a still
    /// moment of a shot nobody is in; nothing under a picture above it,
    /// and file seconds turned into sequence seconds.
    #[test]
    fn markers_then_faces_then_still_moments_one_per_shot() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let media = MediaId::new();
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        // Seconds 10-30 of the file at sequence seconds 0-20.
        seq.tracks[v].clips = vec![Clip::media(media, tc(0.0), tc(20.0), tc(10.0), tc(30.0))];
        let marker = |at: f64, kind: MarkerKind, label: &str| klipp_timeline::Marker { id: klipp_core::MarkerId::new(), position: tc(at), label: label.into(), color: klipp_core::MarkerColor::Red, duration: None, comment: String::new(), kind, review: None };
        seq.markers = vec![marker(3.0, MarkerKind::None, "plain"), marker(15.0, MarkerKind::BestTake, "best"), marker(8.0, MarkerKind::ToDo, "fix")];
        // Shots of the file: 0-20, 20-25, 25-.
        let seen = Seen {
            shot_starts: vec![20.0, 25.0],
            samples: vec![(12.0, vec![face(0.1, 0.0)]), (14.0, vec![face(0.4, 0.0)]), (22.0, vec![face(0.3, 0.0), face(0.2, 0.0)])],
            motion: (0..40).map(|k| Some(if k == 27 { 0.5 } else { 5.0 })).collect(),
        };
        let got = propose(&seq, &HashMap::from([(media, seen.clone())]));
        let said: Vec<(f64, &str)> = got.iter().map(|p| (p.at_secs, p.why.as_str())).collect();
        assert_eq!(said, vec![(15.0, "marker \"best\""), (3.0, "marker \"plain\""), (4.0, "a face"), (12.0, "2 faces"), (17.5, "a still moment")], "{got:?}");
        assert_eq!(got[2].room, (0.0, 10.0), "its shot inside the clip");

        // A picture above covers seconds 3-5: the face there goes, the
        // smaller one in its shot comes instead.
        let mut above = klipp_timeline::Track::new("V2", TrackKind::Video);
        above.clips = vec![Clip::media(MediaId::new(), tc(3.0), tc(5.0), tc(0.0), tc(2.0))];
        seq.tracks.insert(0, above);
        let got = propose(&seq, &HashMap::from([(media, seen)]));
        assert!(got.iter().any(|p| p.at_secs == 2.0 && p.why == "a face") && !got.iter().any(|p| p.at_secs == 4.0), "{got:?}");
    }

    /// A sharp picture measures sharper than the same blurred, and a flat
    /// one near nothing.
    #[test]
    fn a_sharp_frame_measures_sharper_than_a_blurred_one() {
        let (w, h) = (64u32, 36u32);
        let sharp: Vec<u8> = (0..w * h).flat_map(|k| { let v = if (k % w / 4 + k / w / 4) % 2 == 0 { 230 } else { 20 }; [v, v, v, 255] }).collect();
        let blur: Vec<u8> = (0..w * h).flat_map(|k| { let x = (k % w) as f32; let v = (125.0 + 100.0 * (x / 8.0).sin()) as u8; [v, v, v, 255] }).collect();
        let flat = vec![128u8; (w * h * 4) as usize];
        let (s, b, f) = (sharpness(&sharp, w, h), sharpness(&blur, w, h), sharpness(&flat, w, h));
        assert!(s > b * 10.0 && f < 1.0, "sharp {s}, blurred {b}, flat {f}");
    }
}
