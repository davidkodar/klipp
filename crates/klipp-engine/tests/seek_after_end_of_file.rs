//! A seek after the decoder has read to the end of the file still shows a
//! frame.
//!
//! H.264/HEVC in MP4 reaches NVDEC through ffmpeg's mp4toannexb bitstream
//! filter. Once that filter has seen the end of the file it refuses every
//! later packet ("A non-NULL packet sent after an EOF") unless it is
//! flushed, and the decoder stayed silent: parked near the end of a clip,
//! every later click or step on it showed nothing.

mod common;

use std::time::Duration;

use klipp_core::AppCommand;

#[test]
fn a_seek_back_from_the_end_of_the_file_shows_a_frame() {
    let Some(src) = common::hevc_source("sdr-hevc", 640, 360, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 640, 360);
    let fps = parked.fps;
    // Near the end of the 50-frame file: the decoder reads ahead to EOF.
    for (to, label) in [(47, "to the end"), (12, "back"), (30, "forward again")] {
        let g = parked.generation();
        parked.send(AppCommand::SetPlayhead(common::frame_tc(to, fps)));
        let got = parked.settle_after(g, Duration::from_secs(3));
        eprintln!("seek {label}: frame after {got:?}, playhead at {:?}", parked.frame_number());
        assert!(got.is_some(), "seek {label}: no frame");
        assert_eq!(parked.frame_number(), Some(to as f64), "seek {label}: the playhead is elsewhere");
    }
    // And a step still steps.
    let g = parked.generation();
    parked.send(AppCommand::SeekRelative(1));
    assert!(parked.wait_new(g, Duration::from_secs(3)).is_some(), "a step after them showed no frame");
}
