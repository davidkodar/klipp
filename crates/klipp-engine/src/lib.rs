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

/// Lightweight media metadata read from a file's container header. No frame
/// decode, no seek — just opens the input and reads stream parameters.
/// Typically <50 ms for any container.
///
/// Returned as a plain struct so downstream crates can display it without
/// depending on `ffmpeg_next` directly.
#[derive(Debug, Clone, Default)]
pub struct MediaHeader {
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub duration_secs: Option<f64>,
}

/// Read just the format header via ffmpeg. Errors are returned as strings
/// so the caller can show them in the UI.
pub fn probe_media_header(path: &std::path::Path) -> Result<MediaHeader, String> {
    let format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let mut info = MediaHeader::default();

    if let Some(stream) = format_ctx.streams().best(ffmpeg_next::media::Type::Video) {
        let params = stream.parameters();
        info.codec = Some(format!("{:?}", params.id()));

        if let Ok(decoder_ctx) = ffmpeg_next::codec::context::Context::from_parameters(params) {
            if let Ok(video) = decoder_ctx.decoder().video() {
                info.width = Some(video.width());
                info.height = Some(video.height());
            }
        }

        let fr = stream.avg_frame_rate();
        if fr.numerator() > 0 && fr.denominator() > 0 {
            info.fps = Some(fr.numerator() as f64 / fr.denominator() as f64);
        }
    }

    let dur = format_ctx.duration();
    if dur > 0 {
        info.duration_secs = Some(dur as f64 / 1_000_000.0);
    }

    Ok(info)
}
