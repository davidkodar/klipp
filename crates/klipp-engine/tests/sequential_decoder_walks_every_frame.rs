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

mod common;

use std::path::Path;

use klipp_engine::SequentialFrameDecoder;

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;
const N: usize = common::RAMP_N;
use common::{damaged, ffv1_avi_ramp, hevc_ramp, ramp};

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
    let path = common::cut_after_packet(&src, 27, "truncated");

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
