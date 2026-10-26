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
                self.state.surface_tool = Some(crate::state::SurfaceTool { clip_id, surface: Some(s.id), name: Some(s.name.clone()), corners, direction: way, drag: None });
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
    /// corners found are on the frame a track would start from.
    pub(super) fn start_surface_snap(&mut self, clip_id: klipp_core::ClipId, click: [f32; 2], at_secs: Option<f64>, then_track: bool, name: Option<String>) {
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
        let event_tx = self.proxy_event_tx.clone();
        self.state.surface_snaps_pending += 1;
        std::thread::spawn(move || {
            let picture = klipp_engine::SequentialFrameDecoder::open(&path, SURFACE_TRACK_SIDE)
                .and_then(|mut d| {
                    d.seek_to(source_secs)?;
                    d.next_frame()
                })
                .ok()
                .flatten();
            let corners = picture.and_then(|(rgba, w, h)| klipp_effects::surface_snap::snap_quad(&rgba, w as usize, h as usize, (point[0], point[1])));
            let _ = event_tx.send(klipp_core::AppEvent::SurfaceSnapped { clip_id, at_secs: at, corners, then_track, name });
        });
    }

    /// Snapped corners into the surface tool, or tracked from at once for
    /// the assistant; or, found nothing, said so.
    pub(super) fn apply_surface_snap(&mut self, clip_id: klipp_core::ClipId, at: f64, corners: Option<[[f32; 2]; 4]>, then_track: bool, name: Option<String>) {
        let Some(clip) = self.state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let Some(on_source) = corners else {
            self.state.toast.show("No surface with an edge all round there: drag the corners onto it instead");
            return;
        };
        let Some((shown, placed)) = self.clip_picture_at(&clip, at) else { return };
        let frame = [self.state.sequence().resolution.width, self.state.sequence().resolution.height];
        let on_frame = klipp_timeline::surface::source_quad_in_frame(&on_source, shown, frame, Some(placed));
        if then_track {
            self.send_commands(vec![AppCommand::TrackSurface { clip_id, corners: on_frame, at_secs: Some(at), name, surface: None, direction: 0, interactive: false }]);
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
                };
                surface_on_frame(self.state.sequence(), &clip, &middle, at, None)
            }
        };
        self.state.surface_tool = Some(crate::state::SurfaceTool { clip_id, surface, name: existing.map(|s| s.name.clone()), corners, direction: 0, drag: None });
        self.state.selected_clip = Some(clip_id);
    }

    /// Follow a surface in perspective both ways from its reference frame
    /// (#684), on a thread: the frames are read once, as for a window, and
    /// each followed frame's corners come back by file time.
    pub(super) fn start_surface_track(&mut self, job: SurfaceTrackJob) {
        let SurfaceTrackJob { clip_id, surface, name, path, media_fps, frames: (first, last), reference, corners, pixels, direction, interactive, cancel } = job;
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
            let quad_px = corners.map(|c| (c[0] as f64 * gw, c[1] as f64 * gh));
            // The whole surface aligned frame to frame (#2754), its own
            // outline included, each frame with its confidence.
            let opts = klipp_effects::planar::Options::default();
            let forward: Vec<usize> = if direction >= 0 { (reference + 1..=last).collect() } else { Vec::new() };
            let backward: Vec<usize> = if direction <= 0 { (first..reference).rev().collect() } else { Vec::new() };
            let share = (forward.len() as f32 / (forward.len() + backward.len()).max(1) as f32).max(0.01);
            let report = |p: f32| going() && event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: loading + p * (1.0 - loading) }).is_ok();
            let ahead = klipp_effects::planar::track(&reference_frame, quad_px, &forward, &mut |f| decode(f), &opts, &|p| report(p * share));
            let behind = klipp_effects::planar::track(&reference_frame, quad_px, &backward, &mut |f| decode(f), &opts, &|p| report(share + p * (1.0 - share)));
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
            let _ = event_tx.send(klipp_core::AppEvent::SurfaceTracked { clip_id, surface, name, corners, reference_secs: reference as f64 / media_fps, keys, source_size, media_fps, held_secs, direction, interactive });
        });
    }
}
