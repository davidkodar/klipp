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
//!
//! And the walk must not end quietly for any reason but the end of the
//! file. A frame the decoder cannot decode, and a file that holds fewer
//! frames than it declares, are each an error — because to a caller
//! counting frames for motion analysis, a quiet `None` is the end of a
//! shorter clip, and the analysis would simply be of that.

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
/// 10-bit 4:2:0 and encoded with `codec` into `ext`. `None` without ffmpeg;
/// an encode that fails with ffmpeg present is a broken test, and panics.
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
        panic!("ffmpeg could not encode the {name} ramp");
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// HEVC with four B-frames and a long GOP, `moov` at the front so a
/// truncated copy still opens.
fn hevc_ramp() -> Option<PathBuf> {
    ramp("hevc-faststart", &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "bframes=4:keyint=250:log-level=error", "-movflags", "+faststart"], "mp4")
}

/// FFV1 in AVI: every frame is a `00dc` chunk, so a frame's data can be
/// found and damaged without touching the container.
fn ffv1_avi_ramp() -> Option<PathBuf> {
    ramp("ffv1-avi", &["-c:v", "ffv1"], "avi")
}

/// `src` with `len` bytes of `fill` written from `at`, as a new file.
fn damaged(src: &Path, name: &str, at: usize, len: usize, fill: u8) -> PathBuf {
    let mut bytes = std::fs::read(src).expect("read source");
    let end = (at + len).min(bytes.len());
    for b in &mut bytes[at..end] {
        *b = fill;
    }
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}-{}", src.file_name().unwrap().to_string_lossy()));
    std::fs::write(&out, bytes).expect("write damaged copy");
    out
}

/// Walk until the decoder stops, however it stops.
fn walk_to_the_end(path: &Path) -> (usize, Result<(), String>) {
    let mut dec = SequentialFrameDecoder::open(path, W).expect("open");
    let mut n = 0;
    loop {
        match dec.next_frame() {
            Ok(Some(_)) => n += 1,
            Ok(None) => return (n, Ok(())),
            Err(e) => {
                // And it stays over: the next call is a clean end, not a
                // second error or another frame.
                assert!(matches!(dec.next_frame(), Ok(None)), "the walk went on after an error");
                return (n, Err(e));
            }
        }
    }
}

/// Walk the whole file; the grey of each frame's centre pixel, in the
/// order the decoder gave them, and whether the GPU decoded them.
fn walk(path: &Path) -> (Vec<u8>, bool) {
    let mut dec = SequentialFrameDecoder::open(path, W).expect("open");
    let hw = dec.is_hardware();
    let mut greys = Vec::new();
    while let Some((rgba, w, h)) = dec.next_frame().expect("the walk stopped on an error") {
        assert_eq!((w, h), (W, H), "frame size");
        let p = ((h / 2 * w + w / 2) * 4) as usize;
        greys.push(rgba[p]);
    }
    assert!(dec.next_frame().expect("after EOF").is_none(), "the walk continued past EOF");
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
    let Some(src) = hevc_ramp() else {
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

/// Garbage inside one frame's data: the decoder reports it and the walk
/// stops there, with an error, rather than skipping the frame and going
/// on — which is what a decode error mistaken for "needs more input" did.
#[test]
fn a_frame_the_decoder_cannot_decode_stops_the_walk_with_an_error() {
    let Some(src) = ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // In the `movi` list, the 28th `00dc` chunk is frame 27's: tag,
    // little-endian size, data. (The stream header carries a `00dc` of its
    // own, so the search starts at the list.) All of the data goes, header
    // included: damage confined to slice data trips FFV1's CRC and is
    // *concealed* — the slice copied from the previous frame and the
    // frame returned as good — so this is the case it cannot paper over.
    let bytes = std::fs::read(&src).unwrap();
    let movi = bytes.windows(4).position(|w| w == b"movi").expect("an AVI movi list");
    let chunk = movi + bytes[movi..].windows(4).enumerate().filter(|(_, w)| *w == b"00dc").map(|(i, _)| i).nth(27).expect("frame 27's chunk");
    let size = u32::from_le_bytes(bytes[chunk + 4..chunk + 8].try_into().unwrap()) as usize;
    let path = damaged(&src, "damaged-frame", chunk + 8, size, 0xff);

    let (n, end) = walk_to_the_end(&path);
    eprintln!("damaged frame 27: {n} frames, then {end:?}");
    let err = end.expect_err("a frame that cannot be decoded was passed over in silence");
    assert_eq!(n, 27, "the frames before the damaged one all came out, and none after");
    assert!(err.starts_with("frame 27: decode error: "), "the error does not name the frame and the cause: {err}");
    // And says something after the cause: ffmpeg's own text for this code
    // is blank, and a code number is not a reason.
    let reason = err.trim_start_matches("frame 27: decode error: ");
    assert!(reason.chars().any(|c| c.is_alphabetic()) && !reason.contains("Error("), "the reason is not in words: {err:?}");
    let _ = std::fs::remove_file(&path);
}

/// A file cut short still ends cleanly at the container level, and a
/// demuxer that resyncs past a damaged block drops the packet in silence.
/// Where the container declares its frame count, a walk that comes up
/// short is an error — not the end of a shorter clip.
#[test]
fn a_file_with_fewer_frames_than_it_declares_is_an_error() {
    let Some(src) = hevc_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // Cut exactly where the 28th video packet ends, so the demuxer meets
    // a clean end of file rather than a torn sample. A torn sample is a
    // decode error at the cut — also an error, but the wrong one to pin
    // here: this is about the walk that ends *cleanly* and short.
    let cut = {
        let mut input = ffmpeg_next::format::input(&src).expect("open the source with ffmpeg");
        let video = input.streams().best(ffmpeg_next::media::Type::Video).expect("a video stream").index();
        let mut ends = Vec::new();
        for (stream, packet) in input.packets() {
            if stream.index() == video {
                ends.push((packet.position() as usize) + packet.size());
            }
        }
        ends[27]
    };
    let bytes = std::fs::read(&src).unwrap();
    let path = std::env::temp_dir().join("klipp-seq-ramp-truncated.mp4");
    std::fs::write(&path, &bytes[..cut]).unwrap();

    let (n, end) = walk_to_the_end(&path);
    eprintln!("cut after packet 27: {n} frames, then {end:?}");
    let err = end.expect_err("a file missing a quarter of its frames ended as if complete");
    assert!(n < N, "the truncated file still produced every frame ({n})");
    assert!(
        err.contains(&format!("declares {N} frames")) && err.contains(&format!("produced {n}")),
        "the error does not say what the file declares and what came out: {err}",
    );
    let _ = std::fs::remove_file(&path);
}
