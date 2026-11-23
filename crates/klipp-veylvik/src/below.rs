//! A composition drawn over what is below its clip: the tracks under it in
//! the frame Klipp renders, as the footage of [`crate::BELOW_KEY`], with
//! what the composition made laid over them.

use std::sync::{Arc, Mutex};

use klipp_timeline::{Clip, Sequence};
use veylvik_document::{Composition, Node, Operation, Time};
use veylvik_render::{HostFrame, HostMedia, SourceInfo};

use crate::{fault, mix};

/// The composition in `bytes` with the layers below its clip under its
/// picture: a footage node of [`crate::BELOW_KEY`] and its output merged
/// over it. One that reads them already is given back as it is.
pub fn over_below(bytes: &[u8]) -> Result<Arc<[u8]>, String> {
    let mut c: Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    if crate::bytes_read_below(bytes) {
        return Ok(bytes.into());
    }
    let below = Node::new(
        "Layers below",
        Operation::Footage(veylvik_document::nodes::Footage {
            asset: crate::BELOW_KEY.into(),
            timing: Default::default(),
            // Klipp hands it at the render's size; fitted, it fills the
            // frame of a composition at the sequence's size exactly.
            placement: veylvik_document::media::Placement::Fit,
            colour: Default::default(),
            layer: None,
            fallback: String::new(),
        }),
        &[],
    );
    let merge = Node::new("Over the layers below", Operation::Merge(veylvik_document::nodes::Merge {}), &[("background", below.id), ("foreground", c.output)]);
    c.output = merge.id;
    c.nodes.insert(below.id, below);
    c.nodes.insert(merge.id, merge);
    c.validate().map_err(|e| e.to_string())?;
    Ok(serde_json::to_vec(&c).map_err(|e| e.to_string())?.into())
}

/// The frame the Veylvik page's editor is given for what is below the
/// open clip: the sequence drawn as the export draws it, with the clip's
/// track and those above it left out, at the timeline's moment of each
/// composition time (the clip's nearest end in its handles). Rendered on a
/// thread of its own and handed over on the card.
pub struct PageBelow {
    info: SourceInfo,
    size: [u32; 2],
    pixels: veylvik_render::HostPixels,
    salt: u64,
    /// The clip's media frame rate and the sequence's, and the clip: a
    /// composition time to the timeline's frame.
    clip: Clip,
    fps: f64,
    media_fps: f64,
    asks: Mutex<std::sync::mpsc::Sender<Ask>>,
}

/// A timeline moment asked of the render below, and where its frame goes.
type Ask = (f64, std::sync::mpsc::Sender<Result<wgpu::Buffer, String>>);

/// The renders a page's below has made, so each new one is named afresh.
static PAGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl PageBelow {
    /// What is below `clip`, its media `frames` long at `media_fps`,
    /// `snapshot` being its sequence frozen with the clip's track and those
    /// above it empty ([`without_from`]).
    pub fn new(snapshot: klipp_render::SequenceSnapshot, clip: Clip, media_fps: f64, frames: u32, device: Arc<wgpu::Device>) -> Self {
        let media_fps = if media_fps > 0.0 { media_fps } else { 25.0 };
        let size = [snapshot.width.max(1), snapshot.height.max(1)];
        // Timed as the clip's footage: a composition's time is its media's.
        let rate = crate::compose::rate_of(media_fps);
        let duration = Time { ticks: i64::from(frames) * i64::from(rate.rate), rate: rate.ticks as u32 };
        let info = SourceInfo { width: size[0], height: size[1], duration: Some(duration), frame_rate: Some(rate), frames: Some(frames), description: "what is below the clip in Klipp".into() };
        let fps = snapshot.fps;
        let working = snapshot.working_colorspace;
        let pixels = crate::working_pixels(working);
        let salt = mix(PAGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) ^ 0x000b_e103);
        let (tx, rx) = std::sync::mpsc::channel::<Ask>();
        let spawned = std::thread::Builder::new().name("klipp-veylvik-page-below".into()).spawn(move || {
            let timeline = &snapshot.video.sequence;
            let mut renderer = klipp_render::export_frames::export_renderer(&snapshot, "veylvik page below");
            renderer.set_gpu_enabled(true);
            let mut src = klipp_render::ExportFrameSource::new(&snapshot.video.media, snapshot.fps, working);
            let opts = klipp_compose::RenderOptions::default();
            let [w, h] = size;
            while let Ok((secs, reply)) = rx.recv() {
                let time = klipp_core::RationalTime::from_secs_f64(secs.max(0.0), snapshot.fps_num);
                let linear = renderer.render_linear_on_gpu(timeline, time, w, h, &mut src, opts).unwrap_or_else(|| renderer.render(timeline, time, w, h, &mut src, opts));
                let _ = src.end_frame();
                let made = (linear.len() == w as usize * h as usize * 4).then(|| {
                    wgpu::util::DeviceExt::create_buffer_init(device.as_ref(), &wgpu::util::BufferInitDescriptor { label: Some("veylvik page below"), contents: bytemuck::cast_slice(&linear), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC })
                });
                let _ = reply.send(made.ok_or_else(|| format!("Klipp drew no frame below the clip at {secs:.3} s")));
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("the Veylvik page's below could not start its thread: {e}");
        }
        Self { info, size, pixels, salt, clip, fps, media_fps, asks: Mutex::new(tx) }
    }

    /// The timeline frame composition time `time` stands at: as Klipp
    /// shows it, or the clip's nearest end past its used part.
    fn timeline_frame(&self, time: Time) -> i64 {
        let t = crate::clip_time::ClipTime { clip: &self.clip, fps: self.fps, media_fps: self.media_fps };
        if let Some(frame) = t.timeline_frame_at(time) {
            return frame;
        }
        let (start, end) = t.used_frames();
        let at = crate::compose::frame_index(time.seconds().unwrap_or(0.0), self.media_fps).clamp(start, end - 1);
        t.timeline_frame(at).unwrap_or_else(|| (self.clip.timeline_in.to_secs_f64() * self.fps).round() as i64)
    }
}

impl HostMedia for PageBelow {
    fn info(&self) -> SourceInfo {
        self.info.clone()
    }

    fn identity(&self, time: Time) -> u64 {
        mix(self.salt ^ mix(self.timeline_frame(time) as u64))
    }

    fn frame(&self, time: Time, _target: Option<[u32; 2]>) -> veylvik_render::Result<HostFrame> {
        // A quarter into the timeline's frame: inside it whichever way a
        // reader rounds.
        let secs = (self.timeline_frame(time) as f64 + 0.25) / self.fps;
        let (tx, rx) = std::sync::mpsc::channel();
        self.asks.lock().map_err(|_| fault("Klipp's render below the clip is unavailable"))?.send((secs, tx)).map_err(|_| fault("Klipp's render below the clip has stopped"))?;
        let buffer = rx.recv().map_err(|_| fault("Klipp's render below the clip has stopped"))?.map_err(fault)?;
        Ok(HostFrame { buffer, width: self.size[0], height: self.size[1], pixels: self.pixels })
    }
}

/// `seq` with track `track` and every picture track above it emptied, and
/// its subtitle tracks, which are drawn over everything: what is below a
/// clip on `track`. The first track is the top one, as the renders stack
/// them. Its sound tracks stay as they are.
pub fn without_from(seq: &Sequence, track: usize) -> Sequence {
    use klipp_timeline::TrackKind;
    let mut below = seq.clone();
    for (i, t) in below.tracks.iter_mut().enumerate() {
        if t.kind != TrackKind::Audio && (i <= track || t.kind == TrackKind::Subtitle) {
            t.clips.clear();
        }
    }
    below
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_a_clip_is_the_picture_tracks_under_its_own() {
        use klipp_timeline::{Track, TrackKind};
        let clip = |at: f64| {
            let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
            Clip::media(klipp_core::MediaId::new(), t(at), t(at + 1.0), t(0.0), t(1.0))
        };
        let track = |kind, name| {
            let mut t = Track::new(name, kind);
            t.clips = vec![clip(0.0)];
            t
        };
        let mut seq = Sequence::new_default();
        // Top first, as the renders stack them.
        seq.tracks = vec![track(TrackKind::Video, "V3"), track(TrackKind::Video, "V2"), track(TrackKind::Audio, "A1"), track(TrackKind::Subtitle, "S1"), track(TrackKind::Video, "V1")];
        let below = without_from(&seq, 1);
        let kept: Vec<&str> = below.tracks.iter().filter(|t| !t.clips.is_empty()).map(|t| t.name.as_str()).collect();
        assert_eq!(kept, ["A1", "V1"], "its own track, those above and the subtitles left out");
    }

    #[test]
    fn a_composition_over_the_layers_below_reads_them_and_keeps_its_picture_on_top() {
        let c = Composition::from_host_footage(crate::CLIP_MEDIA_KEY, "Clip", None, [64, 36], veylvik_document::Time { ticks: 24, rate: 1 }, 24).unwrap();
        let before = c.output;
        let bytes = serde_json::to_vec(&c).unwrap();
        assert!(!crate::bytes_read_below(&bytes));
        let after = over_below(&bytes).unwrap();
        assert!(crate::bytes_read_below(&after));
        let c: Composition = serde_json::from_slice(&after).unwrap();
        assert_eq!(c.nodes[&c.output].inputs.get("foreground"), Some(&before), "its own picture on top");
        assert_eq!(&over_below(&after).unwrap()[..], &after[..], "given once");
    }
}
