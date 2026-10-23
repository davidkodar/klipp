//! A large JPEG still shows (#2474): a 4032x3024 phone photo was opened
//! through the GPU's JPEG decoder, which, read again for a clip that starts
//! later in its source, reached the end of the file with no frame, so the
//! viewer showed nothing where the export drew it. A still is decoded in
//! software, and its frame comes from any point.

use std::time::{Duration, Instant};

use klipp_engine::video_decoder::VideoDecoder;

/// A 4032x3024 baseline JPEG, 4:2:0 full range, as a phone writes one.
fn photo() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-photo-4032x3024-{}.jpeg", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=4032x3024", "-frames:v", "1", "-pix_fmt", "yuvj420p", "-q:v", "3"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

#[test]
fn a_large_jpeg_still_hands_over_its_picture() {
    let Some(path) = photo() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dec = VideoDecoder::open(&path, 0).expect("open");
    dec.resume();
    let until = Instant::now() + Duration::from_secs(10);
    let frame = loop {
        if let Some(f) = dec.frame_queue().lock().unwrap().pop() {
            assert!(!f.is_eof, "the still ended with no picture");
            break f;
        }
        assert!(Instant::now() < until, "no frame of the still within 10 s");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(frame.width > 0 && frame.height > 0 && frame.width * 3 == frame.height * 4, "a 4:3 picture: {}x{}", frame.width, frame.height);
    // A clip of it from 3 s into its source: the still is read again.
    let epoch = dec.seek(3.0);
    let until = Instant::now() + Duration::from_secs(10);
    let again = loop {
        if let Some(f) = dec.frame_queue().lock().unwrap().pop() {
            if f.seek_epoch < epoch {
                continue;
            }
            assert!(!f.is_eof, "after a seek the still ended with no picture");
            break f;
        }
        assert!(Instant::now() < until, "no frame of the still within 10 s after a seek");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(again.width == frame.width);
    assert!(!dec.hw_active.load(std::sync::atomic::Ordering::Relaxed), "decoded in software");
    let _ = std::fs::remove_file(&path);
}
