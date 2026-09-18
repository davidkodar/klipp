//! What is in the footage, as facts a client can read: shot boundaries,
//! loudness and silence, later the words and the framing.
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

pub mod audio;
pub mod audio_sync;
pub mod motion;
pub mod report;
pub mod sheet;
pub mod service;
pub mod shots;
pub mod store;
#[cfg(feature = "transcript")]
pub mod transcript;
#[cfg(feature = "framing")]
pub mod framing;

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

/// Every analyser this build ships, in the order they pay off. Each AI
/// analyser is left out when its feature is switched off in preferences.
pub fn analysers() -> Vec<Box<dyn Analyser>> {
    let mut v: Vec<Box<dyn Analyser>> = vec![Box::new(shots::ShotsAnalyser), Box::new(audio::AudioAnalyser), Box::new(motion::MotionAnalyser)];
    #[cfg(feature = "transcript")]
    if transcription_enabled() {
        v.push(Box::new(transcript::TranscriptAnalyser));
    }
    #[cfg(feature = "framing")]
    if subject_detection_enabled() {
        v.push(Box::new(framing::FramingAnalyser));
    }
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
    Ok(got)
}

/// Run `ffmpeg` with `args` and return its stderr, which is where the
/// filters print.
pub(crate) fn ffmpeg_stderr(ffmpeg: &Path, args: &[String]) -> Result<String, String> {
    let out = std::process::Command::new(ffmpeg)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
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
