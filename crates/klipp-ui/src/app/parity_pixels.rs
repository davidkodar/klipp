//! The picture oracle for the parity families' Pixels obligations (M3
//! step 18): a clip as the real renderer draws it, on the CPU and on the
//! native GPU stack, from a synthetic, non-flat source.
//!
//! A case earns Pixels only when its edit is seen: the edited clip's
//! frame differs from the unedited one on each route (an edit that draws
//! nothing proves nothing), and the old view's clip and the generated
//! view's draw the same frame on each route, to the level. The routes are
//! never compared with each other here; their own bar is the CPU/GPU
//! parity suites'.
//!
//! A GPU that is missing or declines the frame, or a frame that leaves
//! the native GPU route (a layer drawn on the CPU and uploaded, an effect
//! on the CPU kernels, a fallback list, a mask the run couldn't give), is
//! BLOCKED: an error, so the case fails and earns no key. It is never a
//! skip that passes.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

/// The frame size the oracle draws at, unless a case asks for another:
/// large enough that every Video Transform typed value keeps the moved
/// clip in frame (positions are in the sequence's pixels).
pub(super) const SMALL: (u32, u32) = (320, 180);

/// The delivery size, for effects whose strength is scaled to the frame
/// (a gate weave of a few pixels at 1920 wide is under one at 320).
pub(super) const FULL: (u32, u32) = (1920, 1080);

/// The time every frame is drawn at: inside the oracle's clips, which
/// start at zero and run two seconds, early enough that a clip sped up
/// to twice its speed still shows.
fn at() -> Timecode {
    time(AT)
}

/// The oracle's sample time, in seconds.
pub(super) const AT: f64 = 0.5;

fn time(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// A frame of the source at `secs`: hue and brightness ramps across and
/// down, a checker, a patch brighter than white, and a fine grain that
/// changes every frame, the whole picture panning two pixels a frame. So
/// any move, scale, tilt, colour change, highlight effect or retime
/// shows, a rolling-shutter correction has motion to measure, and a
/// temporal denoise has noise to take away.
fn source(secs: f64, (w, h): (u32, u32)) -> Arc<Vec<f32>> {
    let frame = (secs * 24.0).round() as u32;
    let pan = frame * 2;
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let xs = x + pan;
            let (fx, fy) = ((xs % w) as f32 / w as f32, y as f32 / h as f32);
            let check = if (xs / 8 + y / 8).is_multiple_of(2) { 0.15 } else { 0.0 };
            let bright = (0.6..0.75).contains(&fx) && (0.2..0.4).contains(&fy);
            let lift = if bright { 1.5 } else { 0.0 };
            let hash = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ frame.wrapping_mul(83492791)) % 1000;
            let grain = (hash as f32 / 1000.0 - 0.5) * 0.06;
            v.extend_from_slice(&[fx * 0.8 + check + lift + grain, fy * 0.7 + lift + grain, (1.0 - fx) * 0.6 + check + lift + grain, 1.0]);
        }
    }
    Arc::new(v)
}

/// The synthetic source, counting what it was asked for: a frame drawn
/// without reading it isn't evidence of the clip.
struct Synthetic {
    size: (u32, u32),
    reads: u32,
}

/// [`Synthetic`]'s subject matte sequence at `size` for `model`, made
/// once.
fn subject_matte((w, h): (u32, u32), model: u32) -> Option<std::path::PathBuf> {
    use klipp_nodes::matte_sequence as seq;
    static MADE: std::sync::Mutex<Vec<((u32, u32, u32), std::path::PathBuf)>> = std::sync::Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, dir)) = made.iter().find(|(k, _)| *k == (w, h, model)) {
        return Some(dir.clone());
    }
    let dir = std::env::temp_dir().join(format!("klipp-parity-subject-{}-{w}x{h}-{model}", std::process::id()));
    let manifest = seq::Manifest { format: seq::FORMAT, fps: 24.0, width: w, height: h, rotation: 0, model: "parity".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: false };
    seq::create(&dir, &manifest).ok()?;
    let alpha: Vec<u8> = (0..w * h).map(|i| if i % w < w / 2 { 255 } else { 0 }).collect();
    for f in 0..=240 {
        seq::put(&dir, &manifest, seq::key_of(f as f64 / 24.0), &alpha).ok()?;
    }
    made.push(((w, h, model), dir.clone()));
    Some(dir)
}

impl FrameSource for Synthetic {
    fn frame(&mut self, _clip: &Clip, secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        self.reads += 1;
        Some(SourceFrame { pixels: SourcePixels::Linear(source(secs, self.size)), width: self.size.0, height: self.size.1, pts: None })
    }

    /// Frames at other clip times, for a temporal denoise's neighbours:
    /// the same source, linear, as the working space takes it.
    fn temporal_frames_linear(&mut self, _clip: &Clip, _working: klipp_core::WorkingColorspace) -> Option<klipp_compose::LinearFrameProviderFn> {
        let size = self.size;
        Some(Box::new(move |secs| Some((source(secs, size), size.0, size.1))))
    }

    /// Every media clip's analyses, under one stand-in key: those the
    /// oracle's renderers hold ([`analyses`]).
    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        matches!(clip.kind, klipp_timeline::ClipKind::Media { .. }).then(|| stand_in_key(kind)).flatten()
    }

    /// A tracked subject's matte (#2758): its left half, for every frame
    /// of its first ten seconds, the subject's (model 0) and the
    /// foreground's (model 1) the same, so picking one shows.
    fn subject_matte_dir(&mut self, _clip: &Clip, model: u32) -> Option<std::path::PathBuf> {
        subject_matte(self.size, model)
    }

    /// A media source's item (#1746), [`MEDIA_ITEM`]: the same pattern
    /// further on and upside down, so it differs from the clip's own and
    /// from itself at another time.
    fn media_frame(&mut self, media: &str, secs: f64, _working: klipp_core::WorkingColorspace, width: u32, height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        if media != MEDIA_ITEM {
            return Err(klipp_nodes::side_picture::SideMissing::File(media.to_owned()));
        }
        let (w, h) = self.size;
        let frame = source(secs + 3.0, self.size);
        let flipped: Vec<f32> = (0..h).rev().flat_map(|y| frame[(y * w * 4) as usize..((y + 1) * w * 4) as usize].iter().copied()).collect();
        klipp_compose::render::fit_linear(flipped, w, h, width, height).ok_or(klipp_nodes::side_picture::SideMissing::Unavailable)
    }
}

/// The one media item the harness's sources hold besides each clip's own
/// (#1746).
pub(super) const MEDIA_ITEM: &str = "3c8e6d52-7a14-4b0f-9e21-5d6f8a9b0c13";

/// The key the oracle's analyses are kept under: a stand-in file that
/// exists (this binary), as an analysis key reads the file.
fn stand_in_key(kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
    klipp_analysis::assets::key_for(kind, &std::env::current_exe().ok()?, klipp_analysis::assets::ANALYSIS_WIDTH, 270, (24, 1))
}

/// The analyses an analysis effect draws by, as the analysis leaves them
/// (M4 step 9), for every media clip the oracle draws: a measured
/// flicker, so the gains move the picture and the strength and window
/// move the gains; a drift with a shake on it, so the smoothing window
/// moves the correction; the picture moving between frames, so the rows
/// shear. Each renderer derives them with its effect's settings.
pub(super) fn analyses() -> Box<klipp_analysis::assets::MemoryStore> {
    use klipp_analysis::assets::{self, AssetKind};
    use klipp_effects::rolling_shutter::{Homography, RollingShutterData};
    use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, StabilizationData};
    let deflicker = klipp_effects::deflicker::DeflickerData { luma: (0..48).map(|i| 0.35 + 0.25 * ((i % 4) as f32) / 3.0).collect(), fps: 24.0 };
    let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 6.0 } else { -2.0 }], [0.0, 1.0, if i.is_multiple_of(5) { 3.0 } else { 0.5 }], [0.0, 0.0, 1.0]] };
    let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
    let accumulated = accumulate_trajectory(&frame_transforms);
    let trajectory = accumulated.iter().map(decompose_homography).collect();
    let stabilisation = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
    let moved = |dx: f64, dy: f64| Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] };
    let shutter = RollingShutterData { motion: vec![(moved(-6.0, -0.5), moved(6.0, 0.5)); 48], fps: 24.0 };
    let mut store = assets::MemoryStore::default();
    for (kind, data) in [
        (AssetKind::Deflicker, serde_json::to_value(deflicker)),
        (AssetKind::Stabilisation, serde_json::to_value(stabilisation)),
        (AssetKind::RollingShutter, serde_json::to_value(shutter)),
    ] {
        if let (Some(key), Ok(data)) = (stand_in_key(kind), data) {
            let _ = assets::store(&mut store, key, data);
        }
    }
    Box::new(store)
}

/// A renderer for the oracle, on the GPU or not, holding its analyses.
fn oracle_renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_asset_store(analyses());
    r
}

/// Where the oracle's time goes, summed over a process: a renderer made,
/// a frame rendered, on each route, and the egui driving the families
/// time themselves. Read with [`timing_report`].
pub(super) mod timing {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(in super::super) static CPU_NEW: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    pub(in super::super) static CPU_FRAME: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    pub(in super::super) static GPU_NEW: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    pub(in super::super) static GPU_FRAME: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    pub(in super::super) static DRIVING: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

    /// Add the time since `t0` to `phase`: its nanoseconds and its count.
    pub(in super::super) fn add(phase: &[AtomicU64; 2], t0: std::time::Instant) {
        phase[0].fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        phase[1].fetch_add(1, Ordering::Relaxed);
    }

    /// Each phase's total seconds and count, one line.
    pub(in super::super) fn report() -> String {
        let line = |name: &str, p: &[AtomicU64; 2]| format!("{name} {:.1} s / {}", p[0].load(Ordering::Relaxed) as f64 / 1e9, p[1].load(Ordering::Relaxed));
        [line("cpu renderer made", &CPU_NEW), line("cpu frame", &CPU_FRAME), line("gpu renderer made", &GPU_NEW), line("gpu frame", &GPU_FRAME), line("egui driving", &DRIVING)].join("; ")
    }
}

/// One clip's frame on each route.
#[derive(Clone, PartialEq)]
pub(super) struct Frames {
    /// The CPU executor's frame: linear RGBA floats.
    pub cpu: Vec<f32>,
    /// The native GPU stack's display frame: RGBA bytes.
    pub gpu: Vec<u8>,
}

/// `clip` alone on a sequence of the oracle's size.
fn sequence(clip: &Clip, (width, height): (u32, u32)) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width, height };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    // A clip that reads what is below it (#1746) is drawn over a track of
    // the same source a second on: a picture of its own to read.
    if klipp_compose::resolve::reads_below(clip) {
        let mut below = Clip { kind: klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(clip.timeline_in, clip.timeline_out) };
        let second = klipp_core::RationalTime::from_secs_f64(1.0, clip.timeline_in.rate.max(1));
        below.source_in = clip.source_in + second;
        below.source_out = clip.source_out + second;
        let mut track = Track::new("V0", TrackKind::Video);
        track.clips = vec![below];
        seq.tracks.push(track);
    }
    seq
}

/// `clip` drawn on the CPU executor: linear RGBA floats, full, finite,
/// not flat, and read from the clip's source.
fn render_cpu(seq: &Sequence, size: (u32, u32)) -> Result<Vec<f32>, String> {
    render_cpu_at(seq, size, AT)
}

thread_local! {
    /// The test thread's renderers, one per route, kept across frames: a
    /// new one pays for its pipelines on its first frame, about a tenth of
    /// a second on the GPU, which made renderer setup most of a family's
    /// time. What a renderer keeps between frames is safe to keep here:
    /// its effect plans, pins and shutter samples are rebuilt each frame,
    /// and its per-clip motion and neighbour-frame providers read the
    /// oracle's source, the same for every clip, by clip ids no two cases
    /// share. Leaked, one per route and test thread: a GPU renderer
    /// dropped while the thread's locals are torn down panics, which
    /// aborts the test process.
    static CPU: std::cell::RefCell<Option<&'static mut Renderer>> = const { std::cell::RefCell::new(None) };
    static GPU: std::cell::RefCell<Option<&'static mut Renderer>> = const { std::cell::RefCell::new(None) };
}

/// `f` with the thread's renderer for a route, made the first time.
fn with_renderer<R>(slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<&'static mut Renderer>>>, gpu: bool, f: impl FnOnce(&mut Renderer) -> R) -> R {
    slot.with(|cell| {
        let mut held = cell.borrow_mut();
        let renderer = held.get_or_insert_with(|| {
            let t0 = std::time::Instant::now();
            let r = oracle_renderer(gpu);
            timing::add(if gpu { &timing::GPU_NEW } else { &timing::CPU_NEW }, t0);
            Box::leak(Box::new(r))
        });
        f(renderer)
    })
}

/// [`render_cpu`] at `secs`.
fn render_cpu_at(seq: &Sequence, size: (u32, u32), secs: f64) -> Result<Vec<f32>, String> {
    let (w, h) = size;
    let mut src = Synthetic { size, reads: 0 };
    let t0 = std::time::Instant::now();
    let (frame, _) = with_renderer(&CPU, false, |cpu| cpu.render_inspected(seq, time(secs), w, h, &mut src, RenderOptions::default(), Inspect::default()));
    timing::add(&timing::CPU_FRAME, t0);
    if src.reads == 0 {
        return Err("the CPU frame never read the clip's source".into());
    }
    if frame.len() != (w * h * 4) as usize || frame.iter().any(|v| !v.is_finite()) {
        return Err(format!("the CPU frame isn't a full, finite {w}x{h} frame ({} values)", frame.len()));
    }
    if frame.chunks_exact(4).all(|p| p == &frame[..4]) {
        return Err("the CPU frame is flat".into());
    }
    Ok(frame)
}

/// `clip` drawn on the native GPU stack: display RGBA bytes, or an error
/// beginning `BLOCKED` when the stack declines the frame or the frame
/// leaves the native route.
fn render_gpu(seq: &Sequence, size: (u32, u32)) -> Result<Vec<u8>, String> {
    render_gpu_at(seq, size, AT)
}

/// [`render_gpu`] at `secs`.
fn render_gpu_at(seq: &Sequence, size: (u32, u32), secs: f64) -> Result<Vec<u8>, String> {
    let (w, h) = size;
    let mut src = Synthetic { size, reads: 0 };
    let t0 = std::time::Instant::now();
    let (rendered, decline) = with_renderer(&GPU, true, |gpu| {
        let rendered = gpu.render_display_u8_on_gpu(seq, time(secs), w, h, &mut src, RenderOptions::default());
        (rendered, gpu.last_decline())
    });
    timing::add(&timing::GPU_FRAME, t0);
    let Some((bytes, stats)) = rendered else {
        return Err(format!("BLOCKED: the GPU stack declined the frame: {}", decline.unwrap_or("no reason given")));
    };
    let off_route = [
        ("layers drawn on the CPU", stats.cpu_layers),
        ("effect runs on the CPU kernels", stats.effects_on_cpu_kernels),
        ("failed plan runs", stats.effects_on_cpu),
        ("fallback effect lists", stats.effects_fallback_lists),
        ("masks unavailable", stats.effects_mask_unavailable),
    ];
    if let Some((what, n)) = off_route.iter().find(|(_, n)| *n > 0) {
        return Err(format!("BLOCKED: the GPU frame left the native route: {n} {what}"));
    }
    if src.reads == 0 {
        return Err("the GPU frame never read the clip's source".into());
    }
    if bytes.len() != (w * h * 4) as usize {
        return Err(format!("the GPU frame isn't a full {w}x{h} frame ({} bytes)", bytes.len()));
    }
    if bytes.chunks_exact(4).all(|p| p == &bytes[..4]) {
        return Err("the GPU frame is flat".into());
    }
    Ok(bytes)
}

/// `seq` drawn on the CPU and on the native GPU stack, or why it can't
/// be (an error beginning `BLOCKED` when the GPU route isn't available or
/// wasn't kept to).
fn render_sequence(seq: &Sequence, size: (u32, u32)) -> Result<Frames, String> {
    Ok(Frames { cpu: render_cpu(seq, size)?, gpu: render_gpu(seq, size)? })
}

/// Which routes an edit is drawn on: both, or the CPU alone for a family
/// whose production route is the CPU by design (its GPU half is declared
/// not applicable, citing the step that moves it, never BLOCKED).
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes {
    Both,
    CpuByDesign,
}

/// The CPU half of [`same_edit`]: the edit changes the CPU frame, and the
/// two views' clips draw it bit for bit alike.
fn same_on_cpu(before: &Clip, old: &Clip, new: &Clip, size: (u32, u32)) -> Result<(), String> {
    let cpu = |c: &Clip| render_cpu(&sequence(c, size), size);
    let (before, old, new) = (cpu(before)?, cpu(old)?, cpu(new)?);
    if new == before {
        return Err("the edit doesn't change the CPU frame".into());
    }
    if let Some(i) = (0..new.len()).find(|&i| new[i].to_bits() != old[i].to_bits()) {
        return Err(format!("old and generated CPU frames differ, first at value {i}: {} vs {}", old[i], new[i]));
    }
    Ok(())
}

/// Whether `a` and `b` draw different CPU frames: a fixture's positive
/// control, that the frame can show the change a case looks for at all.
pub(super) fn cpu_frames_differ(a: &Clip, b: &Clip, size: (u32, u32)) -> Result<bool, String> {
    Ok(render_cpu(&sequence(a, size), size)? != render_cpu(&sequence(b, size), size)?)
}

/// `clip`'s frame at `secs` on the routes `routes` names: the CPU's, and
/// the native GPU stack's unless the route is the CPU by design. For a
/// case that compares frames at times of its own (a key lane).
pub(super) fn frames_at(routes: Routes, size: (u32, u32), clip: &Clip, secs: f64) -> Result<(Vec<f32>, Option<Vec<u8>>), String> {
    let seq = sequence(clip, size);
    let cpu = render_cpu_at(&seq, size, secs)?;
    let gpu = match routes {
        Routes::Both => Some(render_gpu_at(&seq, size, secs)?),
        Routes::CpuByDesign => None,
    };
    Ok((cpu, gpu))
}

/// [`same_edit`] on `routes`, drawn at `size`.
pub(super) fn same_edit_on(routes: Routes, size: (u32, u32), before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    match routes {
        Routes::Both => same_edit_at(size, before, old, new),
        Routes::CpuByDesign => same_on_cpu(before, old, new, size),
    }
}

/// The Pixels bar for one edit: `old` and `new` (the clip after the edit
/// in the old view and in the generated one) draw `before`'s clip changed,
/// and the same frame as each other, on each route.
pub(super) fn same_edit(before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    same_edit_at(SMALL, before, old, new)
}

/// [`same_edit`], drawn at `size`.
fn same_edit_at(size: (u32, u32), before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    let on = |clip: &Clip| sequence(clip, size);
    same_sequence_edit_at(size, &on(before), &on(old), &on(new))
}

fn same_sequence_edit_at(size: (u32, u32), before: &Sequence, old: &Sequence, new: &Sequence) -> Result<(), String> {
    let (before, old, new) = (render_sequence(before, size)?, render_sequence(old, size)?, render_sequence(new, size)?);
    if new.cpu == before.cpu {
        return Err("the edit doesn't change the CPU frame".into());
    }
    if new.gpu == before.gpu {
        return Err("the edit doesn't change the GPU frame".into());
    }
    if let Some(i) = (0..new.cpu.len()).find(|&i| new.cpu[i].to_bits() != old.cpu[i].to_bits()) {
        return Err(format!("old and generated CPU frames differ, first at value {i}: {} vs {}", old.cpu[i], new.cpu[i]));
    }
    if let Some(i) = (0..new.gpu.len()).find(|&i| new.gpu[i] != old.gpu[i]) {
        return Err(format!("old and generated GPU frames differ, first at byte {i}: {} vs {}", old.gpu[i], new.gpu[i]));
    }
    Ok(())
}

/// The CPU frame as a display shows it: the bytes pixel_check compares,
/// through the renderer's working space (`WorkingColorspace::default`).
fn cpu_display(linear: &[f32]) -> Vec<u8> {
    // The viewer's CPU encode, dithered as the GPU's display encode is
    // since #2072: against an undithered encode the dither alone spends the
    // level the routes are held to.
    klipp_color::display::display_transform_ws_dithered_to(linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::default(), klipp_core::SdrTransfer::Srgb)
}

thread_local! {
    /// Set while a case pinned as a known CPU/GPU defect runs: its frames
    /// must still change, but the routes aren't held to agree.
    static ROUTES_APART: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` for a case whose type is a known CPU/GPU defect (its Pixels key
/// excluded, citing its issue): the edit must still change the frame on
/// each route, but the CPU's isn't held to the GPU's.
pub(super) fn with_routes_apart<R>(f: impl FnOnce() -> R) -> R {
    ROUTES_APART.with(|c| c.set(true));
    let out = f();
    ROUTES_APART.with(|c| c.set(false));
    out
}

/// Whether the CPU frame, as a display shows it, is the GPU stack's frame
/// within pixel_check's bar: no channel more than a level apart.
fn cpu_matches_gpu(frames: &Frames) -> Result<(), String> {
    if ROUTES_APART.with(|c| c.get()) {
        return Ok(());
    }
    let cpu = cpu_display(&frames.cpu);
    if cpu.len() != frames.gpu.len() {
        return Err(format!("the CPU and GPU frames differ in size ({} and {} bytes)", cpu.len(), frames.gpu.len()));
    }
    let over: Vec<usize> = (0..cpu.len()).filter(|&i| cpu[i].abs_diff(frames.gpu[i]) > 1).collect();
    match over.first() {
        None => Ok(()),
        Some(&i) => Err(format!("the CPU and GPU frames differ by more than a level at {} values, first at byte {i}: {} vs {}", over.len(), cpu[i], frames.gpu[i])),
    }
}

/// Whether `clip`'s frame at `secs` is drawn alike on the CPU and the GPU
/// stack, within pixel_check's bar; the CPU alone has nothing to agree
/// with.
pub(super) fn routes_agree_at(routes: Routes, size: (u32, u32), clip: &Clip, secs: f64) -> Result<(), String> {
    match frames_at(routes, size, clip, secs)? {
        (cpu, Some(gpu)) => cpu_matches_gpu(&Frames { cpu, gpu }),
        (_, None) => Ok(()),
    }
}

/// The Pixels bar for one edit of the generated view (M3, the old view no
/// longer compared): drawn on the CPU executor and the native GPU stack,
/// `after` changes the frame `before` draws on each route, and the CPU's
/// frame is the GPU's within a level, before and after. Sequences are
/// flattened, as the engine is given them.
pub(super) fn edit_drawn(before: &Sequence, after: &Sequence) -> Result<(), String> {
    let sized = |seq: &Sequence| {
        let mut seq = klipp_timeline::flatten::flatten_sequence(seq, &[]);
        seq.resolution = klipp_core::Resolution { width: SMALL.0, height: SMALL.1 };
        seq
    };
    drawn_changed(&render_sequence(&sized(before), SMALL)?, &render_sequence(&sized(after), SMALL)?)
}

/// The Pixels bar over two drawn frames: the edit changes the CPU frame
/// and the GPU frame, and the CPU's is the GPU's within a level, before
/// and after.
fn drawn_changed(before: &Frames, after: &Frames) -> Result<(), String> {
    if after.cpu == before.cpu {
        return Err("the edit doesn't change the CPU frame".into());
    }
    if after.gpu == before.gpu {
        return Err("the edit doesn't change the GPU frame".into());
    }
    cpu_matches_gpu(before).map_err(|e| format!("before the edit: {e}"))?;
    cpu_matches_gpu(after).map_err(|e| format!("after the edit: {e}"))
}

/// [`edit_drawn`] for one clip, alone on a sequence of the oracle's size.
pub(super) fn clip_edit_drawn(before: &Clip, after: &Clip) -> Result<(), String> {
    clip_edit_drawn_on(Routes::Both, SMALL, before, after)
}

/// [`clip_edit_drawn`] on `routes` at `size`: for a type the CPU draws by
/// design, the edit changes the CPU frame (its GPU half not applicable);
/// on both, the bar of [`edit_drawn`], drawn at `size`.
pub(super) fn clip_edit_drawn_on(routes: Routes, size: (u32, u32), before: &Clip, after: &Clip) -> Result<(), String> {
    match routes {
        Routes::CpuByDesign => {
            if !cpu_frames_differ(before, after, size)? {
                return Err("the edit doesn't change the CPU frame".into());
            }
            Ok(())
        }
        Routes::Both => drawn_changed(&render_sequence(&sequence(before, size), size)?, &render_sequence(&sequence(after, size), size)?),
    }
}

/// The synthetic source for clips whose analyses the app holds: each
/// media clip keyed as the app keys it (its media in `state`'s pool, by
/// `asset_key`), so a frame is drawn from the analysis the app holds and
/// nothing else.
struct Held<'a> {
    inner: Synthetic,
    state: &'a crate::state::AppState,
}

impl FrameSource for Held<'_> {
    fn frame(&mut self, clip: &Clip, secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.inner.frame(clip, secs, width, height)
    }

    fn temporal_frames_linear(&mut self, clip: &Clip, working: klipp_core::WorkingColorspace) -> Option<klipp_compose::LinearFrameProviderFn> {
        self.inner.temporal_frames_linear(clip, working)
    }

    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        klipp_analysis::assets::asset_key(kind, self.state.media_pool.get(&clip.media_id()?)?)
    }

    fn media_frame(&mut self, media: &str, secs: f64, working: klipp_core::WorkingColorspace, width: u32, height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        self.inner.media_frame(media, secs, working, width, height)
    }
}

/// What an analysis landing draws (M4 step 9): `clip`'s frame from
/// renderers holding no analysis, and from renderers holding exactly the
/// analyses `state` holds, its clips keyed as the app keys them, on
/// `routes`: the analysis the app holds changes the frame, and with it the
/// CPU's frame is within a level of the GPU's.
pub(super) fn analysis_drawn_on(routes: Routes, size: (u32, u32), clip: &Clip, state: &crate::state::AppState) -> Result<(), String> {
    let seq = sequence(clip, size);
    let holding = |gpu: bool, held: bool| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let mut store = klipp_analysis::assets::MemoryStore::default();
        if held {
            for (key, data) in &state.analyses {
                let _ = klipp_analysis::assets::store(&mut store, key.clone(), (**data).clone());
            }
        }
        r.set_asset_store(Box::new(store));
        r
    };
    let draw = |gpu: bool, held: bool| -> Result<(Vec<f32>, Vec<u8>), String> {
        let mut src = Held { inner: Synthetic { size, reads: 0 }, state };
        if gpu {
            let (bytes, _) = holding(true, held)
                .render_display_u8_on_gpu(&seq, at(), size.0, size.1, &mut src, RenderOptions::default())
                .ok_or("BLOCKED: the GPU stack declined the frame")?;
            Ok((Vec::new(), bytes))
        } else {
            let (frame, _) = holding(false, held).render_inspected(&seq, at(), size.0, size.1, &mut src, RenderOptions::default(), Inspect::default());
            Ok((frame, Vec::new()))
        }
    };
    let (before, after) = (draw(false, false)?.0, draw(false, true)?.0);
    match routes {
        Routes::CpuByDesign => {
            if before == after {
                return Err("the held analysis doesn't change the CPU frame".into());
            }
            Ok(())
        }
        Routes::Both => drawn_changed(&Frames { cpu: before, gpu: draw(true, false)?.1 }, &Frames { cpu: after, gpu: draw(true, true)?.1 }),
    }
}

/// How long the GPU stack takes over `clip`'s frame at `size`, as a median
/// of `runs`, and how many layers it drew on the CPU: a cost probe, not a
/// parity check.
pub(super) fn gpu_frame_cost(clip: &Clip, size: (u32, u32), runs: usize) -> Option<(f64, u32)> {
    let seq = sequence(clip, size);
    let mut src = Synthetic { size, reads: 0 };
    let mut gpu = oracle_renderer(true);
    gpu.render_display_u8_on_gpu(&seq, at(), size.0, size.1, &mut src, RenderOptions::default())?;
    let mut times = Vec::with_capacity(runs);
    let mut cpu_layers = 0;
    for _ in 0..runs {
        let t0 = std::time::Instant::now();
        let (_, stats) = gpu.render_display_u8_on_gpu(&seq, at(), size.0, size.1, &mut src, RenderOptions::default())?;
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        cpu_layers = stats.cpu_layers;
    }
    times.sort_by(f64::total_cmp);
    Some((times[times.len() / 2], cpu_layers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect as E;
    use klipp_timeline::test_clip::{self, TestClip};

    fn clip(effects: &[E]) -> Clip {
        TestClip::media(2.0).effects(effects.iter().cloned()).build()
    }

    /// The cost of the stack's temporal gate for M4 step 9: a stabilised
    /// clip's frame on the GPU stack, without analysis and with it, against
    /// the same clip with no effect. Run by hand (`--ignored --nocapture`),
    /// in a release build for numbers that mean anything.
    #[test]
    #[ignore = "a timing probe, run by hand; not a gate"]
    fn stabilised_layer_cost() {
        // The oracle's renderers hold the stabiliser's analysis for every
        // media clip (`analyses`).
        let plain = clip(&[]);
        let analysed = clip(&[E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]);
        for (name, c) in [("no effect", &plain), ("stabiliser, analysed", &analysed)] {
            match gpu_frame_cost(c, FULL, 15) {
                Some((ms, cpu_layers)) => eprintln!("{name}: {ms:.2} ms median at 1920x1080, cpu_layers {cpu_layers}"),
                None => eprintln!("{name}: the GPU stack declined"),
            }
        }
    }

    /// `clip`'s frame at `secs` from renderers made for it alone: what the
    /// thread's shared ones must draw too.
    fn fresh_frames(clip: &Clip, size: (u32, u32), secs: f64) -> (Vec<f32>, Vec<u8>) {
        let seq = sequence(clip, size);
        let fresh = oracle_renderer;
        let mut src = Synthetic { size, reads: 0 };
        let (cpu, _) = fresh(false).render_inspected(&seq, time(secs), size.0, size.1, &mut src, RenderOptions::default(), Inspect::default());
        let mut src = Synthetic { size, reads: 0 };
        let (gpu, _) = fresh(true).render_display_u8_on_gpu(&seq, time(secs), size.0, size.1, &mut src, RenderOptions::default()).expect("a GPU stack frame");
        (cpu, gpu)
    }

    /// The shared renderers keep nothing a case could read as another's:
    /// for a clip of each kind the families draw (a plain effect, a keyed
    /// one, a placed one), the shared frame is a fresh renderer's on each
    /// route; and after another clip of the same id drawn in between, a
    /// clip draws as it did before it.
    #[test]
    fn the_shared_renderers_draw_what_fresh_ones_draw() {
        let plain = clip(&[E::Gamma { gamma: 1.6 }]);
        let keyed = TestClip::media(2.0).effect(E::GaussianBlur { radius: 2.0 }).key(0, "radius", 0.25, 2.0).key(0, "radius", 1.25, 12.0).build();
        let placed = clip(&[E::Transform { position_x: 40.0, position_y: -20.0, scale_x: 0.7, scale_y: 0.7, rotation: 15.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }]);
        for (name, c) in [("plain", &plain), ("keyed", &keyed), ("placed", &placed)] {
            let (cpu, gpu) = frames_at(Routes::Both, SMALL, c, 0.75).unwrap();
            let (fresh_cpu, fresh_gpu) = fresh_frames(c, SMALL, 0.75);
            assert!(cpu == fresh_cpu, "{name}: the shared CPU renderer draws another frame");
            assert_eq!(gpu.as_deref(), Some(fresh_gpu.as_slice()), "{name}: the shared GPU renderer draws another frame");
        }
        // A, then B (the same clip id, another effect), then A again.
        let mut other = plain.clone();
        let id = test_clip::effect_id(&other, 0);
        other.replace_effect(id, E::Gamma { gamma: 0.6 });
        let a = frames_at(Routes::Both, SMALL, &plain, AT).unwrap();
        let b = frames_at(Routes::Both, SMALL, &other, AT).unwrap();
        assert!(a != b, "the control: B draws another frame");
        assert!(frames_at(Routes::Both, SMALL, &plain, AT).unwrap() == a, "A drew differently after B");
    }

    /// The oracle's own controls: an edit that draws nothing is refused,
    /// an edit drawn differently by the two views is refused, and the same
    /// edit passes.
    #[test]
    fn the_oracle_refuses_an_unseen_edit_and_a_different_one() {
        let before = clip(&[E::Gamma { gamma: 1.0 }]);
        let seen = clip(&[E::Gamma { gamma: 1.6 }]);
        let other = clip(&[E::Gamma { gamma: 1.3 }]);
        match same_edit(&before, &seen, &seen) {
            Err(e) if e.starts_with("BLOCKED") => panic!("{e}"),
            r => r.expect("the same seen edit passes"),
        }
        let unseen = same_edit(&before, &before, &before).unwrap_err();
        assert!(unseen.contains("doesn't change"), "{unseen}");
        let differ = same_edit(&before, &other, &seen).unwrap_err();
        assert!(differ.contains("differ"), "{differ}");
    }
}
