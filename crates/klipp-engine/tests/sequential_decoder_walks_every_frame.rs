//! `SequentialFrameDecoder` hands back every frame of a file, in order,
//! once each — including the ones the decoder is still holding at EOF.
//!
//! It walks files for stabilisation and motion analysis, where a missing
//! or repeated frame corrupts the motion vectors of everything after it.
//! It had exactly that bug: on a `send_packet` refusal it collected one
//! frame and returned before resending the packet, so the packet's frame
//! was never decoded. Nothing checked the sequence; `clippy::never_loop`
//! found the shape of it.
//!
//! The refusal itself cannot be provoked through the public API — the
//! walk drains the decoder before every send, which is the condition under
//! which libavcodec accepts a packet — so this pins the property the walk
//! exists to provide, on a source where every frame says which one it is.
//! Two encodings of the same frames: HEVC with B-frames, which reorders
//! and buffers and decodes on NVDEC where there is one; and FFV1, intra
//! and software-only, so both decode paths are walked.

use std::path::{Path, PathBuf};
use std::process::Command;

use klipp_engine::SequentialFrameDecoder;

/// NVDEC declines anything under 144 pixels a side and falls back to
/// software, which would quietly leave the hardware path unwalked.
const W: u32 = 160;
const H: u32 = 160;
/// Enough frames for a B-frame pyramid to reorder across, and for the
/// decoder to hold several at EOF.
const N: usize = 40;

/// The source: `N` solid grey frames whose luma climbs from black to white
/// in equal steps, so frame `i` is the `i`-th brightest. Written as raw
/// 10-bit 4:2:0 and encoded with `codec` into `ext`. `None` without ffmpeg.
fn ramp(name: &str, codec: &[&str], ext: &str) -> Option<PathBuf> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}-{W}x{H}x{N}.{ext}"));
    if out.exists() {
        return Some(out);
    }
    let mut bytes = Vec::new();
    for i in 0..N {
        // Limited range, 64..=940, evenly spaced.
        let y = 64 + (i as u32 * 876 / (N as u32 - 1)) as u16;
        let (pw, ph) = (W as usize / 2, H as usize / 2);
        for _ in 0..(W * H) {
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        for _ in 0..(2 * pw * ph) {
            bytes.extend_from_slice(&512u16.to_le_bytes());
        }
    }
    let raw = out.with_extension("yuv");
    std::fs::write(&raw, bytes).ok()?;
    let partial = out.with_extension(format!("partial.{ext}"));
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le"])
        .args(["-s", &format!("{W}x{H}"), "-r", "25", "-i"])
        .arg(&raw)
        .args(["-vf", "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709"])
        .args(codec)
        .arg(&partial)
        .status()
        .ok()?
        .success();
    let _ = std::fs::remove_file(&raw);
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// Walk the whole file; the grey of each frame's centre pixel, in the
/// order the decoder gave them, and whether the GPU decoded them.
fn walk(path: &Path) -> (Vec<u8>, bool) {
    let mut dec = SequentialFrameDecoder::open(path, W).expect("open");
    let hw = dec.is_hardware();
    let mut greys = Vec::new();
    while let Some((rgba, w, h)) = dec.next_frame() {
        assert_eq!((w, h), (W, H), "frame size");
        let p = ((h / 2 * w + w / 2) * 4) as usize;
        greys.push(rgba[p]);
    }
    assert!(dec.next_frame().is_none(), "the walk continued past EOF");
    (greys, hw)
}

/// Exactly `N` frames, each brighter than the last. A dropped frame
/// leaves the count short; a repeated or reordered one breaks the climb.
fn every_frame_once_in_order(label: &str, greys: &[u8]) {
    eprintln!("{label}: {} frames, greys {:?}", greys.len(), greys);
    assert_eq!(greys.len(), N, "{label}: the walk returned {} of {N} frames", greys.len());
    for i in 1..greys.len() {
        assert!(
            greys[i] > greys[i - 1],
            "{label}: frame {i} ({}) is not brighter than frame {} ({}) — repeated, reordered, or dropped",
            greys[i], i - 1, greys[i - 1],
        );
    }
    // And the ramp reached both ends: black at the start, white at the end.
    assert!(greys[0] < 24, "{label}: the first frame is {} — not the darkest", greys[0]);
    assert!(greys[N - 1] > 231, "{label}: the last frame is {} — not the brightest", greys[N - 1]);
}

#[test]
fn hevc_with_b_frames_yields_every_frame_in_order() {
    // Four B-frames and a long GOP: the decoder reorders across a pyramid
    // and is holding frames at EOF that only the flush releases.
    let Some(src) = ramp("hevc", &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "bframes=4:keyint=250:log-level=error"], "mp4") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (greys, hw) = walk(&src);
    // Which path this walked depends on the machine; say so rather than
    // let a silent software fallback pass as hardware coverage.
    eprintln!("hevc: decoded by {}", if hw { "the GPU" } else { "software (no hardware decoder took the file)" });
    every_frame_once_in_order("hevc", &greys);
}

#[test]
fn ffv1_through_the_software_decoder_yields_every_frame_in_order() {
    let Some(src) = ramp("ffv1", &["-c:v", "ffv1"], "mkv") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (greys, hw) = walk(&src);
    assert!(!hw, "FFV1 has no hardware decoder; this walk was meant to cover the software path");
    every_frame_once_in_order("ffv1", &greys);
}
