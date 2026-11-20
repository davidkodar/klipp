//! A clip's picture through its Veylvik composition, for Klipp's renderer:
//! [`Compositor::source`] wraps the viewer's or the export's frame source,
//! and a clip that names a composition gets its frame from Veylvik, its own
//! frame from that source as the footage. Every other clip, and everything
//! else the renderer asks, goes to the source as it was.
//!
//! The composition's time is its media's: time 0 is the media's first
//! frame, so a trim shows more or less of it. Veylvik renders it at its own
//! size and Klipp fits the picture into the frame as it fits any media's,
//! then grades it and adds its effects on top.
//!
//! A composition that can't be read or rendered shows the clip's media as
//! it is, said once in the log.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use klipp_compose::{FrameSource, GpuPixels, SourceFrame, SourcePixels};
use klipp_core::{MediaId, WorkingColorspace};
use klipp_nodes::frame_context::GpuFramePixels;
use klipp_timeline::{Clip, CompositionId};
use veylvik_document::{Composition, Time};

use crate::{working_pixels, ClipFootage, ClipSource, HostSource, KlippHost, Renderer, SourceInfo};

/// The project's compositions, by id: each file's bytes (as
/// `klipp_project::compositions::Compositions`).
pub type Compositions = BTreeMap<CompositionId, Arc<[u8]>>;

/// The key under which a composition finds its clip's own media: what a
/// composition made for a clip names its footage by.
pub const CLIP_MEDIA_KEY: &str = "clip";

/// Footage frames a composition keeps of one clip's media.
const FOOTAGE_BUDGET: u64 = 256 << 20;

/// Clip sources kept at once; the oldest goes past it.
const LIVE_MAX: usize = 16;

/// Veylvik's renderer on Klipp's device, and the compositions it shows.
pub struct Compositor {
    renderer: Arc<Renderer>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    state: Mutex<State>,
    renders: std::sync::atomic::AtomicU64,
}

/// A composition's bytes and what reading them gave.
type Parsed = (Arc<[u8]>, Result<Composition, String>);

/// Klipp's frame a render reads as its footage, by its index.
type Current = Arc<Mutex<Option<(i64, Arc<dyn GpuFramePixels>)>>>;

#[derive(Default)]
struct State {
    compositions: Compositions,
    parsed: HashMap<CompositionId, Parsed>,
    live: HashMap<LiveKey, Live>,
    /// Live keys, oldest first.
    order: Vec<LiveKey>,
    /// Bumped when the media changes under the same clip: a frame Veylvik
    /// kept of it is not the same frame any more.
    generation: u64,
    told: HashSet<CompositionId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LiveKey {
    composition: CompositionId,
    media: Option<MediaId>,
    working: WorkingColorspace,
    footage: [u32; 2],
    generation: u64,
}

/// One composition rendering for one clip's media: its frames and Klipp's
/// frame the footage is read from.
struct Live {
    source: ClipSource,
    current: Current,
    fps: f64,
    /// The last picture, kept to be written into again once nothing holds it.
    spare: Option<Arc<Composed>>,
}

static SHARED: OnceLock<Option<Arc<Compositor>>> = OnceLock::new();

impl Compositor {
    /// Veylvik's renderer on `device`.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>, adapter: wgpu::AdapterInfo) -> Result<Self, String> {
        let renderer = Renderer::from_device((*device).clone(), (*queue).clone(), adapter).map_err(|e| e.to_string())?;
        Ok(Self { renderer: Arc::new(renderer), device, queue, state: Mutex::new(State::default()), renders: Default::default() })
    }

    /// The one compositor of this process, on Klipp's shared GPU context:
    /// the viewer's and the export's, so a frame either made is kept for
    /// both. `None` without a GPU, or when Veylvik can't use it.
    pub fn shared() -> Option<Arc<Compositor>> {
        SHARED
            .get_or_init(|| {
                let ctx = klipp_effects::gpu_context::GpuContext::shared()?;
                match Compositor::new(ctx.device(), ctx.queue(), ctx.adapter_info()?.clone()) {
                    Ok(c) => {
                        if let Some(held) = PROJECT.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                            c.set_compositions(held);
                        }
                        Some(Arc::new(c))
                    }
                    Err(why) => {
                        tracing::warn!("Veylvik compositions are shown as their media: Veylvik's renderer could not start on Klipp's device ({why})");
                        None
                    }
                }
            })
            .clone()
    }

    /// Pictures asked of Veylvik so far, rendered or failed.
    pub fn renders(&self) -> u64 {
        self.renders.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The compositions the project holds now. A composition whose bytes
    /// changed renders as it is now from the next frame.
    pub fn set_compositions(&self, compositions: Compositions) {
        let mut state = self.state();
        let changed: HashSet<CompositionId> = state
            .live
            .keys()
            .map(|k| k.composition)
            .filter(|id| match (state.compositions.get(id), compositions.get(id)) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b) && a != b,
                _ => true,
            })
            .collect();
        state.live.retain(|k, _| !changed.contains(&k.composition));
        state.order.retain(|k| !changed.contains(&k.composition));
        state.parsed.retain(|id, _| !changed.contains(id));
        state.told.retain(|id| !changed.contains(id));
        state.compositions = compositions;
    }

    /// The media under the clips changed (a file replaced or relinked, a
    /// proxy made, a source setting): no footage frame kept is used again.
    pub fn media_changed(&self) {
        let mut state = self.state();
        state.generation += 1;
        state.live.clear();
        state.order.clear();
    }

    /// `inner` with every clip that names a composition shown through it,
    /// in `working`.
    pub fn source<'a>(self: &Arc<Self>, inner: &'a mut dyn FrameSource, working: WorkingColorspace) -> CompositionSource<'a> {
        CompositionSource { compositor: Arc::clone(self), inner, working }
    }

    /// The composition `id`, read from its bytes once.
    fn composition(state: &mut State, id: CompositionId) -> Result<Composition, String> {
        let bytes = state.compositions.get(&id).cloned().ok_or_else(|| "the project does not hold it".to_string())?;
        if let Some((held, parsed)) = state.parsed.get(&id) {
            if Arc::ptr_eq(held, &bytes) {
                return parsed.clone();
            }
        }
        let parsed = serde_json::from_slice::<Composition>(&bytes)
            .map_err(|e| veylvik_document::explain_parse(&e))
            .and_then(|c| c.validate().map(|_| c).map_err(|e| e.to_string()));
        state.parsed.insert(id, (bytes, parsed.clone()));
        parsed
    }

    /// `clip`'s picture at `secs` in its media through composition `id`,
    /// `frame` being Klipp's own frame of it there. `Err` says why not.
    fn render(&self, clip: &Clip, id: CompositionId, secs: f64, fps: f64, frame: &SourceFrame, working: WorkingColorspace) -> Result<SourceFrame, String> {
        self.renders.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let footage = self.on_gpu(frame)?;
        let mut state = self.state();
        let composition = Self::composition(&mut state, id)?;
        let key = LiveKey { composition: id, media: clip.media_id(), working, footage: [frame.width, frame.height], generation: state.generation };
        if let std::collections::hash_map::Entry::Vacant(slot) = state.live.entry(key) {
            slot.insert(self.live(composition, key, fps)?);
            state.order.push(key);
            while state.order.len() > LIVE_MAX {
                let old = state.order.remove(0);
                state.live.remove(&old);
            }
        }
        let live = state.live.get_mut(&key).expect("just made");
        let index = frame_index(secs, live.fps);
        *live.current.lock().unwrap_or_else(|e| e.into_inner()) = Some((index, footage));
        let ([w, h], _, _) = live.source.format().map_err(|e| e.to_string())?;
        let out = match live.spare.take().and_then(|s| Arc::try_unwrap(s).ok()).filter(|c| (c.width, c.height) == (w, h)) {
            Some(mut reused) => {
                reused.linear = OnceLock::new();
                reused
            }
            None => Composed::new(&self.device, &self.queue, w, h),
        };
        let at = crate::time_from_secs(secs);
        let rendered = live.source.render(at, [w, h], &out.buffer, &working_pixels(working)).map_err(|e| e.to_string());
        // Klipp's frame is the footage of this render only.
        *live.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
        rendered?;
        let out = Arc::new(out);
        live.spare = Some(Arc::clone(&out));
        Ok(SourceFrame { pixels: SourcePixels::Gpu(out), width: w, height: h, pts: frame.pts })
    }

    fn live(&self, composition: Composition, key: LiveKey, fps: f64) -> Result<Live, String> {
        let current: Current = Arc::default();
        let held = Arc::clone(&current);
        let fetch: crate::FrameFetch = Box::new(move |at| {
            let want = frame_index(at, fps);
            match &*held.lock().unwrap_or_else(|e| e.into_inner()) {
                Some((index, frame)) if *index == want => Some(Arc::clone(frame)),
                _ => None,
            }
        });
        let [w, h] = key.footage;
        let info = SourceInfo { width: w, height: h, duration: None, frame_rate: Some(rate_of(fps)), frames: None, description: "the clip's media in Klipp".into() };
        let footage = ClipFootage::new(info, fps, [w, h], working_pixels(key.working), salt(&key), fetch);
        let host = Arc::new(KlippHost::new());
        host.insert(CLIP_MEDIA_KEY, Arc::new(HostSource::new(footage, FOOTAGE_BUDGET)));
        // A composition made for a clip reads nothing but its media: no
        // folder of its own to find assets in yet.
        let assets = std::env::temp_dir().join("klipp-veylvik-assets");
        let source = ClipSource::new(Arc::clone(&self.renderer), composition, assets, Some(host as Arc<dyn crate::Host>)).map_err(|e| e.to_string())?;
        Ok(Live { source, current, fps, spare: None })
    }

    /// Klipp's frame as linear floats in the working space on the device.
    fn on_gpu(&self, frame: &SourceFrame) -> Result<Arc<dyn GpuFramePixels>, String> {
        let linear = match &frame.pixels {
            SourcePixels::Linear(p) => Arc::clone(p),
            // A decoder's frame on the GPU, read back to be sent up again
            // as floats: correct, not yet fast.
            SourcePixels::Gpu(g) => g.read(),
            SourcePixels::SceneReferred { codes, transfer, source_to_working } => Arc::new(codes.to_scene_linear_rgba(*transfer, *source_to_working)),
        };
        if linear.len() != frame.width as usize * frame.height as usize * 4 {
            return Err(format!("Klipp's frame holds {} values for {}×{}", linear.len(), frame.width, frame.height));
        }
        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            self.device.as_ref(),
            &wgpu::util::BufferInitDescriptor { label: Some("veylvik footage"), contents: bytemuck::cast_slice(&linear), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC },
        );
        Ok(Arc::new(Footage { buffer, linear }))
    }

    fn tell_once(&self, id: CompositionId, why: &str) {
        if self.state().told.insert(id) {
            tracing::warn!("a clip's Veylvik composition {id} is shown as its media: {why}");
        }
    }
}

/// The open project's compositions, as the app last said: what every
/// render's clips are shown through once [`install`] has run.
static PROJECT: Mutex<Option<Compositions>> = Mutex::new(None);

/// The open project's compositions now: on open, on a save that gave a
/// copied clip its own, on a new project (none). Veylvik's renderer starts
/// with the first frame that needs it, never for a project without any.
pub fn set_project_compositions(compositions: Compositions) {
    let mut project = PROJECT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Some(compositor)) = SHARED.get() {
        compositor.set_compositions(compositions.clone());
    }
    *project = (!compositions.is_empty()).then_some(compositions);
}

/// The media under the clips changed: Veylvik's footage frames of it are
/// not used again.
pub fn media_changed() {
    if let Some(Some(compositor)) = SHARED.get() {
        compositor.media_changed();
    }
}

/// Show clips through their compositions in every Klipp render from now
/// on. Once, at start-up; `false` when something else was installed.
pub fn install() -> bool {
    klipp_compose::source_wrap::install(Arc::new(Wrap))
}

struct Wrap;

impl klipp_compose::source_wrap::SourceWrap for Wrap {
    fn active(&self) -> bool {
        // The lock let go before the renderer starts: starting reads it.
        let any = PROJECT.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        any && Compositor::shared().is_some()
    }

    fn wrap<'a>(&self, inner: &'a mut dyn FrameSource, working: WorkingColorspace) -> Box<dyn FrameSource + 'a> {
        match Compositor::shared() {
            Some(compositor) => Box::new(compositor.source(inner, working)),
            None => Box::new(Unwrapped(inner)),
        }
    }
}

/// A source read as it is: only if the renderer went away between
/// `active` and `wrap`, which it doesn't.
struct Unwrapped<'a>(&'a mut dyn FrameSource);

impl FrameSource for Unwrapped<'_> {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.frame(clip, source_secs, width, height)
    }
}

/// What a live clip source's footage frames are of, as one number: the
/// session (Veylvik may keep pictures on disk, and nothing here is the same
/// in another session), the media, how Klipp delivers its frames, and the
/// media's generation.
fn salt(key: &LiveKey) -> u64 {
    static SESSION: OnceLock<u64> = OnceLock::new();
    let session = *SESSION.get_or_init(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        crate::mix(nanos ^ crate::mix(u64::from(std::process::id())))
    });
    let media = key.media.map(|m| m.0.as_u64_pair()).unwrap_or((0, 0));
    let working = key.working as u64;
    [media.0, media.1, working, u64::from(key.footage[0]) << 32 | u64::from(key.footage[1]), key.generation, key.composition.0.as_u64_pair().0]
        .into_iter()
        .fold(session, |acc, part| crate::mix(acc ^ part))
}

fn frame_index(secs: f64, fps: f64) -> i64 {
    (secs * fps + 1e-6).floor() as i64
}

/// `fps` as Veylvik's exact rate: whole rates as they are, NTSC's as
/// thousandths over 1001.
fn rate_of(fps: f64) -> Time {
    let ntsc = fps * 1001.0 / 1000.0;
    if (fps - fps.round()).abs() < 1e-6 {
        Time { ticks: fps.round() as i64, rate: 1 }
    } else if (ntsc - ntsc.round()).abs() < 1e-3 {
        Time { ticks: ntsc.round() as i64 * 1000, rate: 1001 }
    } else {
        Time { ticks: (fps * 1000.0).round() as i64, rate: 1000 }
    }
}

/// Klipp's frame handed to Veylvik as footage.
struct Footage {
    buffer: wgpu::Buffer,
    linear: Arc<Vec<f32>>,
}

impl GpuFramePixels for Footage {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(&self.linear)
    }
}

/// A composition's picture on the device, in Klipp's pixels: the clip's
/// source frame from here on.
struct Composed {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    linear: OnceLock<Arc<Vec<f32>>>,
}

impl Composed {
    fn new(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, width: u32, height: u32) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("veylvik picture"),
            size: u64::from(width) * u64::from(height) * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { buffer, width, height, device: Arc::clone(device), queue: Arc::clone(queue), linear: OnceLock::new() }
    }
}

impl GpuPixels for Composed {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        if (self.width, self.height) == (width, height) {
            exec.prime_chain_from_gpu_linear_buffer(&self.buffer, width, height)
        } else {
            exec.prime_chain_fit_from_gpu_linear_buffer(&self.buffer, self.width, self.height, width, height)
        }
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(self.linear.get_or_init(|| Arc::new(read_back(&self.device, &self.queue, &self.buffer).unwrap_or_else(|| vec![0.0; self.width as usize * self.height as usize * 4]))))
    }
}

/// `buffer` on the CPU, by polling as Klipp's own readbacks do on a device
/// the UI shares.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer) -> Option<Vec<f32>> {
    let size = buffer.size();
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik picture read back"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik picture read back") });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    match klipp_effects::gpu::poll_until_mapped(device, &rx) {
        Ok(Ok(())) => {
            let out = bytemuck::cast_slice::<u8, f32>(&staging.slice(..).get_mapped_range()).to_vec();
            staging.unmap();
            Some(out)
        }
        other => {
            tracing::warn!("a Veylvik picture could not be read back ({other:?})");
            None
        }
    }
}

/// A frame source whose clips with a composition are shown through it.
pub struct CompositionSource<'a> {
    compositor: Arc<Compositor>,
    inner: &'a mut dyn FrameSource,
    working: WorkingColorspace,
}

impl CompositionSource<'_> {
    fn composed(clip: &Clip) -> Option<CompositionId> {
        clip.composition.filter(|_| clip.media_id().is_some())
    }
}

impl FrameSource for CompositionSource<'_> {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let frame = self.inner.frame(clip, source_secs, width, height)?;
        let Some(id) = Self::composed(clip) else { return Some(frame) };
        let fps = self.inner.media_fps(clip).filter(|f| *f > 0.0).unwrap_or(25.0);
        match self.compositor.render(clip, id, source_secs, fps, &frame, self.working) {
            Ok(picture) => Some(picture),
            Err(why) => {
                self.compositor.tell_once(id, &why);
                Some(frame)
            }
        }
    }

    // A composed clip's other frames would be its media's, not its
    // composition's: until they are composed too, it has none, and what
    // reads them reads the current frame alone.
    fn frame_after_on_gpu(&mut self, clip: &Clip, source_secs: f64, working: WorkingColorspace, width: u32, height: u32) -> Option<(f64, Arc<dyn GpuFramePixels>)> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.frame_after_on_gpu(clip, source_secs, working, width, height)
    }

    fn frame_after(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.frame_after(clip, source_secs, width, height)
    }

    // Its picture is not its file's: never held as a still or a parked file.
    fn still_file(&mut self, clip: &Clip) -> Option<klipp_compose::HeldFile> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.still_file(clip)
    }

    fn held_file(&mut self, clip: &Clip) -> Option<klipp_compose::HeldFile> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.held_file(clip)
    }

    fn temporal_frames(&mut self, clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.temporal_frames(clip)
    }

    fn temporal_frames_linear(&mut self, clip: &Clip, working: WorkingColorspace) -> Option<klipp_compose::LinearFrameProviderFn> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.temporal_frames_linear(clip, working)
    }

    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, working: WorkingColorspace, width: u32, height: u32) -> Option<klipp_compose::LinearFrameProviderFn> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.temporal_frames_linear_at_size(clip, working, width, height)
    }

    fn temporal_frames_on_gpu(&mut self, clip: &Clip, working: WorkingColorspace, width: u32, height: u32) -> Option<klipp_compose::GpuFrameProviderFn> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.temporal_frames_on_gpu(clip, working, width, height)
    }

    // The media's own facts and other media stay the source's.
    fn media_range(&mut self, clip: &Clip) -> Option<(f64, f64)> {
        self.inner.media_range(clip)
    }

    fn media_fps(&mut self, clip: &Clip) -> Option<f64> {
        self.inner.media_fps(clip)
    }

    // Analysis (faces, scopes' stored results) is of the media; a composed
    // clip's picture is another, so none is used for it.
    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.analysis_key(clip, kind)
    }

    fn subject_matte_dir(&mut self, clip: &Clip, model: u32) -> Option<std::path::PathBuf> {
        if Self::composed(clip).is_some() {
            return None;
        }
        self.inner.subject_matte_dir(clip, model)
    }

    fn media_frame(&mut self, media: &str, secs: f64, working: WorkingColorspace, width: u32, height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        self.inner.media_frame(media, secs, working, width, height)
    }

    fn media_transfer(&mut self, media: &str) -> Option<klipp_color::hdr::HdrTransfer> {
        self.inner.media_transfer(media)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(t: Time) -> (i64, u32) {
        (t.ticks, t.rate)
    }

    #[test]
    fn rates_are_exact() {
        assert_eq!(parts(rate_of(25.0)), (25, 1));
        assert_eq!(parts(rate_of(30000.0 / 1001.0)), (30000, 1001));
        assert_eq!(parts(rate_of(24000.0 / 1001.0)), (24000, 1001));
        assert_eq!(parts(rate_of(12.5)), (12500, 1000));
    }

    /// The wrapper answers every question the trait asks: a method the
    /// trait gains and the wrapper doesn't forward would answer with the
    /// default, and a feature would stop working for every clip.
    #[test]
    fn every_frame_source_method_is_answered() {
        let trait_src = include_str!("../../klipp-compose/src/render.rs");
        let start = trait_src.find("pub trait FrameSource {").expect("the trait");
        let end = start + trait_src[start..].find("\n}\n").expect("its end");
        let names = |text: &str| -> Vec<String> {
            text.lines().filter_map(|l| l.trim_start().strip_prefix("fn ")).map(|l| l.split(['(', '<']).next().unwrap_or("").to_string()).collect()
        };
        let own = include_str!("compose.rs");
        let at = own.find("impl FrameSource for CompositionSource").expect("the impl");
        let impl_end = at + own[at..].find("\n}\n").expect("its end");
        let mut wanted = names(&trait_src[start..end]);
        let mut have = names(&own[at..impl_end]);
        wanted.sort();
        have.sort();
        assert_eq!(wanted, have);
    }
}
