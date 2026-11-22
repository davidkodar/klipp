//! A clip's media for the Veylvik tab: any frame of it, decoded by Klipp
//! when the editor asks (scrubbing, playing, tracking) the way an export
//! decodes it: on the card's own decoder, converted to linear light in
//! the working space there and handed over on the card (scene light for
//! log and HDR footage), the same picture a composition is given in
//! Klipp's renders. The decoder is kept from frame to frame, so playing
//! on reads the next frame without a seek.

use std::sync::{Arc, Mutex};

use klipp_compose::{FrameSource as _, OnDevice, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_render::ExportFrameSource;
use klipp_timeline::Clip;
use veylvik_render::{HostFrame, HostMedia, Result, SourceInfo};

use crate::{fault, identity, mix, working_pixels, Time};

/// How far ahead a frame is asked before a decoder is started there,
/// rather than reading on to it.
const JUMP_SECS: f64 = 2.0;

/// A footage frame on the card and its size, or why there is none.
type Decoded = std::result::Result<(wgpu::Buffer, u32, u32), String>;

/// A frame asked of the decoder's thread, and where to answer.
type Ask = (f64, std::sync::mpsc::Sender<Decoded>);

/// The media of the clip open in the tab.
pub struct EditorFootage {
    working: WorkingColorspace,
    fps: f64,
    info: SourceInfo,
    /// The frames' names: the file and how it is decoded, never another
    /// file's, so one kept is the same picture again.
    salt: u64,
    /// The decoder's own thread: its decoder and converters stay on the
    /// thread that made them, and Veylvik's render workers ask it.
    asks: Mutex<std::sync::mpsc::Sender<Ask>>,
}

impl EditorFootage {
    /// `item` decoded into `working`; `state` names what its frames are
    /// made from (the media's state, as the pool sync gives it).
    pub fn new(item: &klipp_media::MediaItem, working: WorkingColorspace, state: u64, device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let fps = item.fps.as_f64();
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let rate = crate::compose::rate_of(fps);
        let frames = (item.duration.to_secs_f64() * fps).floor() as u32;
        let duration = Time { ticks: i64::from(frames) * i64::from(rate.rate), rate: rate.ticks as u32 };
        let size = [item.resolution.width.max(1), item.resolution.height.max(1)];
        let info = SourceInfo { width: size[0], height: size[1], duration: Some(duration), frame_rate: Some(rate), frames: Some(frames), description: "the clip's media, decoded by Klipp".into() };
        let salt = mix(state ^ mix(working as u64 ^ 0x7ab));
        let id: MediaId = item.id;
        let length = RationalTime::from_secs_f64(item.duration.to_secs_f64(), 24000);
        let zero = RationalTime::from_secs_f64(0.0, 24000);
        let clip = Clip::media(id, zero, length, zero, length);
        let media: Arc<std::collections::HashMap<MediaId, klipp_render::MediaFile>> = Arc::new([(id, klipp_render::MediaFile::of(item))].into_iter().collect());
        let (tx, rx) = std::sync::mpsc::channel::<Ask>();
        let spawned = std::thread::Builder::new().name("klipp-veylvik-tab-decode".into()).spawn(move || {
            let fresh = || ExportFrameSource::shared(Arc::clone(&media), fps, working);
            // Made when the first frame is asked for: footage never asked
            // of (a clip opened and left) compiles no converter for nothing.
            let mut source: Option<ExportFrameSource<'static>> = None;
            let mut last = f64::NEG_INFINITY;
            let duration_secs = f64::from(frames) / fps;
            let upload = |pixels: &[f32]| wgpu::util::DeviceExt::create_buffer_init(
                device.as_ref(),
                &wgpu::util::BufferInitDescriptor { label: Some("veylvik tab footage"), contents: bytemuck::cast_slice(pixels), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC },
            );
            // The decoder's converter may reuse its buffer for the next
            // frame: each frame is copied to one of its own before it is
            // handed over, so reading ahead can't change one Veylvik reads.
            let own = |buffer: &wgpu::Buffer| {
                let copy = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik tab footage"), size: buffer.size(), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik tab footage") });
                encoder.copy_buffer_to_buffer(buffer, 0, &copy, 0, buffer.size());
                queue.submit([encoder.finish()]);
                copy
            };
            let decode = |source: &mut ExportFrameSource<'static>, secs: f64| -> Decoded {
                let frame = source.frame(&clip, secs, size[0], size[1]).ok_or_else(|| format!("Klipp has no frame of the clip at {secs:.3} s"))?;
                let (fw, fh) = (frame.width, frame.height);
                let buffer = match frame.pixels {
                    // Decoded on the card and converted there.
                    SourcePixels::Gpu(g) => match g.on_device() {
                        Some((OnDevice::Linear(buffer), bw, bh)) if (bw, bh) == (fw, fh) => own(buffer),
                        _ => upload(&g.read()),
                    },
                    SourcePixels::SceneReferred { codes, transfer, source_to_working } => upload(&codes.to_scene_linear_rgba(transfer, source_to_working)),
                    SourcePixels::Linear(pixels) => upload(&pixels),
                };
                Ok((buffer, fw, fh))
            };
            let frame_secs = 1.0 / fps;
            // The frame after the one last asked for, read while Veylvik
            // renders that one: playing on, it is ready when asked.
            let mut ahead: Option<(f64, Decoded)> = None;
            // A request that came while reading ahead, taken next.
            let mut pending: Option<Ask> = None;
            // Until the footage is let go (another clip opened, the tab closed).
            loop {
                let (secs, answer) = match pending.take() {
                    Some(ask) => ask,
                    None => match rx.recv() {
                        Ok(ask) => ask,
                        Err(_) => break,
                    },
                };
                let made = match ahead.take() {
                    Some((at, made)) if (at - secs).abs() < 0.5 * frame_secs => made,
                    _ => {
                        // An export's decoder reads on, never back: a scrub
                        // back, or a jump far ahead, moves it to the frame,
                        // the decoder kept (made again, a scrub waited
                        // ~180 ms on a 4K file before decoding anything).
                        match source.as_mut() {
                            None => source = Some(fresh()),
                            Some(s) if secs < last - 0.5 * frame_secs || secs > last + JUMP_SECS => s.seek(&clip, secs),
                            Some(_) => {}
                        }
                        last = secs;
                        decode(source.as_mut().expect("just made"), secs)
                    }
                };
                let _ = answer.send(made);
                // Read on only while nobody is waiting, and inside the media.
                match rx.try_recv() {
                    Ok(ask) => pending = Some(ask),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        let next = secs + frame_secs;
                        if let (true, Some(source)) = (next < duration_secs, source.as_mut()) {
                            last = next;
                            ahead = Some((next, decode(source, next)));
                        }
                    }
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("the Veylvik tab's decoder thread could not start: {e}");
        }
        Self { working, fps, info, salt, asks: Mutex::new(tx) }
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
        // A quarter into the frame: inside it whichever way the decoder rounds.
        let secs = (self.index(time).max(0) as f64 + 0.25) / self.fps;
        let (tx, rx) = std::sync::mpsc::channel();
        self.asks.lock().map_err(|_| fault("the clip's decoder is unavailable"))?.send((secs, tx)).map_err(|_| fault("the clip's decoder has stopped"))?;
        let (buffer, width, height) = rx.recv().map_err(|_| fault("the clip's decoder has stopped"))?.map_err(fault)?;
        Ok(HostFrame { buffer, width, height, pixels: working_pixels(self.working) })
    }
}

/// The clip open in the tab, for the editor's host: its media under
/// [`crate::CLIP_MEDIA_KEY`], whichever clip it is now.
#[derive(Default)]
pub struct TabHost {
    footage: Mutex<Option<Arc<dyn veylvik_render::FrameSource>>>,
    sound: Mutex<Option<Arc<crate::clip_sound::ClipSound>>>,
}

impl TabHost {
    /// The open clip's media from now on; none while no clip is open.
    pub fn set(&self, footage: Option<EditorFootage>) {
        // Lasting: a frame's name is made of the file as it is on disk, how
        // it is decoded and the working space, nothing of this session's,
        // and is no other file's. Veylvik's editor then keeps the pictures
        // it prepares, on disk too, and plays them on time; unnamed, it
        // kept none, and preparing to play made nothing.
        let source = footage.map(|f| Arc::new(veylvik_render::HostSource::new(f, 512 << 20).lasting()) as Arc<dyn veylvik_render::FrameSource>);
        *self.footage.lock().unwrap_or_else(|e| e.into_inner()) = source;
    }

    /// The open clip's sound from now on; none while no clip is open or
    /// it has none.
    pub fn set_sound(&self, sound: Option<crate::clip_sound::ClipSound>) {
        *self.sound.lock().unwrap_or_else(|e| e.into_inner()) = sound.map(Arc::new);
    }
}

impl crate::Host for TabHost {
    fn pick(&self, _request: &crate::Pick) -> std::result::Result<Option<crate::Picked>, String> {
        Ok(None)
    }

    fn media(&self, key: &str) -> Option<Arc<dyn veylvik_render::FrameSource>> {
        (key == crate::CLIP_MEDIA_KEY).then(|| self.footage.lock().unwrap_or_else(|e| e.into_inner()).clone()).flatten()
    }

    fn media_sound(&self, key: &str, window: [f64; 2], rate: u32) -> Option<veylvik_audio::Pcm> {
        if key != crate::CLIP_MEDIA_KEY {
            return None;
        }
        // Mixed off the lock: the editor's mix thread may take a while.
        let sound = self.sound.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        sound.pcm(window, rate)
    }
}
