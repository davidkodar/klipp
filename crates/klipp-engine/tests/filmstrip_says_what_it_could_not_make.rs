//! The filmstrip reports every tile it could not make, and why.
//!
//! A tile the decoder could not reach, seek to or convert used to be left
//! out in silence, and the run came back as `Ok(count)` — `Ok(0)` when a
//! hardware decoder opened the file and then rejected every frame — which
//! the app then wrote to the disk cache as the clip's complete filmstrip.
//! The code's own comment says a partial cache must never pose as a
//! complete one; this is the case it did not cover.

mod common;

use klipp_engine::{generate_filmstrip_incremental, FilmstripOutcome};

const FPS: f64 = 25.0;
/// The ramp's length, as the media item would declare it.
const DURATION: f64 = common::RAMP_N as f64 / FPS;
const INTERVAL: f64 = 0.4;
/// Tiles wanted at 0, 0.4, 0.8, 1.2 and 1.6 s.
const TILES: usize = 5;

/// Run the strip; the times of the tiles made, and the outcome.
fn strip(path: &std::path::Path, duration: f64) -> (Vec<f64>, FilmstripOutcome) {
    let mut made = Vec::new();
    let outcome = generate_filmstrip_incremental(path, duration, INTERVAL, 48, |t, _, _, _| made.push(t), None).expect("the strip runs");
    (made, outcome)
}

#[test]
fn a_whole_file_makes_every_tile_and_skips_none() {
    let Some(src) = common::hevc_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (made, outcome) = strip(&src, DURATION);
    eprintln!("whole: made {made:?}, skipped {:?}", outcome.skipped);
    assert_eq!(made.len(), TILES);
    assert_eq!(outcome.tiles, TILES);
    assert!(outcome.skipped.is_empty(), "a whole file skipped tiles: {:?}", outcome.skipped);
}

/// The file ends after 28 of its 40 packets — about 1.1 s of a clip the
/// media item still says is 1.6 s long. The tiles past its end are
/// reported, with the time each was wanted at, and the rest are made.
///
/// Where exactly the content ends is not a whole tile: the cut is in
/// decode order, and a P-frame kept while the B-frames before it were cut
/// still displays late, so the tile at 1.2 s may be made from it. The
/// tile at 1.6 s cannot be.
#[test]
fn a_file_that_ends_early_reports_the_tiles_past_its_end() {
    let Some(src) = common::hevc_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let path = common::cut_after_packet(&src, 27, "strip-truncated");
    let (made, outcome) = strip(&path, DURATION);
    eprintln!("cut short: made {made:?}, skipped {:?}", outcome.skipped);
    assert!(made.starts_with(&[0.0, 0.4, 0.8]), "the tiles inside the file were not made: {made:?}");
    assert_eq!(made.len() + outcome.skipped.len(), TILES, "every tile is accounted for, made or skipped");
    let skipped_at: Vec<f64> = outcome.skipped.iter().map(|(t, _)| *t).collect();
    assert!(skipped_at.contains(&1.6), "the tile past the end was not reported: {skipped_at:?}");
    assert!(skipped_at.iter().all(|t| *t > 1.1), "a tile inside the file was reported as unreachable: {skipped_at:?}");
    for (t, why) in &outcome.skipped {
        assert!(why.contains("no frame reached"), "tile at {t}s: the reason does not say what happened: {why}");
    }
    let _ = std::fs::remove_file(&path);
}

/// Every frame destroyed, container intact: the decoder opens the file
/// and can make nothing of it. No tiles, every one reported — which is
/// what lets the caller refuse to cache an empty strip as a finished one.
#[test]
fn a_file_with_no_decodable_frame_makes_no_tiles_and_reports_every_one() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let path = common::every_frame_damaged(&src, "strip-all-damaged");
    let (made, outcome) = strip(&path, DURATION);
    eprintln!("all damaged: made {made:?}, skipped {:?}", outcome.skipped);
    assert!(made.is_empty(), "tiles were made from frames that cannot be decoded: {made:?}");
    assert_eq!(outcome.tiles, 0);
    assert_eq!(outcome.skipped.len(), TILES, "every tile is accounted for");
    for (t, why) in &outcome.skipped {
        assert!(why.contains("no frame reached"), "tile at {t}s: {why}");
    }
    let _ = std::fs::remove_file(&path);
}
