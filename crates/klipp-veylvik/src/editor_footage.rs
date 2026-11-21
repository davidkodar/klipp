//! A clip's media for the Veylvik tab: any frame of it, decoded by Klipp
//! from its file when the editor asks (scrubbing, playing, tracking), as
//! linear light in the working space, scene light for log and HDR footage:
//! the same picture a composition is given in Klipp's renders. It goes up
//! to the shared GPU device and Veylvik takes it from there.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use klipp_core::{LogFormat, WorkingColorspace};
use klipp_media::frame_reader::SeekableFrameReader;
use veylvik_render::{HostFrame, HostMedia, Result, SourceInfo};

use crate::{fault, identity, mix, working_pixels, Time};

/// The media of the clip open in the tab.
pub struct EditorFootage {
    path: PathBuf,
    log_format: Option<LogFormat>,
    working: WorkingColorspace,
    fps: f64,
    info: SourceInfo,
    /// The frames' names: the file and how it is decoded, never another
    /// file's, so one kept is the same picture again.
    salt: u64,
    device: Arc<wgpu::Device>,
    reader: Mutex<Option<SeekableFrameReader<Vec<f32>>>>,
}

impl EditorFootage {
    /// `path` at `fps`, `size` pixels and `frames` long, decoded as Klipp
    /// decodes it (`log_format`, into `working`); `state` names what its
    /// frames are made from (the media's state, as the pool sync gives it).
    #[allow(clippy::too_many_arguments)]
    pub fn new(path: PathBuf, log_format: Option<LogFormat>, working: WorkingColorspace, fps: f64, size: [u32; 2], frames: Option<u32>, state: u64, device: Arc<wgpu::Device>) -> Self {
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let rate = crate::compose::rate_of(fps);
        let duration = frames.map(|n| Time { ticks: i64::from(n) * i64::from(rate.rate), rate: rate.ticks as u32 });
        let info = SourceInfo { width: size[0], height: size[1], duration, frame_rate: Some(rate), frames, description: "the clip's media, decoded by Klipp".into() };
        let salt = mix(state ^ mix(working as u64 ^ 0x7ab));
        Self { path, log_format, working, fps, info, salt, device, reader: Mutex::new(None) }
    }

    fn index(&self, time: Time) -> i64 {
        (time.seconds().unwrap_or(0.0) * self.fps + 1e-6).floor() as i64
    }
}

impl HostMedia for EditorFootage {
    fn info(&self) -> SourceInfo {
        self.info.clone()
    }

    fn identity(&self, time: Time) -> u64 {
        identity(self.salt, self.index(time))
    }

    fn frame(&self, time: Time, _target: Option<[u32; 2]>) -> Result<HostFrame> {
        // A quarter into the frame: inside it whichever way the reader rounds.
        let secs = (self.index(time).max(0) as f64 + 0.25) / self.fps;
        let mut reader = self.reader.lock().map_err(|_| fault("the clip's reader is unavailable"))?;
        if reader.is_none() {
            *reader = SeekableFrameReader::open_linear(&self.path, self.log_format, self.working);
        }
        let read = reader.as_mut().ok_or_else(|| fault(format!("Klipp could not open {}", self.path.display())))?;
        let (pixels, w, h) = read.decode_at(secs).ok_or_else(|| fault(format!("Klipp has no frame of the clip at {secs:.3} s")))?;
        if pixels.len() != w as usize * h as usize * 4 {
            return Err(fault(format!("a frame of {} values for {w}×{h}", pixels.len())));
        }
        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            self.device.as_ref(),
            &wgpu::util::BufferInitDescriptor { label: Some("veylvik tab footage"), contents: bytemuck::cast_slice(&pixels), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC },
        );
        Ok(HostFrame { buffer, width: w, height: h, pixels: working_pixels(self.working) })
    }
}

/// The clip open in the tab, for the editor's host: its media under
/// [`crate::CLIP_MEDIA_KEY`], whichever clip it is now.
#[derive(Default)]
pub struct TabHost {
    footage: Mutex<Option<Arc<dyn veylvik_render::FrameSource>>>,
}

impl TabHost {
    /// The open clip's media from now on; none while no clip is open.
    pub fn set(&self, footage: Option<EditorFootage>) {
        let source = footage.map(|f| Arc::new(veylvik_render::HostSource::new(f, 512 << 20)) as Arc<dyn veylvik_render::FrameSource>);
        *self.footage.lock().unwrap_or_else(|e| e.into_inner()) = source;
    }
}

impl crate::Host for TabHost {
    fn pick(&self, _request: &crate::Pick) -> std::result::Result<Option<crate::Picked>, String> {
        Ok(None)
    }

    fn media(&self, key: &str) -> Option<Arc<dyn veylvik_render::FrameSource>> {
        (key == crate::CLIP_MEDIA_KEY).then(|| self.footage.lock().unwrap_or_else(|e| e.into_inner()).clone()).flatten()
    }
}
