//! Whether an 8-bit export will band (#2074): a dark, soft gradient or a
//! glow that spans only a few code values over a wide area shows as steps
//! in an 8-bit file. Checked on a few frames rendered small, before the
//! export and beside it, never in its frame loop.

use crate::snapshot::SequenceSnapshot;
use crate::ExportFrameSource;
use klipp_core::{ExportSettings, Timecode};
use std::sync::atomic::{AtomicBool, Ordering};

/// The suggestion shown where an 8-bit export of the sequence will band.
pub const SUGGESTION: &str = "Smooth gradients here may show steps in an 8-bit file: export the master as 10-bit (HEVC Main10 or ProRes), or turn on the gradient's built-in grain";

/// How wide the sampled frames are rendered.
pub const SAMPLE_WIDTH: u32 = 320;

/// Fewer code values than this across a smooth run bands.
const FEW_LEVELS: u32 = 20;

/// The most one pixel may differ from the last, in codes, within a smooth
/// run.
const SMOOTH_STEP: u8 = 2;

/// A smooth run in an 8-bit frame that bands: at `secs` on the timeline,
/// `levels` code values in its widest channel over half the frame's
/// width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Banding {
    pub secs: f64,
    pub levels: u32,
}

/// The fewest levels per half width a banding row of `rgba` (8-bit,
/// `w`×`h`) shows, when at least a quarter of its rows band: a run longer
/// than half the width where each pixel is within two codes of the last
/// in every channel, not flat (three values or more), whose widest
/// channel changes by fewer than 20 values over half the frame's width.
/// Two, because a limited-range file's step of one luma code is 1.16
/// codes of R'G'B' and rounds to two now and then (#3266). Texture
/// (footage, grain) breaks the runs; a flat colour has no steps to see.
pub fn bands(rgba: &[u8], w: usize, h: usize) -> Option<u32> {
    if w < 4 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let mut banding = 0usize;
    let mut levels = u32::MAX;
    for row in rgba.chunks_exact(w * 4).take(h) {
        let px = |x: usize| &row[x * 4..x * 4 + 3];
        let mut start = 0;
        let mut worst: Option<u32> = None;
        for x in 1..=w {
            let smooth = x < w && px(x).iter().zip(px(x - 1)).all(|(a, b)| a.abs_diff(*b) <= SMOOTH_STEP);
            if smooth {
                continue;
            }
            if x - start > w / 2 {
                let span = (0..3)
                    .map(|c| {
                        let (lo, hi) = (start..x).fold((u8::MAX, 0u8), |(lo, hi), i| (lo.min(px(i)[c]), hi.max(px(i)[c])));
                        u32::from(hi - lo) + 1
                    })
                    .max()
                    .unwrap_or(1);
                // As many levels as half the frame's width holds.
                let half = (span as usize * (w / 2)).div_ceil(x - start) as u32;
                if span >= 3 && half < FEW_LEVELS {
                    worst = Some(worst.map_or(half, |s| s.min(half)));
                }
            }
            start = x;
        }
        if let Some(s) = worst {
            banding += 1;
            levels = levels.min(s);
        }
    }
    (banding * 4 >= h).then_some(levels)
}

/// The first of five frames across `snapshot` (as the pixel check samples
/// them) that bands at 8 bits, rendered as it will export, grain and all,
/// on the CPU [`SAMPLE_WIDTH`] wide, through the plain display encode with
/// no dither. A gradient that keeps its built-in grain doesn't band: the
/// grain breaks its runs, as it does in the file (#2073).
pub fn sample(snapshot: &SequenceSnapshot) -> Option<Banding> {
    sample_unless(snapshot, &AtomicBool::new(false)).ok().flatten()
}

/// The check was called off before it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalledOff;

/// [`sample`], stopping before the next frame once `stop` is set: a
/// render that starts has the machine to itself.
pub fn sample_unless(snapshot: &SequenceSnapshot, stop: &AtomicBool) -> Result<Option<Banding>, CalledOff> {
    let called_off = || stop.load(Ordering::Acquire);
    let (first, last) = (snapshot.start_frame(), snapshot.end_frame());
    if last <= first || snapshot.fps_num == 0 || snapshot.width == 0 {
        return Ok(None);
    }
    if called_off() {
        return Err(CalledOff);
    }
    let plain = snapshot;
    let span = last - 1 - first;
    let w = SAMPLE_WIDTH;
    let h = ((u64::from(w) * u64::from(plain.height) / u64::from(plain.width)) as u32).max(2) & !1;
    let mut r = crate::export_frames::export_renderer(plain, "banding check");
    r.set_gpu_enabled(false);
    let mut source = ExportFrameSource::new(&plain.video.media, plain.fps, plain.working_colorspace);
    let mut frames: Vec<u64> = [0, span / 3, span / 2, span * 2 / 3, span].iter().map(|f| first + f).collect();
    frames.dedup();
    for f in frames {
        if called_off() {
            return Err(CalledOff);
        }
        let Some(value) = i64::try_from(f).ok().and_then(|f| f.checked_mul(i64::from(plain.fps_den))) else { return Ok(None) };
        let time = Timecode { value, rate: plain.fps_num };
        let linear = r.render(&plain.video.sequence, time, w, h, &mut source, klipp_compose::RenderOptions::default());
        if source.end_frame().is_err() {
            return Ok(None);
        }
        let px = klipp_color::display::display_transform_ws_to(&linear, klipp_color::display::ToneMapOp::None, plain.working_colorspace, klipp_core::SdrTransfer::Srgb);
        if let Some(levels) = bands(&px, w as usize, h as usize) {
            return Ok(Some(Banding { secs: time.to_secs_f64(), levels }));
        }
    }
    Ok(None)
}

/// The suggestion for exporting `snapshot` with `settings`, or `None`: a
/// 10-bit target renders nothing and needs none. Changes no setting.
pub fn export_hint(snapshot: &SequenceSnapshot, settings: &ExportSettings) -> Option<&'static str> {
    if !settings.video_codec.writes_8bit(settings.bit_depth) {
        return None;
    }
    sample(snapshot).map(|_| SUGGESTION)
}
