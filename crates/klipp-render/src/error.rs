use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("decode error: {0}")]
    Decode(#[from] ffmpeg_next::Error),

    #[error("encoder error: {0}")]
    Encode(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("media file not found: {0}")]
    MediaNotFound(PathBuf),

    #[error("no video stream in {0}")]
    NoVideoStream(PathBuf),

    #[error("no audio stream in {0}")]
    NoAudioStream(PathBuf),

    #[error("export cancelled")]
    Cancelled,

    #[error("snapshot error: {0}")]
    Snapshot(String),

    #[error("GPU pipeline error: {0}")]
    Gpu(String),
}
