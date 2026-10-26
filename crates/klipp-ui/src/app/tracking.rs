//! Window and surface tracking: the jobs, the grey frames they read, and the
//! tracked quad turned into keys (#890).

#[allow(unused_imports)]
use super::*;

/// A window track to run (#238): the clip's file, its rate, the file
/// frames the clip uses, the frame the window was drawn on, and the
/// window's four corners and pivot there, in frame fractions, and
/// whether to follow those corners in perspective.
pub(super) struct WindowTrackJob {
    pub(super) clip_id: klipp_core::ClipId,
    pub(super) point_index: usize,
    pub(super) path: std::path::PathBuf,
    pub(super) media_fps: f64,
    pub(super) frames: (usize, usize),
    pub(super) reference: usize,
    pub(super) quad: [(f32, f32); 4],
    pub(super) pivot: (f32, f32),
    pub(super) perspective: bool,
}

/// Where `surface` on `clip` is drawn at timeline second `at`, in
/// fractions of the frame: the clip's picture fitted and placed by its own
/// transform, as the viewer shows it. `source_secs` reads the track there
/// rather than at the frame shown.
pub(crate) fn surface_on_frame(seq: &klipp_timeline::Sequence, clip: &klipp_timeline::Clip, surface: &klipp_timeline::Surface, at: f64, source_secs: Option<f64>) -> [[f32; 2]; 4] {
    let fps = seq.fps.as_f64();
    let elapsed = (at - clip.timeline_in.to_secs_f64()).max(0.0);
    let q = surface.corners_shown_at(source_secs.unwrap_or_else(|| clip.source_secs_at(elapsed, fps)));
    let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(elapsed, clip.timeline_in.rate.max(1));
    let placed = klipp_compose::eval::evaluate_clip_own_transform(clip, elapsed, &klipp_compose::eval::eval_context(clip, when, fps));
    klipp_timeline::surface::source_quad_in_frame(&q, surface.source_size, [seq.resolution.width, seq.resolution.height], Some(placed))
}

/// A surface's stored track area and exclusions where the frame shows
/// them at `at` (#2754), carried there by the plane's motion from the
/// reference frame, as [`surface_on_frame`] places its corners.
pub(crate) fn surface_area_on_frame(seq: &klipp_timeline::Sequence, clip: &klipp_timeline::Clip, surface: &klipp_timeline::Surface, at: f64, source_secs: Option<f64>) -> (Option<[[f32; 2]; 4]>, Vec<Vec<[f32; 2]>>) {
    let fps = seq.fps.as_f64();
    let elapsed = (at - clip.timeline_in.to_secs_f64()).max(0.0);
    let Some(m) = surface.map_shown_at(source_secs.unwrap_or_else(|| clip.source_secs_at(elapsed, fps))) else { return (None, Vec::new()) };
    let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(elapsed, clip.timeline_in.rate.max(1));
    let placed = klipp_compose::eval::evaluate_clip_own_transform(clip, elapsed, &klipp_compose::eval::eval_context(clip, when, fps));
    let frame = [seq.resolution.width, seq.resolution.height];
    let on_frame = |points: &[[f32; 2]]| {
        let moved: Vec<[f32; 2]> = points.iter().map(|p| klipp_timeline::surface::mapped_point(&m, *p)).collect();
        klipp_timeline::surface::source_points_in_frame(&moved, surface.source_size, frame, Some(placed))
    };
    let track = surface.track_area.map(|a| {
        let p = on_frame(&a);
        [p[0], p[1], p[2], p[3]]
    });
    (track, surface.exclude.iter().map(|e| on_frame(e)).collect())
}

/// A surface to follow through a clip's file (#684): the file, its
/// rate, the frames the clip uses, the frame the corners were set on, and
/// the corners there in frame fractions.
pub(super) struct SurfaceTrackJob {
    pub(super) clip_id: klipp_core::ClipId,
    pub(super) surface: uuid::Uuid,
    pub(super) name: String,
    pub(super) path: std::path::PathBuf,
    pub(super) media_fps: f64,
    pub(super) frames: (usize, usize),
    pub(super) reference: usize,
    pub(super) corners: [[f32; 2]; 4],
    /// The area followed when it isn't the corners, and the parts of it
    /// left out, as the corners are (#2754).
    pub(super) track: Option<[[f32; 2]; 4]>,
    pub(super) exclude: Vec<Vec<[f32; 2]>>,
    /// Pixels in one frame as it is read for tracking.
    pub(super) pixels: usize,
    /// 1 or -1: only later or only earlier frames, merged; 0 both ways.
    pub(super) direction: i8,
    pub(super) interactive: bool,
    pub(super) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The width surfaces are tracked at, and the memory the frames may take
/// while they are (#684).
pub(super) const SURFACE_TRACK_SIDE: u32 = 1280;

pub(super) const SURFACE_TRACK_BYTES: usize = 1_250_000_000;

/// Frames after a click a surface found there is followed through, to
/// tell a flat surface from something that only looks four-sided in one
/// frame (#2754).
pub(super) const SNAP_CHECK_FRAMES: usize = 8;

/// The frames `first..=last` of the file at `path` (`fps` frames a
/// second), grey, for the window tracker (#238): read once, in order,
/// from a seek to the first, at half of a decode no more than 960 on its
/// long side. `progress` hears the share read, and stops it by returning
/// false.
pub(super) fn grey_frames(path: &std::path::Path, fps: f64, first: usize, last: usize, progress: &dyn Fn(f32) -> bool) -> Result<Vec<klipp_effects::window_track::GrayFrame>, String> {
    grey_frames_sized(path, fps, first, last, 960, true, progress)
}

/// [`grey_frames`] at a decode no more than `long_side` wide as shown,
/// halved again when `half`. A surface is followed on the sharper frames
/// (#684): a pin sits on its corners to about a pixel only when the
/// tracker sees pixels that small.
pub(super) fn grey_frames_sized(path: &std::path::Path, fps: f64, first: usize, last: usize, long_side: u32, half: bool, progress: &dyn Fn(f32) -> bool) -> Result<Vec<klipp_effects::window_track::GrayFrame>, String> {
    let mut decoder = klipp_engine::SequentialFrameDecoder::open(path, long_side)?;
    decoder.seek_to(first as f64 / fps)?;
    let count = last + 1 - first;
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let Some((rgba, w, h)) = decoder.next_frame()? else { break };
        let (data, gw, gh) = if half {
            klipp_effects::rolling_shutter::to_gray_half(&rgba, w, h)
        } else {
            // The same luma as the half-size reduction, a pixel at a time.
            (rgba.chunks_exact(4).map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8).collect(), w, h)
        };
        out.push(klipp_effects::window_track::GrayFrame { data, width: gw as usize, height: gh as usize });
        if out.len() % 8 == 0 && !progress(out.len() as f32 / count as f32) {
            return Err("cancelled".into());
        }
    }
    if out.is_empty() {
        return Err("no frames could be read from the clip".into());
    }
    Ok(out)
}

/// The region of the picture a window covers, as four corners in frame
/// fractions, on a frame `aspect` (height over width) tall: what the
/// window tracker follows. A window's sizes are in frame widths both
/// ways (see `klipp_color::masks`), so a circle reaches its radius across
/// and radius / aspect of the height down; a rectangle turns in the same
/// space; a gradient, which has no area, follows a square
/// about its middle as wide as it is long.
pub(super) fn window_quad(shape: &klipp_core::MaskShape, aspect: f32) -> [(f32, f32); 4] {
    use klipp_core::MaskShape;
    let bbox = |x0: f32, y0: f32, x1: f32, y1: f32| [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let quad = match shape {
        MaskShape::Circle { center_x, center_y, radius } => bbox(center_x - radius, center_y - radius / aspect, center_x + radius, center_y + radius / aspect),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
            let (sin, cos) = rotation.to_radians().sin_cos();
            let corner = |lx: f32, ly: f32| (center_x + lx * cos - ly * sin, center_y + (lx * sin + ly * cos) / aspect);
            let (hw, hh) = (width / 2.0, height / 2.0);
            [corner(-hw, -hh), corner(hw, -hh), corner(hw, hh), corner(-hw, hh)]
        }
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
            let (mx, my) = ((start_x + end_x) * 0.5, (start_y + end_y) * 0.5);
            let half = ((end_x - start_x).hypot((end_y - start_y) * aspect) * 0.5).max(0.05);
            bbox(mx - half, my - half / aspect, mx + half, my + half / aspect)
        }
        MaskShape::RadialGradient { center_x, center_y, outer, .. } => bbox(center_x - outer, center_y - outer / aspect, center_x + outer, center_y + outer / aspect),
        MaskShape::Bezier { points } => {
            // Round its curves too, not only its points (#74).
            let outline = klipp_core::mask_path::flatten(points, 1e-3, aspect);
            let (x0, x1) = outline.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p.0), b.max(p.0)));
            let (y0, y1) = outline.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p.1), b.max(p.1)));
            bbox(x0, y0, x1, y1)
        }
    };
    quad.map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
}

/// When each frame of a window track (#238) falls on the clip's own
/// clock — the grade's — as (index into the track's frames, time), for
/// the frames inside the clip, earliest first. Through the clip's speed
/// and in its direction (#617), at the sequence's `seq_fps`.
pub(super) fn window_track_times(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Vec<(usize, f64)> {
    let Some(fps) = clip.media_id().and_then(|m| pool.get(&m)).map(|it| it.fps.as_f64()).filter(|f| *f > 0.0) else { return Vec::new() };
    let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    let mut times: Vec<_> = (0..tp.positions.len())
        .filter_map(|i| {
            let at = (tp.start_frame + i) as f64 / fps;
            (at >= s_in - 0.5 / fps && at <= s_out + 0.5 / fps).then(|| (i, clip.elapsed_for_source_secs(at, seq_fps)))
        })
        .collect();
    times.sort_by(|a, b| a.1.total_cmp(&b.1));
    times
}

/// A window track's keyframes (#238): for each file frame tracked, its
/// time on the clip's own clock and the window's move, size and turn
/// from the reference frame, as (time, dx, dy, scale, degrees). Frames
/// outside the clip are left out; a reversed clip's keys come in its
/// own order, earliest first (see [`window_track_times`]).
pub(super) fn window_track_keys(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Vec<(f64, f32, f32, f32, f32)> {
    let reference = tp.reference_frame.unwrap_or(tp.start_frame);
    let Some(&pivot) = tp.positions.get(reference.saturating_sub(tp.start_frame)) else { return Vec::new() };
    window_track_times(clip, tp, pool, seq_fps)
        .into_iter()
        .filter_map(|(i, t)| {
            let (&(x, y), &(scale, rot)) = (tp.positions.get(i)?, tp.motion.get(i)?);
            Some((t, (x - pivot.0) as f32, (y - pivot.1) as f32, scale as f32, rot as f32))
        })
        .collect()
}

/// A window track in perspective (#238) as the window carries it: the
/// corners on the reference frame, and where they are at each moment on
/// the clip's own clock. `None` without the reference frame's corners.
pub(super) fn window_perspective_track(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Option<klipp_core::PerspectiveTrack> {
    let reference = tp.reference_frame.unwrap_or(tp.start_frame);
    let corner = |q: &[(f64, f64); 4]| q.map(|(x, y)| [x as f32, y as f32]);
    let from = corner(tp.corners.get(reference.checked_sub(tp.start_frame)?)?);
    let keys = window_track_times(clip, tp, pool, seq_fps)
        .into_iter()
        .filter_map(|(i, t)| Some(klipp_core::CornerKey { time_secs: t, corners: corner(tp.corners.get(i)?) }))
        .collect();
    Some(klipp_core::PerspectiveTrack { from, keys })
}

#[cfg(test)]
mod window_track_tests {
    use super::{window_perspective_track, window_quad, window_track_keys};

    /// #238: a window track's keyframes sit on the clip's own clock — the
    /// grade's — not the file's: a clip trimmed to start 2 s into its file
    /// and played at double speed has the file's frame 60 (2 s at 30 fps)
    /// at its first moment, and 75 a quarter second later. Frames the clip
    /// does not play are left out.
    #[test]
    fn a_window_tracks_keys_land_on_the_clips_own_clock() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/walk.mov".into(), None);
        item.fps = klipp_core::Fps::new(30, 1);
        let media = pool.add(item);
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 30);
        let mut clip = klipp_timeline::Clip::media(media, tc(10.0), tc(11.0), tc(2.0), tc(4.0));
        clip.speed = 2.0;
        // Frames 58..=92 tracked from frame 60, moving right 0.01 a frame.
        let tp = klipp_core::TrackPoint {
            start_frame: 58,
            positions: (58..=92).map(|f| (0.5 + 0.01 * (f as f64 - 60.0), 0.4)).collect(),
            motion: (58..=92).map(|f| (1.0 + 0.001 * (f as f64 - 60.0), 0.5 * (f as f64 - 60.0))).collect(),
            reference_frame: Some(60),
            ..Default::default()
        };
        let keys = window_track_keys(&clip, &tp, &pool, 30.0);
        let first = keys.first().unwrap();
        assert!(first.0.abs() < 1e-9 && first.1.abs() < 1e-6 && (first.3 - 1.0).abs() < 1e-6, "{first:?}");
        let quarter = keys.iter().find(|k| (k.0 - 0.25).abs() < 1e-6).expect("a key a quarter second in");
        assert!((quarter.1 - 0.15).abs() < 1e-5 && (quarter.4 - 7.5).abs() < 1e-4, "{quarter:?}");
        assert!(keys.iter().all(|k| k.0 >= 0.0 && k.0 <= 1.0 + 1e-6), "only what the clip plays");

        // Reversed (#617): the same frames run the other way on the clip's
        // clock — the reference frame, its range's start, comes last.
        clip.speed = -2.0;
        let back = window_track_keys(&clip, &tp, &pool, 30.0);
        assert!(back.windows(2).all(|w| w[0].0 <= w[1].0), "earliest first");
        let last = back.last().unwrap();
        assert!((last.0 - (1.0 - 1.0 / 30.0)).abs() < 1e-6 && last.1.abs() < 1e-6, "the reference frame shows a frame before the clip's end: {last:?}");
        assert!(back.first().unwrap().1 > 0.0, "the later frames show first: {:?}", back.first());
    }

    /// #238: a track in perspective becomes the window's corner course on
    /// the same clock, measured from the corners on the reference frame;
    /// a window evaluated from it is carried there.
    #[test]
    fn a_perspective_track_lands_its_corners_on_the_clips_own_clock() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/sign.mov".into(), None);
        item.fps = klipp_core::Fps::new(30, 1);
        let media = pool.add(item);
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 30);
        let clip = klipp_timeline::Clip::media(media, tc(10.0), tc(11.0), tc(2.0), tc(3.0));
        let quad = [(0.4, 0.4), (0.6, 0.4), (0.6, 0.6), (0.4, 0.6)];
        // Frames 58..=70 from frame 60: the right side sliding up and in.
        let course = |f: usize| {
            let k = f as f64 - 60.0;
            [quad[0], (0.6 - 0.002 * k, 0.4 + 0.003 * k), (0.6 - 0.002 * k, 0.6 - 0.003 * k), quad[3]]
        };
        let tp = klipp_core::TrackPoint {
            start_frame: 58,
            positions: (58..=70).map(|_| (0.5, 0.5)).collect(),
            corners: (58..=70).map(course).collect(),
            reference_frame: Some(60),
            ..Default::default()
        };
        let track = window_perspective_track(&clip, &tp, &pool, 30.0).unwrap();
        assert_eq!(track.from, quad.map(|(x, y)| [x as f32, y as f32]));
        assert!(track.keys.first().unwrap().time_secs.abs() < 1e-9, "frame 60 opens the clip");
        assert_eq!(track.keys.len(), 11, "frames 60..=70; 58 and 59 are before the clip");
        let at = track.keys.iter().find(|k| (k.time_secs - 10.0 / 30.0).abs() < 1e-6).unwrap();
        assert!((at.corners[1][0] - 0.58).abs() < 1e-6 && (at.corners[1][1] - 0.43).abs() < 1e-6, "{at:?}");
        let mut mask = klipp_core::Mask::default();
        mask.keyframes.track_perspective = Some(track);
        let warp = mask.evaluated_in(10.0 / 30.0, 9.0 / 16.0).keyframes.warp.unwrap();
        let back = warp.apply((0.58, 0.43)).unwrap();
        assert!((back.0 - 0.6).abs() < 1e-4 && (back.1 - 0.4).abs() < 1e-4, "{back:?}");
    }

    /// The region followed is the one the window covers, as the matte
    /// draws it: a circle as far down as across, `radius / aspect` of the
    /// height.
    #[test]
    fn the_region_followed_is_the_window() {
        let aspect = 9.0 / 16.0;
        let q = window_quad(&klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 }, aspect);
        assert!((q[0].0 - 0.4).abs() < 1e-6 && (q[0].1 - (0.5 - 0.1 / aspect)).abs() < 1e-6 && (q[2].0 - 0.6).abs() < 1e-6);
        let q = window_quad(&klipp_core::MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.2, height: 0.1, rotation: 0.0 }, aspect);
        assert!((q[1].0 - 0.6).abs() < 1e-6 && (q[2].1 - (0.5 + 0.05 / aspect)).abs() < 1e-6);
    }
}

#[cfg(test)]
mod surface_area_tests {
    use super::{surface_area_on_frame, surface_on_frame};

    /// #2754: a surface's track area and the parts left out open in the
    /// surface tool where the frame shows them, carried by the plane's
    /// motion as its corners are: a plane moved a tenth of the frame right
    /// by the key at one second moves the area and each outline with it.
    #[test]
    fn the_area_followed_and_the_parts_left_out_move_with_the_plane() {
        let seq = klipp_timeline::Sequence::new_default();
        let (w, h) = (seq.resolution.width, seq.resolution.height);
        let rate = seq.fps.as_f64().round() as u32;
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, rate);
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let corners = [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]];
        let shifted = corners.map(|c| [c[0] + 0.1, c[1]]);
        let surface = klipp_timeline::Surface {
            id: uuid::Uuid::new_v4(),
            name: "Sign".into(),
            corners,
            reference_secs: 0.0,
            keys: vec![klipp_timeline::SurfaceKey { source_secs: 1.0, map: klipp_timeline::surface::map_between(&corners, &shifted).unwrap(), confidence: Some(1.0), state: Default::default() }],
            source_size: [w, h],
            media_fps: 30.0,
            track_area: Some([[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]]),
            exclude: vec![vec![[0.45, 0.45], [0.55, 0.45], [0.5, 0.55]]],
        };
        let near = |a: &[[f32; 2]], b: &[[f32; 2]]| a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3);
        let on = surface_on_frame(&seq, &clip, &surface, 1.0, None);
        assert!(near(&on, &shifted), "the corners moved: {on:?}");
        let (area, parts) = surface_area_on_frame(&seq, &clip, &surface, 1.0, None);
        assert!(near(&area.unwrap(), &[[0.3, 0.2], [0.9, 0.2], [0.9, 0.8], [0.3, 0.8]]), "{area:?}");
        assert_eq!(parts.len(), 1);
        assert!(near(&parts[0], &[[0.55, 0.45], [0.65, 0.45], [0.6, 0.55]]), "{parts:?}");
        // A surface without an area of its own opens without one.
        let plain = klipp_timeline::Surface { track_area: None, exclude: Vec::new(), ..surface };
        assert_eq!(surface_area_on_frame(&seq, &clip, &plain, 1.0, None), (None, Vec::new()));
    }
}
