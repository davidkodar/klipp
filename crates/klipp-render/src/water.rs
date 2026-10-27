//! The water under a reflection (#2795): its colour, measured once from
//! the picture below the reflected layer, not per frame, so playback,
//! seeking and export all draw with the same value.
//!
//! The reflection never sees the picture below it; this renders it. Only
//! the tracks below the reflected clip are drawn, at a few times across
//! the clip, and the water is read from the band just below the
//! waterline, across the middle of the frame. Each channel's median over
//! the pixels between the 5th and 95th percentile of brightness, so sun
//! glints and dark ripple troughs don't pull it. On the GPU stack where
//! there is one, the CPU path otherwise: an offline measurement, a frame
//! each.
//!
//! What it was measured for is a fingerprint ([`footprint_key`]) of the
//! clips under the reflection over its range, its range and its
//! waterline: when that changes, the measurement is outdated, said so,
//! and measured again only on request.

use crate::ExportFrameSource;
use crate::snapshot::SequenceSnapshot;
use klipp_core::{ClipId, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence};

/// The width the picture below is rendered at to be measured.
const SAMPLE_WIDTH: u32 = 320;

/// The band measured: from the waterline down this share of the frame.
const BAND: f32 = 0.15;

/// The water's colour, sRGB, as measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Water {
    pub srgb: [u8; 3],
}

/// The reflection on `clip`, with its node: the first one.
pub fn reflection_of(clip: &Clip) -> Option<(uuid::Uuid, BuiltinEffect)> {
    clip.picture_effects().ok()?.into_iter().find(|e| matches!(e.effect, BuiltinEffect::Reflection { .. })).map(|e| (e.node, e.effect))
}

/// The track `clip` is on, by index.
fn track_of(seq: &Sequence, clip: ClipId) -> Option<usize> {
    seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip))
}

/// A fingerprint of what the water under `clip`'s reflection is measured
/// from (#2795): the clips on the tracks below it overlapping its range
/// (each one's id, media and source range), its own range and its
/// waterline. 24 bits, never 0: it is stored as a whole number in a float
/// parameter, and 0 is "never measured". FNV-1a, the same in every build.
pub fn footprint_key(seq: &Sequence, clip: ClipId) -> Option<u32> {
    let ti = track_of(seq, clip)?;
    let me = seq.tracks[ti].clips.iter().find(|c| c.id == clip)?;
    let (_, reflection) = reflection_of(me)?;
    let BuiltinEffect::Reflection { waterline, gap, .. } = reflection else { return None };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let tc = |t: Timecode| t.to_secs_f64().to_bits().to_le_bytes();
    put(&tc(me.timeline_in));
    put(&tc(me.timeline_out));
    put(&waterline.to_bits().to_le_bytes());
    put(&gap.to_bits().to_le_bytes());
    for track in seq.tracks.iter().skip(ti + 1) {
        for c in track.clips.iter().filter(|c| c.timeline_in < me.timeline_out && me.timeline_in < c.timeline_out) {
            put(c.id.0.as_bytes());
            if let Some(m) = c.media_id() {
                put(m.0.as_bytes());
            }
            put(&tc(c.source_in));
            put(&tc(c.source_out));
            put(&tc(c.timeline_in));
            put(&tc(c.timeline_out));
        }
    }
    let key = ((h ^ (h >> 24) ^ (h >> 48)) & 0x00ff_ffff) as u32;
    Some(key.max(1))
}

/// The water under `clip`'s reflection in `snapshot` (#2795), or `None`
/// when there is no reflection, nothing below it, or nothing could be
/// rendered there.
pub fn measure(snapshot: &SequenceSnapshot, clip: ClipId) -> Option<Water> {
    let seq = &snapshot.video.sequence;
    let ti = track_of(seq, clip)?;
    let me = seq.tracks[ti].clips.iter().find(|c| c.id == clip)?.clone();
    let (_, reflection) = reflection_of(&me)?;
    let BuiltinEffect::Reflection { waterline, .. } = reflection else { return None };
    // Only what is below the reflected clip.
    let mut below = snapshot.clone();
    below.video.sequence.tracks = seq.tracks.iter().skip(ti + 1).cloned().collect();
    if below.video.sequence.tracks.iter().all(|t| t.clips.is_empty()) {
        return None;
    }
    let w = SAMPLE_WIDTH;
    let h = ((u64::from(w) * u64::from(snapshot.height.max(1)) / u64::from(snapshot.width.max(1))) as u32).max(2);
    let (top, bottom) = ((waterline.clamp(0.0, 1.0) * h as f32) as u32, (((waterline + BAND).clamp(0.0, 1.0)) * h as f32) as u32);
    let (left, right) = (w / 5, w - w / 5);
    if bottom <= top {
        return None;
    }
    let mut renderer = crate::export_frames::export_renderer(&below, "water measurement");
    let mut source = ExportFrameSource::new(&below.video.media, below.fps, below.working_colorspace);
    let span = (me.timeline_out - me.timeline_in).to_secs_f64();
    let mut px: Vec<[u8; 3]> = Vec::new();
    for at in [0.1, 0.5, 0.9] {
        let time = Timecode::from_secs_f64(me.timeline_in.to_secs_f64() + span * at, me.timeline_in.rate.max(1));
        // The GPU stack first; the CPU path where there is none.
        renderer.set_gpu_enabled(true);
        let display = match renderer.render_display_u8_on_gpu(&below.video.sequence, time, w, h, &mut source, klipp_compose::RenderOptions::default()) {
            Some((bytes, _)) if bytes.len() >= (w * h * 4) as usize => bytes,
            _ => {
                renderer.set_gpu_enabled(false);
                let linear = renderer.render(&below.video.sequence, time, w, h, &mut source, klipp_compose::RenderOptions::default());
                klipp_color::display::display_transform_ws_to(&linear, klipp_color::display::ToneMapOp::None, below.working_colorspace, klipp_core::SdrTransfer::Srgb)
            }
        };
        for y in top..bottom.min(h) {
            for x in left..right {
                let i = ((y * w + x) * 4) as usize;
                if let Some(p) = display.get(i..i + 3) {
                    px.push([p[0], p[1], p[2]]);
                }
            }
        }
    }
    median_water(&px)
}

/// Each channel's median over `px`, leaving out the brightest and darkest
/// twentieth by brightness (glints, ripple troughs).
pub fn median_water(px: &[[u8; 3]]) -> Option<Water> {
    if px.is_empty() {
        return None;
    }
    let luma = |p: &[u8; 3]| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
    let mut by: Vec<&[u8; 3]> = px.iter().collect();
    by.sort_by(|a, b| luma(a).total_cmp(&luma(b)));
    let (lo, hi) = (by.len() / 20, by.len() - by.len() / 20);
    let kept = &by[lo..hi.max(lo + 1)];
    let median = |c: usize| {
        let mut v: Vec<u8> = kept.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    };
    Some(Water { srgb: [median(0), median(1), median(2)] })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Water of one colour with glints (white) and troughs (black) over a
    /// tenth of it reads as the water's colour.
    #[test]
    fn glints_and_troughs_do_not_pull_the_water() {
        for water in [[200u8, 120, 60], [40, 110, 170], [12, 18, 30]] {
            let mut px = vec![water; 900];
            px.extend(std::iter::repeat_n([255, 255, 255], 45));
            px.extend(std::iter::repeat_n([0, 0, 0], 45));
            assert_eq!(median_water(&px).unwrap().srgb, water);
        }
        assert_eq!(median_water(&[]), None);
    }
}
