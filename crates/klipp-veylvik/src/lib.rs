//! Veylvik's engine hosted in Klipp: a composition as a clip source on
//! Klipp's GPU device, its footage from Klipp's own decoder.
//!
//! Klipp's frames are linear RGBA f32 with straight alpha in the project's
//! working space; Veylvik's are scene-linear Rec.709, premultiplied. The
//! exchange converts both ways on the GPU through [`HostPixels`], which
//! says how Klipp stores its pixels ([`working_pixels`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub use veylvik_document::{Composition, Time};
pub use veylvik_editor::host::{Host, Pick, Picked};
pub use veylvik_editor::ClipSource;
pub use veylvik_editor;
pub use veylvik_document;
pub use veylvik_render::{FrameSource, HostFrame, HostMedia, HostPixels, HostSource, Renderer, SourceInfo};

use klipp_core::WorkingColorspace;
use klipp_nodes::frame_context::GpuFramePixels;

/// How Klipp stores a frame in `working`: straight alpha, linear light,
/// with the matrix from the working primaries to Rec.709 Klipp itself uses.
pub fn working_pixels(working: WorkingColorspace) -> HostPixels {
    let to_rec709 = match working {
        WorkingColorspace::Rec709Linear => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        WorkingColorspace::AcesCG => klipp_color::primaries::acescg_to_bt709(),
        WorkingColorspace::Bt2020Linear => klipp_color::primaries::bt2020_to_bt709(),
    };
    HostPixels { straight: true, to_rec709 }
}

/// Seconds as Veylvik's exact time, in microseconds: fine enough that two
/// frames of any rate Klipp plays never share one.
pub fn time_from_secs(secs: f64) -> Time {
    Time { ticks: (secs * 1_000_000.0).round() as i64, rate: 1_000_000 }
}

fn fault(message: impl Into<String>) -> veylvik_render::Error {
    veylvik_render::Error::Message(message.into())
}

pub mod clip_time;
pub mod compose;
pub mod editor_footage;
pub use compose::{install, media_changed, preparing, project_assets, project_compositions, revision, set_media_states, set_project_assets, set_project_compositions, Compositor, CompositionSource, Compositions, Declined, CLIP_MEDIA_KEY};

/// Klipp's frames of one clip's media at a time in the media, on the GPU at
/// the size it was made for: the clip's `temporal_frames_on_gpu`.
pub type FrameFetch = Box<dyn FnMut(f64) -> Option<Arc<dyn GpuFramePixels>> + Send>;

/// One clip's media as a Veylvik footage source: frames from Klipp's decoder
/// on the shared device, never copied to the CPU.
///
/// A frame's identity is made of `salt` and the frame's index at the
/// media's rate. The salt names what the frames are of (the media, how
/// Klipp delivers them, the session); Klipp gives a new one when the file
/// is replaced or relinked, a proxy is made or the source grade changes,
/// so Veylvik keeps a picture exactly as long as Klipp's frame under it is
/// the same. Veylvik may keep pictures on disk under it: a salt never
/// comes back to mean other frames in a later session.
pub struct ClipFootage {
    info: SourceInfo,
    fps: f64,
    size: [u32; 2],
    pixels: HostPixels,
    salt: u64,
    fetch: Mutex<FrameFetch>,
}

impl ClipFootage {
    /// `fetch` gives frames `size` pixels in Klipp's working space
    /// (`pixels`); `fps` is the media's frame rate.
    pub fn new(info: SourceInfo, fps: f64, size: [u32; 2], pixels: HostPixels, salt: u64, fetch: FrameFetch) -> Self {
        Self { info, fps: if fps > 0.0 { fps } else { 25.0 }, size, pixels, salt, fetch: Mutex::new(fetch) }
    }

    /// The index of the media frame shown at `secs`.
    fn frame_index(&self, secs: f64) -> i64 {
        (secs * self.fps + 1e-6).floor() as i64
    }
}

/// `salt` and a frame index mixed into one 64-bit name (SplitMix64's
/// finaliser): two salts or two frames meet by chance alone.
fn identity(salt: u64, index: i64) -> u64 {
    mix(salt ^ mix(index as u64))
}

/// SplitMix64's finaliser: every bit of `x` moves every bit out.
pub fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl HostMedia for ClipFootage {
    fn info(&self) -> SourceInfo {
        self.info.clone()
    }

    fn identity(&self, time: Time) -> u64 {
        let secs = time.seconds().unwrap_or(0.0);
        identity(self.salt, self.frame_index(secs))
    }

    fn frame(&self, time: Time, _target: Option<[u32; 2]>) -> veylvik_render::Result<HostFrame> {
        let secs = time.seconds().map_err(|e| fault(format!("a time Klipp cannot read: {e}")))?;
        // The frame's own start, so every time inside a frame asks for the same one.
        let at = self.frame_index(secs) as f64 / self.fps;
        let mut fetch = self.fetch.lock().map_err(|_| fault("Klipp's frames of this media are unavailable"))?;
        let frame = fetch(at).ok_or_else(|| fault(format!("Klipp has no frame of this media at {at:.3} s")))?;
        if frame.srgb8() {
            // A viewer stream's 8-bit frame: Veylvik takes linear floats.
            return Err(fault("Klipp's frame is 8-bit sRGB here; a linear one is needed"));
        }
        Ok(HostFrame { buffer: frame.buffer().clone(), width: self.size[0], height: self.size[1], pixels: self.pixels })
    }
}

/// Klipp as Veylvik's host: the media of the clips whose compositions are
/// open, by key. Klipp picks nothing through Veylvik's own dialogs here.
#[derive(Default)]
pub struct KlippHost {
    media: RwLock<HashMap<String, Arc<dyn FrameSource>>>,
}

impl KlippHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `key` with `source` from now on.
    pub fn insert(&self, key: impl Into<String>, source: Arc<dyn FrameSource>) {
        if let Ok(mut media) = self.media.write() {
            media.insert(key.into(), source);
        }
    }

    pub fn remove(&self, key: &str) {
        if let Ok(mut media) = self.media.write() {
            media.remove(key);
        }
    }
}

impl Host for KlippHost {
    fn pick(&self, _request: &Pick) -> Result<Option<Picked>, String> {
        Ok(None)
    }

    fn media(&self, key: &str) -> Option<Arc<dyn FrameSource>> {
        self.media.read().ok()?.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn klipps_pixels_are_straight_with_klipps_own_matrices() {
        let p = working_pixels(WorkingColorspace::AcesCG);
        assert!(p.straight);
        assert_eq!(p.to_rec709, klipp_color::primaries::acescg_to_bt709());
        assert_eq!(working_pixels(WorkingColorspace::Rec709Linear).to_rec709[1], [0.0, 1.0, 0.0]);
        assert_eq!(working_pixels(WorkingColorspace::Bt2020Linear).to_rec709, klipp_color::primaries::bt2020_to_bt709());
    }

    /// Every time inside a frame names that frame; the next frame and a new
    /// salt name others.
    #[test]
    fn a_frames_identity_is_its_salt_and_index() {
        let footage = |salt| ClipFootage::new(
            SourceInfo { width: 4, height: 4, duration: None, frame_rate: None, frames: None, description: String::new() },
            25.0, [4, 4], working_pixels(WorkingColorspace::Rec709Linear), salt, Box::new(|_| None),
        );
        let a = footage(1);
        let at = |secs: f64| a.identity(time_from_secs(secs));
        assert_eq!(at(0.0), at(0.039));
        assert_ne!(at(0.039), at(0.04));
        assert_eq!(at(1.0), identity(1, 25));
        assert_ne!(identity(1, 25), identity(2, 25));
        assert_ne!(identity(1, 25), identity(1, 26));
        assert_ne!(footage(2).identity(time_from_secs(1.0)), at(1.0));
    }

    #[test]
    fn microsecond_time_keeps_frames_of_any_rate_apart() {
        for fps in [23.976, 24.0, 29.97, 59.94, 120.0, 240.0] {
            let one = 1.0 / fps;
            assert!(time_from_secs(one).ticks > time_from_secs(0.0).ticks);
        }
        assert_eq!(time_from_secs(2.5).seconds().unwrap(), 2.5);
    }

    /// A key Klipp handed out is answered; one it took back is not.
    #[test]
    fn the_host_answers_its_own_keys() {
        struct Nothing;
        impl FrameSource for Nothing {
            fn frame(&self, _: Time) -> veylvik_render::Result<Arc<veylvik_render::Image>> {
                Err(fault("none"))
            }
            fn byte_budget(&self) -> u64 {
                0
            }
        }
        let host = KlippHost::new();
        host.insert("clip-a", Arc::new(Nothing));
        assert!(host.media("clip-a").is_some());
        assert!(host.media("clip-b").is_none());
        host.remove("clip-a");
        assert!(host.media("clip-a").is_none());
    }
}
