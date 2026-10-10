//! The routes a family's cases render through, headless (level 1: the
//! renderer's routes). Shared by every family.
//!
//! A [`Session`] is one render owner: one `Renderer`, kept across frames,
//! so plug-in instances and their history live as they do in the app, and
//! frames come in the order asked. It is fed by a [`Source`] of known
//! frames, its neighbours included, so a case computes its expectation
//! from what it put in.
//!
//! - **Viewer**: the viewer's call, the GPU stack's display frame
//!   (`render_display_u8_on_gpu`), and the timeline render
//!   (`render_inspected`) when the stack declines, as the engine does.
//! - **Export**: export's call (`render_inspected` with no inspection, as
//!   `ExportFrames::frame` renders when its YUV path declines).
//! - A session with the GPU off is the CPU twin of either, for
//!   `CpuGpuAgree`.
//!
//! Level 1 never earns `ViewerEqualsExport`. The viewer and export differ
//! most in their frame sources (decode, bit depth, the sRGB and linear
//! hand-off), which a source of known frames replaces. That key is level
//! 2's: the engine headless against an export decoded back.

use std::sync::Arc;

use klipp_compose::render::{FrameSource, LinearFrameProviderFn, RenderStats, SourceFrame, SourcePixels};
use klipp_compose::Renderer;
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, Sequence};

use super::Route;

/// A frame of known pixels: display-referred linear RGBA f32 in the
/// working space, straight alpha, `width × height`.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownFrame {
    pub linear: Vec<f32>,
    pub width: u32,
    pub height: u32,
    /// The frame's presentation time in the media, as a decoder reports
    /// it (a decoder asked for `t` gives the frame whose time is at or
    /// before `t`). Frame blending weighs two frames by it; `None` is
    /// unknown, and the frame is shown unblended.
    pub pts: Option<f64>,
}

/// Where a session's frames come from, by media and source time. The
/// renderer's current frame and every neighbour reader (`temporal_frames`,
/// `temporal_frames_linear`) read it, unresampled: `None` is a missing
/// frame, and a frame of another size reaches the renderer as that size.
pub trait Source: Send + Sync {
    fn frame(&self, media: MediaId, source_secs: f64) -> Option<KnownFrame>;

    /// The frame after the one at `after_pts`, for frame blending. By
    /// default the frame one media frame on (`fps` is the media's).
    fn next(&self, media: MediaId, after_pts: f64, fps: f64) -> Option<KnownFrame> {
        self.frame(media, after_pts + 1.0 / fps)
    }

    /// The media's decodable range, handles included, in source seconds;
    /// `None` to read it from the scene's pool.
    fn range(&self, _media: MediaId) -> Option<(f64, f64)> {
        None
    }
}

/// What a session renders: a sequence at a size, the pool its media is
/// described in, and the only folders its OFX plug-ins are found in
/// (empty: none). A case never sees the plug-ins installed on the machine.
#[derive(Clone)]
pub struct Scene {
    pub sequence: Sequence,
    pub pool: klipp_media::MediaPool,
    pub width: u32,
    pub height: u32,
    pub plugin_dirs: Vec<std::path::PathBuf>,
}

/// One rendered frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Timeline frame index.
    pub frame: i64,
    /// Display bytes, sRGB RGBA8: what the viewer shows, or export's
    /// linear frame encoded.
    pub srgb8: Vec<u8>,
    /// The linear frame, when the route rendered one (not the GPU stack's
    /// display frame).
    pub linear: Option<Vec<f32>>,
    pub width: u32,
    pub height: u32,
    pub stats: RenderStats,
    /// What the render declared: the GPU stack's decline, inspection
    /// requests it couldn't answer.
    pub notes: Vec<String>,
}

impl Frame {
    /// Whether the route drew only display bytes (the viewer's GPU stack),
    /// with no linear frame to compare: a case then compares in sRGB8, to
    /// within a byte.
    pub fn display_only(&self) -> bool {
        self.linear.is_none()
    }

    /// How far this frame is from `want` (display-referred linear, the
    /// working space, `width × height`): the largest channel difference in
    /// linear when the route rendered a linear frame, else `want` encoded
    /// to sRGB8 against `srgb8`, in bytes / 255. A length mismatch is
    /// infinitely far.
    pub fn off(&self, want: &[f32]) -> f32 {
        match &self.linear {
            Some(linear) if linear.len() == want.len() => linear.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max),
            Some(_) => f32::INFINITY,
            None => {
                let want = klipp_effects::linear_f32_rgba_to_srgb_u8(want);
                if want.len() != self.srgb8.len() {
                    return f32::INFINITY;
                }
                want.iter().zip(&self.srgb8).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0) as f32 / 255.0
            }
        }
    }
}

/// One render owner on one route.
pub struct Session {
    route: Route,
    renderer: Renderer,
    scene: Arc<Scene>,
    source: Arc<dyn Source>,
}

impl Session {
    /// A render owner on `route`, with the GPU or without.
    pub fn new(route: Route, gpu: bool, scene: Arc<Scene>, source: Arc<dyn Source>) -> Self {
        let mut ofx = klipp_plugin::OfxHost::new();
        ofx.discover_plugins_in(&scene.plugin_dirs);
        let mut renderer = Renderer::new(ofx, klipp_title::TitleRenderer::new());
        renderer.set_gpu_enabled(gpu);
        Self { route, renderer, scene, source }
    }

    /// The session's renderer, to set what a case needs before its first
    /// frame (an asset store, a working space).
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// The same render owner (its renderer, and so its plug-in instances)
    /// rendering another scene from another source: a render-scale change,
    /// or an edit, mid-session.
    pub fn with_scene(mut self, scene: Arc<Scene>, source: Arc<dyn Source>) -> Self {
        self.scene = scene;
        self.source = source;
        self
    }

    /// The timeline's frame `frame`, rendered now: call frames in the order
    /// a play, a seek or an export would ask them.
    pub fn frame(&mut self, frame: i64) -> Frame {
        let scene = Arc::clone(&self.scene);
        let seq = &scene.sequence;
        let time = RationalTime { value: frame * seq.fps.den as i64, rate: seq.fps.num };
        let (w, h) = (scene.width, scene.height);
        let mut src = KnownSource { source: Arc::clone(&self.source), scene: Arc::clone(&scene) };
        let opts = klipp_compose::RenderOptions::default();
        let mut notes = Vec::new();
        if self.route == Route::Viewer {
            if let Some((srgb8, stats)) = self.renderer.render_display_u8_on_gpu(seq, time, w, h, &mut src, opts) {
                return Frame { frame, srgb8, linear: None, width: w, height: h, stats, notes };
            }
            if let Some(why) = self.renderer.last_decline() {
                notes.push(format!("gpu stack declined: {why}"));
            }
        }
        let (linear, found) = self.renderer.render_inspected(seq, time, w, h, &mut src, opts, Default::default());
        notes.extend(found.unavailable.iter().map(|(at, why)| format!("unavailable {at:?}: {why}")));
        let srgb8 = klipp_effects::linear_f32_rgba_to_srgb_u8(&linear);
        Frame { frame, srgb8, linear: Some(linear), width: w, height: h, stats: found.stats, notes }
    }
}

/// `frames` rendered on the viewer's route and on export's, each by its
/// own render owner, with the GPU: (viewer, export).
pub fn both(scene: Arc<Scene>, source: Arc<dyn Source>, frames: &[i64]) -> (Vec<Frame>, Vec<Frame>) {
    let run = |route| {
        let mut s = Session::new(route, true, Arc::clone(&scene), Arc::clone(&source));
        frames.iter().map(|&f| s.frame(f)).collect()
    };
    (run(Route::Viewer), run(Route::Export))
}

/// The renderer's `FrameSource` over a [`Source`].
struct KnownSource {
    source: Arc<dyn Source>,
    scene: Arc<Scene>,
}

impl KnownSource {
    /// The media a clip shows, and the source time a neighbour reader's
    /// time is (readers count from the clip's source in-point).
    fn media(clip: &Clip) -> Option<MediaId> {
        clip.media_id()
    }
}

impl FrameSource for KnownSource {
    fn frame(&mut self, clip: &Clip, source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        let f = self.source.frame(Self::media(clip)?, source_secs)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(f.linear)), width: f.width, height: f.height, pts: f.pts })
    }

    fn frame_after(&mut self, clip: &Clip, source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        let media = Self::media(clip)?;
        let pts = self.source.frame(media, source_secs)?.pts?;
        let fps = self.scene.pool.get(&media).map_or(25.0, |m| m.fps.as_f64());
        let f = self.source.next(media, pts, fps)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(f.linear)), width: f.width, height: f.height, pts: f.pts })
    }

    fn temporal_frames(&mut self, clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        let (media, source, start) = (Self::media(clip)?, Arc::clone(&self.source), clip.source_in.to_secs_f64());
        Some(Box::new(move |t| {
            let f = source.frame(media, start + t)?;
            Some((klipp_effects::linear_f32_rgba_to_srgb_u8(&f.linear), f.width, f.height))
        }))
    }

    fn temporal_frames_linear(&mut self, clip: &Clip, _working: WorkingColorspace) -> Option<LinearFrameProviderFn> {
        let (media, source, start) = (Self::media(clip)?, Arc::clone(&self.source), clip.source_in.to_secs_f64());
        Some(Box::new(move |t| {
            let f = source.frame(media, start + t)?;
            Some((f.linear, f.width, f.height))
        }))
    }
}

impl KnownSource {
    /// The media's decodable range: the source's, else the pool's
    /// duration from 0. Answers `FrameSource::media_range` once that
    /// lands (step 6).
    #[allow(dead_code)]
    fn range(&self, clip: &Clip) -> Option<(f64, f64)> {
        let media = Self::media(clip)?;
        self.source.range(media).or_else(|| self.scene.pool.get(&media).map(|m| (0.0, m.duration.to_secs_f64())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey frame whose level is its source time, so a case can see
    /// which frame came.
    struct Clock;

    impl Source for Clock {
        fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
            (source_secs >= 0.0).then(|| {
                let v = (source_secs as f32 / 10.0).min(1.0);
                KnownFrame { linear: [v, v, v, 1.0].repeat(16 * 8), width: 16, height: 8, pts: Some((source_secs * 25.0 + 1e-9).floor() / 25.0) }
            })
        }
    }

    fn scene() -> Arc<Scene> {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/known/clock.mov".into(), None);
        item.duration = RationalTime { value: 250, rate: 25 };
        item.fps = klipp_core::Fps::new(25, 1);
        let media = item.id;
        pool.add(item);
        let mut sequence = Sequence::new_default();
        sequence.fps = klipp_core::Fps::new(25, 1);
        let tc = |f: i64| RationalTime { value: f, rate: 25 };
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![Clip::media(media, tc(0), tc(100), tc(25), tc(125))];
        sequence.tracks = vec![v1];
        Arc::new(Scene { sequence, pool, width: 16, height: 8, plugin_dirs: Vec::new() })
    }

    /// The source's frames reach the render, from the clip's source
    /// in-point, on both routes, in the order asked, and a neighbour
    /// reader reads the same source with the in-point added.
    #[test]
    fn a_session_renders_the_known_frames_in_order() {
        let scene = scene();
        for gpu in [false, true] {
            for route in [Route::Viewer, Route::Export] {
                let mut s = Session::new(route, gpu, Arc::clone(&scene), Arc::new(Clock));
                let a = s.frame(0);
                let b = s.frame(25);
                assert_eq!((a.width, a.height, a.srgb8.len()), (16, 8, 16 * 8 * 4), "{route:?} gpu {gpu}");
                // Frame 0 shows source 1 s (in-point 25 frames), frame 25
                // source 2 s: brighter.
                assert!(b.srgb8[0] > a.srgb8[0], "{route:?} gpu {gpu}: {} then {}", a.srgb8[0], b.srgb8[0]);
            }
        }
        let mut src = KnownSource { source: Arc::new(Clock), scene: Arc::clone(&scene) };
        let clip = &scene.sequence.tracks[0].clips[0];
        let mut read = src.temporal_frames_linear(clip, WorkingColorspace::Rec709Linear).expect("a reader");
        let (px, _, _) = read(0.5).expect("in range");
        assert!((px[0] - 0.15).abs() < 1e-6, "reader time 0.5 is source 1.5 s: {}", px[0]);
        assert_eq!(src.range(clip), Some((0.0, 10.0)), "the pool's duration");
        // The frame after: the next on the media's grid, carrying its time.
        let after = src.frame_after(clip, 1.01, 16, 8).expect("a next frame");
        assert_eq!(after.pts, Some(26.0 / 25.0), "1.01 s is frame 25 (1.0 s); the next is frame 26");
    }

    /// A frame's distance: in linear when it has one, else in sRGB8 bytes.
    #[test]
    fn a_frame_is_compared_in_what_it_has() {
        let want = vec![0.5f32, 0.25, 0.0, 1.0];
        let linear = Frame { frame: 0, srgb8: vec![], linear: Some(vec![0.5, 0.26, 0.0, 1.0]), width: 1, height: 1, stats: Default::default(), notes: vec![] };
        assert!((linear.off(&want) - 0.01).abs() < 1e-6);
        assert!(!linear.display_only());
        let mut bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&want);
        bytes[0] += 2;
        let display = Frame { frame: 0, srgb8: bytes, linear: None, width: 1, height: 1, stats: Default::default(), notes: vec![] };
        assert!(display.display_only());
        assert!((display.off(&want) - 2.0 / 255.0).abs() < 1e-6);
        assert_eq!(display.off(&[0.0; 8]), f32::INFINITY, "another size");
    }

    /// `both` is the viewer's frames, then export's, for the same frames.
    #[test]
    fn both_routes_render_the_same_frames() {
        let (viewer, export) = both(scene(), Arc::new(Clock), &[0, 1, 2]);
        assert_eq!(viewer.iter().map(|f| f.frame).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(export.iter().map(|f| f.frame).collect::<Vec<_>>(), [0, 1, 2]);
    }
}
