//! What a 10-bit source costs the export on the way in.
//!
//! The display-referred branch scales to RGBA64 with swscale; the
//! scene-referred one reads the planes and builds codes on the CPU. This
//! measures both at 4K so the next optimisation targets the real one.
#[test]
fn cost() {
    use ffmpeg_next::format::Pixel;
    ffmpeg_next::init().ok();
    let (w, h) = (3840u32, 2160u32);

    let mut src = ffmpeg_next::util::frame::Video::new(Pixel::P010LE, w, h);
    // Fill both planes with something non-constant.
    for plane in 0..2 {
        let stride = src.stride(plane);
        let rows = if plane == 0 { h as usize } else { h as usize / 2 };
        let data = src.data_mut(plane);
        for r in 0..rows {
            for b in 0..stride.min(w as usize * 2) {
                data[r * stride + b] = ((r * 7 + b * 13) % 251) as u8;
            }
        }
    }

    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        Pixel::P010LE, w, h,
        Pixel::RGBA64LE, w, h,
        ffmpeg_next::software::scaling::Flags::LANCZOS
            | ffmpeg_next::software::scaling::Flags::ACCURATE_RND,
    )
    .expect("scaler");
    for _ in 0..3 {
        let mut dst = ffmpeg_next::util::frame::Video::empty();
        let t = std::time::Instant::now();
        scaler.run(&src, &mut dst).expect("scale");
        eprintln!("swscale P010 -> RGBA64: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    }

    // And the scene-referred branch's plane read, for comparison.
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let yuv = unsafe { klipp_media::yuv_samples::read_yuv_samples(src.as_ptr()) };
        let n = yuv.as_ref().map(|y| y.samples().y.len()).unwrap_or(0);
        eprintln!("read_yuv_samples: {:.1} ms ({n} luma)", t.elapsed().as_secs_f64() * 1000.0);
    }
}
