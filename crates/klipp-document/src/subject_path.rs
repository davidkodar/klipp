//! A camera path that keeps a subject in a frame of another aspect
//! (#2211): one subject linked through the framing analysis' samples,
//! and the pan that follows it with a held middle, bounded speed and
//! eased moves.

use crate::ops::PanKey;

/// A detection box: x, y, width, height as fractions of the picture.
pub type Box4 = [f32; 4];

/// How far from the frame's middle, as a share of its width or height,
/// the subject may drift before the picture moves to it again.
const HOLD: f32 = 1.0 / 6.0;

/// The fastest the picture moves, in frame widths (or heights) a second.
const MAX_SPEED: f32 = 0.6;

/// The shortest move, in seconds: a quicker one reads as a jump.
const MIN_MOVE: f64 = 0.5;

/// A jump of the subject's centre further than this, as a fraction of
/// the picture, is a different subject rather than the same one moving.
const SAME_SUBJECT: f32 = 0.3;

/// One subject through `samples` (each a time and the boxes seen then):
/// the largest box first, then the box nearest the one before it, and
/// the largest again when nothing is near. Each is the point to keep in
/// frame: a face's centre, or a person's head, near the top of the box.
pub fn subject_track(samples: &[(f64, Vec<Box4>)], faces: bool) -> Vec<(f64, [f32; 2])> {
    let aim = |b: &Box4| [b[0] + b[2] * 0.5, b[1] + b[3] * if faces { 0.5 } else { 0.25 }];
    let area = |b: &&Box4| b[2] * b[3];
    let mut track: Vec<(f64, [f32; 2])> = Vec::new();
    for (t, boxes) in samples {
        let largest = boxes.iter().max_by(|a, b| area(a).total_cmp(&area(b)));
        let near = track.last().and_then(|(_, p)| {
            let dist = |b: &Box4| { let c = aim(b); ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2)).sqrt() };
            boxes.iter().filter(|b| dist(b) <= SAME_SUBJECT).min_by(|a, b| dist(a).total_cmp(&dist(b)))
        });
        if let Some(b) = near.or(largest) {
            track.push((*t, aim(b)));
        }
    }
    track
}

/// Detections grouped by sample time, in time order.
pub fn by_time(detections: &[(f64, Box4)]) -> Vec<(f64, Vec<Box4>)> {
    let mut sorted: Vec<&(f64, Box4)> = detections.iter().collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, Vec<Box4>)> = Vec::new();
    for (t, b) in sorted {
        match out.last_mut() {
            Some((last, boxes)) if (*last - t).abs() < 1e-6 => boxes.push(*b),
            _ => out.push((*t, vec![*b])),
        }
    }
    out
}

/// The pan keys, over a clip `dur` seconds long, that keep `subject` (in
/// clip seconds, as points of a `frame` the picture is fitted into at
/// `picture` size) in frame at `scale`. The picture holds while the
/// subject stays near the middle, and moves to it, eased and no faster
/// than [`MAX_SPEED`], when it drifts out; it never moves so far that its
/// edge shows. A subject that holds still gives one pose, keyed at both
/// ends.
pub fn camera_keys(subject: &[(f64, [f32; 2])], dur: f64, scale: f32, frame: (f32, f32), picture: (f32, f32)) -> Vec<PanKey> {
    let (w, h) = frame;
    let reach = |pic: f32, dim: f32| ((scale * pic - dim) * 0.5).max(0.0);
    let (rx, ry) = (reach(picture.0, w), reach(picture.1, h));
    // Where the picture sits, in frame pixels from the middle, to put the
    // point in the middle, as far as it can go.
    let centre_on = |p: [f32; 2]| [(-(p[0] - 0.5) * scale * w).clamp(-rx, rx), (-(p[1] - 0.5) * scale * h).clamp(-ry, ry)];
    // And the frame point a position puts in the middle: a key's point.
    let key = |secs: f64, pos: [f32; 2]| PanKey {
        secs,
        scale,
        x: (0.5 - pos[0] / (scale * w)).clamp(0.0, 1.0),
        y: (0.5 - pos[1] / (scale * h)).clamp(0.0, 1.0),
        ease: klipp_core::keyframe::Ease::InOut,
    };
    let dur = dur.max(0.05);
    let first = subject.first().map_or([0.5, 0.5], |s| s.1);
    let mut at = centre_on(first);
    let mut keys = vec![key(0.0, at)];
    for &(t, p) in subject.iter().skip(1) {
        let t = t.clamp(0.0, dur);
        // Where the subject is on screen with the picture where it is.
        let on_screen = [(p[0] - 0.5) * scale * w + at[0], (p[1] - 0.5) * scale * h + at[1]];
        if on_screen[0].abs() <= HOLD * w && on_screen[1].abs() <= HOLD * h {
            continue;
        }
        let to = centre_on(p);
        let travel = ((to[0] - at[0]).abs() / w).max((to[1] - at[1]).abs() / h);
        if travel < 1e-4 {
            continue;
        }
        let move_secs = (travel / MAX_SPEED) as f64;
        let move_secs = move_secs.max(MIN_MOVE);
        let last = keys.last().map_or(0.0, |k| k.secs);
        // Arrive by the sample when there is time to; else as soon as the
        // speed allows.
        let start = (t - move_secs).max(last);
        let end = (start + move_secs).min(dur);
        if end <= last + 1e-3 {
            break;
        }
        if start > last + 1e-3 {
            keys.push(key(start, at));
        }
        at = to;
        keys.push(key(end, at));
    }
    if keys.last().is_some_and(|k| k.secs < dur - 1e-3) {
        keys.push(key(dur, at));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTRAIT: (f32, f32) = (1080.0, 1920.0);
    // A 16:9 picture fitted into it: full width, 607.5 high.
    const WIDE_IN_PORTRAIT: (f32, f32) = (1080.0, 607.5);
    const FILL: f32 = 1920.0 / 607.5;

    #[test]
    fn the_largest_subject_is_picked_and_followed_not_swapped_for_a_bigger_one_passing() {
        let a = [0.20, 0.30, 0.10, 0.20];
        let samples = vec![
            (0.0, vec![a, [0.70, 0.30, 0.05, 0.10]]),
            (2.0, vec![[0.25, 0.30, 0.10, 0.20], [0.60, 0.20, 0.30, 0.40]]),
        ];
        let track = subject_track(&samples, true);
        assert_eq!(track.len(), 2);
        assert!((track[0].1[0] - 0.25).abs() < 1e-6, "the larger face first");
        assert!((track[1].1[0] - 0.30).abs() < 1e-6, "the same face, not the bigger one that came in");
    }

    #[test]
    fn a_person_is_aimed_at_their_head() {
        let track = subject_track(&[(1.0, vec![[0.4, 0.2, 0.2, 0.8]])], false);
        assert!((track[0].1[1] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_still_subject_is_one_pose_keyed_at_both_ends() {
        let s = vec![(0.0, [0.3, 0.5]), (2.0, [0.31, 0.5]), (4.0, [0.29, 0.5])];
        let keys = camera_keys(&s, 5.0, FILL, PORTRAIT, WIDE_IN_PORTRAIT);
        assert_eq!(keys.len(), 2);
        assert_eq!((keys[0].secs, keys[1].secs), (0.0, 5.0));
        assert_eq!((keys[0].x, keys[0].y), (keys[1].x, keys[1].y));
        assert!((keys[0].x - 0.3).abs() < 1e-4, "the subject in the middle: {}", keys[0].x);
        assert!((keys[0].y - 0.5).abs() < 1e-4, "no room to move up or down");
    }

    #[test]
    fn a_subject_crossing_the_frame_is_followed_no_faster_than_the_bound() {
        let s = vec![(0.0, [0.3, 0.5]), (1.0, [0.7, 0.5]), (6.0, [0.7, 0.5])];
        let keys = camera_keys(&s, 6.0, FILL, PORTRAIT, WIDE_IN_PORTRAIT);
        assert!(keys.len() >= 3, "{keys:?}");
        for k in keys.windows(2) {
            let travel = ((k[1].x - k[0].x) * FILL).abs();
            let secs = (k[1].secs - k[0].secs) as f32;
            assert!(secs > 0.0);
            assert!(travel / secs <= MAX_SPEED + 1e-3, "{travel} frame widths in {secs} s");
        }
        assert!((keys.last().unwrap().x - 0.7).abs() < 1e-4);
    }

    #[test]
    fn the_picture_never_moves_past_its_edge() {
        let s = vec![(0.0, [0.02, 0.5]), (3.0, [0.98, 0.5])];
        for k in camera_keys(&s, 3.0, FILL, PORTRAIT, WIDE_IN_PORTRAIT) {
            let pos = (0.5 - k.x) * FILL * PORTRAIT.0;
            assert!(pos.abs() <= (FILL * WIDE_IN_PORTRAIT.0 - PORTRAIT.0) / 2.0 + 0.01, "{k:?}");
        }
    }

    #[test]
    fn samples_group_by_time() {
        let g = by_time(&[(2.0, [0.0; 4]), (0.0, [0.1; 4]), (2.0, [0.2; 4])]);
        assert_eq!(g.iter().map(|(t, b)| (*t, b.len())).collect::<Vec<_>>(), vec![(0.0, 1), (2.0, 2)]);
    }
}
