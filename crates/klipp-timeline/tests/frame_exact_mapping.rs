//! #691: a moment of a file maps onto the timeline frame that shows it,
//! exactly, at the fractional rates and far into a long timeline, through
//! a clip's trim, speed and direction; and a timeline frame maps to the
//! source and back to itself.

use klipp_core::{Fps, MediaId, Timecode};
use klipp_timeline::Clip;

/// Frame `f` at `fps`, as the timeline counts ticks.
fn at(f: i64, fps: Fps) -> Timecode {
    Timecode::from_frames(f * fps.den as i64, fps.num)
}

const RATES: [(u32, u32); 3] = [(24000, 1001), (30000, 1001), (60000, 1001)];
/// 0, one hour and ten hours in, as a frame count at each rate.
fn starts(fps: Fps) -> [i64; 3] {
    let per_hour = (3600.0 * fps.as_f64()).round() as i64;
    [0, per_hour, 10 * per_hour]
}

/// A clip of `len` frames at `t0` on the timeline, showing the file from
/// frame `s0`, at `speed`.
fn clip(fps: Fps, t0: i64, s0: i64, len: i64, speed: f64) -> Clip {
    let source_len = (len as f64 * speed.abs()).round() as i64;
    let mut c = Clip::media(MediaId::new(), at(t0, fps), at(t0 + len, fps), at(s0, fps), at(s0 + source_len, fps));
    c.speed = speed;
    c
}

/// The timeline frame a source second lands on, as the callers compute
/// it (scene cuts, cut points): the clip's start plus the elapsed time,
/// in frames, rounded.
fn frame_for_source(c: &Clip, fps: Fps, source_secs: f64) -> i64 {
    ((c.timeline_in.to_secs_f64() + c.elapsed_for_source_secs(source_secs, fps.as_f64())) * fps.as_f64()).round() as i64
}

/// The same through `sequence_time_for_source_time` (cut preparation).
fn frame_via_timecode(c: &Clip, fps: Fps, source_secs: f64) -> Option<i64> {
    let source = Timecode::from_secs_f64(source_secs, c.source_in.rate);
    c.sequence_time_for_source_time(source, fps.as_f64()).map(|t| (t.to_secs_f64() * fps.as_f64()).round() as i64)
}

/// The source frame shown at timeline frame `f`, as the decoder is asked
/// for it.
fn source_frame_at(c: &Clip, fps: Fps, f: i64) -> i64 {
    let elapsed = (at(f, fps) - c.timeline_in).to_secs_f64();
    (c.source_secs_at(elapsed, fps.as_f64()) * fps.as_f64()).round() as i64
}

#[test]
fn a_source_frame_lands_on_its_timeline_frame_at_1x() {
    for (num, den) in RATES {
        let fps = Fps::new(num, den);
        for t0 in starts(fps) {
            let s0 = 12_345;
            let c = clip(fps, t0, s0, 2_000, 1.0);
            for k in (1..2_000).step_by(7) {
                let secs = (s0 + k) as f64 / fps.as_f64();
                assert_eq!(frame_for_source(&c, fps, secs), t0 + k, "{num}/{den} at frame {t0}: source frame {}", s0 + k);
                assert_eq!(frame_via_timecode(&c, fps, secs), Some(t0 + k), "{num}/{den} at frame {t0} via timecode: source frame {}", s0 + k);
                assert_eq!(source_frame_at(&c, fps, t0 + k), s0 + k, "{num}/{den} at frame {t0}: timeline frame {} shows", t0 + k);
            }
        }
    }
}

#[test]
fn double_speed_maps_every_other_source_frame_onto_a_frame() {
    for (num, den) in RATES {
        let fps = Fps::new(num, den);
        for t0 in starts(fps) {
            let s0 = 5_000;
            let c = clip(fps, t0, s0, 1_000, 2.0);
            for k in (1..1_000).step_by(11) {
                let even = (s0 + 2 * k) as f64 / fps.as_f64();
                assert_eq!(frame_for_source(&c, fps, even), t0 + k, "{num}/{den} at {t0}: source frame {}", s0 + 2 * k);
                assert_eq!(source_frame_at(&c, fps, t0 + k), s0 + 2 * k, "{num}/{den} at {t0}: timeline frame {}", t0 + k);
            }
        }
    }
}

#[test]
fn reversed_clips_map_both_ways() {
    for (num, den) in RATES {
        let fps = Fps::new(num, den);
        for t0 in starts(fps) {
            let (s0, len) = (9_000, 1_500);
            let c = clip(fps, t0, s0, len, -1.0);
            // The first frame shows the last of the range, the last frame
            // the first of it.
            assert_eq!(source_frame_at(&c, fps, t0), s0 + len - 1, "{num}/{den} at {t0}: first frame");
            assert_eq!(source_frame_at(&c, fps, t0 + len - 1), s0, "{num}/{den} at {t0}: last frame");
            for k in (0..len).step_by(13) {
                let shown = source_frame_at(&c, fps, t0 + k);
                let back = frame_for_source(&c, fps, shown as f64 / fps.as_f64());
                assert_eq!(back, t0 + k, "{num}/{den} at {t0}: frame {} shows source {shown}, which maps back to {back}", t0 + k);
            }
        }
    }
}

#[test]
fn a_timeline_frame_round_trips_through_the_source() {
    for (num, den) in RATES {
        let fps = Fps::new(num, den);
        for t0 in starts(fps) {
            for speed in [1.0, 2.0, 0.5, -1.0, -2.0] {
                let c = clip(fps, t0, 777, 1_200, speed);
                for k in (0..1_200).step_by(17) {
                    let f = t0 + k;
                    let elapsed = (at(f, fps) - c.timeline_in).to_secs_f64();
                    let source = c.source_secs_at(elapsed, fps.as_f64());
                    let back = frame_for_source(&c, fps, source);
                    assert_eq!(back, f, "{num}/{den} at {t0}, speed {speed}: frame {f} -> source {source} -> frame {back}");
                }
            }
        }
    }
}

/// The source frame (at the file's `media_fps`) timeline frame `f` shows,
/// by the decoders' rule: the frame nearest the source time it asks for
/// (export's `ClipStream::seek_forward` takes the last frame at or before
/// that time plus half a source frame).
fn shown(c: &Clip, fps: Fps, media_fps: f64, f: i64) -> i64 {
    let elapsed = (at(f, fps) - c.timeline_in).to_secs_f64();
    let asked = c.source_secs_at(elapsed, fps.as_f64());
    // The last of the file's frames stamped at or before the time asked
    // for plus half a frame.
    let mut j = (asked * media_fps).floor() as i64 - 1;
    while (j + 1) as f64 / media_fps <= asked + 0.5 / media_fps {
        j += 1;
    }
    j
}

/// Where a cut in the file at source frame `j` belongs on the timeline:
/// the first timeline frame that shows `j` or later.
fn first_showing(c: &Clip, fps: Fps, media_fps: f64, j: i64) -> i64 {
    let (t0, t1) = ((c.timeline_in.to_secs_f64() * fps.as_f64()).round() as i64, (c.timeline_out.to_secs_f64() * fps.as_f64()).round() as i64);
    if c.is_reversed() {
        (t0..t1).find(|&f| shown(c, fps, media_fps, f) < j).unwrap_or(t1)
    } else {
        (t0..t1).find(|&f| shown(c, fps, media_fps, f) >= j).unwrap_or(t1)
    }
}

#[test]
fn a_cut_in_the_file_lands_on_the_first_frame_that_shows_it() {
    let mut wrong = Vec::new();
    for (num, den) in [(24000u32, 1001u32), (30000, 1001), (25, 1)] {
        let fps = Fps::new(num, den);
        for media_fps in [25.0, 24000.0 / 1001.0, 30000.0 / 1001.0] {
            for speed in [1.0, 2.0, 0.5, -1.0, -2.0] {
                for t0 in starts(fps) {
                    let len = 600;
                    let source_in_secs = 400.0 / media_fps;
                    let mut c = clip(fps, t0, 0, len, speed);
                    let span = len as f64 / fps.as_f64() * speed.abs();
                    c.source_in = Timecode::from_secs_f64(source_in_secs, c.source_in.rate);
                    c.source_out = Timecode::from_secs_f64(source_in_secs + span, c.source_in.rate);
                    let j0 = (source_in_secs * media_fps).round() as i64;
                    let jn = ((source_in_secs + span) * media_fps).floor() as i64;
                    for j in (j0 + 1)..jn {
                        let truth = first_showing(&c, fps, media_fps, j);
                        let mapped = c.first_frame_showing(j as f64 / media_fps, media_fps, fps.as_f64()).unwrap_or(-1);
                        // A change no frame of the clip shows has nowhere
                        // to go: None, and the truth is the clip's end.
                        let truth_end = (c.timeline_out.to_secs_f64() * fps.as_f64()).round() as i64;
                        if mapped != truth && !(mapped == -1 && truth == truth_end) {
                            wrong.push(format!("seq {num}/{den} media {media_fps:.3} speed {speed} at {t0}: source frame {j} mapped to {mapped}, first shown at {truth}"));
                        }
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{} wrong of the cuts tried, e.g.:\n{}", wrong.len(), wrong.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}

/// Through a speed ramp, too: the frame is found on the forward map the
/// renderer evaluates, not on an integrated inverse.
#[test]
fn a_cut_through_a_speed_ramp_lands_on_the_first_frame_that_shows_it() {
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    for (num, den) in RATES {
        let fps = Fps::new(num, den);
        for t0 in starts(fps) {
            let media_fps = 25.0;
            let mut c = clip(fps, t0, 0, 480, 1.0);
            c.source_in = Timecode::from_secs_f64(2.0, c.source_in.rate);
            c.source_out = Timecode::from_secs_f64(40.0, c.source_in.rate);
            c.speed_keyframes = vec![key(0.0, 0.5), key(20.0 / fps.as_f64() * 24.0, 2.5)];
            for j in (51..600).step_by(7) {
                let truth = first_showing(&c, fps, media_fps, j);
                let end = (c.timeline_out.to_secs_f64() * fps.as_f64()).round() as i64;
                let mapped = c.first_frame_showing(j as f64 / media_fps, media_fps, fps.as_f64());
                assert!(mapped == Some(truth) || (mapped.is_none() && truth == end), "{num}/{den} at {t0}: source frame {j} mapped to {mapped:?}, first shown at {truth}");
            }
        }
    }
}
