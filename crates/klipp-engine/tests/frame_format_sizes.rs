//! #3212: how large a whole frame is on disk, per format, at 1080p after
//! LZ4, and how close each comes back to the frame it was: what the frame
//! cache keeps. A measurement (`--ignored`), on 24 frames of a moving
//! pattern, clean, with the light grain of usual camera footage, and with
//! heavy grain (the worst case), at full and half 1080p (Half playback).

use std::io::Read;

/// 24 frames of linear RGB in [0, ~1] at `w`x`h`, from ffmpeg: a moving
/// pattern with grain of strength `grain` (0 for none), as 16-bit sRGB
/// then decoded to linear.
fn frames(w: usize, h: usize, grain: u32) -> Option<Vec<Vec<f32>>> {
    let noise = if grain > 0 { format!(",noise=alls={grain}:allf=t") } else { String::new() };
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", &format!("testsrc2=s={w}x{h}:r=24:d=1{noise}"), "-f", "rawvideo", "-pix_fmt", "rgb48le", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut raw = Vec::new();
    child.stdout.take()?.read_to_end(&mut raw).ok()?;
    let per = w * h * 6;
    let decode = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    Some(raw.chunks_exact(per).map(|f| f.chunks_exact(2).map(|b| decode(u16::from_le_bytes([b[0], b[1]]) as f32 / 65535.0)).collect()).collect())
}

fn to_display(c: f32) -> i32 {
    let e = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (e.clamp(0.0, 1.0) * 255.0).round() as i32
}

fn lz4(bytes: &[u8]) -> usize {
    lz4_flex::compress_prepend_size(bytes).len()
}

/// A curve for 10 bits that keeps linear values past 1: sRGB's to 1,
/// then a log segment to 16.
fn enc10(c: f32) -> u32 {
    let e = if c <= 1.0 { if c <= 0.0031308 { c.max(0.0) * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 } } else { 1.0 + (c.min(16.0)).log2() / 4.0 };
    ((e / 2.0).clamp(0.0, 1.0) * 1023.0).round() as u32
}

fn dec10(v: u32) -> f32 {
    let e = v as f32 / 1023.0 * 2.0;
    if e <= 1.0 { if e <= 0.04045 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) } } else { 2f32.powf((e - 1.0) * 4.0) }
}

#[test]
#[ignore]
fn frame_formats_on_disk() {
    for (what, grain) in [("clean", 0), ("light grain, as usual footage", 4), ("heavy grain, the worst case", 14)] {
        for (size, w, h) in [("1080p", 1920, 1080), ("half 1080p", 960, 540)] {
            let Some(frames) = frames(w, h, grain) else {
                eprintln!("skipping: no ffmpeg");
                return;
            };
            eprintln!("MEASURE {what}, {size}:");
            measure(&frames);
        }
    }
}

fn measure(frames: &[Vec<f32>]) {
    let mut sizes = [0usize; 5];
    let mut worst = [0i32; 5];
    for rgb in frames {
        let px = rgb.len() / 3;
        // f16 RGBA, as the store keeps a frozen layer.
        let mut rgba16 = Vec::with_capacity(px * 8);
        let mut rgb16 = Vec::with_capacity(px * 6);
        for p in rgb.chunks_exact(3) {
            for c in p {
                let h = half::f16::from_f32(*c).to_bits().to_le_bytes();
                rgba16.extend_from_slice(&h);
                rgb16.extend_from_slice(&h);
            }
            rgba16.extend_from_slice(&half::f16::ONE.to_bits().to_le_bytes());
        }
        sizes[0] += lz4(&rgba16);
        sizes[1] += lz4(&rgb16);
        // f16 RGB with its bytes split into planes, low then high, which
        // LZ4 finds runs in: the same values, exactly.
        let (lo, hi): (Vec<u8>, Vec<u8>) = rgb16.chunks_exact(2).map(|b| (b[0], b[1])).unzip();
        let mut planar = lo;
        planar.extend(hi);
        sizes[2] += lz4(&planar);
        // 10-bit packed, R10G10B10A2, through the curve.
        let mut p10 = Vec::with_capacity(px * 4);
        let mut back10 = Vec::with_capacity(px * 3);
        for p in rgb.chunks_exact(3) {
            let (r, g, b) = (enc10(p[0]), enc10(p[1]), enc10(p[2]));
            p10.extend_from_slice(&(r | g << 10 | b << 20 | 3 << 30).to_le_bytes());
            back10.extend([dec10(r), dec10(g), dec10(b)]);
        }
        sizes[3] += lz4(&p10);
        // 8-bit display-encoded, as #924 kept them.
        let d8: Vec<u8> = rgb.iter().map(|c| to_display(*c) as u8).collect();
        sizes[4] += lz4(&d8);
        let worst_of = |back: &mut dyn Iterator<Item = f32>| rgb.iter().zip(back).map(|(a, b)| (to_display(*a) - to_display(b)).abs()).max().unwrap_or(0);
        worst[1] = worst[1].max(worst_of(&mut rgb.iter().map(|c| half::f16::from_f32(*c).to_f32())));
        worst[3] = worst[3].max(worst_of(&mut back10.iter().copied()));
    }
    worst[0] = worst[1];
    worst[2] = worst[1];
    let names = ["f16 RGBA", "f16 RGB (alpha dropped)", "f16 RGB, bytes in planes", "10-bit packed, curve", "8-bit display-encoded (#924)"];
    let n = frames.len() as f64;
    for (k, name) in names.iter().enumerate() {
        let per_frame = sizes[k] as f64 / n / 1e6;
        eprintln!("MEASURE   {name}: {per_frame:.2} MB a frame, {:.1} GB a minute at 24 fps, worst {} levels from the frame", per_frame * 24.0 * 60.0 / 1e3, worst[k]);
    }
}
