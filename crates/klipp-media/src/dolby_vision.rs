//! Dolby Vision at import (part of #1224): which profile a file's
//! picture is, read from the configuration record its container carries
//! (MP4 `dvcC`/`dvvC`, Matroska's block addition mapping), which ffmpeg
//! gives as the stream's `DOVI_CONF` side data.
//!
//! A profile 8 picture is a plain HDR10 or HLG base picture with
//! metadata beside it, and imports as that picture. A profile 5 picture
//! is coded in a colour form only the metadata turns back into colour,
//! which Klipp does not apply yet: decoded as it is, its colours are
//! wrong, so the item says so.

/// The profile whose picture Klipp cannot show right.
pub const UNSUPPORTED_PROFILE: u8 = 5;

/// What an item of [`UNSUPPORTED_PROFILE`] says, in its info and listing.
pub const UNSUPPORTED_WARNING: &str = "Dolby Vision profile 5 is not supported yet; colours will be wrong";

/// The profile a configuration record names: its third byte
/// (`AVDOVIDecoderConfigurationRecord`: version major, version minor,
/// profile, level, then flags). A record too short to hold the version,
/// profile and flags is not one.
pub fn record_profile(record: &[u8]) -> Option<u8> {
    // Version major, minor, profile, level and the four flag bytes.
    const LEAST: usize = 8;
    if record.len() < LEAST {
        return None;
    }
    // The profile is a 7-bit field.
    Some(record[2]).filter(|p| *p < 128)
}

/// The Dolby Vision profile of `stream`'s picture, `None` when it carries
/// no configuration record.
pub fn stream_profile(stream: &ffmpeg_next::format::stream::Stream) -> Option<u8> {
    stream
        .side_data()
        .find(|s| s.kind() == ffmpeg_next::codec::packet::side_data::Type::DOVI_CONF)
        .and_then(|s| record_profile(s.data()))
}

/// What an item of Dolby Vision `profile` should say about its picture,
/// `None` when it imports right.
pub fn warning(profile: Option<u8>) -> Option<&'static str> {
    (profile == Some(UNSUPPORTED_PROFILE)).then_some(UNSUPPORTED_WARNING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record as ffmpeg lays it out: version 1.0, `profile`, level 6,
    /// RPU and base layer present, no enhancement layer, compatibility 0.
    fn record(profile: u8) -> Vec<u8> {
        vec![1, 0, profile, 6, 1, 0, 1, 0, 0]
    }

    #[test]
    fn a_record_names_its_profile_and_only_5_warns() {
        assert_eq!(record_profile(&record(5)), Some(5));
        assert_eq!(record_profile(&record(8)), Some(8));
        assert_eq!(warning(record_profile(&record(5))), Some(UNSUPPORTED_WARNING));
        for p in [4, 7, 8, 10] {
            assert_eq!(warning(record_profile(&record(p))), None, "profile {p}");
        }
        assert_eq!(warning(None), None);
    }

    /// A record from a hostile file: cut short, empty, or naming a
    /// profile the 7-bit field cannot hold, reads as no record.
    #[test]
    fn a_short_or_impossible_record_is_no_record() {
        for n in 0..8 {
            assert_eq!(record_profile(&record(5)[..n]), None, "{n} bytes");
        }
        assert_eq!(record_profile(&[1, 0, 0x85, 6, 1, 0, 1, 0]), None);
        assert_eq!(record_profile(&[0xff; 4096]), None);
    }
}
