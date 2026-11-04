//! An EDL whose picture events overlap (Klipp's export of two picture
//! tracks onto CMX 3600's one, the top track first) comes in on as many
//! tracks as the overlaps need, each move in the warnings, and every clip
//! keeps its times through a save and reopen, the open's repair included.

use klipp_core::{Fps, MediaId};
use klipp_media::MediaPool;
use klipp_project::edl_import::{build, parse, Matched};
use klipp_project::{load_project, save_project};
use klipp_timeline::{Sequence, TrackKind};

const LIST: &str = "TITLE: Two tracks
FCM: NON-DROP FRAME

001  SPEECH   V     C        00:00:01:00 00:00:05:00 00:00:03:00 00:00:07:00
* FROM CLIP NAME: speech.mp4
002  SPILL    V     C        00:00:02:00 00:00:08:00 00:00:00:00 00:00:06:00
* FROM CLIP NAME: spillway.mp4
003  MUD      V     C        00:00:00:00 00:00:05:00 00:00:06:00 00:00:11:00
* FROM CLIP NAME: mudpot.mp4
";

type Layout = Vec<(String, Vec<(f64, f64, f64, f64)>)>;

fn layout(seq: &Sequence) -> Layout {
    let secs = |t: klipp_core::Timecode| (t.to_secs_f64() * 100.0).round() / 100.0;
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|t| (t.name.clone(), t.clips.iter().filter(|c| !c.kind.is_gap()).map(|c| (secs(c.timeline_in), secs(c.timeline_out), secs(c.source_in), secs(c.source_out))).collect()))
        .collect()
}

#[test]
fn overlapping_events_get_tracks_of_their_own_and_keep_their_times() {
    let fps = Fps::new(25, 1);
    let ids = [("SPEECH", MediaId::new()), ("SPILL", MediaId::new()), ("MUD", MediaId::new())];
    let edl = parse(LIST, fps);
    let (seq, warnings) = build(&edl, "Two tracks", fps, |e| ids.iter().find(|(r, _)| *r == e.reel).map(|(_, id)| Matched { media_id: *id, start_frames: None }));
    let imported = layout(&seq);
    // The two tracks the export was made from: speech over the other two.
    assert_eq!(
        imported,
        vec![("Video 2".to_string(), vec![(3.0, 7.0, 1.0, 5.0)]), ("Video 1".to_string(), vec![(0.0, 6.0, 2.0, 8.0), (6.0, 11.0, 0.0, 5.0)])],
        "{warnings:?}"
    );
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].starts_with("Event 002: overlaps an event on Video 2, so it was put on Video 1"), "{warnings:?}");

    let dir = std::env::temp_dir().join(format!("klipp_edl_overlap_round_trip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let (mut sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(sequences[0].mend_overlaps(), 0, "nothing for the open to mend");
    assert_eq!(layout(&sequences[0]), imported, "reopened as imported");
}
