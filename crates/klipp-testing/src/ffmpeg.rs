//! Whether the ffmpeg command is here, for the tests that make their
//! media with it: asked once a run.

/// Whether `ffmpeg -version` runs.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()))
}
