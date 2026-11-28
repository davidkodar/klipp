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

/// Frames a decoder reads ahead of the last one asked of it.
const AHEAD: usize = 3;

/// Where one of the footage's decoders is, shared by its thread and the
/// thread that hands it frames to read: the last frame it decoded and the
/// last asked of it, and the frames it has read ahead.
#[derive(Default)]
struct Place {
    last: f64,
    asked: f64,
    ahead: Vec<f64>,
}

/// One of the footage's decoders, on a thread of its own, reading on from
/// where it was last asked and ahead of it while nobody asks.
struct Cursor {
    asks: std::sync::mpsc::Sender<Ask>,
    place: Arc<Mutex<Place>>,
    /// When it was last asked, to give the least used up.
    used: u64,
}

/// The decoder for a frame at `secs`: one that has it read ahead or reads
/// on to it within reach, else a new one while there are fewer than two,
/// else the one asked least lately.
fn pick(cursors: &mut Vec<Cursor>, secs: f64, frame_secs: f64, fresh: &dyn Fn() -> Option<Cursor>) -> Option<usize> {
    let reach = |c: &Cursor| {
        let place = c.place.lock().unwrap_or_else(|e| e.into_inner());
        let ahead = place.ahead.iter().any(|t| (t - secs).abs() < 0.5 * frame_secs);
        let on = secs >= place.last - 0.5 * frame_secs && secs <= place.last + JUMP_SECS;
        (ahead || on).then_some(if ahead { 0.0 } else { secs - place.last })
    };
    if let Some((i, _)) = cursors.iter().enumerate().filter_map(|(i, c)| reach(c).map(|d| (i, d))).min_by(|a, b| a.1.total_cmp(&b.1)) {
        return Some(i);
    }
    if cursors.len() < 2 {
        if let Some(c) = fresh() {
            cursors.push(c);
            return Some(cursors.len() - 1);
        }
    }
    cursors.iter().enumerate().min_by_key(|(_, c)| c.used).map(|(i, _)| i)
}

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
    /// `item` decoded into `working` from its file; `state` names what its
    /// frames are made from (the media's state, as the pool sync gives it).
    pub fn new(item: &klipp_media::MediaItem, working: WorkingColorspace, state: u64, device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        Self::of(item, working, state, false, device, queue)
    }

    /// [`Self::new`], from its proxy when `proxies` and it has one ready,
    /// as Klipp's viewer plays it: its frames fitted to the media's size,
    /// and named apart from the file's.
    pub fn of(item: &klipp_media::MediaItem, working: WorkingColorspace, state: u64, proxies: bool, device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let fps = item.fps.as_f64();
        let fps = if fps > 0.0 { fps } else { 25.0 };
        let rate = crate::compose::rate_of(fps);
        let frames = (item.duration.to_secs_f64() * fps).floor() as u32;
        let duration = Time { ticks: i64::from(frames) * i64::from(rate.rate), rate: rate.ticks as u32 };
        let size = [item.resolution.width.max(1), item.resolution.height.max(1)];
        let info = SourceInfo { width: size[0], height: size[1], duration: Some(duration), frame_rate: Some(rate), frames: Some(frames), description: "the clip's media, decoded by Klipp".into() };
        let proxy = item.proxy_path.clone().filter(|p| proxies && item.proxy_ready && p.exists());
        let salt = mix(state ^ mix(working as u64 ^ 0x7ab ^ (u64::from(proxy.is_some()) << 32)));
        let id: MediaId = item.id;
        let length = RationalTime::from_secs_f64(item.duration.to_secs_f64(), 24000);
        let zero = RationalTime::from_secs_f64(0.0, 24000);
        let clip = Clip::media(id, zero, length, zero, length);
        // A proxy keeps its original's log curve and transfer: decoded as it.
        let mut file = klipp_render::MediaFile::of(item);
        if let Some(proxy) = proxy {
            file.path = proxy;
        }
        let media: Arc<std::collections::HashMap<MediaId, klipp_render::MediaFile>> = Arc::new([(id, file)].into_iter().collect());
        // The decoder for each place Veylvik reads from (the frame shown,
        // the frames it prepares), each on a thread of its own: one never
        // waits on another's decoding, nor on its reading ahead.
        let decoder = move || -> Option<Cursor> {
            let (asks, rx) = std::sync::mpsc::channel::<Ask>();
            let place = Arc::new(Mutex::new(Place { last: f64::NEG_INFINITY, asked: f64::NEG_INFINITY, ahead: Vec::new() }));
            let shared = Arc::clone(&place);
            let (media, device, queue, clip) = (Arc::clone(&media), Arc::clone(&device), Arc::clone(&queue), clip.clone());
            let spawned = std::thread::Builder::new().name("klipp-veylvik-tab-decode".into()).spawn(move || {
                // Made when its first frame is asked for.
                let mut source = ExportFrameSource::shared(media, fps, working);
                let duration_secs = f64::from(frames) / fps;
                let frame_secs = 1.0 / fps;
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
                    let took = std::time::Instant::now();
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
                    tracing::trace!("VEYLVIK_FOOTAGE frame {secs:.3} s in {:.1} ms", took.elapsed().as_secs_f64() * 1000.0);
                    Ok((buffer, fw, fh))
                };
                let mut ahead: std::collections::VecDeque<(f64, Decoded)> = Default::default();
                // How far apart its last two asks were, in frames: playing
                // with frames skipped, the next ask is that far on.
                let mut stride;
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
                    let (last, asked) = {
                        let p = shared.lock().unwrap_or_else(|e| e.into_inner());
                        (p.last, p.asked)
                    };
                    let step = ((secs - asked) / frame_secs).round();
                    stride = if (1.0..=4.0).contains(&step) { step } else { 1.0 };
                    // Read ahead already: taken, and what is behind it let go.
                    ahead.retain(|(t, _)| *t > secs - 0.5 * frame_secs);
                    let made = match ahead.iter().position(|(t, _)| (t - secs).abs() < 0.5 * frame_secs) {
                        Some(i) => ahead.remove(i).map(|(_, m)| m).expect("found"),
                        None => {
                            ahead.clear();
                            // An export's decoder reads on, never back: a
                            // scrub back, or a jump far ahead, moves it to
                            // the frame, the decoder kept (made again, a scrub
                            // waited ~180 ms on a 4K file before decoding).
                            if secs < last - 0.5 * frame_secs || secs > last + JUMP_SECS {
                                source.seek(&clip, secs);
                            }
                            let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
                            p.last = secs;
                            drop(p);
                            decode(&mut source, secs)
                        }
                    };
                    {
                        let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
                        p.asked = secs;
                        p.ahead = ahead.iter().map(|(t, _)| *t).collect();
                    }
                    let _ = answer.send(made);
                    // Read on while nobody is waiting, inside the media, a
                    // frame at a time, up to AHEAD frames on at the stride it
                    // is asked at.
                    while ahead.len() < AHEAD {
                        match rx.try_recv() {
                            Ok(ask) => {
                                pending = Some(ask);
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                        }
                        let next = ahead.back().map_or(secs, |(t, _)| *t) + stride * frame_secs;
                        if next >= duration_secs {
                            break;
                        }
                        let made = decode(&mut source, next);
                        ahead.push_back((next, made));
                        let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
                        p.last = next;
                        p.ahead.push(next);
                    }
                }
            });
            match spawned {
                Ok(_) => Some(Cursor { asks, place, used: 0 }),
                Err(e) => {
                    tracing::warn!("the Veylvik tab's decoder thread could not start: {e}");
                    None
                }
            }
        };
        // Hands each frame asked to the decoder that has it or is nearest.
        let (tx, rx) = std::sync::mpsc::channel::<Ask>();
        let spawned = std::thread::Builder::new().name("klipp-veylvik-tab-footage".into()).spawn(move || {
            let frame_secs = 1.0 / fps;
            let mut cursors: Vec<Cursor> = Vec::new();
            let mut clock = 0u64;
            while let Ok((secs, answer)) = rx.recv() {
                clock += 1;
                let Some(at) = pick(&mut cursors, secs, frame_secs, &decoder) else {
                    let _ = answer.send(Err("the clip's decoder could not start".into()));
                    continue;
                };
                cursors[at].used = clock;
                if let Err(std::sync::mpsc::SendError((_, answer))) = cursors[at].asks.send((secs, answer)) {
                    // Its thread has stopped: let it go.
                    cursors.remove(at);
                    let _ = answer.send(Err("the clip's decoder has stopped".into()));
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
/// [`crate::CLIP_MEDIA_KEY`], whichever clip it is now, and what is below
/// it under [`crate::BELOW_KEY`].
#[derive(Default)]
pub struct TabHost {
    footage: Mutex<Option<Arc<dyn veylvik_render::FrameSource>>>,
    below: Mutex<Option<Arc<dyn veylvik_render::FrameSource>>>,
    /// Media of Klipp's pool the composition lays in, by key.
    pool: Mutex<std::collections::HashMap<String, Arc<dyn veylvik_render::FrameSource>>>,
    sound: Mutex<Option<Arc<crate::clip_sound::ClipSound>>>,
}

impl TabHost {
    /// The media of Klipp's pool the open composition lays in, by key
    /// ([`crate::pool::media_key`]), from now on.
    pub fn set_pool(&self, pool: impl IntoIterator<Item = (String, EditorFootage)>) {
        let sources = pool.into_iter().map(|(key, f)| (key, Arc::new(veylvik_render::HostSource::new(f, 512 << 20).lasting()) as Arc<dyn veylvik_render::FrameSource>)).collect();
        *self.pool.lock().unwrap_or_else(|e| e.into_inner()) = sources;
    }

    /// What is below the open clip from now on; none while no clip is
    /// open. Named for this page alone: kept in this session only.
    pub fn set_below(&self, below: Option<crate::below::PageBelow>) {
        let source = below.map(|b| Arc::new(veylvik_render::HostSource::new(b, 512 << 20)) as Arc<dyn veylvik_render::FrameSource>);
        *self.below.lock().unwrap_or_else(|e| e.into_inner()) = source;
    }

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
        use crate::host_key::HostKey;
        let slot = match HostKey::parse(key)? {
            HostKey::ClipMedia => &self.footage,
            HostKey::Below => &self.below,
            // By the name as the composition writes it, as set.
            HostKey::Media(_) => return self.pool.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned(),
            // The page opens a clip of media, never a precomposition: it
            // has no scene's clips to give.
            HostKey::Clip(_) => return None,
        };
        slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn media_sound(&self, key: &str, window: [f64; 2], rate: u32) -> Option<veylvik_audio::Pcm> {
        // The clip's own sound only: the layers below, a scene's clips and
        // pool media are pictures here.
        if crate::host_key::HostKey::parse(key) != Some(crate::host_key::HostKey::ClipMedia) {
            return None;
        }
        // Mixed off the lock: the editor's mix thread may take a while.
        let sound = self.sound.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        sound.pcm(window, rate)
    }
}
