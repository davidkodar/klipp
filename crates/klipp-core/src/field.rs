//! One name for every property of a media item the browser can show as
//! a column, sort by, filter on, gather into a smart bin, or write to a
//! metadata sheet — so those five agree on what a field is called. What
//! a field reads as on an item lives with the item, in klipp-media.

use serde::{Deserialize, Serialize};

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
    Favourite,
    Rejected,
    Markers,
    /// What the screen-text analysis read off a screen recording: the
    /// lines it showed, so footage can be found and binned by them.
    ScreenText,
}

impl Field {
    pub const ALL: [Field; 28] = [
        Field::Name, Field::Duration, Field::Resolution, Field::Fps, Field::Codec, Field::FileSize, Field::Rating,
        Field::ColourLabel, Field::Tags, Field::Camera, Field::Lens, Field::Timecode, Field::Reel, Field::ShootDate,
        Field::DateAdded, Field::Scene, Field::Shot, Field::Take, Field::Keywords, Field::Description, Field::Comments,
        Field::GoodTake, Field::Kind, Field::Offline, Field::Favourite, Field::Rejected, Field::Markers, Field::ScreenText,
    ];

    /// The fields the list view can show as columns; the name is always
    /// the first column and is not among them.
    pub const COLUMNS: [Field; 25] = [
        Field::Rating, Field::Duration, Field::Resolution, Field::Fps, Field::Codec, Field::FileSize, Field::Camera,
        Field::Lens, Field::Timecode, Field::Reel, Field::ShootDate, Field::DateAdded, Field::Scene, Field::Shot,
        Field::Take, Field::Keywords, Field::Description, Field::Comments, Field::GoodTake, Field::ColourLabel, Field::Tags,
        Field::Favourite, Field::Rejected, Field::Markers, Field::ScreenText,
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
            Field::Favourite => "Favourite",
            Field::Rejected => "Rejected",
            Field::Markers => "Markers",
            Field::ScreenText => "Screen text",
        }
    }

    /// The width a list column of this field gets, in points.
    pub fn column_width(self) -> f32 {
        match self {
            Field::Rating => 48.0,
            Field::Duration | Field::Fps | Field::Take | Field::GoodTake | Field::Offline | Field::Kind | Field::Favourite | Field::Rejected | Field::Markers => 54.0,
            Field::Codec | Field::FileSize | Field::Scene | Field::Shot | Field::Reel | Field::ColourLabel => 64.0,
            Field::Resolution | Field::Timecode => 84.0,
            Field::ShootDate | Field::DateAdded => 110.0,
            Field::Camera | Field::Lens | Field::Tags | Field::Keywords | Field::Name => 140.0,
            Field::Description | Field::Comments | Field::ScreenText => 200.0,
        }
    }

    /// Whether the editor writes this field (it is in the clip log).
    pub fn is_logged(self) -> bool {
        matches!(self, Field::Scene | Field::Shot | Field::Take | Field::Keywords | Field::Description | Field::Comments | Field::GoodTake | Field::Reel)
    }

    /// Whether the field holds a date, compared as one by a smart bin.
    pub fn is_date(self) -> bool {
        matches!(self, Field::ShootDate | Field::DateAdded)
    }

    /// Whether the field is a yes or no.
    pub fn is_flag(self) -> bool {
        matches!(self, Field::GoodTake | Field::Offline | Field::Favourite | Field::Rejected)
    }
}
