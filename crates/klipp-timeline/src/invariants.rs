//! What every clip holds to, whatever made it: a load, an import or an
//! edit. Times have a rate to count in, a clip ends at or after it starts
//! and reads its file from a time at or after the file's start, its speed
//! is a number in the range a clip plays at, and its fades and levels are
//! numbers. A file can say otherwise, made by hand or damaged; a rate of
//! nought divides every time by nothing.
//!
//! [`problems`] says what a sequence breaks, for the tests that check
//! every edit; [`repair`] puts a clip right, for a load, and says what it
//! changed.

use crate::{Clip, Sequence, SPEED_MAX, SPEED_MIN};
use klipp_core::RationalTime;

/// What `clip` breaks, each in a few words; empty when nothing.
pub fn clip_problems(clip: &Clip) -> Vec<&'static str> {
    let mut out = Vec::new();
    let times = [clip.timeline_in, clip.timeline_out, clip.source_in, clip.source_out];
    if times.iter().any(|t| t.rate == 0) {
        out.push("a time with no rate");
        return out;
    }
    if clip.timeline_out < clip.timeline_in {
        out.push("ends before it starts");
    }
    if clip.source_in.value < 0 {
        out.push("reads from before its file's start");
    }
    if clip.source_out < clip.source_in {
        out.push("its range ends before it starts");
    }
    let speed = clip.speed.abs();
    if !speed.is_finite() || !(SPEED_MIN - 1e-9..=SPEED_MAX + 1e-9).contains(&speed) {
        out.push("a speed out of range");
    }
    if !(clip.fade_in_secs.is_finite() && clip.fade_in_secs >= 0.0 && clip.fade_out_secs.is_finite() && clip.fade_out_secs >= 0.0) {
        out.push("a fade that is no length");
    }
    if !(clip.volume.is_finite() && clip.gain.is_finite()) {
        out.push("a level that is no number");
    }
    out
}

/// Each clip of `seq` that breaks something, named, with what.
pub fn problems(seq: &Sequence) -> Vec<String> {
    seq.all_clips()
        .filter_map(|c| {
            let p = clip_problems(c);
            (!p.is_empty()).then(|| format!("clip {}: {}", c.id.0, p.join(", ")))
        })
        .collect()
}

/// `clip` put right: a time with no rate counts in the clip's other rate
/// (else 24), an end before its start and a range read backwards are
/// turned round, a range from before the file starts at its start, a
/// speed out of range is brought into it (1x when it is no number), and a
/// fade or level that is no number is none or unity. What was changed,
/// empty when nothing.
pub fn repair(clip: &mut Clip) -> Vec<&'static str> {
    let found = clip_problems(clip);
    if found.is_empty() {
        return found;
    }
    let rate = [clip.timeline_in, clip.timeline_out, clip.source_in, clip.source_out].iter().map(|t| t.rate).find(|r| *r > 0).unwrap_or(24);
    for t in [&mut clip.timeline_in, &mut clip.timeline_out, &mut clip.source_in, &mut clip.source_out] {
        if t.rate == 0 {
            *t = RationalTime { value: t.value, rate };
        }
    }
    if clip.timeline_out < clip.timeline_in {
        std::mem::swap(&mut clip.timeline_in, &mut clip.timeline_out);
    }
    if clip.source_in.value < 0 {
        clip.source_in.value = 0;
    }
    if clip.source_out < clip.source_in {
        std::mem::swap(&mut clip.source_in, &mut clip.source_out);
    }
    clip.speed = if clip.speed.is_finite() && clip.speed != 0.0 { clip.speed.signum() * clip.speed.abs().clamp(SPEED_MIN, SPEED_MAX) } else { 1.0 };
    for f in [&mut clip.fade_in_secs, &mut clip.fade_out_secs] {
        if !(f.is_finite() && *f >= 0.0) {
            *f = 0.0;
        }
    }
    for l in [&mut clip.volume, &mut clip.gain] {
        if !l.is_finite() {
            *l = 1.0;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::MediaId;

    fn at(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    #[test]
    fn a_sound_clip_breaks_nothing() {
        let c = Clip::media(MediaId::new(), at(1.0), at(3.0), at(0.5), at(2.5));
        assert!(clip_problems(&c).is_empty());
        let mut back = c.clone();
        back.speed = -2.0;
        assert!(clip_problems(&back).is_empty(), "reversed at 2x is in range");
    }

    /// Each thing a file can get wrong is found and put right, and a clip
    /// put right breaks nothing.
    #[test]
    fn a_broken_clip_is_found_and_put_right() {
        let good = Clip::media(MediaId::new(), at(1.0), at(3.0), at(0.5), at(2.5));
        let cases: Vec<(&str, Box<dyn Fn(&mut Clip)>)> = vec![
            ("a time with no rate", Box::new(|c| c.timeline_out.rate = 0)),
            ("ends before it starts", Box::new(|c| std::mem::swap(&mut c.timeline_in, &mut c.timeline_out))),
            ("reads from before its file's start", Box::new(|c| c.source_in.value = -5)),
            ("its range ends before it starts", Box::new(|c| std::mem::swap(&mut c.source_in, &mut c.source_out))),
            ("a speed out of range", Box::new(|c| c.speed = f64::NAN)),
            ("a speed out of range", Box::new(|c| c.speed = 0.0)),
            ("a speed out of range", Box::new(|c| c.speed = -1000.0)),
            ("a fade that is no length", Box::new(|c| c.fade_out_secs = f32::INFINITY)),
            ("a level that is no number", Box::new(|c| c.volume = f32::NAN)),
        ];
        for (what, spoil) in cases {
            let mut c = good.clone();
            spoil(&mut c);
            assert!(clip_problems(&c).contains(&what), "{what}: {:?}", clip_problems(&c));
            assert!(repair(&mut c).contains(&what));
            assert!(clip_problems(&c).is_empty(), "{what} put right: {:?}", clip_problems(&c));
        }
        let mut c = good.clone();
        c.speed = -1000.0;
        repair(&mut c);
        assert_eq!(c.speed, -SPEED_MAX, "its direction kept");
        let mut c = good.clone();
        assert!(repair(&mut c).is_empty(), "nothing to put right");
        assert_eq!(c.timeline_in, good.timeline_in);
    }
}
