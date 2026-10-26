//! Surfaces: the planes a pinned layer rides, snapped to the picture and
//! tracked through the shot (#826).

use super::*;

impl KlippApp {
    /// A tracked surface onto its clip as one undo step (#684), replacing
    /// the one it re-tracks, or, continued one way, laid over its track;
    /// what is pinned to it follows on the next frame drawn. Says how much
    /// of the clip it covers and where it was lost; for a person at the
    /// viewer, the playhead goes to the first frame lost with the corners
    /// up to put right and continue.
    pub(super) fn apply_surface_track(&mut self, clip_id: klipp_core::ClipId, surface: klipp_timeline::Surface, held_secs: (f64, f64), direction: i8, interactive: bool) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let fps = seq.fps.as_f64();
        let Some(old) = seq.all_clips().find(|c| c.id == clip_id).cloned() else {
            self.state.toast.show("The tracked clip is gone; the track was dropped");
            return;
        };
        let mut new = old.clone();
        match new.surfaces.iter_mut().find(|s| s.id == surface.id) {
            Some(s) if direction != 0 => s.merge_keys(surface.keys.clone(), &surface.corners),
            Some(s) => *s = surface.clone(),
            None => new.surfaces.push(surface.clone()),
        }
        let edit = EditCommand::ReplaceClip { description: "Track Surface", old: Box::new(old.clone()), new: Box::new(new.clone()) };
        self.apply_edits_as_one(std::slice::from_ref(&edit), "Track Surface");
        // Where this run ends short of the clip, in the direction it ran.
        let (from, to) = surface.tracked_secs().unwrap_or((surface.reference_secs, surface.reference_secs));
        let on_timeline = |src: f64| old.timeline_in.to_secs_f64() + old.elapsed_for_source_secs(src, fps);
        let (lo, hi) = (old.source_in.to_secs_f64(), old.source_out.to_secs_f64());
        let frame = 1.0 / surface.media_fps.max(fps).max(1.0);
        // Short of the clip at an edge of the frames read is the memory
        // limit, not the tracker losing the surface.
        let (mut lost, mut held) = (Vec::new(), Vec::new());
        let mut first_lost: Option<(f64, i8)> = None;
        for (short, edge, held_edge, side, way) in [(direction >= 0 && to < hi - frame * 1.5, to, held_secs.1, "after", 1i8), (direction <= 0 && from > lo + frame * 1.5, from, held_secs.0, "before", -1i8)] {
            if short {
                let said = format!("{side} {:.2} s", on_timeline(edge));
                if (edge - held_edge).abs() < frame * 1.5 {
                    held.push(said)
                } else {
                    lost.push(said);
                    first_lost.get_or_insert((edge, way));
                }
            }
        }
        let mut note = String::new();
        if !lost.is_empty() {
            note.push_str(&format!("; lost {}", lost.join(" and ")));
        }
        if !held.is_empty() {
            note.push_str(&format!("; not tracked {}, past the frames held at once: track again from a frame there", held.join(" and ")));
        }
        // Stretches followed uncertainly or lost then found again (#2754).
        let weak = surface.weak_spans();
        for (state, said) in [(klipp_timeline::TrackState::Low, "uncertain"), (klipp_timeline::TrackState::Lost, "lost")] {
            let these: Vec<String> = weak.iter().filter(|s| s.2 == state).map(|s| format!("{:.2}–{:.2} s", on_timeline(s.0), on_timeline(s.1))).collect();
            if !these.is_empty() {
                note.push_str(&format!("; {said} {}", these.join(", ")));
            }
        }
        let total = new.surfaces.iter().find(|s| s.id == surface.id).map_or(0, |s| s.keys.len());
        // Stop and fix: on the frame after the last one followed, the
        // corners where they last were, to drag onto the surface.
        if let (true, Some((edge, way))) = (interactive, first_lost) {
            let step = 1.0 / surface.media_fps.max(1.0);
            let src = (edge + way as f64 * step).clamp(lo, (hi - step).max(lo));
            let at = on_timeline(src).clamp(new.timeline_in.to_secs_f64(), new.timeline_out.to_secs_f64() - 1.0 / fps.max(1.0));
            let rate = new.timeline_in.rate.max(1);
            self.send_commands(vec![AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(at, rate))]);
            if let Some(s) = new.surfaces.iter().find(|s| s.id == surface.id) {
                let corners = surface_on_frame(self.state.sequence(), &new, s, at, Some(edge));
                let (track, exclude) = surface_area_on_frame(self.state.sequence(), &new, s, at, Some(edge));
                self.state.surface_tool = Some(crate::state::SurfaceTool { clip_id, surface: Some(s.id), name: Some(s.name.clone()), corners, direction: way, track, exclude, edit: Default::default(), drag: None });
            }
            self.state.toast.show(format!("Lost \u{201c}{}\u{201d} at {at:.2} s: drag the corners onto it and press Continue", surface.name));
            return;
        }
        self.state.toast.show(format!("Tracked \u{201c}{}\u{201d} over {total} frames{note}", surface.name));
    }

    /// Where `clip`'s own picture sits in the frame at timeline second
    /// `at`: its size as shown and its own placement then. `None` for a
    /// clip not from a file.
    fn clip_picture_at(&self, clip: &klipp_timeline::Clip, at: f64) -> Option<([u32; 2], klipp_timeline::surface::Placement)> {
        let item = clip.media_id().and_then(|m| self.state.media_pool.get(&m))?;
        let (w, h) = (item.resolution.width, item.resolution.height);
        let shown = if item.source_rotation.rem_euclid(180) == 90 { [h, w] } else { [w, h] };
        let fps = self.state.sequence().fps.as_f64();
        let elapsed = (at - clip.timeline_in.to_secs_f64()).clamp(0.0, (clip.timeline_out - clip.timeline_in).to_secs_f64());
        let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(elapsed, clip.timeline_in.rate.max(1));
        Some((shown, klipp_compose::eval::evaluate_clip_own_transform(clip, elapsed, &klipp_compose::eval::eval_context(clip, when, fps))))
    }

    /// Find a surface's corners round a click in the clip's picture (#684),
    /// on a thread: the frame is read as the tracker reads it, so the
    /// corners found are on the frame a track would start from. A quad
    /// found is followed through the frames after it, as many as the clip
    /// plays up to [`SNAP_CHECK_FRAMES`], and kept only when it moves as
    /// one plane (#2754).
    pub(super) fn start_surface_snap(&mut self, clip_id: klipp_core::ClipId, click: [f32; 2], at_secs: Option<f64>, then_track: bool, name: Option<String>, request_id: Option<uuid::Uuid>) {
        let at = at_secs.unwrap_or(self.state.playhead_smooth_secs);
        let seq = self.state.sequence();
        let frame = [seq.resolution.width, seq.resolution.height];
        let fps = seq.fps.as_f64();
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)).cloned() else {
            self.state.toast.show("Only a clip from a file has a surface to find");
            return;
        };
        let Some((shown, placed)) = self.clip_picture_at(&clip, at) else { return };
        let point = klipp_timeline::surface::frame_quad_in_source(&[click; 4], shown, frame, Some(placed))[0];
        if !(0.0..=1.0).contains(&point[0]) || !(0.0..=1.0).contains(&point[1]) {
            self.state.toast.show("Click on the clip's picture to find a surface there");
            return;
        }
        let elapsed = (at - clip.timeline_in.to_secs_f64()).clamp(0.0, (clip.timeline_out - clip.timeline_in).to_secs_f64());
        let media_fps = item.fps.as_f64().max(1.0);
        // The start of the file frame shown, as the tracker counts frames.
        let source_secs = (clip.source_secs_at(elapsed, fps) * media_fps).round() / media_fps;
        let path = item.path.clone();
        // The frames after it the clip plays, to check the quad against.
        let after = (((clip.source_out.to_secs_f64() - source_secs) * media_fps).floor().max(0.0) as usize).min(SNAP_CHECK_FRAMES);
        let event_tx = self.proxy_event_tx.clone();
        self.state.surface_snaps_pending += 1;
        std::thread::spawn(move || {
            let read = || -> Result<Option<(Vec<u8>, usize, usize, Vec<klipp_effects::window_track::GrayFrame>)>, String> {
                let mut d = klipp_engine::SequentialFrameDecoder::open(&path, SURFACE_TRACK_SIDE)?;
                d.seek_to(source_secs)?;
                let Some((rgba, w, h)) = d.next_frame()? else { return Ok(None) };
                let mut next = Vec::with_capacity(after);
                while next.len() < after {
                    let Some((f, fw, fh)) = d.next_frame()? else { break };
                    next.push(klipp_effects::window_track::GrayFrame { data: f.chunks_exact(4).map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8).collect(), width: fw as usize, height: fh as usize });
                }
                Ok(Some((rgba, w as usize, h as usize, next)))
            };
            let (corners, why) = match read() {
                Ok(Some((rgba, w, h, next))) => match klipp_effects::surface_snap::find(&rgba, w, h, (point[0], point[1]), &next) {
                    Ok(q) => (Some(q), None),
                    Err(miss) => (None, Some(miss.reason().to_string())),
                },
                Ok(None) => (None, Some("the frame there could not be read".to_string())),
                Err(e) => (None, Some(format!("the frame there could not be read: {e}"))),
            };
            let _ = event_tx.send(klipp_core::AppEvent::SurfaceSnapped { clip_id, at_secs: at, corners, then_track, name, why, request_id });
        });
    }

    /// Snapped corners into the surface tool, or tracked from at once for
    /// the assistant; or, found nothing, said so.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_surface_snap(&mut self, clip_id: klipp_core::ClipId, at: f64, corners: Option<[[f32; 2]; 4]>, then_track: bool, name: Option<String>, why: Option<String>, request_id: Option<uuid::Uuid>) {
        // What the control socket waits for: the corners, or why not.
        let tell = |this: &Self, outcome: serde_json::Value| {
            if let Some(id) = request_id {
                let mut o = outcome;
                o["request_id"] = serde_json::json!(id.to_string());
                *this.state.control_shared.surface_snap.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
            }
        };
        let Some(clip) = self.state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else {
            tell(self, serde_json::json!({ "error": "the clip is gone" }));
            return;
        };
        let Some(on_source) = corners else {
            let why = why.unwrap_or_else(|| klipp_effects::surface_snap::Miss::NoClosedRegion.reason().to_string());
            tell(self, serde_json::json!({ "error": why }));
            let mut said = why.clone();
            if let Some(first) = said.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            self.state.toast.show(format!("{said}: drag the corners onto the surface instead"));
            return;
        };
        let Some((shown, placed)) = self.clip_picture_at(&clip, at) else { return };
        let frame = [self.state.sequence().resolution.width, self.state.sequence().resolution.height];
        let on_frame = klipp_timeline::surface::source_quad_in_frame(&on_source, shown, frame, Some(placed));
        tell(self, serde_json::json!({ "corners": on_frame, "at_secs": at }));
        if then_track {
            self.send_commands(vec![AppCommand::TrackSurface { clip_id, corners: on_frame, at_secs: Some(at), name, surface: None, direction: 0, interactive: false, track: None, exclude: None }]);
            return;
        }
        match self.state.surface_tool.as_mut() {
            Some(tool) if tool.clip_id == clip_id => {
                tool.corners = on_frame;
                tool.drag = None;
                self.state.toast.show("Corners on the surface's edges: adjust them if need be, and Track");
            }
            _ => {}
        }
    }

    /// The viewer's surface tool on `clip_id` (#684 phase 2): corners where
    /// `surface` is at the playhead, or a box in the middle of the clip's
    /// picture for a new one. The playhead goes onto the clip if it is not.
    pub(super) fn open_surface_tool(&mut self, clip_id: klipp_core::ClipId, surface: Option<uuid::Uuid>) {
        let seq = self.state.sequence();
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)).cloned() else {
            self.state.toast.show("Only a clip from a file can be tracked");
            return;
        };
        if item.is_still {
            self.state.toast.show("A still does not move: a surface is tracked in footage");
            return;
        }
        let (a, b) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
        let mut at = self.state.playhead.to_secs_f64();
        if !(a..b).contains(&at) {
            at = surface.and_then(|id| clip.surfaces.iter().find(|s| s.id == id)).map_or(a, |s| a + clip.elapsed_for_source_secs(s.reference_secs, seq.fps.as_f64())).clamp(a, b);
            self.send_commands(vec![AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(at, clip.timeline_in.rate.max(1)))]);
        }
        let (w, h) = (item.resolution.width, item.resolution.height);
        let shown = if item.source_rotation.rem_euclid(180) == 90 { [h, w] } else { [w, h] };
        let existing = surface.and_then(|id| clip.surfaces.iter().find(|s| s.id == id));
        let corners = match existing {
            Some(s) => surface_on_frame(self.state.sequence(), &clip, s, at, None),
            None => {
                let middle = klipp_timeline::surface::Surface {
                    id: uuid::Uuid::nil(),
                    name: String::new(),
                    corners: [[0.3, 0.3], [0.7, 0.3], [0.7, 0.7], [0.3, 0.7]],
                    reference_secs: 0.0,
                    keys: Vec::new(),
                    source_size: shown,
                    media_fps: 0.0,
                    track_area: None,
                    exclude: Vec::new(),
                };
                surface_on_frame(self.state.sequence(), &clip, &middle, at, None)
            }
        };
        let (track, exclude) = existing.map(|s| surface_area_on_frame(self.state.sequence(), &clip, s, at, None)).unwrap_or_default();
        self.state.surface_tool = Some(crate::state::SurfaceTool { clip_id, surface, name: existing.map(|s| s.name.clone()), corners, direction: 0, track, exclude, edit: Default::default(), drag: None });
        self.state.selected_clip = Some(clip_id);
    }

    /// Follow a surface in perspective both ways from its reference frame
    /// (#684), on a thread: the frames are read once, as for a window, and
    /// each followed frame's corners come back by file time.
    pub(super) fn start_surface_track(&mut self, job: SurfaceTrackJob) {
        let SurfaceTrackJob { clip_id, surface, name, path, media_fps, frames: (first, last), reference, corners, track, exclude, pixels, direction, interactive, cancel } = job;
        self.state.tracking_in_progress.insert(clip_id);
        self.state.tracking_progress.insert(clip_id, 0.0);
        let event_tx = self.proxy_event_tx.clone();
        std::thread::spawn(move || {
            use klipp_effects::window_track::GrayFrame;
            let fail = |error: String| {
                let _ = event_tx.send(klipp_core::AppEvent::SurfaceTrackFailed { clip_id, surface, error });
            };
            // Frames held at once: as many as fit about a gigabyte and a
            // quarter at the size they are read. Continuing one way reads
            // only that way's frames, so all of them go that way; both ways,
            // they are shared about the reference.
            let most = (SURFACE_TRACK_BYTES / pixels.max(1)).clamp(50, 3600);
            let (first, last) = match direction {
                1 => (reference, last.min(reference + most - 1)),
                -1 => (first.max((reference + 1).saturating_sub(most)), reference),
                _ if last + 1 - first > most => {
                    let lo = reference.saturating_sub(most / 2).max(first);
                    (lo, (lo + most - 1).min(last))
                }
                _ => (first, last),
            };
            let loading = 0.6;
            let going = || !cancel.load(std::sync::atomic::Ordering::Relaxed);
            let frames = match grey_frames_sized(&path, media_fps, first, last, SURFACE_TRACK_SIDE, false, &|p| {
                going() && event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: p * loading }).is_ok()
            }) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            let decode = |f: usize| -> Option<GrayFrame> { f.checked_sub(first).and_then(|i| frames.get(i)).cloned() };
            let Some(reference_frame) = decode(reference) else {
                return fail("the frame the corners were set on could not be read".into());
            };
            let last = first + frames.len().saturating_sub(1);
            let (gw, gh) = (reference_frame.width as f64, reference_frame.height as f64);
            // The area followed: the corners, or a larger part of the plane
            // round them (#2754); less what is left out. The map found is
            // the plane's, so the corners go where it takes them either way.
            let quad_px = track.unwrap_or(corners).map(|c| (c[0] as f64 * gw, c[1] as f64 * gh));
            let exclude_px: Vec<Vec<(f64, f64)>> = exclude.iter().map(|e| e.iter().map(|c| (c[0] as f64 * gw, c[1] as f64 * gh)).collect()).collect();
            // The whole surface aligned frame to frame (#2754), its own
            // outline included, each frame with its confidence.
            let opts = klipp_effects::planar::Options::default();
            let forward: Vec<usize> = if direction >= 0 { (reference + 1..=last).collect() } else { Vec::new() };
            let backward: Vec<usize> = if direction <= 0 { (first..reference).rev().collect() } else { Vec::new() };
            let share = (forward.len() as f32 / (forward.len() + backward.len()).max(1) as f32).max(0.01);
            let report = |p: f32| going() && event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: loading + p * (1.0 - loading) }).is_ok();
            // On the GPU when there is one (its own passes on the shared
            // device), else the CPU, which follows the same.
            let mut gpu = klipp_effects::gpu::planar::PlanarGpu::new();
            let mut run = |frames: &[usize], progress: &dyn Fn(f32) -> bool| match gpu.as_mut() {
                Some(g) => g.track(&reference_frame, quad_px, &exclude_px, frames, &mut |f| decode(f), &opts, progress),
                None => klipp_effects::planar::track(&reference_frame, quad_px, &exclude_px, frames, &mut |f| decode(f), &opts, progress),
            };
            let ahead = run(&forward, &|p| report(p * share));
            let behind = run(&backward, &|p| report(share + p * (1.0 - share)));
            // Frames lost at the end of a run, never found again, are where
            // the track ends: not kept, so the track reads as lost there.
            let trimmed = |mut t: Vec<klipp_effects::planar::Tracked>| {
                while t.last().is_some_and(|t| t.state == klipp_effects::planar::State::Lost) {
                    t.pop();
                }
                t
            };
            let (ahead, behind) = (trimmed(ahead), trimmed(behind));
            let followed = |t: &[klipp_effects::planar::Tracked]| !t.is_empty();
            if !followed(&ahead) && !followed(&behind) && !(forward.is_empty() && backward.is_empty()) {
                return fail("the surface could not be followed from the frame its corners were set on: set them round the whole flat surface, its edges included".into());
            }
            // The maps between fractions of the picture, not its pixels.
            let to_px = klipp_effects::homography::Homography { h: [[gw, 0.0, 0.0], [0.0, gh, 0.0], [0.0, 0.0, 1.0]] };
            let to_frac = klipp_effects::homography::Homography { h: [[1.0 / gw, 0.0, 0.0], [0.0, 1.0 / gh, 0.0], [0.0, 0.0, 1.0]] };
            let still = klipp_effects::planar::Tracked { frame: reference, h: klipp_effects::homography::Homography::identity(), confidence: 1.0, state: klipp_effects::planar::State::Good, round_trip_px: 0.0 };
            let keys = behind
                .into_iter()
                .rev()
                .chain(std::iter::once(still))
                .chain(ahead)
                .map(|t| {
                    let m = to_frac.multiply(&t.h).multiply(&to_px).h;
                    let state = match t.state {
                        klipp_effects::planar::State::Good => 0,
                        klipp_effects::planar::State::Low => 1,
                        klipp_effects::planar::State::Lost => 2,
                    };
                    (t.frame as f64 / media_fps, [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]], t.confidence, state)
                })
                .collect();
            let held_secs = (first as f64 / media_fps, last as f64 / media_fps);
            let source_size = [gw as u32, gh as u32];
            if !going() {
                return fail("cancelled".into());
            }
            let _ = event_tx.send(klipp_core::AppEvent::SurfaceTracked { clip_id, surface, name, corners, reference_secs: reference as f64 / media_fps, keys, source_size, media_fps, held_secs, direction, interactive, track_area: track, exclude });
        });
    }

    /// Correct `surface` on the frame at `at_secs` (#2754): the key there
    /// becomes the corners given, set by hand, and the track either side
    /// is solved again from it, on a thread, as far as the anchors next to
    /// it, so the rest of the track stays as it was.
    pub(super) fn correct_surface(&mut self, clip_id: klipp_core::ClipId, surface: uuid::Uuid, corners: [[f32; 2]; 4], at_secs: Option<f64>) {
        if !klipp_timeline::surface::quad_is_usable(&corners) {
            self.state.toast.show("The surface's corners need to enclose an area");
            return;
        }
        if self.state.tracking_in_progress.contains(&clip_id) {
            self.state.toast.show("This clip is already being tracked");
            return;
        }
        let at = at_secs.unwrap_or(self.state.playhead_smooth_secs);
        let seq = self.state.sequence();
        let frame = [seq.resolution.width, seq.resolution.height];
        let fps = seq.fps.as_f64();
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let Some(existing) = clip.surfaces.iter().find(|s| s.id == surface).cloned() else {
            self.state.toast.show("A correction goes on a surface the clip already has: track it first");
            return;
        };
        let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)).cloned() else { return };
        let Some((shown, placed)) = self.clip_picture_at(&clip, at) else { return };
        let on_source = klipp_timeline::surface::frame_quad_in_source(&corners, shown, frame, Some(placed));
        if !klipp_timeline::surface::quad_is_usable(&on_source) {
            self.state.toast.show("The surface's corners need to be on the clip's picture");
            return;
        }
        let media_fps = item.fps.as_f64().max(1.0);
        let frame_count = (item.duration.to_secs_f64() * media_fps).round() as usize;
        let clip_time = (at - clip.timeline_in.to_secs_f64()).clamp(0.0, (clip.timeline_out - clip.timeline_in).to_secs_f64());
        let first = (clip.source_in.to_secs_f64() * media_fps).floor() as usize;
        let last = ((clip.source_out.to_secs_f64() * media_fps).ceil() as usize).min(frame_count.saturating_sub(1)).max(first);
        let k = ((clip.source_secs_at(clip_time, fps) * media_fps).round() as usize).clamp(first, last);
        let source_secs = k as f64 / media_fps;
        // The surface as it will be: the correction set, its anchors.
        let mut corrected = existing.clone();
        if !corrected.set_correction(source_secs, &on_source) {
            self.state.toast.show("The corners can't be reached from the surface's own: set them round the same surface");
            return;
        }
        let Some(correction) = corrected.keys.iter().find(|key| key.state == klipp_timeline::TrackState::Manual && (key.source_secs - source_secs).abs() < 0.5 / media_fps).map(|key| key.map) else { return };
        let (before, after) = corrected.anchors_around(source_secs);
        let to_frame = |a: (f64, klipp_timeline::surface::Map)| (((a.0 * media_fps).round() as usize).clamp(first, last), a.1);
        let (before, after) = (before.map(to_frame), after.map(to_frame));
        let (w, h) = (item.resolution.width.max(1) as f64, item.resolution.height.max(1) as f64);
        let shown_w = if item.source_rotation.rem_euclid(180) == 90 { h } else { w };
        let scale = (SURFACE_TRACK_SIDE as f64 / shown_w).min(1.0);
        let pixels = ((w * scale) * (h * scale)) as usize;
        let area = existing.track_area.unwrap_or(existing.corners);
        let exclude = existing.exclude.clone();
        let path = item.path.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.state.surface_track_cancel.insert(clip_id, cancel.clone());
        self.state.surface_tool = None;
        self.state.tracking_in_progress.insert(clip_id);
        self.state.tracking_progress.insert(clip_id, 0.0);
        self.state.toast.show(format!("Correcting \u{201c}{}\u{201d}\u{2026}", existing.name));
        let event_tx = self.proxy_event_tx.clone();
        std::thread::spawn(move || {
            use klipp_effects::homography::Homography;
            use klipp_effects::planar::{Anchor, State};
            let fail = |error: String| {
                let _ = event_tx.send(klipp_core::AppEvent::SurfaceTrackFailed { clip_id, surface, error });
            };
            // The frames read: from the anchor before to the one after, or
            // as many round the correction as fit in memory; an anchor past
            // them is left, and the track beyond the frames read kept.
            let most = (SURFACE_TRACK_BYTES / pixels.max(1)).clamp(50, 3600);
            let lo = before.map_or(first, |b| b.0).max(k.saturating_sub(most / 2));
            let hi = after.map_or(last, |a| a.0).min(lo + most - 1).max(k);
            let lo = lo.min(k);
            let (before, after) = (before.filter(|b| b.0 >= lo), after.filter(|a| a.0 <= hi));
            let loading = 0.5;
            let going = || !cancel.load(std::sync::atomic::Ordering::Relaxed);
            let frames = match grey_frames_sized(&path, media_fps, lo, hi, SURFACE_TRACK_SIDE, false, &|p| {
                going() && event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: p * loading }).is_ok()
            }) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            let Some((gw, gh)) = frames.first().map(|f| (f.width as f64, f.height as f64)) else {
                return fail("the clip's frames could not be read".into());
            };
            let hi = lo + frames.len() - 1;
            let mut decode = |f: usize| -> Option<klipp_effects::window_track::GrayFrame> { f.checked_sub(lo).and_then(|i| frames.get(i)).cloned() };
            let to_px = Homography { h: [[gw, 0.0, 0.0], [0.0, gh, 0.0], [0.0, 0.0, 1.0]] };
            let to_frac = Homography { h: [[1.0 / gw, 0.0, 0.0], [0.0, 1.0 / gh, 0.0], [0.0, 0.0, 1.0]] };
            let px = |m: &klipp_timeline::surface::Map| to_px.multiply(&Homography { h: [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]] }).multiply(&to_frac);
            let anchor = |a: (usize, klipp_timeline::surface::Map)| Anchor { frame: a.0, h: px(&a.1) };
            let here = Anchor { frame: k, h: px(&correction) };
            let quad = area.map(|c| (c[0] as f64 * gw, c[1] as f64 * gh));
            let exclude_px: Vec<Vec<(f64, f64)>> = exclude.iter().map(|e| e.iter().map(|c| (c[0] as f64 * gw, c[1] as f64 * gh)).collect()).collect();
            let opts = klipp_effects::planar::Options::default();
            let report = |p: f32| going() && event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: loading + p * (1.0 - loading) }).is_ok();
            let mut gpu = klipp_effects::gpu::planar::PlanarGpu::new();
            let mut solve = |start: Option<Anchor>, end: Option<Anchor>, span: (usize, usize), progress: &dyn Fn(f32) -> bool| match gpu.as_mut() {
                Some(g) => g.solve_between(quad, &exclude_px, start, end, span, &mut decode, &opts, progress),
                None => klipp_effects::planar::solve_between(quad, &exclude_px, start, end, span, &mut decode, &opts, progress),
            };
            let earlier = solve(before.map(anchor), Some(here), (lo, k), &|p| report(p * 0.5));
            let later = solve(Some(here), after.map(anchor), (k, hi), &|p| report(0.5 + p * 0.5));
            if !going() {
                return fail("cancelled".into());
            }
            let key = |t: &klipp_effects::planar::Tracked| {
                let m = to_frac.multiply(&t.h).multiply(&to_px).h;
                let state = match t.state {
                    State::Good => 0,
                    State::Low => 1,
                    State::Lost => 2,
                };
                (t.frame as f64 / media_fps, [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]], t.confidence, state)
            };
            // Each span's bounds: its anchor, or the edge of the frames
            // read (open where that is the clip's end).
            let secs = |f: usize| f as f64 / media_fps;
            let edge_before = before.map(|b| secs(b.0)).or((lo > first).then(|| secs(lo) - 0.5 / media_fps));
            let edge_after = after.map(|a| secs(a.0)).or((hi < last).then(|| secs(hi) + 0.5 / media_fps));
            let spans = vec![
                (edge_before, Some(source_secs), earlier.iter().map(key).collect()),
                (Some(source_secs), edge_after, later.iter().map(key).collect()),
            ];
            let _ = event_tx.send(klipp_core::AppEvent::SurfaceCorrected { clip_id, surface, source_secs, corners: on_source, spans });
        });
    }

    /// A correction solved onto its clip as one undo step (#2754): the key
    /// set by hand and the spans either side re-solved; says what was
    /// re-solved and where it is still uncertain or lost.
    #[allow(clippy::type_complexity)]
    pub(super) fn apply_surface_correction(&mut self, clip_id: klipp_core::ClipId, surface: uuid::Uuid, source_secs: f64, corners: [[f32; 2]; 4], spans: Vec<(Option<f64>, Option<f64>, Vec<(f64, [f64; 9], f32, u8)>)>) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let fps = seq.fps.as_f64();
        let Some(old) = seq.all_clips().find(|c| c.id == clip_id).cloned() else {
            self.state.toast.show("The corrected clip is gone; the correction was dropped");
            return;
        };
        let mut new = old.clone();
        let Some(s) = new.surfaces.iter_mut().find(|s| s.id == surface) else {
            self.state.toast.show("The corrected surface is gone; the correction was dropped");
            return;
        };
        if !s.set_correction(source_secs, &corners) {
            return;
        }
        let (mut from, mut to) = (source_secs, source_secs);
        for (after, before, keys) in spans {
            let keys: Vec<klipp_timeline::SurfaceKey> = keys.into_iter().map(surface_key_of).collect();
            if let (Some(a), Some(b)) = (keys.first(), keys.last()) {
                from = from.min(a.source_secs);
                to = to.max(b.source_secs);
            }
            s.replace_between(after, before, keys);
        }
        let name = s.name.clone();
        let weak: Vec<(f64, f64, klipp_timeline::TrackState)> = s.weak_spans().into_iter().filter(|w| w.1 >= from && w.0 <= to).collect();
        let edit = EditCommand::ReplaceClip { description: "Correct Surface", old: Box::new(old.clone()), new: Box::new(new) };
        self.apply_edits_as_one(std::slice::from_ref(&edit), "Correct Surface");
        let on_timeline = |src: f64| old.timeline_in.to_secs_f64() + old.elapsed_for_source_secs(src, fps);
        let mut note = String::new();
        for (state, said) in [(klipp_timeline::TrackState::Low, "uncertain"), (klipp_timeline::TrackState::Lost, "lost")] {
            let these: Vec<String> = weak.iter().filter(|w| w.2 == state).map(|w| format!("{:.2}–{:.2} s", on_timeline(w.0), on_timeline(w.1))).collect();
            if !these.is_empty() {
                note.push_str(&format!("; still {said} {}", these.join(", ")));
            }
        }
        self.state.toast.show(format!("Corrected \u{201c}{name}\u{201d} at {:.2} s, the track solved again from {:.2} to {:.2} s{note}", on_timeline(source_secs), on_timeline(from), on_timeline(to)));
    }
}

/// A key as a tracking job sends it, `(file seconds, map, confidence,
/// state)` with the state 0 good, 1 uncertain, 2 lost.
pub(super) fn surface_key_of((source_secs, map, confidence, state): (f64, [f64; 9], f32, u8)) -> klipp_timeline::SurfaceKey {
    let state = match state {
        0 => klipp_timeline::TrackState::Good,
        1 => klipp_timeline::TrackState::Low,
        _ => klipp_timeline::TrackState::Lost,
    };
    klipp_timeline::SurfaceKey { source_secs, map, confidence: Some(confidence), state }
}
