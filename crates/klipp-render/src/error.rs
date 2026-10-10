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

    /// A clip's media stopped decoding part-way through the render.
    /// The export fails rather than freezing the last good frame for
    /// the rest of the clip: a file that plays back with a still where
    /// motion should be is a bad delivery, not a finished one.
    #[error("source decode failed: {0}")]
    Source(String),

    /// The encode finished but the `.partial` file could not be renamed
    /// into place. The partial is complete and is kept for the user to
    /// move by hand — the one failure after which it is not deleted.
    #[error("publish rename failed: {0}")]
    Publish(String),

    #[error("snapshot error: {0}")]
    Snapshot(String),

    #[error("GPU pipeline error: {0}")]
    Gpu(String),

    /// A node didn't draw what the timeline shows (a plug-in missing or
    /// failing, a neighbouring frame unreadable): the export stops rather
    /// than write a picture that isn't the one asked for (M4 step 10).
    #[error("a node didn't render: {0}")]
    Declined(String),
}
