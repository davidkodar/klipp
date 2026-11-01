//! Native Y'CbCr samples of a decoded software frame.
//!
//! Shared by the viewer's decoders (klipp-engine) and export's
//! (klipp-render): both hand scene-referred frames — PQ, HLG, camera log —
//! to `klipp_color::hdr::yuv_to_hdr_codes`, which needs the samples as
//! the file stored them, whatever the pixel format.

/// A software frame's Y'CbCr samples, copied out at native bit depth.
pub struct OwnedYuv {
    pub y: Vec<u16>,
    pub u: Vec<u16>,
    pub v: Vec<u16>,
    pub width: u32,
    pub height: u32,
    pub shift_w: u32,
    pub shift_h: u32,
    pub depth: u32,
}

impl OwnedYuv {
    pub fn samples(&self) -> klipp_color::hdr::YuvSamples<'_> {
        klipp_color::hdr::YuvSamples {
            y: &self.y,
            u: &self.u,
            v: &self.v,
            width: self.width,
            height: self.height,
            chroma_shift_w: self.shift_w,
            chroma_shift_h: self.shift_h,
            bit_depth: self.depth,
        }
    }
}

/// A Y'CbCr frame's own alpha plane, where it has one, into `out`'s
/// alpha at `dst_w`×`dst_h`, turned by `rotation` (0, 90, 180 or 270) as
/// the Y'CbCr decode turns the picture (#3264). The native decode reads
/// the three Y'CbCr planes only, so a ProRes 4444 with alpha came in
/// opaque. Alpha is linear already, straight.
pub fn keep_alpha_plane(frame: &ffmpeg_next::frame::Video, dst_w: u32, dst_h: u32, rotation: u32, out: &mut [f32]) {
    use ffmpeg_next::format::Pixel as P;
    let bits = match frame.format() {
        P::YUVA420P | P::YUVA422P | P::YUVA444P => 8,
        P::YUVA420P9LE | P::YUVA422P9LE | P::YUVA444P9LE => 9,
        P::YUVA420P10LE | P::YUVA422P10LE | P::YUVA444P10LE => 10,
        P::YUVA422P12LE | P::YUVA444P12LE => 12,
        P::YUVA420P16LE | P::YUVA422P16LE | P::YUVA444P16LE => 16,
        _ => return,
    };
    let (src_w, src_h) = (frame.width().max(1), frame.height().max(1));
    if dst_w == 0 || dst_h == 0 || out.len() < (dst_w * dst_h * 4) as usize {
        return;
    }
    let (data, stride) = (frame.data(3), frame.stride(3));
    let max = ((1u32 << bits) - 1) as f32;
    // The source pixel as `yuv_to_linear_display` takes it: each output
    // column maps to one source column (or, turned 90° or 270°, one source
    // row) and each output row to one source row (or column), so the maps
    // are made once, not divided out a pixel at a time.
    let (cols, rows): (Vec<u32>, Vec<u32>) = match rotation {
        90 => ((0..dst_w).map(|dx| (dst_w - 1 - dx) * src_h / dst_w).collect(), (0..dst_h).map(|dy| dy * src_w / dst_h).collect()),
        180 => ((0..dst_w).map(|dx| (dst_w - 1 - dx) * src_w / dst_w).collect(), (0..dst_h).map(|dy| (dst_h - 1 - dy) * src_h / dst_h).collect()),
        270 => ((0..dst_w).map(|dx| dx * src_h / dst_w).collect(), (0..dst_h).map(|dy| (dst_h - 1 - dy) * src_w / dst_h).collect()),
        _ => ((0..dst_w).map(|dx| dx * src_w / dst_w).collect(), (0..dst_h).map(|dy| dy * src_h / dst_h).collect()),
    };
    let turned = matches!(rotation, 90 | 270);
    let sample = |sx: u32, sy: u32| {
        let (sx, sy) = (sx.min(src_w - 1) as usize, sy.min(src_h - 1) as usize);
        let row = &data[sy * stride..];
        let v = if bits == 8 { f32::from(row[sx]) } else { f32::from(u16::from_le_bytes([row[2 * sx], row[2 * sx + 1]])) };
        v / max
    };
    use rayon::prelude::*;
    out[..(dst_w * dst_h * 4) as usize].par_chunks_exact_mut(dst_w as usize * 4).zip(rows.par_iter()).for_each(|(line, &r)| {
        for (px, &c) in line.chunks_exact_mut(4).zip(&cols) {
            px[3] = if turned { sample(r, c) } else { sample(c, r) };
        }
    });
}

/// Read the Y, Cb and Cr samples of any software YUV frame — planar,
/// semi-planar (NV12, P010) or packed, 8 to 16 bits, any subsampling —
/// using the pixel format's descriptor, so ProRes 4:2:2 / 4:4:4, 12-bit
/// and the rest need no per-format code. `None` for RGB, palette,
/// bitstream and hardware formats; callers fall back to their old path.
///
/// # Safety
///
/// `frame` must be a valid, allocated software AVFrame.
pub unsafe fn read_yuv_samples(frame: *const ffmpeg_next::ffi::AVFrame) -> Option<OwnedYuv> {
    use ffmpeg_next::ffi::*;
    use rayon::prelude::*;
    // AV_PIX_FMT_FLAG_* (pixdesc.h); bindgen does not export the macros.
    const FLAG_BE: u64 = 1 << 0;
    const FLAG_PAL: u64 = 1 << 1;
    const FLAG_BITSTREAM: u64 = 1 << 2;
    const FLAG_HWACCEL: u64 = 1 << 3;
    const FLAG_RGB: u64 = 1 << 5;

    let f = &*frame;
    // AVPixelFormat is a repr(i32) enum; transmuting a format value from
    // outside its range would be undefined, so range check first.
    if !(-1..AVPixelFormat::AV_PIX_FMT_NB as i32).contains(&f.format) {
        return None;
    }
    let desc = av_pix_fmt_desc_get(std::mem::transmute::<i32, AVPixelFormat>(f.format));
    if desc.is_null() {
        return None;
    }
    let d = &*desc;
    if d.flags & (FLAG_PAL | FLAG_BITSTREAM | FLAG_HWACCEL | FLAG_RGB) != 0 || d.nb_components < 3 {
        return None;
    }
    let depth = d.comp[0].depth as u32;
    if depth == 0 || depth > 16 || d.comp[1].depth as u32 != depth || d.comp[2].depth as u32 != depth {
        return None;
    }
    let (width, height) = (f.width.max(0) as u32, f.height.max(0) as u32);
    if width == 0 || height == 0 {
        return None;
    }
    let (shift_w, shift_h) = (d.log2_chroma_w as u32, d.log2_chroma_h as u32);
    let cw = (width + (1 << shift_w) - 1) >> shift_w;
    let ch = (height + (1 << shift_h) - 1) >> shift_h;
    let big_endian = d.flags & FLAG_BE != 0;
    let mask = ((1u32 << depth) - 1) as u16;

    let read = |c: usize, pw: u32, ph: u32| -> Option<Vec<u16>> {
        let comp = d.comp[c];
        let plane = comp.plane as usize;
        let base = f.data[plane];
        if base.is_null() {
            return None;
        }
        // Raw pointers are not Send; rows are disjoint reads of a frame
        // that outlives this call.
        let base = base as usize;
        let linesize = f.linesize[plane] as isize;
        let (step, offset, shift) = (comp.step as usize, comp.offset as usize, comp.shift as u32);
        let mut out = vec![0u16; (pw as usize) * (ph as usize)];
        out.par_chunks_mut(pw as usize).enumerate().for_each(|(y, row)| {
            let p = (base as *const u8).offset(y as isize * linesize).add(offset);
            for (x, dst) in row.iter_mut().enumerate() {
                let q = p.add(x * step);
                let raw = if depth > 8 {
                    let b = [*q, *q.add(1)];
                    if big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
                } else {
                    *q as u16
                };
                *dst = (raw >> shift) & mask;
            }
        });
        Some(out)
    };
    Some(OwnedYuv {
        y: read(0, width, height)?,
        u: read(1, cw, ch)?,
        v: read(2, cw, ch)?,
        width,
        height,
        shift_w,
        shift_h,
        depth,
    })
}

#[cfg(test)]
mod alpha_plane_tests {
    use super::keep_alpha_plane;
    use ffmpeg_next::format::Pixel;

    /// A YUVA 4:4:4 10-bit frame whose alpha codes vary across and down.
    fn frame(w: u32, h: u32) -> ffmpeg_next::frame::Video {
        let mut f = ffmpeg_next::frame::Video::new(Pixel::YUVA444P10LE, w, h);
        let stride = f.stride(3);
        let plane = f.data_mut(3);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let code = ((x * 37 + y * 101) % 1024) as u16;
                plane[y * stride + 2 * x..y * stride + 2 * x + 2].copy_from_slice(&code.to_le_bytes());
            }
        }
        f
    }

    /// The alpha a pixel takes, worked out a pixel at a time as the Y'CbCr
    /// decode maps it.
    fn by_pixel(f: &ffmpeg_next::frame::Video, dst_w: u32, dst_h: u32, rotation: u32) -> Vec<f32> {
        let (src_w, src_h) = (f.width(), f.height());
        let (data, stride) = (f.data(3), f.stride(3));
        let mut out = Vec::new();
        for dy in 0..dst_h {
            for dx in 0..dst_w {
                let (sx, sy) = match rotation {
                    90 => (dy * src_w / dst_h, (dst_w - 1 - dx) * src_h / dst_w),
                    180 => ((dst_w - 1 - dx) * src_w / dst_w, (dst_h - 1 - dy) * src_h / dst_h),
                    270 => ((dst_h - 1 - dy) * src_w / dst_h, dx * src_h / dst_w),
                    _ => (dx * src_w / dst_w, dy * src_h / dst_h),
                };
                let at = sy.min(src_h - 1) as usize * stride + 2 * sx.min(src_w - 1) as usize;
                out.push(f32::from(u16::from_le_bytes([data[at], data[at + 1]])) / 1023.0);
            }
        }
        out
    }

    /// The alpha plane lands where the picture does: as worked out a
    /// pixel at a time, at each turn, at the frame's size and scaled.
    #[test]
    fn the_alpha_plane_is_read_where_the_picture_lands() {
        let f = frame(37, 21);
        for rotation in [0, 90, 180, 270] {
            for (dw, dh) in [(37, 21), (21, 37), (18, 10), (10, 18)] {
                let mut out = vec![0.0f32; (dw * dh * 4) as usize];
                keep_alpha_plane(&f, dw, dh, rotation, &mut out);
                let got: Vec<f32> = out.chunks_exact(4).map(|p| p[3]).collect();
                assert_eq!(got, by_pixel(&f, dw, dh, rotation), "turned {rotation} at {dw}x{dh}");
            }
        }
    }

    /// Measured: the alpha plane of a 4K frame.
    #[test]
    #[ignore]
    fn measure_a_4k_alpha_plane() {
        let f = frame(3840, 2160);
        let mut out = vec![0.0f32; 3840 * 2160 * 4];
        keep_alpha_plane(&f, 3840, 2160, 0, &mut out);
        let start = std::time::Instant::now();
        for _ in 0..10 {
            keep_alpha_plane(&f, 3840, 2160, 0, &mut out);
        }
        let mapped = start.elapsed().as_secs_f64() * 100.0;
        let start = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(by_pixel(&f, 3840, 2160, 0));
        }
        eprintln!("MEASURE a 4K alpha plane: {mapped:.2} ms mapped and side by side, {:.2} ms a pixel at a time", start.elapsed().as_secs_f64() * 100.0);
    }
}

#[cfg(test)]
mod yuv_reader_tests {
    use super::*;
    use ffmpeg_next::format::Pixel;

    const W: u32 = 6;
    const H: u32 = 4;

    fn y_at(x: u32, y: u32) -> u16 { (x * 7 + y * 13 + 20) as u16 }
    fn u_at(x: u32, y: u32) -> u16 { (x * 5 + y * 3 + 100) as u16 }
    fn v_at(x: u32, y: u32) -> u16 { (x * 11 + y * 2 + 60) as u16 }

    /// Write one sample of `bytes` width at byte offset `off` in `plane`.
    fn put(f: &mut ffmpeg_next::frame::Video, plane: usize, row: u32, off: usize, bytes: usize, v: u16) {
        let stride = f.stride(plane);
        let d = f.data_mut(plane);
        let i = row as usize * stride + off;
        if bytes == 2 { d[i..i + 2].copy_from_slice(&v.to_le_bytes()) } else { d[i] = v as u8 }
    }

    fn check(f: &ffmpeg_next::frame::Video, shift_w: u32, shift_h: u32, depth: u32) {
        let yuv = unsafe { read_yuv_samples(f.as_ptr()) }.expect("readable YUV format");
        assert_eq!((yuv.width, yuv.height, yuv.shift_w, yuv.shift_h, yuv.depth), (W, H, shift_w, shift_h, depth));
        let cw = (W + (1 << shift_w) - 1) >> shift_w;
        for y in 0..H {
            for x in 0..W {
                assert_eq!(yuv.y[(y * W + x) as usize], y_at(x, y), "{:?} Y({x},{y})", f.format());
            }
        }
        for y in 0..(H >> shift_h) {
            for x in 0..cw {
                assert_eq!(yuv.u[(y * cw + x) as usize], u_at(x, y), "{:?} U({x},{y})", f.format());
                assert_eq!(yuv.v[(y * cw + x) as usize], v_at(x, y), "{:?} V({x},{y})", f.format());
            }
        }
    }

    #[test]
    fn planar_formats_at_each_depth_and_subsampling() {
        for (fmt, sw, sh, depth) in [
            (Pixel::YUV420P, 1, 1, 8),
            (Pixel::YUV422P10LE, 1, 0, 10),
            (Pixel::YUV444P12LE, 0, 0, 12),
        ] {
            let mut f = ffmpeg_next::frame::Video::new(fmt, W, H);
            let b = if depth > 8 { 2 } else { 1 };
            for y in 0..H {
                for x in 0..W {
                    put(&mut f, 0, y, x as usize * b, b, y_at(x, y));
                }
            }
            for y in 0..(H >> sh) {
                for x in 0..(W >> sw) {
                    put(&mut f, 1, y, x as usize * b, b, u_at(x, y));
                    put(&mut f, 2, y, x as usize * b, b, v_at(x, y));
                }
            }
            check(&f, sw, sh, depth);
        }
    }

    /// Semi-planar with interleaved chroma, and P010's samples in the top
    /// ten bits of each word.
    #[test]
    fn semi_planar_nv12_and_p010() {
        let mut f = ffmpeg_next::frame::Video::new(Pixel::NV12, W, H);
        for y in 0..H {
            for x in 0..W { put(&mut f, 0, y, x as usize, 1, y_at(x, y)); }
        }
        for y in 0..H / 2 {
            for x in 0..W / 2 {
                put(&mut f, 1, y, x as usize * 2, 1, u_at(x, y));
                put(&mut f, 1, y, x as usize * 2 + 1, 1, v_at(x, y));
            }
        }
        check(&f, 1, 1, 8);

        let mut f = ffmpeg_next::frame::Video::new(Pixel::P010LE, W, H);
        for y in 0..H {
            for x in 0..W { put(&mut f, 0, y, x as usize * 2, 2, y_at(x, y) << 6); }
        }
        for y in 0..H / 2 {
            for x in 0..W / 2 {
                put(&mut f, 1, y, x as usize * 4, 2, u_at(x, y) << 6);
                put(&mut f, 1, y, x as usize * 4 + 2, 2, v_at(x, y) << 6);
            }
        }
        check(&f, 1, 1, 10);
    }

    /// Packed 4:2:2: Y0 U Y1 V in one plane.
    #[test]
    fn packed_yuyv422() {
        let mut f = ffmpeg_next::frame::Video::new(Pixel::YUYV422, W, H);
        for y in 0..H {
            for x in 0..W {
                put(&mut f, 0, y, x as usize * 2, 1, y_at(x, y));
            }
            for x in 0..W / 2 {
                put(&mut f, 0, y, x as usize * 4 + 1, 1, u_at(x, y));
                put(&mut f, 0, y, x as usize * 4 + 3, 1, v_at(x, y));
            }
        }
        check(&f, 1, 0, 8);
    }

    #[test]
    fn rgb_is_refused() {
        let f = ffmpeg_next::frame::Video::new(Pixel::RGBA, W, H);
        assert!(unsafe { read_yuv_samples(f.as_ptr()) }.is_none());
    }
}

