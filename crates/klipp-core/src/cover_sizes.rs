//! The sizes a cover (#3311) is exported at, one data file:
//! `assets/presets/cover_sizes/builtin.toml`. A size is a picture in
//! pixels and, where the platform has one, the largest file it takes.

use serde::Deserialize;

/// One size a cover is made at.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverSize {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// The largest file the platform takes, in bytes; none for no limit.
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// When the numbers were last checked against the platform.
    pub checked: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    size: Vec<CoverSize>,
}

/// Every size, in the file's order, read once; none when the file
/// doesn't read, which its test keeps from shipping.
pub fn all() -> &'static [CoverSize] {
    static ALL: std::sync::OnceLock<Vec<CoverSize>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| toml::from_str::<File>(include_str!("../../../assets/presets/cover_sizes/builtin.toml")).map(|f| f.size).unwrap_or_default())
}

/// The size with `id`, in any case.
pub fn by_id(id: &str) -> Option<&'static CoverSize> {
    all().iter().find(|s| s.id.eq_ignore_ascii_case(id.trim()))
}

/// The size whose shape is nearest a `width` × `height` frame's, the
/// first of those that tie: what a new cover of that frame starts at.
pub fn for_frame(width: u32, height: u32) -> Option<&'static CoverSize> {
    let aspect = |w: u32, h: u32| (w.max(1) as f64 / h.max(1) as f64).ln();
    let want = aspect(width, height);
    all().iter().min_by(|a, b| (aspect(a.width, a.height) - want).abs().total_cmp(&(aspect(b.width, b.height) - want).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file reads, each size with its own id, a picture of even
    /// sides and a date it was checked; a frame finds the size of its
    /// shape.
    #[test]
    fn every_cover_size_reads_and_a_frame_finds_its_shape() {
        let all = all();
        assert!(all.len() >= 4, "the file reads");
        let mut ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "each its own id");
        for s in all {
            assert!(s.width >= 320 && s.height >= 320 && s.width % 2 == 0 && s.height % 2 == 0 && !s.checked.is_empty(), "{}", s.id);
        }
        assert_eq!(by_id("YouTube").map(|s| (s.width, s.height, s.max_bytes)), Some((1280, 720, Some(2_000_000))));
        assert_eq!(for_frame(3840, 2160).map(|s| s.id.as_str()), Some("youtube"));
        assert_eq!(for_frame(1080, 1920).map(|s| s.id.as_str()), Some("vertical"));
        assert_eq!(for_frame(1440, 1800).map(|s| s.id.as_str()), Some("instagram_feed"));
    }
}
