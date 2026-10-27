//! What is in the footage, as facts a client can read: shot
//! boundaries, loudness and silence, the words, the framing, motion,
//! tempo, and the screen text and beat sheet a screen recording
//! needs.
//!
//! Every analyser implements [`Analyser`]: an id, a schema version, what
//! it wants to run on, and a run that returns plain data. Results are
//! stored by [`store`] under the media file's cache key (its canonical
//! path, size and modification time, the key the filmstrips use), one
//! JSON file per analyser and schema version, so a changed file or a
//! newer analyser re-runs and nothing else does. [`service`] runs
//! analysers in the background, at most two at a time, and answers
//! where each media item stands. [`report`] is what a reader gets:
//! every analyser's result that exists for an item, and the text a
//! model reads.
//!
//! The ffmpeg analysers: [`shots`] (scene change detection), [`audio`]
//! (EBU R128 loudness, true peak, silence spans) and [`motion`] (how
//! much the picture moves, second by second). [`transcript`] is the
//! words, through whisper.cpp with the model from the registry;
//! [`framing`] is who is in each shot and how large, through an object
//! detector from the registry. [`sheet`]
//! makes what a model that can see or hear is shown: contact sheets
//! and audio excerpts, on demand.

pub mod assets;
pub mod audio;
pub mod audio_sync;
pub mod beats;
pub mod beatsheet;
pub mod music_structure;
pub mod hold;
pub mod motion;
pub mod moments;
pub mod shorts;
pub mod providers;
pub mod report;
pub mod sheet;
pub mod spectrogram;
pub mod service;
pub mod shots;
pub mod store;
pub(crate) mod videopass;
pub mod worker;
#[cfg(feature = "transcript")]
pub mod transcript;
#[cfg(feature = "framing")]
pub mod framing;
#[cfg(feature = "ocr")]
pub mod screentext;
#[cfg(feature = "subject_matte")]
pub mod subject_matte;
#[cfg(feature = "audio_role")]
pub mod audio_role;
#[cfg(feature = "visual")]
pub mod visual;

use std::path::{Path, PathBuf};

/// One analysis pass over a media file.
pub trait Analyser: Send + Sync {
    /// Names the result's file: stable, lowercase, no spaces.
    fn id(&self) -> &'static str;
    /// Bump on any change to the output's shape or meaning; a stored
    /// result at another version is ignored and re-made.
    fn schema_version(&self) -> u32;
    /// Whether this analyser has anything to say about the item.
    fn wants(&self, item: &klipp_media::MediaItem) -> bool;
    /// The result as JSON, or why it could not be made.
    fn run(&self, item: &klipp_media::MediaItem, ctx: &AnalysisCtx) -> Result<serde_json::Value, String>;
    /// Whether a stored result, of the right version, should be made
    /// again all the same: a better model has been installed since, or
    /// what the analyser was told has changed.
    fn stale(&self, _stored: &serde_json::Value) -> bool {
        false
    }
    /// Whether it runs native model or inference code, which can bring
    /// the process down with it: run in a [`worker`] then (#689).
    fn native(&self) -> bool {
        false
    }
}

/// Whether `a` has a stored result for `key` that still stands.
pub(crate) fn has_fresh(a: &dyn Analyser, key: &str) -> bool {
    match store::load(key, a.id(), a.schema_version()) {
        Some(v) => !a.stale(&v),
        None => false,
    }
}

/// What an analyser may use.
#[derive(Debug, Clone)]
pub struct AnalysisCtx {
    /// The ffmpeg binary; `ffmpeg` on the path by default.
    pub ffmpeg: PathBuf,
}

impl Default for AnalysisCtx {
    fn default() -> Self {
        Self { ffmpeg: PathBuf::from("ffmpeg") }
    }
}

impl AnalysisCtx {
    /// Whether ffmpeg answers at all.
    pub fn ffmpeg_available(&self) -> bool {
        std::process::Command::new(&self.ffmpeg)
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// The on-device AI analysers are opt-out, each on its own. The non-AI
/// analysers — shots, audio, motion — always run; these two are gated by
/// their preference so a user can, say, keep transcription and drop
/// subject detection. Speech transcription:
static TRANSCRIPTION_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// Subject detection (the framing analyser):
static SUBJECT_DETECTION_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turn automatic speech transcription on or off.
pub fn set_transcription_enabled(on: bool) {
    TRANSCRIPTION_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// Whether automatic speech transcription is enabled.
pub fn transcription_enabled() -> bool {
    TRANSCRIPTION_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
/// Turn subject detection (framing) on or off.
pub fn set_subject_detection_enabled(on: bool) {
    SUBJECT_DETECTION_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// Whether subject detection is enabled.
pub fn subject_detection_enabled() -> bool {
    SUBJECT_DETECTION_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
/// Reading on-screen text (the screen-text analyser):
static SCREEN_TEXT_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turn on-screen text reading (OCR) on or off.
pub fn set_screen_text_enabled(on: bool) {
    SCREEN_TEXT_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// Whether on-screen text reading is enabled.
pub fn screen_text_enabled() -> bool {
    SCREEN_TEXT_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `item` looks like a recording of a screen, the footage the
/// beat sheet and the on-screen text reader are for (#500). Run on every
/// phone clip they read sea foam and house numbers as screen text and
/// reported a beach walk that "never settled".
///
/// A recorder that names itself says yes. Anything a camera leaves says
/// no: a make or model, a rotation, a log or HDR curve, a timecode or
/// reel, lens settings. Without any of that it is taken for a screen.
/// Tagging a file `screen` (or `screen recording`) asks for the screen
/// analysers whatever the file says.
pub fn looks_like_screen(item: &klipp_media::MediaItem) -> bool {
    let tagged = item.tags.iter().any(|t| matches!(t.trim().to_lowercase().as_str(), "screen" | "screen recording" | "screencast"));
    if tagged {
        return true;
    }
    const RECORDERS: [&str; 9] = ["obs", "screen", "wf-recorder", "kooha", "gnome-shell", "gpu-screen-recorder", "peek", "loom", "kazam"];
    let encoder = item.encoder.as_deref().unwrap_or("").to_lowercase();
    if RECORDERS.iter().any(|r| encoder.contains(r)) {
        return true;
    }
    let shot = &item.shot;
    let hdr = item.color_transfer.as_deref().is_some_and(|t| t.contains("2084") || t.contains("arib") || t.contains("hlg"));
    let camera = item.camera_make.is_some()
        || item.camera_model.is_some()
        || item.source_rotation != 0
        || !matches!(item.detected_log_format, klipp_core::LogFormat::Srgb | klipp_core::LogFormat::Linear)
        || hdr
        || shot.start_timecode.is_some()
        || shot.reel_name.is_some()
        || shot.lens.is_some()
        || shot.iso.is_some();
    !camera
}

/// A second whose mean frame-to-frame luma difference is below this held
/// still: a screen that settled. Camera footage never gets there, even on
/// a tripod; sensor noise and compression keep every second above it.
const STILL_YDIF: f32 = 0.05;

/// Of the frames the screen-text reader read, the share that must hold
/// readable text for a file to be a screen: a screen is mostly words (a
/// menu, a file name), a camera shot mostly has none, and a sign or a
/// shirt in one isn't on every frame.
const TEXT_FRAMES_MIN: f32 = 0.5;

/// Whether `item` is a screen recording, the footage whose beat sheet the
/// checks may read (#1568). [`looks_like_screen`] can only say what the
/// file's metadata says, and stock footage re-encoded without any reads as
/// a screen to it: a girl at the beach was "cutting in while the screen is
/// still changing". So the picture has the last word, and it must look
/// like a screen twice over:
/// - it holds still somewhere: a second of `motion` below `STILL_YDIF`;
/// - it shows text: of the frames the screen-text reader read,
///   `text_frames` is the share with a readable line, at least
///   `TEXT_FRAMES_MIN`. A locked-off camera shot can come close to
///   still; it doesn't show a menu.
///
/// A measurement that wasn't made (no motion analysis, no screen-text
/// model installed) doesn't count against the file, and the metadata
/// decides. A file tagged `screen` is one whatever its picture does.
pub fn is_screen_recording(item: &klipp_media::MediaItem, motion: Option<&motion::Motion>, text_frames: Option<f32>) -> bool {
    let tagged = item.tags.iter().any(|t| matches!(t.trim().to_lowercase().as_str(), "screen" | "screen recording" | "screencast"));
    if tagged {
        return true;
    }
    if !looks_like_screen(item) {
        return false;
    }
    let measured: Vec<f32> = motion.map(|m| m.per_second.iter().flatten().copied().collect()).unwrap_or_default();
    let holds_still = measured.is_empty() || measured.iter().any(|v| *v < STILL_YDIF);
    let shows_text = text_frames.is_none_or(|share| share >= TEXT_FRAMES_MIN);
    holds_still && shows_text
}

/// Every analyser this build ships, in the order they pay off. Each AI
/// analyser is left out when its feature is switched off in preferences.
pub fn analysers() -> Vec<Box<dyn Analyser>> {
    // The beat sheet is last of the always-on set: it synthesises the
    // shots and motion that run before it into an idle/action map.
    let mut v: Vec<Box<dyn Analyser>> = vec![
        Box::new(shots::ShotsAnalyser),
        Box::new(audio::AudioAnalyser),
        Box::new(motion::MotionAnalyser),
        Box::new(beatsheet::BeatSheetAnalyser),
    ];
    #[cfg(feature = "transcript")]
    if transcription_enabled() {
        v.push(Box::new(transcript::TranscriptAnalyser));
    }
    #[cfg(feature = "framing")]
    if subject_detection_enabled() {
        v.push(Box::new(framing::FramingAnalyser));
    }
    // After the beat sheet, whose settled frames it reads.
    #[cfg(feature = "ocr")]
    if screen_text_enabled() {
        v.push(Box::new(screentext::ScreenTextAnalyser));
    }
    // Wants nothing until its model is downloaded.
    #[cfg(feature = "audio_role")]
    v.push(Box::new(audio_role::AudioRoleAnalyser));
    #[cfg(feature = "visual")]
    v.push(Box::new(visual::VisualAnalyser));
    v
}

/// Every analyser that runs native code, whatever the preferences: what
/// a [`worker`] may be asked to run.
// Pushed one by one, each under its feature.
#[allow(unused_mut, clippy::vec_init_then_push)]
pub(crate) fn native_analysers() -> Vec<Box<dyn Analyser>> {
    let mut v: Vec<Box<dyn Analyser>> = Vec::new();
    #[cfg(feature = "framing")]
    v.push(Box::new(framing::FramingAnalyser));
    #[cfg(feature = "ocr")]
    v.push(Box::new(screentext::ScreenTextAnalyser));
    #[cfg(feature = "visual")]
    v.push(Box::new(visual::VisualAnalyser));
    v
}

/// Fetch the models the analysers can use but do not have, when asked.
/// Nothing is downloaded otherwise: an analyser without its model
/// simply does not want any item.
pub fn download_models() -> Result<Vec<String>, String> {
    let mut got = Vec::new();
    #[cfg(feature = "transcript")]
    {
        transcript::ensure_model(true)?;
        got.push(transcript::MODEL_ID.to_string());
    }
    #[cfg(feature = "framing")]
    {
        framing::ensure_model(true)?;
        got.push(framing::MODEL_ID.to_string());
    }
    #[cfg(feature = "ocr")]
    if screen_text_enabled() {
        screentext::ensure_models(true)?;
        got.extend(screentext::MODEL_IDS.iter().map(|s| s.to_string()));
    }
    #[cfg(feature = "audio_role")]
    {
        audio_role::ensure_model(true)?;
        got.push(audio_role::MODEL_ID.to_string());
    }
    #[cfg(feature = "visual")]
    {
        visual::ensure_models(true)?;
        got.extend(visual::MODEL_IDS.iter().map(|s| s.to_string()));
    }
    Ok(got)
}

/// Whether `path` is a music track or a sound effect the app fetched
/// from a library: its credit line sits beside it. Such a file has no
/// speech to transcribe and its low end is not a fault.
pub fn is_licensed_audio(path: &Path) -> bool {
    path.with_extension("klipp-credit.txt").is_file()
}

/// Which hardware decoder ffmpeg is asked for, chosen once: the first of
/// the well-known ones this ffmpeg lists. `KLIPP_ANALYSIS_HWACCEL` names
/// one, or `0`/`none` for software decoding. When the driver refuses, ffmpeg
/// itself falls back to software decoding, so a wrong guess costs a
/// warning, not a failure.
pub fn hwaccel_name(ffmpeg: &Path) -> Option<String> {
    static CHOSEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            let env = std::env::var("KLIPP_ANALYSIS_HWACCEL").ok();
            let listed = std::process::Command::new(ffmpeg).args(["-hide_banner", "-hwaccels"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
            pick_hwaccel(&listed, env.as_deref())
        })
        .clone()
}

/// The choice from what ffmpeg lists and what the environment says.
pub fn pick_hwaccel(listed: &str, env: Option<&str>) -> Option<String> {
    let available: Vec<&str> = listed.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Hardware")).collect();
    match env.map(str::trim) {
        Some("0") | Some("none") | Some("off") => None,
        Some(name) if !name.is_empty() => Some(name.to_string()),
        _ => ["cuda", "vaapi", "videotoolbox", "qsv"].iter().find(|w| available.contains(w)).map(|w| w.to_string()),
    }
}

/// How the picture reaches the filters: decoded and scaled on the GPU
/// with only the small result downloaded, decoded on the GPU and
/// scaled on the CPU, or all on the CPU. Tried in that order; the CPU
/// is the last resort, not the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decode {
    GpuScaled,
    Gpu,
    Cpu,
}

/// The ways worth trying for this ffmpeg, best first.
pub(crate) fn decode_plans(ffmpeg: &Path) -> Vec<Decode> {
    match hwaccel_name(ffmpeg).as_deref() {
        Some("cuda") | Some("vaapi") => vec![Decode::GpuScaled, Decode::Gpu, Decode::Cpu],
        Some(_) => vec![Decode::Gpu, Decode::Cpu],
        None => vec![Decode::Cpu],
    }
}

/// The input options for a plan, to go before `-i`. `codec` is the
/// file's video codec as probed: for AV1, ffmpeg's default decoder is
/// the software dav1d, registered ahead of the native one that can use
/// the GPU, so the native one is named.
pub(crate) fn decode_input_args(ffmpeg: &Path, plan: Decode, codec: &str) -> Vec<String> {
    let Some(name) = hwaccel_name(ffmpeg) else { return Vec::new() };
    let mut v = match plan {
        Decode::GpuScaled => vec!["-hwaccel".into(), name.clone(), "-hwaccel_output_format".into(), name],
        Decode::Gpu => vec!["-hwaccel".into(), name],
        Decode::Cpu => return Vec::new(),
    };
    if codec.trim().eq_ignore_ascii_case("av1") {
        v.push("-c:v".into());
        v.push("av1".into());
    }
    v
}

/// The filter that scales to `width`×`height` (`-2` keeps the aspect)
/// and hands the frames to the CPU filters as yuv420p, for a plan.
pub(crate) fn scale_filter(ffmpeg: &Path, plan: Decode, width: u32, height: i64) -> String {
    match (plan, hwaccel_name(ffmpeg).as_deref()) {
        (Decode::GpuScaled, Some("cuda")) => format!("scale_cuda={width}:{height},hwdownload,format=nv12,format=yuv420p"),
        (Decode::GpuScaled, Some("vaapi")) => format!("scale_vaapi=w={width}:h={height},hwdownload,format=nv12,format=yuv420p"),
        _ => format!("scale={width}:{height}"),
    }
}

/// The input options that ask for the hardware decoder alone, to go
/// before `-i`, for a command whose filters need CPU frames as decoded.
pub(crate) fn hwaccel_args(ffmpeg: &Path, codec: &str) -> Vec<String> {
    decode_input_args(ffmpeg, Decode::Gpu, codec)
}

/// Seeks handed to one ffmpeg process: one start-up and one GPU
/// context for all of them, each still a seek to the keyframe before
/// its time and a decode from there, never a pass over the file.
const GRAB_BATCH: usize = 8;

/// How long a batch of grabs may take on the GPU before it is given up
/// for the next plan: ffmpeg's VAAPI decode can deadlock on a seek and
/// wait forever (#519), where eight seeks take a few seconds.
const GPU_GRAB_LIMIT: std::time::Duration = std::time::Duration::from_secs(45);

/// The frames of `media` at `times`, each scaled to `width`×`height`
/// (`-2` keeps the aspect) and put through `after` (a filter chain, or
/// empty), as RGBA8 bytes of `frame_bytes` each. On the GPU where it
/// can be; the CPU is the last resort.
pub(crate) fn grab_frames(ffmpeg: &Path, item: &klipp_media::MediaItem, times: &[f64], width: u32, height: i64, after: &str, frame_bytes: usize) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::with_capacity(times.len());
    for chunk in times.chunks(GRAB_BATCH) {
        out.extend(grab_batch(ffmpeg, item, chunk, width, height, after, frame_bytes)?);
    }
    Ok(out)
}

fn grab_batch(ffmpeg: &Path, item: &klipp_media::MediaItem, times: &[f64], width: u32, height: i64, after: &str, frame_bytes: usize) -> Result<Vec<Vec<u8>>, String> {
    let media = &item.path;
    let mut last = String::new();
    for plan in decode_plans(ffmpeg) {
        let mut cmd = std::process::Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"]);
        let input_args = decode_input_args(ffmpeg, plan, &item.video_codec);
        for t in times {
            cmd.args(&input_args);
            // A still is its one picture, read from the start: a seek
            // before a JPEG (even to 0) gave no frame and no error, so
            // its framing failed (#2474, #2515 review).
            if !item.is_still {
                cmd.arg("-ss").arg(format!("{t:.3}"));
            }
            cmd.arg("-i").arg(media);
        }
        let scale = scale_filter(ffmpeg, plan, width, height);
        let mut graph = String::new();
        for k in 0..times.len() {
            // `:0`: the file's first video stream, the picture — not a
            // cover art stream that some files carry.
            graph.push_str(&format!("[{k}:v:0]trim=end_frame=1,{scale}"));
            if !after.is_empty() {
                graph.push(',');
                graph.push_str(after);
            }
            graph.push_str(&format!("[t{k}];"));
        }
        for k in 0..times.len() {
            graph.push_str(&format!("[t{k}]"));
        }
        graph.push_str(&format!("concat=n={}:v=1:a=0,format=rgba[out]", times.len()));
        // Every frame through as it comes: the raw muxer would otherwise
        // drop the ones whose timestamps step back after the concat.
        cmd.args(["-an", "-sn", "-filter_complex", &graph, "-map", "[out]", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        let limit = (!matches!(plan, Decode::Cpu)).then_some(GPU_GRAB_LIMIT);
        let Some(out) = crate::hold::output_within(&mut cmd, limit).map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))? else {
            last = format!("the {plan:?} decode did not finish within {} s", GPU_GRAB_LIMIT.as_secs());
            tracing::warn!("frames at {times:?}: {last}; trying the next");
            continue;
        };
        if out.status.success() && out.stdout.len() >= frame_bytes * times.len() {
            return Ok(out.stdout.chunks_exact(frame_bytes).take(times.len()).map(|c| c.to_vec()).collect());
        }
        last = String::from_utf8_lossy(&out.stderr).trim().to_string();
        tracing::warn!("frames at {:?}: {plan:?} decode gave {} bytes for {} frames ({last}); trying the next", times, out.stdout.len(), times.len());
    }
    Err(format!("ffmpeg could not decode frames at {times:?}: {last}"))
}

/// Run `ffmpeg` with `args` and return its stderr, which is where the
/// filters print.
pub(crate) fn ffmpeg_stderr(ffmpeg: &Path, args: &[String]) -> Result<String, String> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(args);
    let out = crate::hold::output(&mut cmd).map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        let tail: String = text.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        return Err(format!("ffmpeg failed ({}): {tail}", out.status));
    }
    Ok(text)
}

#[cfg(test)]
mod ai_toggle_tests {
    /// Disabling AI drops the transcript and framing analysers but keeps
    /// shots, audio and motion; re-enabling restores them.
    #[test]
    fn each_ai_feature_gates_its_own_analyser() {
        super::set_transcription_enabled(true);
        super::set_subject_detection_enabled(true);
        let on: Vec<&str> = super::analysers().iter().map(|a| a.id()).collect();
        assert!(on.contains(&"shots") && on.contains(&"audio") && on.contains(&"motion"));
        // Drop subject detection only: transcription stays.
        super::set_subject_detection_enabled(false);
        let ids: Vec<&str> = super::analysers().iter().map(|a| a.id()).collect();
        assert!(!ids.contains(&"framing"), "framing gone: {ids:?}");
        #[cfg(feature = "transcript")]
        assert!(ids.contains(&"transcript"), "transcription stays: {ids:?}");
        // Drop transcription too.
        super::set_transcription_enabled(false);
        let none: Vec<&str> = super::analysers().iter().map(|a| a.id()).collect();
        assert!(none.contains(&"shots") && none.contains(&"audio") && none.contains(&"motion"), "non-AI stay");
        assert!(!none.contains(&"transcript") && !none.contains(&"framing"), "AI gone: {none:?}");
        super::set_transcription_enabled(true);
        super::set_subject_detection_enabled(true);
    }
}

#[cfg(test)]
mod still_frame_tests {
    /// A 4032x3024 phone JPEG (4:2:0, full range) gives its picture to
    /// the analysers: a seek before it gave nothing at all (#2515 review).
    #[test]
    fn a_large_jpeg_still_gives_its_frame() {
        let path = std::env::temp_dir().join(format!("klipp-analysis-photo-{}.jpeg", std::process::id()));
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=4032x3024", "-frames:v", "1", "-pix_fmt", "yuvj420p", "-q:v", "3"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.is_still = true;
        item.video_codec = "mjpeg".into();
        let bytes = 64 * 48 * 4;
        let frames = super::grab_frames(std::path::Path::new("ffmpeg"), &item, &[0.0], 64, 48, "", bytes).expect("the still's frame");
        let _ = std::fs::remove_file(&path);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].len() == bytes && frames[0].chunks_exact(4).any(|p| p[0] > 16 || p[1] > 16 || p[2] > 16), "a picture, not black");
    }
}

#[cfg(test)]
mod hwaccel_tests {
    use super::pick_hwaccel;

    #[test]
    fn the_first_known_decoder_listed_is_taken_unless_the_environment_says() {
        let listed = "Hardware acceleration methods:\nvdpau\ncuda\nvaapi\nvulkan\n";
        assert_eq!(pick_hwaccel(listed, None).as_deref(), Some("cuda"));
        assert_eq!(pick_hwaccel("Hardware acceleration methods:\nvaapi\n", None).as_deref(), Some("vaapi"));
        assert_eq!(pick_hwaccel("Hardware acceleration methods:\nvdpau\n", None), None);
        assert_eq!(pick_hwaccel(listed, Some("0")), None);
        assert_eq!(pick_hwaccel(listed, Some("none")), None);
        assert_eq!(pick_hwaccel(listed, Some("vulkan")).as_deref(), Some("vulkan"));
    }
}

#[cfg(test)]
mod screen_tests {
    use super::looks_like_screen;

    /// #500: a phone clip, a camera's log file or one with a timecode is
    /// camera footage, kept from the screen analysers; a file with no
    /// camera in it, one a recorder names itself in, or one tagged
    /// `screen` is a screen.
    #[test]
    fn only_a_screen_recording_gets_the_screen_analysers() {
        let item = || klipp_media::MediaItem::synthetic_for_test("/tmp/clip.mov".into(), None);
        assert!(looks_like_screen(&item()), "nothing of a camera in it");
        let mut phone = item();
        phone.camera_make = Some("Apple".into());
        phone.camera_model = Some("iPhone 15 Pro".into());
        assert!(!looks_like_screen(&phone));
        let mut turned = item();
        turned.source_rotation = 90;
        assert!(!looks_like_screen(&turned));
        let mut log = item();
        log.detected_log_format = klipp_core::LogFormat::SLog3;
        assert!(!looks_like_screen(&log));
        let mut hdr = item();
        hdr.color_transfer = Some("smpte2084".into());
        assert!(!looks_like_screen(&hdr));
        let mut slate = item();
        slate.shot.start_timecode = Some("01:00:00:00".into());
        assert!(!looks_like_screen(&slate));
        let mut obs = item();
        obs.encoder = Some("OBS Studio".into());
        obs.source_rotation = 90;
        assert!(looks_like_screen(&obs), "a recorder that names itself");
        phone.tags.push("Screen recording".into());
        assert!(looks_like_screen(&phone), "asked for");
    }

    /// #1568: stock footage re-encoded without metadata looks like a
    /// screen to the metadata, and its picture says otherwise. The motion
    /// below is measured from camera files with no metadata: a beach shot,
    /// and two locked-off ones, a sunset over the sea and coffee poured
    /// into a mug, that never hold still and show no text.
    #[test]
    fn camera_footage_without_metadata_is_not_a_screen() {
        use super::is_screen_recording;
        use crate::motion::Motion;
        let item = || klipp_media::MediaItem::synthetic_for_test("/tmp/clip.mp4".into(), None);
        let motion = |secs: &[f32]| Motion::from_samples(secs.len() as f64, secs.iter().map(|v| Some(*v)).collect());
        let beach = motion(&[0.67, 0.63, 0.62, 0.56, 0.55, 0.54, 0.47, 0.42, 0.41, 0.40, 0.40, 0.37]);
        let sunset = motion(&[0.35, 0.39, 0.38, 0.40, 0.36, 0.34, 0.32, 0.30, 0.31, 0.31, 0.31]);
        let coffee = motion(&[0.40, 0.49, 0.40, 0.39, 0.35, 0.41, 0.42]);
        for (name, m) in [("beach", &beach), ("sunset", &sunset), ("coffee", &coffee)] {
            assert!(!is_screen_recording(&item(), Some(m), None), "{name}: never still");
            assert!(!is_screen_recording(&item(), Some(m), Some(0.0)), "{name}: never still, no text");
        }
        // A screen: still for seconds, words on every frame read.
        let demo = motion(&[0.0, 0.0, 0.31, 0.0, 0.02]);
        assert!(is_screen_recording(&item(), Some(&demo), Some(1.0)));
        assert!(is_screen_recording(&item(), Some(&demo), None), "no text model: the motion decides");
        // A locked-off shot that does hold still for a second, and shows
        // no text, or a sign on one frame of three.
        let still_shot = motion(&[0.31, 0.04, 0.33]);
        assert!(!is_screen_recording(&item(), Some(&still_shot), Some(0.0)));
        assert!(!is_screen_recording(&item(), Some(&still_shot), Some(1.0 / 3.0)));
        assert!(is_screen_recording(&item(), None, None), "nothing measured: the metadata decides");
        let unread = Motion::from_samples(2.0, vec![None, None]);
        assert!(is_screen_recording(&item(), Some(&unread), None));
        let mut phone = item();
        phone.camera_make = Some("Apple".into());
        assert!(!is_screen_recording(&phone, Some(&demo), Some(1.0)), "a camera's file stays a camera's");
        let mut tagged = item();
        tagged.tags.push("screen".into());
        assert!(is_screen_recording(&tagged, Some(&beach), Some(0.0)), "asked for");
    }
}
