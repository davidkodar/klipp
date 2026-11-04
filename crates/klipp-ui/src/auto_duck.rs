//! Where a voice speaks under a clip, for Auto Duck: the stretches of the
//! sidechain track's sound loud enough to duck the clip under, measured
//! against the voice's own level.

/// One clip of the sidechain track: where it sits on the timeline, where
/// it starts in its file, and its sound when cached (interleaved samples,
/// rate, channels).
pub struct VoiceClip<'a> {
    pub timeline_in: f64,
    pub timeline_out: f64,
    pub source_in: f64,
    pub sound: Option<(&'a [f32], u32, u16)>,
}

/// The length of a window the level is read over, seconds.
const WINDOW_SECS: f64 = 0.05;
/// Stretches closer than this are one.
const MERGE_GAP_SECS: f64 = 0.05;

/// Where `voice` speaks between `from` and `to` (timeline seconds): each
/// 50 ms window whose level is above `threshold` times the voice's own
/// level (the louder quarter of its windows), touching stretches joined.
/// A clip whose sound is not cached counts as speaking throughout.
///
/// The threshold was a fixed 0.1 RMS, about -20 dBFS: a voice recorded
/// at -23 LUFS never reached it and nothing was ducked. And a stretch
/// touching the one before it, or a clip outside the range, ended the
/// whole search, so two clips meeting at a cut gave no stretch at all.
pub fn speech_regions(voice: &[VoiceClip], from: f64, to: f64, threshold: f32) -> Vec<(f64, f64)> {
    let mut windows: Vec<(f64, f32)> = Vec::new();
    let mut regions: Vec<(f64, f64)> = Vec::new();
    for clip in voice {
        let (start, end) = (clip.timeline_in.max(from), clip.timeline_out.min(to));
        if end <= start {
            continue;
        }
        let Some((samples, rate, channels)) = clip.sound.filter(|(s, r, c)| !s.is_empty() && *r > 0 && *c > 0) else {
            regions.push((start, end));
            continue;
        };
        let channels = channels as usize;
        let window = ((rate as f64 * WINDOW_SECS) as usize).max(1) * channels;
        let frames = samples.len() / channels;
        let mut t = start;
        while t < end {
            let source = clip.source_in + (t - clip.timeline_in);
            let at = (source * rate as f64) as usize;
            if source >= 0.0 && at < frames {
                let slice = &samples[at * channels..(at * channels + window).min(samples.len())];
                let rms = (slice.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / slice.len().max(1) as f64).sqrt() as f32;
                windows.push((t, rms));
            }
            t += WINDOW_SECS;
        }
    }
    // The voice's level: its louder quarter, so pauses and room tone do
    // not pull it down.
    let mut levels: Vec<f32> = windows.iter().map(|(_, r)| *r).collect();
    levels.sort_by(|a, b| a.total_cmp(b));
    let level = levels.get(levels.len() * 3 / 4).copied().unwrap_or(0.0);
    if level > 0.0 {
        let floor = threshold * level;
        let mut open: Option<f64> = None;
        let mut last = f64::NEG_INFINITY;
        for &(t, rms) in &windows {
            // A window after a gap (a cut, another clip) closes the stretch.
            if open.is_some() && t - last > WINDOW_SECS * 1.5 {
                regions.push((open.take().unwrap_or(t), last + WINDOW_SECS));
            }
            if rms > floor {
                open.get_or_insert(t);
            } else if let Some(s) = open.take() {
                regions.push((s, t));
            }
            last = t;
        }
        if let Some(s) = open {
            regions.push((s, (last + WINDOW_SECS).min(to)));
        }
    }
    regions.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for r in regions {
        match merged.last_mut() {
            Some(last) if r.0 <= last.1 + MERGE_GAP_SECS => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of a 220 Hz tone at `amplitude`, with the stretches in
    /// `quiet` (seconds) silent, at 8 kHz mono.
    fn talk(secs: f64, amplitude: f32, quiet: &[(f64, f64)]) -> Vec<f32> {
        (0..(secs * 8000.0) as usize)
            .map(|i| {
                let t = i as f64 / 8000.0;
                if quiet.iter().any(|(a, b)| t >= *a && t < *b) { 0.0 } else { amplitude * (2.0 * std::f64::consts::PI * 220.0 * t).sin() as f32 }
            })
            .collect()
    }

    /// Two clips of a quiet voice (a peak of 0.07, about -26 dBFS RMS, the
    /// level of a -23 LUFS talking head) meeting at a cut, a pause in the
    /// second: the voice is found where it speaks, on both sides of the
    /// cut, and not in the pause. With the fixed 0.1 RMS it was never
    /// loud enough, and the cut ended the search.
    #[test]
    fn a_quiet_voice_across_a_cut_is_found() {
        let first = talk(4.0, 0.07, &[]);
        let second = talk(4.0, 0.07, &[(1.5, 2.5)]);
        let voice = [
            VoiceClip { timeline_in: 0.0, timeline_out: 4.0, source_in: 0.0, sound: Some((&first, 8000, 1)) },
            VoiceClip { timeline_in: 4.0, timeline_out: 8.0, source_in: 0.0, sound: Some((&second, 8000, 1)) },
        ];
        let regions = speech_regions(&voice, 0.0, 8.0, 0.1);
        assert_eq!(regions.len(), 2, "speech, the pause, speech: {regions:?}");
        assert!(regions[0].0 < 0.06 && (regions[0].1 - 5.5).abs() < 0.1, "across the cut to the pause: {regions:?}");
        assert!((regions[1].0 - 6.5).abs() < 0.1 && (regions[1].1 - 8.0).abs() < 0.06, "after the pause: {regions:?}");
    }

    /// A clip outside the range is passed over, and one with no cached
    /// sound counts as speaking where it is.
    #[test]
    fn clips_outside_or_uncached_do_not_end_the_search() {
        let sound = talk(2.0, 0.5, &[]);
        let voice = [
            VoiceClip { timeline_in: 20.0, timeline_out: 22.0, source_in: 0.0, sound: Some((&sound, 8000, 1)) },
            VoiceClip { timeline_in: 0.0, timeline_out: 2.0, source_in: 0.0, sound: Some((&sound, 8000, 1)) },
            VoiceClip { timeline_in: 5.0, timeline_out: 6.0, source_in: 0.0, sound: None },
        ];
        let regions = speech_regions(&voice, 0.0, 10.0, 0.1);
        assert_eq!(regions.len(), 2, "{regions:?}");
        assert!((regions[1].0 - 5.0).abs() < 1e-9 && (regions[1].1 - 6.0).abs() < 1e-9, "{regions:?}");
    }
}
