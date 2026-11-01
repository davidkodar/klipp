//! #3212: what showing a kept 4K frame costs, phase by phase: the file
//! read, LZ4, the unpack to linear light, and the display encode, on the
//! CPU and (#3300) on the GPU. Measured, not a check: run alone with
//! `--ignored --nocapture`.

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
    let mut gpu = klipp_effects::gpu_context::GpuContext::shared().and_then(klipp_effects::gpu::TenBitDisplay::new);
    let (mut t, mut on_cpu, mut on_gpu) = (klipp_engine::cache_store::ReadTimes::default(), 0.0f32, 0.0f32);
    for run in 0..=runs {
        let (stored, times) = klipp_engine::cache_store::read_stored_for_screen(&root, "frames", at).unwrap();
        let klipp_engine::cache_store::Stored::TenBit(words) = &stored else { panic!("kept in ten bits") };
        let s = Instant::now();
        let gpu_bytes = encode.bytes_on(klipp_engine::cache_store::Stored::TenBit(words.clone()), (w, h), gpu.as_mut());
        let gpu_ms = s.elapsed().as_secs_f32() * 1000.0;
        let s = Instant::now();
        let bytes = encode.bytes(stored);
        let cpu_ms = s.elapsed().as_secs_f32() * 1000.0;
        assert_eq!(bytes.len(), (w * h * 4) as usize);
        assert!(gpu.is_none() || (gpu_bytes.1 && gpu_bytes.0 == bytes), "the GPU's bytes are the CPU's");
        // The first run builds the GPU's buffers: not counted.
        if run > 0 {
            t.read_ms += times.read_ms;
            t.lz4_ms += times.lz4_ms;
            t.unpack_ms += times.unpack_ms;
            on_cpu += cpu_ms;
            on_gpu += gpu_ms;
        }
    }
    let n = runs as f32;
    let file_mb = std::fs::metadata(&file).unwrap().len() as f64 / 1e6;
    eprintln!(
        "MEASURE kept 4K frame ({file_mb:.1} MB on disk), on the reading thread: read {:.1} ms, LZ4 {:.1} ms, ten bits to display bytes {:.1} ms on the CPU, {} on the GPU; total {:.1} ms on the CPU",
        t.read_ms / n, t.lz4_ms / n, on_cpu / n, if gpu.is_some() { format!("{:.1} ms", on_gpu / n) } else { "no GPU".into() }, (t.read_ms + t.lz4_ms + t.unpack_ms + on_cpu) / n,
    );
    let _ = std::fs::remove_dir_all(&root);
}
