//! Measuring clips for a normalize (#1843): each clip's sound over its
//! source range, decoded as the export decodes it, through the BS.1770
//! meter with 4x true peak, and all of them together as one, for a group.
//! The gain each takes is `klipp_core::normalize`'s.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use klipp_core::normalize::Level;

use crate::error::RenderError;

/// The rate clips are measured at.
const RATE: u32 = 48_000;

/// One clip's sound: the file it plays and the part of it.
#[derive(Debug, Clone)]
pub struct ClipSound {
    pub path: PathBuf,
    pub source_in_secs: f64,
    pub source_out_secs: f64,
}

/// The clips of `clip_ids` in `seq` that play sound from a file, each
/// once, with the file and the part of it they play. A clip and its
/// linked partner share their sound settings, so of a pair asked for
/// together only the first is kept. A clip that is no media, or whose
/// media has no sound, is left out, and said in the second list.
pub fn clip_sounds(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, clip_ids: &[klipp_core::ClipId]) -> (Vec<(klipp_core::ClipId, ClipSound)>, Vec<klipp_core::ClipId>) {
    let (mut found, mut skipped) = (Vec::new(), Vec::new());
    for &id in clip_ids {
        let Some(clip) = seq.all_clips().find(|c| c.id == id) else {
            skipped.push(id);
            continue;
        };
        if found.iter().any(|(f, _): &(klipp_core::ClipId, ClipSound)| *f == id || clip.linked_clip_id == Some(*f)) {
            continue;
        }
        let item = match &clip.kind {
            klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).filter(|m| m.has_audio),
            _ => None,
        };
        match item {
            Some(item) => found.push((id, ClipSound { path: item.path.clone(), source_in_secs: clip.source_in.to_secs_f64(), source_out_secs: clip.source_out.to_secs_f64() })),
            None => skipped.push(id),
        }
    }
    (found, skipped)
}

/// What [`measure`] found: each clip's level, in order, and theirs
/// together.
#[derive(Debug, Clone)]
pub struct Measured {
    pub levels: Vec<Level>,
    pub together: Level,
}

fn db(linear: f64) -> f64 {
    if linear > 0.0 { 20.0 * linear.log10() } else { f64::NEG_INFINITY }
}

fn level_of(meter: &klipp_audio::lufs::LufsMeter, sample_peak: f32) -> Level {
    let r = meter.readings();
    let finite = |v: f32| if v.is_finite() && v > -200.0 { v as f64 } else { f64::NEG_INFINITY };
    Level { integrated_lufs: finite(r.integrated), sample_peak_dbfs: db(sample_peak as f64), true_peak_dbtp: db(r.true_peak_linear as f64) }
}

/// Measure `clips`, stopping with [`RenderError::Cancelled`] when
/// `cancel` is set. A clip whose file has no sound measures as silence.
pub fn measure(clips: &[ClipSound], cancel: &AtomicBool) -> Result<Measured, RenderError> {
    let mut all = klipp_audio::lufs::LufsMeter::new(RATE as f32, true);
    let mut all_peak = 0.0f32;
    let mut levels = Vec::with_capacity(clips.len());
    for clip in clips {
        if cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        let mut samples = match crate::audio_mixdown::decode_clip_audio(&clip.path, clip.source_in_secs, Some(clip.source_out_secs), RATE) {
            Ok(s) => s,
            Err(RenderError::NoAudioStream(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let frames = ((clip.source_out_secs - clip.source_in_secs).max(0.0) * RATE as f64).round() as usize;
        samples.truncate(frames * 2);
        let mut meter = klipp_audio::lufs::LufsMeter::new(RATE as f32, true);
        let mut peak = 0.0f32;
        // A second at a time, so a cancel is heard inside a long clip.
        for block in samples.chunks(RATE as usize * 2) {
            if cancel.load(Ordering::Relaxed) {
                return Err(RenderError::Cancelled);
            }
            meter.process(block);
            all.process(block);
            peak = block.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        all_peak = all_peak.max(peak);
        levels.push(level_of(&meter, peak));
    }
    Ok(Measured { levels, together: level_of(&all, all_peak) })
}
