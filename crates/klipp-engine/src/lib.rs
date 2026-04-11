pub mod audio_decoder;
pub mod audio_output;
pub mod audio_source;
pub mod composite;
pub mod engine;
pub mod mix_thread;
pub mod render_cache;
pub mod pcm_reader;
pub mod skim_audio;
pub mod sync;
pub mod texture;
pub mod transition;
pub mod types;
pub mod video_decoder;

pub use composite::{composite_layers, CompositeLayer};
pub use engine::{detect_rotation_from_path, generate_filmstrip_incremental, generate_thumbnail, generate_thumbnail_at_size, generate_thumbnail_non_black, generate_thumbnail_at_size_non_black, is_frame_black, PlaybackEngine, PlaybackState, SequentialFrameDecoder};
pub use texture::{FrameBuffer, SharedFrameBuffer};
pub use types::{AudioRingBuf, DecodedFrame, FrameData, FrameQueue, SharedFrameQueue};

/// Initialize ffmpeg. Idempotent — safe to call multiple times. Required
/// before any decode operation. The app binary calls this once at startup;
/// tests in downstream crates that exercise decode helpers should call it
/// themselves to avoid depending on `ffmpeg_next` directly.
pub fn init_ffmpeg() -> Result<(), ffmpeg_next::Error> {
    ffmpeg_next::init()
}
