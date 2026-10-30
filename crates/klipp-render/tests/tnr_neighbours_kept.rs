//! An export keeps the neighbour reader temporal noise reduction reads
//! (#1980): one per file for the whole export, not one a frame, and every
//! frame is bit for bit the frame a reader opened per frame makes,
//! across a cut, a dissolve between two clips of the file and a reversed
//! clip, whose neighbours are behind the reader. Needs the ffmpeg CLI;
//! skips without it.

use klipp_core::*;
use klipp_render::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use klipp_render::*;
use std::path::{Path, PathBuf};

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: i64 = 30;

fn tmp(name: &str) -> PathBuf {
    // The scratch directory is per process, while these tests run in
    // parallel. Each invocation owns both its source and probe (#2076).
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    klipp_testing::scratch::scratch_file("klipp-tnr-neighbours-kept", &format!("{id}-{name}"))
}

/// Two seconds of a moving pattern: every frame differs, so a neighbour
/// of the wrong time is another picture.
fn source() -> Option<PathBuf> {
    source_in("yuv420p")
}

/// [`source`] in `pix_fmt`.
fn source_in(pix_fmt: &str) -> Option<PathBuf> {
    let path = tmp(&format!("pattern-{pix_fmt}.mkv"));
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:r=25:d=2"])
        .args(["-vf", &format!("format={pix_fmt}"), "-c:v", "ffv1"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

fn denoised(clip: klipp_timeline::Clip) -> klipp_timeline::Clip {
    klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.3 }).build()
}

/// Frames 0-10 and 10-20 two clips of the file with a four-frame
/// dissolve over the cut, then 20-30 a third played backward, all with
/// temporal noise reduction.
fn snapshot(src: &Path) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let a = denoised(klipp_timeline::Clip::media(id, tc(0), tc(10), tc(5), tc(15)));
    let b = denoised(klipp_timeline::Clip::media(id, tc(10), tc(20), tc(25), tc(35)));
    let mut c = denoised(klipp_timeline::Clip::media(id, tc(20), tc(30), tc(10), tc(20)));
    c.speed = -1.0;
    let dissolve = klipp_timeline::Transition {
        id: TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(4),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    };
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![a, b, c];
    t.transitions.push(dissolve);
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(2.0), fps: Some((25, 1)), ..Default::default() });
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

/// Every frame of the timeline as the export makes it, and the readers
/// its source opened, with the neighbour readers kept or not, and the
/// neighbours from the clips' streams or not (#1988).
fn frames(snap: &SequenceSnapshot, kept: bool) -> (Vec<Vec<u8>>, usize) {
    frames_with(snap, kept, false)
}

fn frames_with(snap: &SequenceSnapshot, kept: bool, from_stream: bool) -> (Vec<Vec<u8>>, usize) {
    let probe = tmp(&format!("probe-{kept}-{from_stream}.mp4"));
    let settings = ExportSettings { output_path: probe.clone(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264, audio_codec: ExportAudioCodec::Copy, ..Default::default() };
    let encoder = Encoder::open(&probe, &settings, snap).unwrap();
    let mut maker = ExportFrameMaker::new(&settings, snap, &encoder, "test");
    drop(encoder);
    let _ = std::fs::remove_file(&probe);
    let mut renderer = export_renderer(snap, "test");
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    source.set_neighbour_readers_kept(kept);
    source.set_neighbours_from_stream(from_stream);
    let made = (0..FRAMES)
        .map(|f| match maker.frame(&mut renderer, &snap.video.sequence, RationalTime { value: f, rate: 25 }, &mut source).unwrap() {
            ExportFrame::Planes { planes, .. } => planes,
            ExportFrame::Linear(px) => px.iter().flat_map(|v| v.to_le_bytes()).collect(),
        })
        .collect();
    (made, source.readers_opened())
}

#[test]
fn kept_neighbour_readers_make_the_same_frames() {
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let snap = snapshot(&src);
    let (kept, kept_opened) = frames(&snap, true);
    let (per_frame, per_frame_opened) = frames(&snap, false);
    for (f, (a, b)) in kept.iter().zip(&per_frame).enumerate() {
        assert!(a == b, "frame {f}: {} bytes differ from a reader opened per frame", a.iter().zip(b).filter(|(x, y)| x != y).count());
    }
    eprintln!("readers opened: {kept_opened} kept, {per_frame_opened} per frame");
    // The clips' streams and one neighbour reader for the file, against
    // one neighbour reader a frame (two in the dissolve) before.
    assert!(kept_opened <= 4, "{kept_opened} readers opened with the neighbour reader kept");
    assert!(per_frame_opened >= FRAMES as usize, "{per_frame_opened}: the old way opens one a frame");
}

/// #1988: the neighbours the clip's stream holds (the frame it showed
/// last and the one it reads next) make frames within two levels of the
/// ones a second decoder's neighbours make (the viewer's, which reads
/// them from a reader of its own), across the cut, the dissolve and the
/// reversed clip, which keeps its reader.
///
/// 4:4:4: from 4:2:0 the export's software decode and the reader rebuild
/// chroma differently at an edge (one repeats samples, the other
/// interpolates them, #1995), and on this pattern, all edges, that is
/// most of what there is to see. Two levels: a reader's neighbour is
/// within a level of the stream's frame, and the planes round once more.
#[test]
fn neighbours_from_the_stream_make_the_readers_frames() {
    let Some(src) = source_in("yuv444p") else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let snap = snapshot(&src);
    let (streamed, _) = frames_with(&snap, true, true);
    let (read, _) = frames_with(&snap, true, false);
    let mut worst = 0i32;
    // A frame made on the CPU is linear floats: compared as the 8-bit
    // picture it encodes to; planes as they are.
    let shown = |bytes: &[u8]| -> Vec<u8> {
        if bytes.len() == (W * H * 16) as usize {
            let floats: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            klipp_effects::linear_f32_rgba_to_srgb_u8(&floats)
        } else {
            bytes.to_vec()
        }
    };
    for (f, (a, b)) in streamed.iter().zip(&read).enumerate() {
        let (a, b) = (shown(a), shown(b));
        assert_eq!(a.len(), b.len());
        let w = a.iter().zip(&b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0);
        if w > 0 {
            eprintln!("frame {f}: worst {w} levels, {} of {} values differ", a.iter().zip(&b).filter(|(x, y)| x != y).count(), a.len());
        }
        worst = worst.max(w);
    }
    eprintln!("neighbours from the stream against a reader's: worst {worst} levels");
    assert!(worst <= 2, "the frames differ by {worst} levels");
}

/// #1988: a neighbour from the stream is the very picture the stream
/// shows at that time, bit for bit.
#[test]
fn a_neighbour_from_the_stream_is_the_streams_frame() {
    use klipp_compose::FrameSource;
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let snap = snapshot(&src);
    let clip = snap.video.sequence.tracks[0].clips[0].clone();
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    let at = |f: f64| clip.source_in.to_secs_f64() + f / 25.0;
    let shown = |src: &mut ExportFrameSource, f: f64| klipp_compose::render::source_linear(src.frame(&clip, at(f), W, H).expect("a frame"), W, H);
    // As an export asks, a frame at a time: once the clip's neighbours
    // have been asked for, its stream keeps the frame before.
    let _ = shown(&mut source, 2.0);
    drop(source.temporal_frames_linear_at_size(&clip, snap.working_colorspace, W, H));
    let _ = shown(&mut source, 3.0);
    let mut provider = source.temporal_frames_linear_at_size(&clip, snap.working_colorspace, W, H).expect("neighbours");
    let (after, ..) = provider(4.0 / 25.0).expect("the frame after");
    let (before, ..) = provider(2.0 / 25.0).expect("the frame before");
    drop(provider);
    assert!(*after == *shown(&mut source, 4.0), "the neighbour after is not the stream's next frame");
    let mut fresh = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    assert!(*before == *shown(&mut fresh, 2.0), "the neighbour before is not the stream's frame before");
}

#[test]
#[ignore = "probe"]
fn probe_reader_against_stream() {
    use klipp_compose::FrameSource;
    let Some(src) = source() else { return };
    let snap = snapshot(&src);
    let clip = snap.video.sequence.tracks[0].clips[0].clone();
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    let mut reader = klipp_media::frame_reader::SeekableFrameReader::open_linear(&src, None, snap.working_colorspace).unwrap();
    let mean = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32;
    for f in 5..10 {
        let secs = f as f64 / 25.0;
        let s = klipp_compose::render::source_linear(source.frame(&clip, secs, W, H).unwrap(), W, H);
        for off in [-1i32, 0, 1] {
            let (r, w, h) = reader.decode_at(secs + off as f64 / 25.0).unwrap();
            eprintln!("file frame {f}: reader at {off:+} ({w}x{h}) differs from the stream by {:.4} on average", mean(&s, &r));
        }
    }
}
