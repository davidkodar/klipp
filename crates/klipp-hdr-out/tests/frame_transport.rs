//! The shared frame carries HDR display light intact, and refuses what it
//! cannot carry.
//!
//! This is the part that has to be exactly right: the presenter shows
//! whatever comes out of here, so a value mangled in transport is a wrong
//! reference picture with nothing to compare it against. The tests below
//! therefore care most about the two things a monitoring path gets wrong
//! quietly — values *above* 1.0, which are the whole point of an HDR feed
//! and the first casualty of any clamp; and a reader landing mid-publish,
//! which would show half of one frame and half of another.

use klipp_hdr_out::{FrameReader, FrameWriter, ReadError};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-hdr-out-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A frame of HDR display light: 1.0 is SDR reference white, so this
/// spans deep shadow to about fifty times it. Deliberately includes the
/// values an SDR path cannot hold.
fn hdr_frame(w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / w.max(1) as f32;
            let l = (y as f32 / h.max(1) as f32) * 50.0;
            v.extend_from_slice(&[l * (0.2 + 0.8 * t), l * 0.9, l * (0.1 + 0.9 * t), 1.0]);
        }
    }
    v
}

/// A published frame arrives bit-for-bit, highlights included.
#[test]
fn a_frame_survives_the_round_trip_exactly() {
    let path = scratch("roundtrip.frame");
    let (w, h) = (64u32, 32u32);
    let mut writer = FrameWriter::create(&path, w, h).unwrap();
    let frame = hdr_frame(w, h);
    assert!(writer.publish(&frame));

    let mut reader = FrameReader::open(&path).unwrap();
    assert_eq!((reader.width(), reader.height()), (w, h));
    let mut got = Vec::new();
    assert!(reader.read_latest(&mut got), "a frame was published");
    assert_eq!(got.len(), frame.len());
    // Bit-for-bit, not approximately: this is a memcpy, and anything less
    // than exact means something is interpreting the values on the way.
    let differing = got.iter().zip(frame.iter()).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
    assert_eq!(differing, 0, "{differing} of {} values changed in transit", frame.len());

    // And the frame really does carry HDR: if anything clamped at 1.0 the
    // comparison above would still pass on a fixture that never exceeded
    // it, so check the fixture does.
    let peak = frame.iter().cloned().fold(0.0f32, f32::max);
    assert!(peak > 40.0, "the fixture only reaches {peak}x reference white");
}

/// Reading twice returns the frame once: a presenter that re-uploaded the
/// same frame every wake would spend a 4K memcpy per vsync for nothing.
#[test]
fn a_frame_is_offered_once() {
    let path = scratch("once.frame");
    let mut writer = FrameWriter::create(&path, 16, 16).unwrap();
    let mut reader = FrameReader::open(&path).unwrap();
    let mut got = Vec::new();
    assert!(!reader.read_latest(&mut got), "nothing published yet");

    assert!(writer.publish(&hdr_frame(16, 16)));
    assert!(reader.read_latest(&mut got), "the published frame");
    assert!(!reader.read_latest(&mut got), "the same frame again");

    assert!(writer.publish(&hdr_frame(16, 16)));
    assert!(reader.read_latest(&mut got), "the next frame");
}

/// The newest frame wins, and an old one is never shown after it.
///
/// A monitor feed wants the latest picture, not a queue: falling behind
/// should drop frames, not delay them.
#[test]
fn the_reader_gets_the_newest_frame() {
    let path = scratch("newest.frame");
    let (w, h) = (8u32, 8u32);
    let mut writer = FrameWriter::create(&path, w, h).unwrap();
    let mut reader = FrameReader::open(&path).unwrap();

    // Publish several without reading, then read once.
    for k in 1..=5u32 {
        let frame: Vec<f32> = (0..(w * h * 4)).map(|_| k as f32).collect();
        assert!(writer.publish(&frame));
    }
    let mut got = Vec::new();
    assert!(reader.read_latest(&mut got), "a frame");
    assert!(got.iter().all(|v| *v == 5.0), "got a stale frame: {:?}", &got[..4]);
}

/// A frame of the wrong size is refused rather than published.
///
/// Publishing it would put a sheared picture on a reference monitor, which
/// is the kind of wrongness that gets blamed on the grade.
#[test]
fn a_frame_of_the_wrong_size_is_refused() {
    let path = scratch("size.frame");
    let mut writer = FrameWriter::create(&path, 16, 16).unwrap();
    assert!(!writer.publish(&hdr_frame(16, 15)), "a short frame");
    assert!(!writer.publish(&hdr_frame(17, 16)), "a long frame");
    assert!(!writer.publish(&[]), "nothing");
    assert!(writer.publish(&hdr_frame(16, 16)), "the right size");

    // And nothing was published by the refusals: the reader sees one frame.
    let mut reader = FrameReader::open(&path).unwrap();
    let mut got = Vec::new();
    assert!(reader.read_latest(&mut got));
    assert!(!reader.read_latest(&mut got));
}

/// A zero-sized frame is refused at creation.
#[test]
fn a_zero_sized_frame_is_refused() {
    assert!(FrameWriter::create(&scratch("zero.frame"), 0, 16).is_err());
    assert!(FrameWriter::create(&scratch("zero2.frame"), 16, 0).is_err());
}

/// What the presenter does with a file it cannot trust.
///
/// Each of these would otherwise be shown as a picture: an unrelated file
/// read as pixels, a frame laid out by a different Klipp, or a mapping
/// shorter than its header claims.
#[test]
fn a_file_it_cannot_trust_is_refused() {
    let path = scratch("junk.frame");
    std::fs::write(&path, vec![0u8; 8192]).unwrap();
    assert_eq!(FrameReader::open(&path).err(), Some(ReadError::NotAFrame));

    let missing = scratch("absent.frame");
    let _ = std::fs::remove_file(&missing);
    assert_eq!(FrameReader::open(&missing).err(), Some(ReadError::NotAFrame));

    // A valid frame with its version bumped: a presenter from another
    // build must say so rather than guess at the layout.
    let good = scratch("version.frame");
    let w = FrameWriter::create(&good, 8, 8).unwrap();
    drop(w);
    // `drop` clears the magic and removes the file, so rebuild one and
    // patch the version byte in place.
    let w = FrameWriter::create(&good, 8, 8).unwrap();
    let mut bytes = std::fs::read(&good).unwrap();
    bytes[8..12].copy_from_slice(&(klipp_hdr_out::PROTOCOL_VERSION + 1).to_le_bytes());
    let other = scratch("version_other.frame");
    std::fs::write(&other, &bytes).unwrap();
    assert_eq!(
        FrameReader::open(&other).err(),
        Some(ReadError::Version(klipp_hdr_out::PROTOCOL_VERSION + 1)),
    );
    drop(w);

    // A header that claims more than the file holds.
    let short = scratch("short.frame");
    let w = FrameWriter::create(&short, 64, 64).unwrap();
    let bytes = std::fs::read(&short).unwrap();
    let truncated = scratch("truncated.frame");
    std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(FrameReader::open(&truncated).err(), Some(ReadError::Truncated));
    drop(w);
}

/// The presenter can tell the viewer has gone.
///
/// Without this it would keep showing the last frame for ever, which on a
/// reference monitor reads as "the grade is fine" long after the grade
/// stopped being sent.
#[test]
fn the_reader_notices_the_writer_leaving() {
    let path = scratch("leaving.frame");
    let mut writer = FrameWriter::create(&path, 8, 8).unwrap();
    writer.publish(&hdr_frame(8, 8));
    let reader = FrameReader::open(&path).unwrap();
    assert!(reader.writer_present());
    drop(writer);
    assert!(!reader.writer_present(), "the writer is gone and the reader still believes in it");
}

/// A reader running flat out against a writer never sees two frames
/// mixed together.
///
/// The interesting failure is not a dropped frame — a monitor feed can
/// afford those — it is a *torn* one: the top half of frame N over the
/// bottom half of N+1, which looks like a glitch in the picture rather
/// than in the plumbing. Each frame here is a single repeated value, so a
/// tear is unmistakable.
#[test]
fn a_reader_never_sees_two_frames_at_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let path = scratch("tearing.frame");
    // Big enough that a publish takes long enough to be caught mid-copy.
    let (w, h) = (512u32, 512u32);
    let mut writer = FrameWriter::create(&path, w, h).unwrap();
    let n = (w * h * 4) as usize;
    let stop = Arc::new(AtomicBool::new(false));

    let reader_stop = Arc::clone(&stop);
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        let mut reader = FrameReader::open(&reader_path).unwrap();
        let mut seen = 0usize;
        let mut torn = Vec::new();
        let mut frame = Vec::new();
        while !reader_stop.load(Ordering::Relaxed) {
            if reader.read_latest(&mut frame) {
                seen += 1;
                let first = frame[0];
                if let Some(bad) = frame.iter().find(|v| **v != first) {
                    torn.push((first, *bad));
                }
            }
        }
        (seen, torn)
    });

    // Paced roughly like a real feed rather than flat out. Unthrottled,
    // the writer finishes 400 publishes of 4 MB before the reader has
    // taken a handful, and the reader then sees two frames of four
    // hundred — no tearing, but no race either, and a test that does not
    // race proves nothing about a lock-free read.
    let mut frame = vec![0.0f32; n];
    const FRAMES: u32 = 300;
    for k in 1..=FRAMES {
        frame.fill(k as f32);
        assert!(writer.publish(&frame));
        std::thread::sleep(std::time::Duration::from_micros(500));
    }
    stop.store(true, Ordering::Relaxed);
    let (seen, torn) = reader.join().unwrap();

    eprintln!("reader saw {seen} frames of {FRAMES}, {} torn", torn.len());
    assert!(torn.is_empty(), "torn frames: {:?}", &torn[..torn.len().min(4)]);
    // And it really did race: the reader has to have kept up with a good
    // share of the frames, or it never overlapped a publish at all.
    assert!(
        seen > FRAMES as usize / 4,
        "the reader only saw {seen} of {FRAMES}, so it barely raced the writer and \
         tearing was never given a chance to happen",
    );
}

/// The master's peak travels alongside the picture, for the presenter's
/// own reporting. It is information, not a scale — the pixels carry light.
#[test]
fn the_peak_travels_with_the_frame() {
    let path = scratch("peak.frame");
    let writer = FrameWriter::create(&path, 8, 8).unwrap();
    writer.set_peak_nits(4000.0);
    let reader = FrameReader::open(&path).unwrap();
    assert_eq!(reader.peak_nits(), 4000.0);
}

/// The light survives the whole path, at the levels it is judged on.
///
/// The presenter uploads the frame as `Rgba16Float`, because that is the
/// surface's own precision. Sixteen-bit float has about three decimal
/// digits, so the question a colourist would ask — does a 4000 cd/m²
/// highlight still read 4000 — has a number, and this is it. Also the
/// question that matters more: f16's largest value is 65504, so nothing
/// in an HDR grade overflows it, and a wrong conversion would show as
/// infinity or a wrapped sign rather than a small error.
#[test]
fn the_light_survives_the_transport() {
    /// BT.2408 reference white — what display light 1.0 means.
    const REF_WHITE_NITS: f32 = 203.0;
    let path = scratch("light.frame");
    // One pixel per level, so nothing is averaged or filtered.
    let levels = [0.005f32, 5.0, 26.0, 100.0, 203.0, 400.0, 1000.0, 4000.0, 10000.0];
    let (w, h) = (levels.len() as u32, 1u32);
    let mut writer = FrameWriter::create(&path, w, h).unwrap();
    let mut frame = Vec::new();
    for nits in levels {
        let l = nits / REF_WHITE_NITS;
        frame.extend_from_slice(&[l, l, l, 1.0]);
    }
    assert!(writer.publish(&frame));

    let mut reader = FrameReader::open(&path).unwrap();
    let mut got = Vec::new();
    assert!(reader.read_latest(&mut got));

    let mut worst_rel = 0.0f64;
    for (i, nits) in levels.iter().enumerate() {
        // Shared memory is exact; the f16 the presenter uploads is not.
        let shared = got[i * 4];
        assert_eq!(shared.to_bits(), frame[i * 4].to_bits(), "{nits} cd/m² changed in shm");
        let as_f16 = half::f16::from_f32(shared);
        assert!(as_f16.is_finite(), "{nits} cd/m² became {as_f16} as f16");
        let back = as_f16.to_f32() * REF_WHITE_NITS;
        let rel = ((back - nits) / nits.max(1e-6)).abs() as f64;
        worst_rel = worst_rel.max(rel);
        eprintln!("  {nits:>8.3} cd/m² -> f16 -> {back:>8.3}  ({:+.4}%)", rel * 100.0);
    }
    // f16 carries eleven bits of mantissa, so a twentieth of a percent is
    // the floor. Anything larger means something clamped or rounded badly.
    assert!(
        worst_rel < 0.001,
        "the worst level moved by {:.4}%, which is more than f16 costs",
        worst_rel * 100.0,
    );
}
