//! #3212: what showing a kept 4K frame costs, phase by phase: the file
//! read, LZ4, the unpack to linear light, and the display encode. Measured,
//! not a check: run alone with `--ignored --nocapture`.

use std::time::Instant;

use klipp_engine::cache_store::{CacheStore, FrameAt};

#[test]
#[ignore]
fn measure_a_kept_4k_frames_load() {
    let (w, h) = (3840u32, 2160u32);
    let root = std::env::temp_dir().join(format!("klipp-load-cost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // A picture with detail and gradients, opaque: kept in ten bits.
    let pixels: Vec<f32> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let n = ((i.wrapping_mul(2_654_435_761)) >> 24) as f32 / 255.0 * 0.05;
            [x * 0.8 + n, y * 0.6 + n, (x * y) + n, 1.0]
        })
        .collect();
    let at = FrameAt { tick: 0, key: 1, size: (w, h) };
    let mut store = CacheStore::open(&root, 1 << 34);
    store.put_frame("frames", at, &pixels).unwrap();
    let file = root.join("frames").join(at.file_name());
    let runs = 10;
    let encode = klipp_engine::frame_cache::DisplayEncode { tone: klipp_core::ToneMapOp::None, working: klipp_core::WorkingColorspace::Rec709Linear, transfer: klipp_core::SdrTransfer::Srgb };
    let (mut t, mut to_screen) = (klipp_engine::cache_store::ReadTimes::default(), 0.0f32);
    for _ in 0..runs {
        let (stored, times) = klipp_engine::cache_store::read_stored_for_screen(&root, "frames", at).unwrap();
        t.read_ms += times.read_ms;
        t.lz4_ms += times.lz4_ms;
        t.unpack_ms += times.unpack_ms;
        let s = Instant::now();
        let bytes = encode.bytes(stored);
        to_screen += s.elapsed().as_secs_f32() * 1000.0;
        assert_eq!(bytes.len(), (w * h * 4) as usize);
    }
    let n = runs as f32;
    let file_mb = std::fs::metadata(&file).unwrap().len() as f64 / 1e6;
    eprintln!(
        "MEASURE kept 4K frame ({file_mb:.1} MB on disk), on the reading thread: read {:.1} ms, LZ4 {:.1} ms, ten bits to display bytes {:.1} ms, total {:.1} ms",
        t.read_ms / n, t.lz4_ms / n, to_screen / n, (t.read_ms + t.lz4_ms + t.unpack_ms + to_screen) / n,
    );
    let _ = std::fs::remove_dir_all(&root);
}
