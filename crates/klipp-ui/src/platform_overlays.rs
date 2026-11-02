//! Platform overlays in the viewer (#3315): where a platform's player
//! covers the picture — its buttons, its caption and handle, its progress
//! bar — as labelled outlines, one data file a platform under
//! `assets/presets/platform_overlays/`. Off until chosen in the viewer's
//! overlay menu; nothing else reads them.

use serde::Deserialize;

/// One platform's player, as its file describes it.
#[derive(Debug, Clone, Deserialize)]
pub struct PlatformOverlay {
    pub id: String,
    pub name: String,
    /// The frame the zones are laid out for, width to height.
    pub aspect: [u32; 2],
    /// When the numbers were last checked against the platform.
    pub checked: String,
    #[serde(rename = "zone")]
    pub zones: Vec<Zone>,
}

/// One area the player covers.
#[derive(Debug, Clone, Deserialize)]
pub struct Zone {
    pub label: String,
    /// `[x, y, width, height]` as a share of the frame, from its top left.
    pub rect: [f32; 4],
}

const FILES: [&str; 4] = [
    include_str!("../../../assets/presets/platform_overlays/youtube.toml"),
    include_str!("../../../assets/presets/platform_overlays/youtube_shorts.toml"),
    include_str!("../../../assets/presets/platform_overlays/instagram_reels.toml"),
    include_str!("../../../assets/presets/platform_overlays/tiktok.toml"),
];

/// Every platform overlay, in the menu's order, read once. A file that
/// doesn't read is left out and logged.
pub fn all() -> &'static [PlatformOverlay] {
    static ALL: std::sync::OnceLock<Vec<PlatformOverlay>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        FILES
            .iter()
            .filter_map(|text| toml::from_str(text).map_err(|e| tracing::warn!("a platform overlay file doesn't read: {e}")).ok())
            .collect()
    })
}

/// The overlay with `id`.
pub fn by_id(id: &str) -> Option<&'static PlatformOverlay> {
    all().iter().find(|p| p.id == id)
}

/// Every overlay in the viewer menu's order for a sequence whose brief
/// names `platform`: those its destination lists first (#3307), in the
/// destination's order, then the rest.
pub fn menu_order(platform: &str) -> Vec<&'static PlatformOverlay> {
    let platform = platform.trim();
    let first: &[String] = klipp_core::project_formats::formats()
        .iter()
        .filter_map(|f| f.destination.as_ref())
        .find(|d| !platform.is_empty() && d.platform.trim().eq_ignore_ascii_case(platform))
        .map_or(&[], |d| d.overlays.as_slice());
    let mut out: Vec<&'static PlatformOverlay> = first.iter().filter_map(|id| by_id(id)).collect();
    out.extend(all().iter().filter(|p| !first.contains(&p.id)));
    out
}

impl PlatformOverlay {
    /// Whether a frame `width` by `height` has the shape this overlay is
    /// laid out for, within 1%.
    pub fn fits(&self, width: f32, height: f32) -> bool {
        let want = self.aspect[0] as f32 / self.aspect[1] as f32;
        height > 0.0 && ((width / height) / want - 1.0).abs() < 0.01
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every platform's file reads, each with its own id, a frame shape,
    /// a date it was checked, and zones that are labelled and lie inside
    /// the frame.
    #[test]
    fn every_platform_file_reads_and_its_zones_fit_the_frame() {
        let all = all();
        assert_eq!(all.len(), FILES.len(), "every file reads");
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "each its own id");
        for p in all {
            assert!(!p.name.is_empty() && p.aspect[0] > 0 && p.aspect[1] > 0 && !p.checked.is_empty(), "{}: name, frame and date", p.id);
            assert!(!p.zones.is_empty(), "{}: zones", p.id);
            for z in &p.zones {
                let [x, y, w, h] = z.rect;
                assert!(!z.label.is_empty(), "{}: a zone without a label", p.id);
                assert!(w > 0.0 && h > 0.0, "{} {}: an empty rectangle", p.id, z.label);
                assert!(x >= 0.0 && y >= 0.0 && x + w <= 1.0 + 1e-4 && y + h <= 1.0 + 1e-4, "{} {}: {:?} past the frame", p.id, z.label, z.rect);
            }
        }
        assert!(by_id("tiktok").is_some_and(|p| p.fits(1080.0, 1920.0) && !p.fits(1920.0, 1080.0)));
    }

    /// A sequence made for a destination lists that destination's
    /// overlays first, the rest after; with none, or one Klipp doesn't
    /// know, the menu's own order. Every destination's overlay is one
    /// there is.
    #[test]
    fn a_destinations_overlays_come_first_in_the_menu() {
        let ids = |platform: &str| menu_order(platform).iter().map(|p| p.id.as_str()).collect::<Vec<_>>();
        let own: Vec<&str> = all().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids("youtube shorts, instagram reels, tiktok"), ["youtube_shorts", "instagram_reels", "tiktok", "youtube"]);
        assert_eq!(ids("YouTube"), own);
        assert_eq!(ids(""), own);
        assert_eq!(ids("Podcast"), own);
        for d in klipp_core::project_formats::formats().iter().filter_map(|f| f.destination.as_ref()) {
            assert!(d.overlays.iter().all(|id| by_id(id).is_some()), "{}: {:?} names an overlay there isn't", d.platform, d.overlays);
            assert_eq!(menu_order(&d.platform).len(), all().len(), "{}: each overlay once", d.platform);
        }
    }
}
