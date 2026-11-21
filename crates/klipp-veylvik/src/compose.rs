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
//!
//! A composition is made ready on a thread of its own the first time a
//! clip shows it: read, its sources found and its kernels compiled, which
//! can take a driver many seconds. Meanwhile a live render (the viewer)
//! shows the clip's media and [`preparing`] says so; any other render (an
//! export, a still) waits for it. Once one is ready, [`revision`] moves on,
//! so what showed the media draws the frame again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

/// How long a render that waits waits for a composition to be ready
/// before it shows the media and says why.
const PREPARE_WAIT: Duration = Duration::from_secs(300);

/// Veylvik's renderer on Klipp's device, and the compositions it shows.
pub struct Compositor {
    renderer: Arc<Renderer>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    state: Mutex<State>,
    /// Told when a composition being prepared is ready or failed.
    prepared: Condvar,
    renders: AtomicU64,
    bridge: OnceLock<Bridge>,
}

/// Klipp's sRGB bytes to linear floats on the device (`srgb8_to_linear.wgsl`).
struct Bridge {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    lut: wgpu::Buffer,
}

impl Bridge {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("veylvik footage srgb8"), source: wgpu::ShaderSource::Wgsl(include_str!("shaders/srgb8_to_linear.wgsl").into()) });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("veylvik footage srgb8"),
            entries: &[
                storage(0, true),
                storage(1, false),
                storage(2, true),
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("veylvik footage srgb8"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("veylvik footage srgb8"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        // The tables of Klipp's own expansion, made by it: every byte once.
        let bytes: Vec<u8> = (0..=255u8).flat_map(|b| [b, b, b, b]).collect();
        let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes);
        let pixels = linear.as_chunks::<4>().0;
        let table: Vec<f32> = pixels.iter().map(|p| p[0]).chain(pixels.iter().map(|p| p[3])).collect();
        let lut = wgpu::util::DeviceExt::create_buffer_init(device, &wgpu::util::BufferInitDescriptor { label: Some("veylvik footage srgb8 table"), contents: bytemuck::cast_slice(&table), usage: wgpu::BufferUsages::STORAGE });
        Self { pipeline, layout, lut }
    }
}

/// A composition's bytes and what reading them gave.
type Parsed = (Arc<[u8]>, Result<Composition, String>);

/// What a render reads its footage from: Klipp's frame at the time
/// rendered and, for a composition that reads other times, Klipp's readers
/// of the clip's other frames. Set for one render only.
type Current = Arc<Mutex<Option<Now>>>;

struct Now {
    index: i64,
    frame: Arc<dyn GpuFramePixels>,
    neighbours: Option<Neighbours>,
    /// Asked for another frame with no readers to give it.
    missed: bool,
}

/// Klipp's readers of a clip's other frames, by clip time (as its own
/// temporal nodes read them): those its viewer holds on the GPU first,
/// else decoded from the file. Either may be missing.
pub(crate) struct Neighbours {
    pub(crate) gpu: Option<klipp_compose::GpuFrameProviderFn>,
    pub(crate) cpu: Option<klipp_compose::LinearFrameProviderFn>,
    /// Where the clip starts in its media, in seconds: clip time 0.
    pub(crate) source_in: f64,
}

/// One frame asked of the compositor.
struct Ask<'a> {
    clip: &'a Clip,
    id: CompositionId,
    secs: f64,
    fps: f64,
    frame: &'a SourceFrame,
    working: WorkingColorspace,
    live: bool,
    /// The media's length in frames, when the source knows it.
    frames: Option<u32>,
    /// Klipp's readers of the clip's other frames, asked for only when the
    /// composition reads other times: with the file's reader too, or only
    /// the frames held on the GPU (opening the file costs every frame).
    neighbours: &'a mut dyn FnMut(Reach) -> Neighbours,
}

/// How far a composition reads past the frame rendered, as learnt from
/// what it asked for: each render begins with its clip source's.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Reach {
    /// The frame rendered alone.
    Frame,
    /// Frames the viewer holds on the GPU (the one before and after).
    Held,
    /// Any frame, read from the file when not held.
    Files,
}

impl Reach {
    fn wider(self) -> Option<Reach> {
        match self {
            Reach::Frame => Some(Reach::Held),
            Reach::Held => Some(Reach::Files),
            Reach::Files => None,
        }
    }
}

#[derive(Default)]
struct State {
    compositions: Compositions,
    parsed: HashMap<CompositionId, Parsed>,
    live: HashMap<LiveKey, Slot>,
    /// Live keys, oldest first.
    order: Vec<LiveKey>,
    /// Bumped when every media's frames may have changed (a decode setting
    /// of all of them): a frame Veylvik kept is not the same frame any more.
    generation: u64,
    /// What each media's frames are made from, as one number per media
    /// ([`set_media_states`]): one that changes is that media's alone.
    media_states: HashMap<MediaId, u64>,
    /// Each composition's assets folder, where the project keeps it: the
    /// files it names relative to it are read there.
    assets: HashMap<CompositionId, std::path::PathBuf>,
    told: HashSet<CompositionId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LiveKey {
    composition: CompositionId,
    media: Option<MediaId>,
    working: WorkingColorspace,
    footage: [u32; 2],
    generation: u64,
    /// The media's state when its clip source was made.
    media_state: u64,
    /// For a live view: its frames of the media are its own (8-bit
    /// streams, proxies), not an export's, so neither is given a picture
    /// made of the other's.
    live: bool,
}

/// A composition for one clip's media: being made ready, ready, or what
/// stopped it.
enum Slot {
    Preparing,
    Ready(Box<Live>),
    Failed(String),
}

/// Why a render gave no picture: the media is shown instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declined {
    /// Still being made ready, and the render was live: nothing to say.
    Preparing,
    /// It can't be shown, for this reason.
    Failed(String),
}

impl From<String> for Declined {
    fn from(why: String) -> Self {
        Declined::Failed(why)
    }
}

/// One composition rendering for one clip's media: its frames and Klipp's
/// frame the footage is read from.
struct Live {
    source: ClipSource,
    current: Current,
    fps: f64,
    /// How far it has read: Klipp's readers of the clip's other frames
    /// that go with every render from now on.
    reach: Reach,
    /// The last picture, kept to be written into again once nothing holds it.
    spare: Option<Arc<Composed>>,
}

static SHARED: OnceLock<Option<Arc<Compositor>>> = OnceLock::new();

/// Bumped each time a composition is ready or failed: see [`revision`].
static REVISION: AtomicU64 = AtomicU64::new(0);

/// Moves on each time a composition being prepared is ready, or failed: a
/// view that showed a clip's media meanwhile draws it again.
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

/// Whether a composition is being made ready now: what the viewer shows of
/// its clip is the media until then.
pub fn preparing() -> bool {
    matches!(SHARED.get(), Some(Some(c)) if c.preparing())
}

impl Compositor {
    /// Veylvik's renderer on `device`.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>, adapter: wgpu::AdapterInfo) -> Result<Self, String> {
        let renderer = Renderer::from_device((*device).clone(), (*queue).clone(), adapter).map_err(|e| e.to_string())?;
        // Compiled programs kept between runs, in Klipp's cache folder (its
        // clear-cache clears them): a new build of Klipp doesn't compile
        // every kernel again. A device that can't keep them works as before.
        if let Some(folder) = dirs::cache_dir().map(|d| d.join("klipp").join("veylvik-pipelines")) {
            match renderer.use_pipeline_cache(&folder) {
                Ok(kept) => tracing::debug!("Veylvik's compiled programs {} in {}", if kept { "kept" } else { "not kept (the device can't)" }, folder.display()),
                Err(e) => tracing::warn!("Veylvik's compiled programs not kept between runs: {e}"),
            }
        }
        Ok(Self { renderer: Arc::new(renderer), device, queue, state: Mutex::new(State::default()), prepared: Condvar::new(), renders: Default::default(), bridge: OnceLock::new() })
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
                        c.set_media_states(MEDIA_STATES.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default());
                        c.set_assets(ASSETS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default());
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
        self.renders.load(Ordering::Relaxed)
    }

    /// Whether a composition is being made ready now.
    pub fn preparing(&self) -> bool {
        self.state().live.values().any(|s| matches!(s, Slot::Preparing))
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
            .chain(state.parsed.keys().copied())
            .filter(|id| match (state.compositions.get(id), compositions.get(id)) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b) && a != b,
                _ => true,
            })
            .collect();
        state.parsed.retain(|id, _| !changed.contains(id));
        state.told.retain(|id| !changed.contains(id));
        state.compositions = compositions;
        // A composition edited (in the Veylvik tab, say) is taken by the
        // clip sources it has, in place: no frame of its clips shows the
        // media while it would be made again. One that is gone, or can't
        // be read or made now, is let go.
        let mut parsed: HashMap<CompositionId, Result<Composition, String>> = HashMap::new();
        for id in &changed {
            if state.compositions.contains_key(id) {
                parsed.insert(*id, Self::composition(&mut state, *id));
            }
        }
        let mut gone = Vec::new();
        for (key, slot) in state.live.iter_mut() {
            if !changed.contains(&key.composition) {
                continue;
            }
            let taken = match (slot, parsed.get(&key.composition)) {
                (Slot::Ready(live), Some(Ok(c))) => live.source.set_composition(c.clone()).is_ok(),
                _ => false,
            };
            if !taken {
                gone.push(*key);
            }
        }
        state.live.retain(|k, _| !gone.contains(k));
        state.order.retain(|k| !gone.contains(k));
        drop(state);
        // A render waiting on one that is gone asks again.
        self.prepared.notify_all();
    }

    /// The media under the clips changed (a file replaced or relinked, a
    /// proxy made, a source setting): no footage frame kept is used again.
    pub fn media_changed(&self) {
        let mut state = self.state();
        state.generation += 1;
        state.live.clear();
        state.order.clear();
        drop(state);
        self.prepared.notify_all();
    }

    /// What each media's frames are made from now, as one number per media
    /// (its file, proxy, decoding): a media whose number changed has its
    /// footage made again, and only that media. Asked on every sync of the
    /// media pool, most of which change nothing.
    pub fn set_media_states(&self, states: HashMap<MediaId, u64>) {
        let mut state = self.state();
        if state.media_states == states {
            return;
        }
        let stale = |k: &LiveKey| k.media.is_some_and(|m| states.get(&m).copied().unwrap_or(0) != k.media_state);
        state.live.retain(|k, _| !stale(k));
        state.order.retain(|k| !stale(k));
        state.media_states = states;
        drop(state);
        self.prepared.notify_all();
    }

    /// Each composition's assets folder now: in the project's folder, or
    /// none for a project never saved. A composition whose folder moved (a
    /// save somewhere else) is made again, reading its assets there.
    pub fn set_assets(&self, assets: HashMap<CompositionId, std::path::PathBuf>) {
        let mut state = self.state();
        if state.assets == assets {
            return;
        }
        let moved: HashSet<CompositionId> = state.live.keys().map(|k| k.composition).filter(|id| state.assets.get(id) != assets.get(id)).collect();
        state.live.retain(|k, _| !moved.contains(&k.composition));
        state.order.retain(|k| !moved.contains(&k.composition));
        state.assets = assets;
        drop(state);
        self.prepared.notify_all();
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
    /// `frame` being Klipp's own frame of it there, in `working`. A
    /// composition not ready yet is made ready on a thread of its own: for a
    /// `live` view this gives [`Declined::Preparing`] meanwhile; else it waits.
    fn render(self: &Arc<Self>, ask: Ask<'_>) -> Result<SourceFrame, Declined> {
        let Ask { clip, id, secs, fps, frame, working, live, frames, neighbours } = ask;
        self.renders.fetch_add(1, Ordering::Relaxed);
        let footage = self.on_gpu(frame)?;
        let mut state = self.state();
        let deadline = Instant::now() + PREPARE_WAIT;
        let key = loop {
            // Asked again each time round: the media may have changed while
            // this waited.
            let media = clip.media_id();
            let media_state = media.and_then(|m| state.media_states.get(&m).copied()).unwrap_or(0);
            let key = LiveKey { composition: id, media, working, footage: [frame.width, frame.height], generation: state.generation, media_state, live };
            match state.live.get(&key) {
                Some(Slot::Ready(_)) => break key,
                Some(Slot::Failed(why)) => return Err(Declined::Failed(why.clone())),
                Some(Slot::Preparing) => {}
                None => {
                    let composition = Self::composition(&mut state, id)?;
                    let assets = state.assets.get(&id).cloned();
                    state.live.insert(key, Slot::Preparing);
                    state.order.push(key);
                    while state.order.len() > LIVE_MAX {
                        let old = state.order.remove(0);
                        state.live.remove(&old);
                    }
                    if let Err(why) = self.prepare(composition, key, fps, frames, assets) {
                        state.live.insert(key, Slot::Failed(why.clone()));
                        return Err(Declined::Failed(why));
                    }
                }
            }
            if live {
                return Err(Declined::Preparing);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Declined::Failed(format!("not ready after {} s", PREPARE_WAIT.as_secs())));
            }
            state = self.prepared.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        };
        let Some(Slot::Ready(live)) = state.live.get_mut(&key) else { unreachable!("ready, just seen") };
        let index = frame_index(secs, live.fps);
        let ([w, h], _, _) = live.source.format().map_err(|e| e.to_string())?;
        let mut out = match live.spare.take().and_then(|s| Arc::try_unwrap(s).ok()).filter(|c| (c.width, c.height) == (w, h)) {
            Some(mut reused) => {
                reused.linear = OnceLock::new();
                reused
            }
            None => Composed::new(&self.device, &self.queue, w, h),
        };
        out.scene = match &frame.pixels {
            SourcePixels::SceneReferred { transfer, .. } => Some(*transfer),
            SourcePixels::Gpu(g) => g.scene_transfer(),
            SourcePixels::Linear(_) => None,
        };
        // The frame's own start, exactly: a time rounded on the way (to
        // microseconds, say) can fall a hair before it, and Veylvik then
        // asks for the frame before, which Klipp hasn't given it.
        let at = frame_time(index, live.fps);
        let attempt = |live: &mut Live, readers: Option<Neighbours>| {
            *live.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(Now { index, frame: Arc::clone(&footage), neighbours: readers, missed: false });
            let rendered = live.source.render(at, [w, h], &out.buffer, &working_pixels(working)).map_err(|e| e.to_string());
            // Klipp's frames are the footage of this render only.
            let now = live.current.lock().unwrap_or_else(|e| e.into_inner()).take();
            (rendered, now.is_some_and(|n| n.missed))
        };
        let mut readers = |reach| (reach != Reach::Frame).then(|| neighbours(reach));
        let (mut rendered, mut missed) = attempt(live, readers(live.reach));
        // It read another time and found none: from now on, and this frame
        // again, with Klipp's readers reaching further.
        while rendered.is_err() && missed {
            let Some(wider) = live.reach.wider() else { break };
            live.reach = wider;
            (rendered, missed) = attempt(live, readers(wider));
        }
        rendered?;
        let out = Arc::new(out);
        live.spare = Some(Arc::clone(&out));
        Ok(SourceFrame { pixels: SourcePixels::Gpu(out), width: w, height: h, pts: frame.pts })
    }

    /// Make `composition` ready for `key` on a thread of its own, its slot
    /// [`Slot::Preparing`] until then.
    fn prepare(self: &Arc<Self>, composition: Composition, key: LiveKey, fps: f64, frames: Option<u32>, assets: Option<std::path::PathBuf>) -> Result<(), String> {
        let compositor = Arc::clone(self);
        std::thread::Builder::new().name("klipp-veylvik-prepare".into()).spawn(move || {
            let started = Instant::now();
            // Made here, on this thread: a composition that films a scene
            // makes Veylvik's 3D renderer, its pipelines compiled then.
            let made = compositor.live(composition, key, fps, frames, assets).and_then(|live| {
                // Its kernels compiled now, so its first frame doesn't wait
                // on a driver compiling one; kept on disk for the next run.
                live.source.compile().map_err(|e| e.to_string())?;
                Ok(live)
            });
            tracing::info!("a clip's Veylvik composition {} prepared in {:.1} s{}", key.composition, started.elapsed().as_secs_f64(), made.as_ref().err().map(|e| format!(" and failed: {e}")).unwrap_or_default());
            let mut state = compositor.state();
            // Gone meanwhile (the media or the composition changed): what
            // was made is of the old one.
            if let Some(slot @ Slot::Preparing) = state.live.get_mut(&key) {
                *slot = match made {
                    Ok(live) => Slot::Ready(Box::new(live)),
                    Err(why) => Slot::Failed(why),
                };
            }
            drop(state);
            REVISION.fetch_add(1, Ordering::Release);
            compositor.prepared.notify_all();
        })
        .map(|_| ())
        .map_err(|e| format!("no thread to prepare it on ({e})"))
    }

    fn live(self: &Arc<Self>, composition: Composition, key: LiveKey, fps: f64, frames: Option<u32>, assets: Option<std::path::PathBuf>) -> Result<Live, String> {
        let current: Current = Arc::default();
        let held = Arc::clone(&current);
        // Weak: the compositor holds this through its clip sources.
        let compositor = Arc::downgrade(self);
        let [w, h] = key.footage;
        let fetch: crate::FrameFetch = Box::new(move |at| {
            let want = frame_index(at, fps);
            let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
            let now = held.as_mut()?;
            if now.index == want {
                return Some(Arc::clone(&now.frame));
            }
            let compositor = compositor.upgrade()?;
            let found = now.neighbours.as_mut().and_then(|readers| {
                // A quarter into the frame, in clip time: inside it
                // whichever way a reader rounds.
                let t = (want as f64 + 0.25) / fps - readers.source_in;
                if let Some(frame) = readers.gpu.as_mut().and_then(|read| read(t)).and_then(|g| compositor.linear_on_device(g, w, h)) {
                    return Some(frame);
                }
                let (pixels, fw, fh) = readers.cpu.as_mut()?(t)?;
                ((fw, fh) == (w, h) && pixels.len() == w as usize * h as usize * 4).then(|| compositor.upload(pixels, w, h))
            });
            // None here: a render reaching further may find it.
            now.missed |= found.is_none();
            found
        });
        let rate = rate_of(fps);
        let duration = frames.map(|n| Time { ticks: i64::from(n) * i64::from(rate.rate), rate: rate.ticks as u32 });
        let info = SourceInfo { width: w, height: h, duration, frame_rate: Some(rate), frames, description: "the clip's media in Klipp".into() };
        let footage = ClipFootage::new(info, fps, [w, h], working_pixels(key.working), salt(&key), fetch);
        let host = Arc::new(KlippHost::new());
        host.insert(CLIP_MEDIA_KEY, Arc::new(HostSource::new(footage, FOOTAGE_BUDGET)));
        // Its assets in its folder in the project; a project never saved
        // has none yet, and an empty folder of the session's own stands in.
        let assets = assets.unwrap_or_else(no_assets);
        let source = ClipSource::new(Arc::clone(&self.renderer), composition, assets, Some(host as Arc<dyn crate::Host>)).map_err(|e| e.to_string())?;
        Ok(Live { source, current, fps, reach: Reach::Frame, spare: None })
    }

    /// Klipp's frame as linear floats in the working space on the device:
    /// copied or expanded there when it is there, else sent up.
    fn on_gpu(&self, frame: &SourceFrame) -> Result<Arc<dyn GpuFramePixels>, String> {
        let (w, h) = (frame.width, frame.height);
        let pixels = u64::from(w) * u64::from(h);
        if let SourcePixels::Gpu(g) = &frame.pixels {
            match g.on_device() {
                Some((klipp_compose::OnDevice::Linear(buf), fw, fh)) if (fw, fh) == (w, h) && buf.size() >= pixels * 16 => {
                    let out = self.footage_buffer(pixels);
                    let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik footage copy") });
                    encoder.copy_buffer_to_buffer(buf, 0, &out, 0, pixels * 16);
                    self.queue.submit([encoder.finish()]);
                    return Ok(Arc::new(Footage::on_device(out, w, h, &self.device, &self.queue)));
                }
                Some((klipp_compose::OnDevice::Srgb8(buf), fw, fh)) if (fw, fh) == (w, h) && buf.size() >= pixels * 4 => {
                    let out = self.footage_buffer(pixels);
                    self.expand_srgb8(buf, &out, pixels);
                    return Ok(Arc::new(Footage::on_device(out, w, h, &self.device, &self.queue)));
                }
                _ => {}
            }
        }
        let linear = match &frame.pixels {
            SourcePixels::Linear(p) => Arc::clone(p),
            SourcePixels::Gpu(g) => g.read(),
            SourcePixels::SceneReferred { codes, transfer, source_to_working } => Arc::new(codes.to_scene_linear_rgba(*transfer, *source_to_working)),
        };
        if linear.len() as u64 != pixels * 4 {
            return Err(format!("Klipp's frame holds {} values for {w}×{h}", linear.len()));
        }
        Ok(self.upload(linear, w, h))
    }

    /// Linear floats from the CPU, sent up.
    fn upload(&self, linear: Arc<Vec<f32>>, w: u32, h: u32) -> Arc<dyn GpuFramePixels> {
        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            self.device.as_ref(),
            &wgpu::util::BufferInitDescriptor { label: Some("veylvik footage"), contents: bytemuck::cast_slice(&linear), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC },
        );
        let footage = Footage::on_device(buffer, w, h, &self.device, &self.queue);
        let _ = footage.linear.set(linear);
        Arc::new(footage)
    }

    /// A frame of Klipp's on the device as linear floats `w` × `h`: as it
    /// is, or a viewer stream's sRGB bytes expanded there. `None` for bytes
    /// at another size.
    fn linear_on_device(&self, frame: Arc<dyn GpuFramePixels>, w: u32, h: u32) -> Option<Arc<dyn GpuFramePixels>> {
        let pixels = u64::from(w) * u64::from(h);
        if !frame.srgb8() {
            return (frame.buffer().size() >= pixels * 16).then_some(frame);
        }
        if frame.source_size().is_some_and(|s| s != (w, h)) || frame.buffer().size() < pixels * 4 {
            return None;
        }
        let out = self.footage_buffer(pixels);
        self.expand_srgb8(frame.buffer(), &out, pixels);
        Some(Arc::new(Footage::on_device(out, w, h, &self.device, &self.queue)))
    }

    fn footage_buffer(&self, pixels: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("veylvik footage"),
            size: pixels * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// `src`'s sRGB bytes into `dst` as linear floats, `pixels` of them.
    fn expand_srgb8(&self, src: &wgpu::Buffer, dst: &wgpu::Buffer, pixels: u64) {
        let bridge = self.bridge.get_or_init(|| Bridge::new(&self.device));
        const GROUP: u64 = 256;
        const MAX_X: u64 = 65_535;
        let groups = pixels.div_ceil(GROUP);
        let (x, y) = (groups.min(MAX_X), groups.div_ceil(MAX_X));
        let count = wgpu::util::DeviceExt::create_buffer_init(
            self.device.as_ref(),
            &wgpu::util::BufferInitDescriptor { label: Some("veylvik footage srgb8 count"), contents: bytemuck::cast_slice(&[pixels as u32, (x * GROUP) as u32, 0, 0]), usage: wgpu::BufferUsages::UNIFORM },
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("veylvik footage srgb8"),
            layout: &bridge.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: bridge.lut.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: count.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik footage srgb8") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("veylvik footage srgb8"), timestamp_writes: None });
            pass.set_pipeline(&bridge.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(x as u32, y as u32, 1);
        }
        self.queue.submit([encoder.finish()]);
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

/// Each composition's assets folder in the open project: see
/// [`Compositor::set_assets`]. Held for a compositor not started yet.
pub fn set_project_assets(assets: HashMap<CompositionId, std::path::PathBuf>) {
    let mut held = ASSETS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Some(compositor)) = SHARED.get() {
        compositor.set_assets(assets.clone());
    }
    *held = Some(assets);
}

static ASSETS: Mutex<Option<HashMap<CompositionId, std::path::PathBuf>>> = Mutex::new(None);

/// The assets folder of a composition whose project has none yet: empty,
/// the session's own, so nothing is found that another put there.
fn no_assets() -> std::path::PathBuf {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("klipp-veylvik-no-assets-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
    .clone()
}

/// What each media's frames are made from now, one number per media: see
/// [`Compositor::set_media_states`]. Held for a compositor not started yet.
pub fn set_media_states(states: HashMap<MediaId, u64>) {
    let mut held = MEDIA_STATES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Some(compositor)) = SHARED.get() {
        compositor.set_media_states(states.clone());
    }
    *held = Some(states);
}

/// The media states as the app last said, for the compositor when it starts.
static MEDIA_STATES: Mutex<Option<HashMap<MediaId, u64>>> = Mutex::new(None);

/// Every media's frames may have changed (a decode setting of all of
/// them): Veylvik's footage frames are not used again.
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

    fn revision(&self) -> u64 {
        revision()
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
    [media.0, media.1, working, u64::from(key.footage[0]) << 32 | u64::from(key.footage[1]), key.generation, key.media_state, u64::from(key.live), key.composition.0.as_u64_pair().0]
        .into_iter()
        .fold(session, |acc, part| crate::mix(acc ^ part))
}

fn frame_index(secs: f64, fps: f64) -> i64 {
    (secs * fps + 1e-6).floor() as i64
}

/// The start of frame `index` at `fps`, exact: the frame Veylvik's
/// footage reads at it is `index` again.
fn frame_time(index: i64, fps: f64) -> Time {
    let rate = rate_of(fps);
    Time { ticks: index * i64::from(rate.rate), rate: rate.ticks as u32 }
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

/// Klipp's frame handed to Veylvik as footage: linear floats on the device,
/// read back only if something on the CPU asks.
struct Footage {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    linear: OnceLock<Arc<Vec<f32>>>,
}

impl Footage {
    fn on_device(buffer: wgpu::Buffer, width: u32, height: u32, device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> Self {
        Self { buffer, width, height, device: Arc::clone(device), queue: Arc::clone(queue), linear: OnceLock::new() }
    }
}

impl GpuFramePixels for Footage {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(self.linear.get_or_init(|| Arc::new(read_back(&self.device, &self.queue, &self.buffer).unwrap_or_else(|| vec![0.0; self.width as usize * self.height as usize * 4]))))
    }
}

/// A composition's picture on the device, in Klipp's pixels: the clip's
/// source frame from here on.
struct Composed {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    /// The scene-referred source whose scene light the composition was
    /// given: the picture is that light still, and owes its display step.
    scene: Option<klipp_color::hdr::HdrTransfer>,
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
        Self { buffer, width, height, scene: None, device: Arc::clone(device), queue: Arc::clone(queue), linear: OnceLock::new() }
    }
}

impl GpuPixels for Composed {
    fn scene_transfer(&self) -> Option<klipp_color::hdr::HdrTransfer> {
        self.scene
    }

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
        let live = self.inner.shows_live();
        let frames = self.inner.media_range(clip).map(|(start, end)| ((end - start).max(0.0) * fps + 1e-6).floor() as u32);
        let (working, [w, h]) = (self.working, [frame.width, frame.height]);
        let inner = &mut *self.inner;
        let mut neighbours = |reach| Neighbours {
            gpu: inner.temporal_frames_on_gpu(clip, working, w, h),
            cpu: if reach == Reach::Files { inner.temporal_frames_linear_at_size(clip, working, w, h) } else { None },
            source_in: clip.source_in.to_secs_f64(),
        };
        let ask = Ask { clip, id, secs: source_secs, fps, frame: &frame, working, live, frames, neighbours: &mut neighbours };
        match self.compositor.render(ask) {
            Ok(picture) => Some(picture),
            Err(Declined::Preparing) => Some(frame),
            Err(Declined::Failed(why)) => {
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

    fn shows_live(&mut self) -> bool {
        self.inner.shows_live()
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

    /// A frame's time names that frame again where Veylvik's footage reads
    /// it, at every rate and far into a clip. In microseconds, every third
    /// frame at 23.976 named the frame before.
    #[test]
    fn a_frames_time_names_that_frame() {
        for fps in [24000.0 / 1001.0, 30000.0 / 1001.0, 60000.0 / 1001.0, 25.0, 24.0, 12.5] {
            for index in (0..20_000).chain([1_000_000, 10_000_000]) {
                let secs = frame_time(index, fps).seconds().unwrap();
                assert_eq!(frame_index(secs, fps), index, "{fps} fps, frame {index}");
            }
        }
        let rounded = crate::time_from_secs(1.0 / (24000.0 / 1001.0)).seconds().unwrap();
        assert_eq!(frame_index(rounded, 24000.0 / 1001.0), 0, "what microseconds did");
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
