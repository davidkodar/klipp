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
pub mod exif_read;
pub mod field;
pub mod card;
pub mod smart_bin;
pub mod sidecar;
mod item;
mod pool;
mod thumbnail;
pub mod waveform;
pub mod yuv_samples;

pub use audio_cache::AudioCacheEntry;
pub use item::{describe_secs, merge_reprobed, same_file_as_probed, Bin, ColorLabel, ColourSpace, MediaItem, MediaItemKind};
pub use pool::MediaPool;
pub use thumbnail::{FilmstripFrame, ThumbnailCache, ThumbnailEntry};
pub use waveform::generate_waveform_from_samples;
pub use field::{Field, FieldValue, SortKey};
pub use smart_bin::SmartMatch;
pub use klipp_core::{RuleOp, SmartBin, SmartRule};
