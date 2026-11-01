//! Bounce a track to a media file (#3264): the track's frames drawn alone
//! at the sequence's full size, with their alpha, and the sound linked to
//! its clips, into one file from its first clip to its last. Gaps between
//! clips are transparent and silent, so the clip that replaces the track
//! sits where its content was.
//!
//! What is baked in: the track's effects (the render copy carries them on
//! its clips) and each clip's opacity, into the alpha. What is not: the
//! track's blend mode and opacity, which the new track carries.
//!
//! The file is display-referred, as an export is: linear light through the
//! SDR encode, so highlights above white are clipped. Bouncing is SDR only
//! in this version (BT.709); an HDR or wide-gamut project is refused, and
//! says why.

use std::path::PathBuf;
use std::sync::Arc;

use klipp_compose::{FrameSource, Renderer};
use klipp_core::{ExportAudioCodec, ExportSettings, Timecode, WorkingColorspace};
use klipp_jobs::{JobPriority, ResourceClass, ThreadJob};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_timeline::Sequence;

pub use klipp_core::BounceFormat;

/// What the dialog and the guide say of the picture a bounce keeps.
pub const CLIPPING_NOTE: &str = "Highlights above white are clipped; grade before bouncing.";

/// Frames `first..end` of track `track` (at the sequence's rate): from its
/// first clip's first frame to its last clip's last. `None` with nothing
/// on it.
pub fn span(seq: &Sequence, track: usize) -> Option<(i64, i64)> {
    let frames = crate::freeze::track_frames(seq, track, Timecode { value: 0, rate: seq.fps.num.max(1) });
    let at = |t: &Timecode| t.value / i64::from(seq.fps.den.max(1));
    let first = frames.iter().map(at).min()?;
    let last = frames.iter().map(at).max()?;
    Some((first, last + 1))
}

/// Frame `f`'s time at `seq`'s rate.
fn frame_time(seq: &Sequence, f: i64) -> Timecode {
    Timecode { value: f * i64::from(seq.fps.den.max(1)), rate: seq.fps.num.max(1) }
}

/// Why track `track` of `seq` can't be bounced, or `None` if it can: not a
/// visible picture track, nothing on it, a project that isn't SDR BT.709,
/// or a frame that can't be drawn alone (it reads or writes other tracks'
/// picture), with its time.
pub fn refusal(seq: &Sequence, track: usize, working: WorkingColorspace) -> Option<String> {
    let Some(t) = seq.tracks.get(track) else { return Some("no such track".into()) };
    if !matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text) {
        return Some("only a picture track can be bounced".into());
    }
    if !t.visible {
        return Some("the track is hidden: show it to bounce it".into());
    }
    if working != WorkingColorspace::Rec709Linear {
        return Some("bouncing is SDR in this version: the file is BT.709, and this project works in a wider space. HDR and wide-gamut bouncing come later".into());
    }
    let Some((first, end)) = span(seq, track) else { return Some("nothing on the track to bounce".into()) };
    // A blend mode is laid against the tracks below, so it can't be baked:
    // the bounced clip takes the clips' one mode.
    if blend_mode(seq, track).is_none() {
        return Some("its clips blend in different ways: a bounced clip has one blend mode. Set them alike first".into());
    }
    (first..end).find_map(|f| {
        let at = frame_time(seq, f);
        klipp_compose::freeze::live_reason(seq, track, at).map(|why| format!("the frame at {:.2} s can't be drawn alone: {why}", at.to_secs_f64()))
    })
}

/// The one blend mode track `track`'s clips share, which the bounced clip
/// takes; `None` when they differ.
pub fn blend_mode(seq: &Sequence, track: usize) -> Option<klipp_core::BlendMode> {
    let t = seq.tracks.get(track)?;
    let first = t.clips.first()?.blend_mode;
    t.clips.iter().all(|c| c.blend_mode == first).then_some(first)
}

/// About how many bytes a ProRes 4444 bounce of `frames` frames at `size`
/// takes at most: its target rate is about 5.3 bits a pixel a frame with
/// alpha (330 Mb/s at 1080p 29.97); a simple picture takes less (a test
/// pattern, under half). `None` for FFV1, whose size follows the picture.
pub fn estimate_bytes(format: BounceFormat, size: (u32, u32), frames: i64) -> Option<u64> {
    match format {
        BounceFormat::ProRes4444 => Some((f64::from(size.0) * f64::from(size.1) * 5.3 / 8.0 * frames.max(0) as f64) as u64),
        BounceFormat::Ffv1 => None,
    }
}

/// What a bounce is asked to do.
pub struct BounceRequest {
    /// The render copy (`flatten_sequence`).
    pub seq: Arc<Sequence>,
    pub track: usize,
    /// Frames `first..end` at the sequence's rate ([`span`]).
    pub first: i64,
    pub end: i64,
    /// The sequence's full size.
    pub size: (u32, u32),
    pub working: WorkingColorspace,
    pub format: BounceFormat,
    /// Where the file goes: written beside it as `.partial`, renamed when
    /// whole.
    pub path: PathBuf,
    /// The linked sound, alone ([`klipp_render::stem::linked_sound_snapshot`]),
    /// or `None` for a silent file.
    pub sound: Option<SequenceSnapshot>,
}

/// How a bounce ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BounceResult {
    /// The file, when written whole.
    pub path: Option<PathBuf>,
    pub frames: usize,
    pub cancelled: bool,
    pub error: Option<String>,
}

impl BounceRequest {
    fn settings(&self) -> ExportSettings {
        ExportSettings {
            output_path: self.path.clone(),
            format: self.format.container(),
            video_codec: self.format.codec(),
            alpha: true,
            audio_codec: if self.sound.is_some() { ExportAudioCodec::Pcm24 } else { ExportAudioCodec::Copy },
            ..Default::default()
        }
    }

    /// The snapshot the encoder opens from: the sound's, at the full size,
    /// or one with no sound.
    fn snapshot(&self) -> SequenceSnapshot {
        let seq = &self.seq;
        let mut snap = self.sound.clone().unwrap_or_else(|| SequenceSnapshot {
            audio_buses: Vec::new(),
            fps: seq.fps.as_f64(),
            fps_num: seq.fps.num,
            fps_den: seq.fps.den,
            width: self.size.0,
            height: self.size.1,
            sample_rate: 48_000,
            duration_frames: self.end.max(0) as u64,
            export_range: None,
            video: Default::default(),
            audio_tracks: Vec::new(),
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![],
            chapters: vec![],
            working_colorspace: self.working,
            grade_encoding: Default::default(),
        });
        (snap.width, snap.height) = self.size;
        snap.duration_frames = snap.duration_frames.max(self.end.max(0) as u64);
        snap
    }

    /// Frame `f` drawn alone, its clip's opacity baked into its alpha:
    /// transparent where nothing is on the track.
    pub fn draw(&self, renderer: &mut Renderer, src: &mut dyn FrameSource, f: i64) -> Vec<f32> {
        let (w, h) = self.size;
        let at = frame_time(&self.seq, f);
        let drawn = renderer.render_track_layer_on_gpu(&self.seq, self.track, at, w, h, src).or_else(|| renderer.render_track_layer(&self.seq, self.track, at, w, h, src));
        let Some(mut pixels) = drawn else { return vec![0.0; (w * h * 4) as usize] };
        // A transition carries its clips' opacities in its own mix; a clip
        // alone has its opacity applied at the blend, which a bounce bakes.
        let layers = klipp_compose::active_layers(&self.seq, at);
        if let Some(layer) = layers.iter().find(|l| l.track == self.track && !l.transition) {
            if let [clip] = layer.clips.as_slice() {
                let opacity = klipp_compose::eval::evaluate_clip_opacity(clip, at, self.seq.fps.as_f64()).clamp(0.0, 1.0);
                if opacity < 1.0 {
                    pixels.chunks_exact_mut(4).for_each(|p| p[3] *= opacity);
                }
            }
        }
        pixels
    }
}

/// Bounce `req` in a background job: a frame at a time, drawn, encoded and
/// its sound muxed beside it, waiting while the scheduler pauses it and
/// stopping when cancelled, the partial file removed. `source` makes the
/// job's pictures on its own thread; `done` hears how it ended, once the
/// job's decoders, renderer and encoder are let go: a caller that waits on
/// it and then exits doesn't race them closing down.
pub fn spawn_bounce<S: FrameSource + 'static>(req: BounceRequest, source: impl FnOnce() -> S + Send + 'static, done: impl FnOnce(BounceResult) + Send + 'static) -> Arc<ThreadJob> {
    let name = format!("Bounce track {}", req.track + 1);
    ThreadJob::spawn(name, JobPriority::Background, ResourceClass::GpuMixed, move |pause, progress| {
        let result = run(&req, source, &pause, &|share| progress.set(share));
        done(result);
    })
}

/// The bounce itself; everything it opens is dropped when it returns.
fn run<S: FrameSource>(req: &BounceRequest, source: impl FnOnce() -> S, pause: &klipp_jobs::PauseToken, progress: &dyn Fn(f32)) -> BounceResult {
    let partial = klipp_render::partial_path_for(&req.path);
    let fail = |error: String, frames: usize| {
        let _ = std::fs::remove_file(&partial);
        BounceResult { path: None, frames, cancelled: false, error: Some(error) }
    };
    let settings = req.settings();
    let snapshot = req.snapshot();
    let mut encoder = match klipp_render::Encoder::open(&partial, &settings, &snapshot) {
        Ok(e) => e,
        Err(e) => return fail(format!("the file couldn't be opened: {e}"), 0),
    };
    let mut mix = match req.sound.as_ref().map(klipp_render::AudioMixdown::new).transpose() {
        Ok(m) => m,
        Err(e) => return fail(format!("the sound couldn't be read: {e}"), 0),
    };
    let mut src = source();
    let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    renderer.set_gpu_enabled(true);
    renderer.set_working_colorspace(req.working);
    let n = (req.end - req.first).max(1) as f32;
    let mut written = 0;
    for f in req.first..req.end {
        pause.wait_while_paused();
        if pause.is_cancelled() {
            drop(encoder);
            let _ = std::fs::remove_file(&partial);
            return BounceResult { path: None, frames: written, cancelled: true, error: None };
        }
        let pixels = req.draw(&mut renderer, &mut src, f);
        if let Err(e) = encoder.encode_linear_frame(&pixels, req.size.0, req.size.1) {
            return fail(format!("frame {f} couldn't be encoded: {e}"), written);
        }
        if let Some(mix) = &mut mix {
            if let Err(e) = encoder.encode_audio_samples(&mix.get_frame_audio(f.max(0) as u64)) {
                return fail(format!("the sound at frame {f} couldn't be encoded: {e}"), written);
            }
        }
        written += 1;
        progress((f - req.first + 1) as f32 / n);
    }
    if let Err(e) = encoder.finalise() {
        return fail(format!("the file couldn't be finished: {e}"), written);
    }
    if let Err(e) = std::fs::rename(&partial, &req.path) {
        return fail(format!("the file couldn't be put in place: {e}"), written);
    }
    BounceResult { path: Some(req.path.clone()), frames: written, cancelled: false, error: None }
}
