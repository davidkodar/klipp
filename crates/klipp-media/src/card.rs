//! Camera cards and what is on them. A card copied whole brings proxies,
//! thumbnails and index files beside the clips, which an import should
//! leave alone; and a long take a camera split at the file-system's
//! limit arrives as numbered parts that belong back to back.

use crate::MediaItem;
use klipp_core::{timecode_to_secs, MediaId};
use std::path::Path;

/// Folders on a card that hold proxies, thumbnails and indexes rather
/// than clips: Sony's Sub and Thmbnl, AVCHD's Clipinf, Playlist and
/// Backup, Panasonic's Proxy, Icon and Voice, Sony XDCAM's General.
const SIDE_DIRS: [&str; 11] = ["sub", "thmbnl", "clipinf", "playlist", "backup", "proxy", "icon", "voice", "general", "takr", "mediapro"];

/// Whether a file on a card is one of the extras beside the clips, by
/// its path below the card: a folder of the same name above the card,
/// or off a card, says nothing.
pub fn is_card_side_file(path: &Path) -> bool {
    path.ancestors()
        .skip(1)
        .filter_map(|d| d.file_name().and_then(|n| n.to_str()))
        .any(|n| SIDE_DIRS.contains(&n.to_ascii_lowercase().as_str()))
}

/// What kind of card a folder is, if it is one, from the layout its
/// maker uses.
pub fn card_kind(root: &Path) -> Option<&'static str> {
    let has = |rel: &str| root.join(rel).is_dir();
    if has("PRIVATE/M4ROOT") || has("M4ROOT") {
        Some("Sony XAVC card")
    } else if has("XDROOT") {
        Some("Sony XDCAM card")
    } else if has("PRIVATE/AVCHD") || has("AVCHD") || has("BDMV") {
        Some("AVCHD card")
    } else if has("CONTENTS/CLIPS001") {
        Some("Canon XF card")
    } else if has("CONTENTS/VIDEO") {
        Some("Panasonic P2 card")
    } else if has("DCIM") {
        Some("camera card")
    } else {
        None
    }
}

/// A file name's trailing number and what comes before it: "C0012" is
/// ("C", 12, 4 digits). Camera parts count in a fixed width.
fn numbered_stem(name: &str) -> Option<(&str, u64, usize)> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    let digits = stem.chars().rev().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let (prefix, number) = stem.split_at(stem.len() - digits);
    Some((prefix, number.parse().ok()?, digits))
}

/// A part a camera wrote when it hit its file-size limit: two gigabytes
/// less a little, or four.
const SPLIT_SIZE: u64 = 1_900 * 1024 * 1024;

/// Whether `b` continues `a`: the next number in the same series, and
/// either its timecode picks up where `a`'s ends or `a` is a full
/// split-size part.
fn continues(a: &MediaItem, b: &MediaItem) -> bool {
    let (Some((pa, na, wa)), Some((pb, nb, wb))) = (numbered_stem(&a.filename), numbered_stem(&b.filename)) else { return false };
    if !pa.eq_ignore_ascii_case(pb) || wa != wb || nb != na + 1 {
        return false;
    }
    if a.path.parent() != b.path.parent() || a.video_codec != b.video_codec || a.resolution != b.resolution || a.fps != b.fps {
        return false;
    }
    match (&a.shot.start_timecode, &b.shot.start_timecode) {
        (Some(ta), Some(tb)) => match (timecode_to_secs(ta, a.fps), timecode_to_secs(tb, a.fps)) {
            (Some(sa), Some(sb)) => (sb - (sa + a.duration.to_secs_f64())).abs() <= 1.5 / a.fps.as_f64().max(1.0),
            _ => a.file_size_bytes >= SPLIT_SIZE,
        },
        _ => a.file_size_bytes >= SPLIT_SIZE,
    }
}

/// The runs of parts among `items`, each in order, only runs of two or
/// more.
pub fn spanned_groups(items: &[&MediaItem]) -> Vec<Vec<MediaId>> {
    let mut sorted: Vec<&MediaItem> = items.iter().copied().filter(|m| !m.is_still && !m.video_codec.is_empty()).collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut groups: Vec<Vec<MediaId>> = Vec::new();
    let mut run: Vec<&MediaItem> = Vec::new();
    for item in sorted {
        if let Some(last) = run.last() {
            if continues(last, item) {
                run.push(item);
                continue;
            }
            if run.len() >= 2 {
                groups.push(run.iter().map(|m| m.id).collect());
            }
        }
        run = vec![item];
    }
    if run.len() >= 2 {
        groups.push(run.iter().map(|m| m.id).collect());
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaItemKind;
    use std::path::PathBuf;

    fn part(name: &str, size_gib: f64, tc: Option<&str>, secs: f64) -> MediaItem {
        let mut m = MediaItem::synthetic_for_test(PathBuf::from("/card/PRIVATE/M4ROOT/CLIP").join(name), None);
        m.kind = MediaItemKind::File;
        m.filename = name.into();
        m.video_codec = "H264".into();
        m.resolution = klipp_core::Resolution { width: 3840, height: 2160 };
        m.fps = klipp_core::Fps::new(25, 1);
        m.file_size_bytes = (size_gib * 1024.0 * 1024.0 * 1024.0) as u64;
        m.shot.start_timecode = tc.map(str::to_string);
        m.duration = klipp_core::RationalTime::from_secs_f64(secs, 25);
        m
    }

    #[test]
    fn the_extras_on_a_card_are_told_from_the_clips() {
        assert!(is_card_side_file(Path::new("/card/PRIVATE/M4ROOT/SUB/C0001S01.MP4")));
        assert!(is_card_side_file(Path::new("/card/PRIVATE/AVCHD/BDMV/CLIPINF/00000.CPI")));
        assert!(is_card_side_file(Path::new("/card/CONTENTS/PROXY/0001AB.MP4")));
        assert!(!is_card_side_file(Path::new("/card/PRIVATE/M4ROOT/CLIP/C0001.MP4")));
        let dir = std::env::temp_dir().join(format!("klipp-card-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("PRIVATE/M4ROOT/CLIP")).unwrap();
        assert_eq!(card_kind(&dir), Some("Sony XAVC card"));
        assert_eq!(card_kind(&dir.join("PRIVATE/M4ROOT/CLIP")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn numbered_parts_join_when_their_timecodes_meet_or_the_first_is_a_full_part() {
        // Timecode continuity: 10 s at 25 fps from 01:00:00:00 ends at 01:00:10:00.
        let a = part("C0001.MP4", 0.5, Some("01:00:00:00"), 10.0);
        let b = part("C0002.MP4", 0.5, Some("01:00:10:00"), 5.0);
        let c = part("C0003.MP4", 0.5, Some("02:00:00:00"), 5.0);
        let groups = spanned_groups(&[&c, &b, &a]);
        assert_eq!(groups, vec![vec![a.id, b.id]]);
        // No timecode: only a full-size first part continues.
        let d = part("MVI_0100.MP4", 3.99, None, 600.0);
        let e = part("MVI_0101.MP4", 0.7, None, 90.0);
        let f = part("MVI_0102.MP4", 0.6, None, 30.0);
        assert_eq!(spanned_groups(&[&d, &e, &f]), vec![vec![d.id, e.id]]);
        // A different series, width or folder does not join.
        let g = part("C0002.MP4", 3.99, None, 10.0);
        let mut h = part("C0003.MP4", 0.5, None, 10.0);
        h.path = PathBuf::from("/elsewhere/C0003.MP4");
        assert!(spanned_groups(&[&g, &h]).is_empty());
        assert_eq!(numbered_stem("C0012.MP4"), Some(("C", 12, 4)));
        assert_eq!(numbered_stem("take.mov"), None);
    }
}
