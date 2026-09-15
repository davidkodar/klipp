//! One name for every property of a media item the browser can show as
//! a column, sort by, filter on, gather into a smart bin, or write to a
//! metadata sheet — so those five agree on what a field is called and
//! what it reads as.

use crate::{MediaItem, MediaItemKind};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Field {
    Name,
    Duration,
    Resolution,
    Fps,
    Codec,
    FileSize,
    Rating,
    ColourLabel,
    Tags,
    Camera,
    Lens,
    Timecode,
    Reel,
    ShootDate,
    DateAdded,
    Scene,
    Shot,
    Take,
    Keywords,
    Description,
    Comments,
    GoodTake,
    Kind,
    Offline,
}

/// What a field sorts and compares by: a number where it has one, text
/// otherwise. Numbers order before text, so a clip without a value
/// sorts to one end rather than among the values.
#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    Number(f64),
    Text(String),
}

impl Eq for SortKey {}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => a.total_cmp(b),
            (SortKey::Text(a), SortKey::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            (SortKey::Number(_), SortKey::Text(_)) => Ordering::Less,
            (SortKey::Text(_), SortKey::Number(_)) => Ordering::Greater,
        }
    }
}

impl Field {
    pub const ALL: [Field; 24] = [
        Field::Name, Field::Duration, Field::Resolution, Field::Fps, Field::Codec, Field::FileSize, Field::Rating,
        Field::ColourLabel, Field::Tags, Field::Camera, Field::Lens, Field::Timecode, Field::Reel, Field::ShootDate,
        Field::DateAdded, Field::Scene, Field::Shot, Field::Take, Field::Keywords, Field::Description, Field::Comments,
        Field::GoodTake, Field::Kind, Field::Offline,
    ];

    /// The fields the list view can show as columns; the name is always
    /// the first column and is not among them.
    pub const COLUMNS: [Field; 21] = [
        Field::Rating, Field::Duration, Field::Resolution, Field::Fps, Field::Codec, Field::FileSize, Field::Camera,
        Field::Lens, Field::Timecode, Field::Reel, Field::ShootDate, Field::DateAdded, Field::Scene, Field::Shot,
        Field::Take, Field::Keywords, Field::Description, Field::Comments, Field::GoodTake, Field::ColourLabel, Field::Tags,
    ];

    /// The columns a fresh list shows.
    pub const DEFAULT_COLUMNS: [Field; 9] = [
        Field::Rating, Field::Duration, Field::Fps, Field::Codec, Field::FileSize, Field::Camera, Field::Timecode, Field::Scene, Field::Take,
    ];

    /// The fields the browser sorts by.
    pub const SORTABLE: [Field; 18] = [
        Field::Name, Field::DateAdded, Field::ShootDate, Field::Duration, Field::Rating, Field::Codec, Field::FileSize,
        Field::Resolution, Field::Fps, Field::Camera, Field::Lens, Field::Timecode, Field::Reel, Field::Scene, Field::Shot,
        Field::Take, Field::Keywords, Field::Kind,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Duration => "Duration",
            Field::Resolution => "Resolution",
            Field::Fps => "FPS",
            Field::Codec => "Codec",
            Field::FileSize => "Size",
            Field::Rating => "Rating",
            Field::ColourLabel => "Label",
            Field::Tags => "Tags",
            Field::Camera => "Camera",
            Field::Lens => "Lens",
            Field::Timecode => "Timecode",
            Field::Reel => "Reel",
            Field::ShootDate => "Shot on",
            Field::DateAdded => "Added",
            Field::Scene => "Scene",
            Field::Shot => "Shot",
            Field::Take => "Take",
            Field::Keywords => "Keywords",
            Field::Description => "Description",
            Field::Comments => "Comments",
            Field::GoodTake => "Good",
            Field::Kind => "Kind",
            Field::Offline => "Offline",
        }
    }

    /// The width a list column of this field gets, in points.
    pub fn column_width(self) -> f32 {
        match self {
            Field::Rating => 48.0,
            Field::Duration | Field::Fps | Field::Take | Field::GoodTake | Field::Offline | Field::Kind => 54.0,
            Field::Codec | Field::FileSize | Field::Scene | Field::Shot | Field::Reel | Field::ColourLabel => 64.0,
            Field::Resolution | Field::Timecode => 84.0,
            Field::ShootDate | Field::DateAdded => 110.0,
            Field::Camera | Field::Lens | Field::Tags | Field::Keywords | Field::Name => 140.0,
            Field::Description | Field::Comments => 200.0,
        }
    }

    /// Whether the editor writes this field (it is in the clip log).
    pub fn is_logged(self) -> bool {
        matches!(self, Field::Scene | Field::Shot | Field::Take | Field::Keywords | Field::Description | Field::Comments | Field::GoodTake | Field::Reel)
    }

    /// The field as text, as a column shows it. Empty when unknown.
    pub fn text(self, item: &MediaItem) -> String {
        match self {
            Field::Name => item.filename.clone(),
            Field::Duration => duration_text(item.duration.to_secs_f64()),
            Field::Resolution => {
                if item.resolution.width == 0 { String::new() } else { format!("{}x{}", item.resolution.width, item.resolution.height) }
            }
            Field::Fps => {
                if item.fps.den == 0 || (item.fps.num == 1 && item.fps.den == 1) {
                    String::new()
                } else {
                    let f = item.fps.as_f64();
                    if (f - f.round()).abs() < 1e-3 { format!("{}", f.round() as u32) } else { format!("{f:.3}") }
                }
            }
            Field::Codec => item.video_codec.to_lowercase(),
            Field::FileSize => size_text(item.file_size_bytes),
            Field::Rating => "\u{2605}".repeat(item.rating as usize),
            Field::ColourLabel => item.color_label.label().to_string(),
            Field::Tags => item.tags.join(", "),
            Field::Camera => {
                let make = item.camera_make.as_deref().unwrap_or("");
                let model = item.camera_model.as_deref().unwrap_or("");
                if model.is_empty() { make.to_string() } else if make.is_empty() || model.contains(make) { model.to_string() } else { format!("{make} {model}") }
            }
            Field::Lens => item.shot.lens.clone().unwrap_or_default(),
            Field::Timecode => item.shot.start_timecode.clone().unwrap_or_default(),
            Field::Reel => item.reel().unwrap_or("").to_string(),
            Field::ShootDate => item.media_created_at.or(item.file_modified_at).map(date_text).unwrap_or_default(),
            Field::DateAdded => if item.added_at == 0 { String::new() } else { date_text(item.added_at) },
            Field::Scene => item.log.scene.clone(),
            Field::Shot => item.log.shot.clone(),
            Field::Take => item.log.take.clone(),
            Field::Keywords => item.log.keywords_line(),
            Field::Description => item.log.description.clone(),
            Field::Comments => item.log.comments.clone(),
            Field::GoodTake => if item.log.good_take { "\u{2713}".to_string() } else { String::new() },
            Field::Kind => kind_text(item).to_string(),
            Field::Offline => if item.offline { "offline".to_string() } else { String::new() },
        }
    }

    /// The field as a number, for the fields that have one.
    pub fn number(self, item: &MediaItem) -> Option<f64> {
        match self {
            Field::Duration => Some(item.duration.to_secs_f64()),
            Field::Resolution => Some(f64::from(item.resolution.width) * f64::from(item.resolution.height)),
            Field::Fps => (item.fps.den != 0).then(|| item.fps.as_f64()),
            Field::FileSize => Some(item.file_size_bytes as f64),
            Field::Rating => Some(f64::from(item.rating)),
            Field::ShootDate => item.media_created_at.or(item.file_modified_at).map(|s| s as f64),
            Field::DateAdded => Some(item.added_at as f64),
            Field::GoodTake => Some(if item.log.good_take { 1.0 } else { 0.0 }),
            Field::Offline => Some(if item.offline { 1.0 } else { 0.0 }),
            Field::Take | Field::Scene | Field::Shot => self.text(item).trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// What the browser sorts this field by.
    pub fn sort_key(self, item: &MediaItem) -> SortKey {
        match self.number(item) {
            Some(n) if !matches!(self, Field::Scene | Field::Shot) || !self.text(item).is_empty() => SortKey::Number(n),
            _ => SortKey::Text(self.text(item)),
        }
    }
}

/// "1:24:05" or "0:07" — hours only when there are any.
pub fn duration_text(secs: f64) -> String {
    let whole = secs.round().max(0.0) as u64;
    let (h, m, s) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// "63.9 MB", "1.2 GB".
pub fn size_text(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.1} MB") }
}

/// "2024-03-15 10:30", from Unix seconds, in UTC as the camera counted.
pub fn date_text(epoch: u64) -> String {
    let (y, m, d, h, mi) = klipp_core::shot::epoch_to_civil(epoch);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

fn kind_text(item: &MediaItem) -> &'static str {
    match item.kind {
        MediaItemKind::File => {
            if item.is_still {
                "Still"
            } else if item.video_codec.is_empty() && item.has_audio {
                "Audio"
            } else {
                "Video"
            }
        }
        _ => "Title",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> MediaItem {
        let mut m = MediaItem::synthetic_for_test("/tmp/A001_C003.mov".into(), None);
        m.filename = "A001_C003.mov".into();
        m.kind = MediaItemKind::File;
        m.rating = 3;
        m.camera_make = Some("Sony".into());
        m.camera_model = Some("ILCE-7M4".into());
        m.log.scene = "12".into();
        m.log.take = "2".into();
        m.log.keywords = vec!["interview".into(), "wide".into()];
        m.shot.start_timecode = Some("01:02:03:04".into());
        m.file_size_bytes = 67_000_000;
        m
    }

    #[test]
    fn every_field_reads_as_text_and_the_numeric_ones_as_numbers() {
        let m = item();
        assert_eq!(Field::Rating.text(&m), "\u{2605}\u{2605}\u{2605}");
        assert_eq!(Field::Camera.text(&m), "Sony ILCE-7M4");
        assert_eq!(Field::Keywords.text(&m), "interview, wide");
        assert_eq!(Field::Timecode.text(&m), "01:02:03:04");
        assert_eq!(Field::FileSize.text(&m), "63.9 MB");
        assert_eq!(Field::Kind.text(&m), "Video");
        assert_eq!(Field::Take.number(&m), Some(2.0));
        assert_eq!(Field::Rating.number(&m), Some(3.0));
        assert_eq!(Field::Lens.number(&m), None);
        for f in Field::ALL {
            let _ = f.text(&m);
            let _ = f.sort_key(&m);
            assert!(!f.label().is_empty());
        }
    }

    #[test]
    fn numbers_sort_as_numbers_and_missing_values_sort_apart() {
        assert!(SortKey::Number(2.0) < SortKey::Number(10.0));
        assert!(SortKey::Text("b".into()) > SortKey::Text("A".into()));
        assert!(SortKey::Number(1.0) < SortKey::Text(String::new()));
        let mut a = item();
        a.log.take = "10".into();
        let b = item();
        assert!(Field::Take.sort_key(&b) < Field::Take.sort_key(&a), "take 2 before take 10");
        assert_eq!(duration_text(3725.0), "1:02:05");
        assert_eq!(duration_text(7.0), "0:07");
        assert_eq!(date_text(1_710_498_600), "2024-03-15 10:30");
    }
}
