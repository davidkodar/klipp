//! Media pool, bins, thumbnails, waveforms, and on-disk caches.
//!
//! A [`MediaItem`] is the imported representation of a source file: path,
//! probed metadata (duration, fps, resolution, codec, channel layout),
//! user-editable fields (rating, tags, notes, colour label, bin), and
//! cache references for thumbnails / waveforms / conformed PCM. The
//! [`MediaPool`] is the flat owner of every item plus a [`Bin`] tree for
//! folder-like grouping.
//!
//! # Caches
//!
//! * [`audio_cache`] — conformed-PCM-on-disk cache keyed by media
//!   fingerprint. Lets the engine skim & scrub audio at zero decode cost.
//! * [`disk_cache`] — shared disk-cache primitive (LZ4 + FNV keys) used
//!   by thumbnail / waveform / render pipelines.
//! * `thumbnail` — [`ThumbnailCache`] + [`FilmstripFrame`] for the
//!   browser grid and timeline filmstrip.
//! * [`waveform`] — mono waveform generation for clip audio visualisation.
//!
//! # Conventions
//!
//! * Paths stored on [`MediaItem`] are always absolute at runtime. The
//!   `.klipp` save path converts to project-relative via
//!   `klipp-project::path_util::to_project_relative`; the load path
//!   resolves back to absolute. External importers (MLT / OTIO / FCPXML)
//!   route untrusted paths through `safe_resolve` before they ever reach
//!   `MediaItem::path`.
//! * Offline media is represented by `MediaItem::offline = true`, not by
//!   removing the item. The UI and engine both check this flag before
//!   trying to open the file.

pub mod audio_cache;
pub mod disk_cache;
pub mod made;
pub mod exif_read;
pub mod field;
pub mod card;
pub mod smart_bin;
pub mod relink;
pub mod sidecar;
mod item;
mod pool;
mod thumbnail;
pub mod waveform;
pub mod frame_reader;
pub mod yuv_samples;

/// Whether `path` can be imported: ffmpeg opens it and finds a picture or
/// a sound in it, as the import's own first step does (#654). The error
/// says why not, in ffmpeg's words ("moov atom not found", "Invalid data
/// found when processing input").
pub fn readable(path: &std::path::Path) -> Result<(), String> {
    // ffmpeg's own words, or its error's name where it has none: its
    // Display is blank for some codes.
    let input = ffmpeg_next::format::input(path).map_err(|e| {
        let said = e.to_string();
        if said.trim().is_empty() { format!("{e:?}") } else { said }
    })?;
    let any = input.streams().any(|s| matches!(s.parameters().medium(), ffmpeg_next::media::Type::Video | ffmpeg_next::media::Type::Audio));
    if any { Ok(()) } else { Err("it has no picture or sound in it".into()) }
}

/// Whether `stream` is an attached picture: cover art in an MP3, FLAC or
/// M4A, one still frame marked as the file's artwork, not footage.
fn is_artwork(stream: &ffmpeg_next::Stream) -> bool {
    stream.disposition().contains(ffmpeg_next::format::stream::Disposition::ATTACHED_PIC)
}

/// The index of the video stream that is the file's picture: the one
/// ffmpeg ranks best, unless that is an attached picture (cover art), then
/// another real video stream. `None`: the file has no picture; a sound
/// with album art is a sound. The one rule for the probe, a relink's
/// check and what a listing reports (#1892).
pub fn picture_stream(input: &ffmpeg_next::format::context::Input) -> Option<usize> {
    match input.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) if !is_artwork(&s) => Some(s.index()),
        _ => input
            .streams()
            .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video && !is_artwork(s))
            .map(|s| s.index()),
    }
}

/// The size of the file's cover art, if it carries one (#1892).
pub fn artwork_size(path: &std::path::Path) -> Option<(u32, u32)> {
    let input = ffmpeg_next::format::input(path).ok()?;
    let art = input.streams().find(is_artwork)?;
    let video = ffmpeg_next::codec::context::Context::from_parameters(art.parameters()).ok()?.decoder().video().ok()?;
    (video.width() > 0 && video.height() > 0).then(|| (video.width(), video.height()))
}

pub use audio_cache::AudioCacheEntry;
pub use item::{describe_secs, merge_reprobed, same_file_as_probed, Bin, ColorLabel, ColourSpace, MediaItem, MediaItemKind};
pub use pool::MediaPool;
pub use thumbnail::{FilmstripFrame, ThumbnailCache, ThumbnailEntry};
pub use waveform::generate_waveform_from_samples;
pub use field::{Field, FieldValue, SortKey};
pub use smart_bin::SmartMatch;
pub use klipp_core::{RuleOp, SmartBin, SmartRule};

#[cfg(test)]
mod readable_tests {
    /// #654: a real file opens; a truncated one, random bytes and text do
    /// not, and each says why.
    #[test]
    fn only_a_file_with_picture_or_sound_is_readable() {
        let dir = std::env::temp_dir().join(format!("klipp-readable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=64x64:r=25:d=0.4", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&good)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            assert_eq!(super::readable(&good), Ok(()));
            let bytes = std::fs::read(&good).unwrap();
            let cut = dir.join("cut.mp4");
            std::fs::write(&cut, &bytes[..bytes.len() / 3]).unwrap();
            assert!(super::readable(&cut).is_err(), "a truncated file is not readable");
        }
        let noise = dir.join("noise.mov");
        std::fs::write(&noise, (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect::<Vec<_>>()).unwrap();
        assert!(super::readable(&noise).is_err());
        let text = dir.join("notes.mp4");
        std::fs::write(&text, b"just some words, not a video").unwrap();
        let why = super::readable(&text).expect_err("text is not a video");
        assert!(!why.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
