//! The HDR reference output carries the picture the export would write.
//!
//! `klipp-hdr-out`'s own tests prove the transport is lossless and the
//! presenter picks an HDR surface. What they cannot check is the thing
//! that decides whether the output is worth trusting: that the *engine*
//! publishes HDR display light — the same numbers the delivered file gets
//! — and not the SDR picture the screen happens to be showing.
//!
//! So this drives the real `PlaybackEngine` on a real PQ source, turns the
//! output on, and reads the published frame back through the presenter's
//! own reader. The levels are then checked in cd/m² against the source's,
//! because that is the only unit in which "is the reference output
//! correct" means anything.

mod common;

use std::time::Duration;

use klipp_core::{AppCommand, HdrMonitor};
use klipp_hdr_out::FrameReader;

/// BT.2408 reference white — what display light 1.0 means.
const REF_WHITE_NITS: f32 = 203.0;

/// The light of a pixel as the encoder measures it: the brightest channel
/// in BT.2020, cd/m².
fn nits_of(px: [f32; 3], ws: klipp_core::WorkingColorspace) -> f32 {
    klipp_color::hdr_view::HdrMeasure::for_working_space(ws).nits_of(px)
}

/// A PQ source whose bands sit on levels a colourist knows, published to
/// the reference output and read back at those same levels.
#[test]
fn the_published_frame_is_hdr_display_light() {
    let Some(path) = common::pq_bands() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (width, height) = (common::BANDS_W, common::BANDS_H);
    // The neutral bands plus the saturated one, in the order the fixture
    // paints them across the frame.
    let bands: Vec<f32> = common::BAND_NITS
        .iter()
        .map(|n| *n as f32)
        .chain(std::iter::once(common::SATURATED_RED_NITS as f32))
        .collect();
    // Ungraded, so the published levels can be checked against the
    // source's. The graded harness applies half a stop, and every band
    // came back 1.414x high until this said `start_ungraded` — which is a
    // correct reference output being measured against the wrong number.
    let parked = common::Parked::start_ungraded(path, width, height);

    // Monitoring on so the render produces display light, and the output
    // on so it is published. Both, because they are separate switches: one
    // decides what this screen shows, the other whether a reference
    // monitor is being fed.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Preview));
    parked.send(AppCommand::SetHdrReferenceOutput(true));
    std::thread::sleep(Duration::from_millis(1200));

    let path = klipp_hdr_out::default_path();
    let mut reader = match FrameReader::open(&path) {
        Ok(r) => r,
        Err(e) => panic!("the engine published nothing to {}: {e:?}", path.display()),
    };
    assert!(reader.writer_present(), "the engine is not publishing");
    assert_eq!(
        (reader.width(), reader.height()),
        (width, height),
        "the published frame is not the render's size",
    );

    let mut frame = Vec::new();
    let mut got = false;
    for _ in 0..200 {
        if reader.read_latest(&mut frame) {
            got = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(got, "the engine published no frame within four seconds");

    // The bands, measured where the file measures them.
    let ws = klipp_core::WorkingColorspace::Rec709Linear;
    let w = width as usize;
    let mut worst_rel = 0.0f64;
    for (i, want) in bands.iter().enumerate() {
        // The centre of each band, a quarter of the way down.
        let x = (i * 2 + 1) * w / (bands.len() * 2);
        let y = (height / 4) as usize;
        let p = (y * w + x) * 4;
        let got = nits_of([frame[p], frame[p + 1], frame[p + 2]], ws);
        let rel = ((got - want) / want.max(1e-6)) as f64;
        worst_rel = worst_rel.max(rel.abs());
        eprintln!("  band {i}: source {want:>7.1} cd/m², published {got:>7.1} ({:+.2}%)", rel * 100.0);
    }
    // Two per cent: the source's own 10-bit quantisation plus the PQ
    // curve's slope at these levels, not the output path's doing. A wrong
    // path here would be out by a factor, not a per cent — publishing the
    // SDR picture instead would read every band above reference white at
    // roughly the same clipped value.
    assert!(
        worst_rel < 0.02,
        "the published levels are {:.1}% out, which is more than the source's quantisation",
        worst_rel * 100.0,
    );

    // And it is *not* the SDR picture, which is the failure this whole
    // path exists to avoid: publishing the display-encoded frame would
    // look like a picture and be the wrong one. The discriminator is
    // simple — an SDR frame cannot hold anything above reference white, so
    // the 1000 cd/m² band would come back at 203 or below.
    let sample = |i: usize| {
        let x = (i * 2 + 1) * w / (bands.len() * 2);
        let p = ((height / 4) as usize * w + x) * 4;
        nits_of([frame[p], frame[p + 1], frame[p + 2]], ws)
    };
    let brightest = bands
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, n)| (i, *n))
        .unwrap();
    let got = sample(brightest.0);
    eprintln!(
        "the brightest band is {:.0} cd/m² in the source and {got:.0} published; \
         an SDR frame could not exceed {REF_WHITE_NITS:.0}",
        brightest.1,
    );
    assert!(brightest.1 > REF_WHITE_NITS * 4.0, "the fixture needs a real highlight");
    assert!(
        got > REF_WHITE_NITS * 2.0,
        "the brightest band came back at {got:.0} cd/m², under twice reference white — \
         this is the SDR picture, not display light",
    );

    // Turning it off stops the publishing, so a presenter does not sit on
    // a frame nothing is updating.
    parked.send(AppCommand::SetHdrReferenceOutput(false));
    std::thread::sleep(Duration::from_millis(600));
    assert!(!reader.writer_present(), "the engine kept the shared frame open after being told to stop");
}
