//! Track Subject (#239): a clip's grade gated by a matte that follows the
//! subject, made in the background by the subject matte pass over every
//! frame the render reads; its progress on the External Matte card and in
//! the status bar, and its frames shown as they come.

use klipp_analysis::subject_matte::{self, MatteModel};
use klipp_timeline::GradeAccess;

use super::*;

/// None means continue only: no correction or alpha binding is created.
fn bind_requested_subject(graph: &mut klipp_nodes::NodeGraph, model: klipp_nodes::SubjectMatte, chosen: Option<klipp_nodes::NodeId>, into: Option<klipp_core::SubjectMatteUse>) {
    if let Some(target) = into {
        bind_subject(graph, model, chosen, target == klipp_core::SubjectMatteUse::CutOut);
    }
}

pub(super) fn restore_clip_background(state: &mut AppState, clip_id: klipp_core::ClipId) -> Result<(), String> {
    let clip = state.sequence().all_clips().find(|c| c.id == clip_id).ok_or("clip not found")?;
    let old_graph = clip.grade().clone();
    let matte = old_graph.cut_out_matte().ok_or("This clip is not cut out by a subject or image matte")?;
    if !matches!(super::colour::edit_clip_graph(state, clip_id, &[], false, |graph| { graph.restore_background(); }), Some(Ok(()))) {
        return Err("The clip's grade could not be edited".into());
    }
    let new_graph = state.sequence().all_clips().find(|c| c.id==clip_id).unwrap().grade().clone();
    if state.selected_node_id == Some(matte) && !new_graph.nodes.iter().any(|n|n.id==matte) { state.selected_node_id=None; }
    state.push_history(crate::history::EditCommand::Named { label:"Restore Background".into(), edit:Box::new(crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph }) });
    state.dirty=true;
    Ok(())
}

/// The clip's opacity faded across the frame by `mask`, or that taken off
/// (#2704), as one undo step.
pub(super) fn set_clip_opacity_gradient(state: &mut AppState, clip_id: klipp_core::ClipId, mask: Option<klipp_core::Mask>) -> Result<(), String> {
    use klipp_nodes::opacity_gradient::OpacityGradientError as E;
    let clip = state.sequence().all_clips().find(|c| c.id == clip_id).ok_or("clip not found")?;
    let old_graph = clip.grade().clone();
    let mut new_graph = old_graph.clone();
    let label = if mask.is_some() { "Opacity Gradient" } else { "Remove Opacity Gradient" };
    new_graph.set_opacity_gradient(mask).map_err(|e| match e {
        E::AlphaTaken(name) => format!("The clip's opacity is already set by {name}"),
        E::NoneToClear => "The clip has no opacity gradient".to_string(),
    })?;
    let graph = new_graph.clone();
    if !matches!(super::colour::edit_clip_graph(state, clip_id, &[], false, move |g| { *g = graph; }), Some(Ok(()))) {
        return Err("The clip's grade could not be edited".into());
    }
    // Its handles in the viewer, ready to drag.
    state.selected_node_id = new_graph.opacity_gradient();
    state.push_history(crate::history::EditCommand::Named { label: label.into(), edit: Box::new(crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph }) });
    state.dirty = true;
    Ok(())
}

/// How often a running pass is looked at for new frames to show.
const LOOK_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

fn model_of(name: &str) -> MatteModel {
    if name == "foreground" { MatteModel::Foreground } else { MatteModel::Subject }
}

impl KlippApp {
    pub(super) fn handle_track_subject_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::TrackSubject { clip_id, model, into } => {
                self.track_subject(*clip_id, model, *into, needs_refresh);
                true
            }
            AppCommand::RestoreBackground { clip_id } => {
                match restore_clip_background(&mut self.state, *clip_id) {
                    Ok(()) => { *needs_refresh = true; self.refresh_track_cards(); }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::SetOpacityGradient { clip_id, mask } => {
                match set_clip_opacity_gradient(&mut self.state, *clip_id, mask.as_deref().cloned()) {
                    Ok(()) => { *needs_refresh = true; self.refresh_track_cards(); }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::TrackTransitionSubject { transition_id } => {
                self.track_transition_subject(*transition_id);
                true
            }
            AppCommand::CancelTrackSubject { clip_id } => {
                if let Some(t) = self.state.tracking_subjects.get(clip_id) {
                    if let Err(why) = klipp_analysis::service::cancel_subject_matte(t.media) {
                        self.state.toast.show(why);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Start tracking the subject through `clip_id`: the model checked
    /// (its download asked for, never begun, when it isn't there), the
    /// matte added to the grade as one undo step, the pass started.
    fn track_subject(&mut self, clip_id: klipp_core::ClipId, model_name: &str, into: Option<klipp_core::SubjectMatteUse>, needs_refresh: &mut bool) {
        let model = model_of(model_name);
        let seq = self.state.sequence();
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let times = klipp_compose::subject_matte::source_times_held(seq, &clip, last_frame_of(&self.state.media_pool, &clip));
        let Some(mut item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)).cloned() else {
            self.state.toast.show("Track Subject needs a clip with a picture from a file");
            return;
        };
        // Made upright by the turn the clip is decoded by, as it is read.
        item.source_rotation = klipp_compose::subject_matte::decode_rotation(&clip, item.source_rotation);
        if item.is_still || item.video_codec.is_empty() {
            self.state.toast.show("Track Subject needs a video clip");
            return;
        }
        let Some((model_id, weights)) = subject_matte::model_path(model) else {
            // The download is offered, not begun; the track starts once
            // the model is there (`tick_track_subject`).
            self.state.pending_track_subject = Some((AppCommand::TrackSubject { clip_id, model: model_name.to_string(), into }, model_name.to_string()));
            let id = *model.ids().last().expect("a model id");
            self.request_model_download(id);
            return;
        };
        let bytes = std::fs::metadata(&weights).map(|m| m.len()).unwrap_or(0);
        let Some(dir) = subject_matte::sequence_dir(&item, model_id, bytes, item.source_rotation) else {
            self.state.toast.show(format!("{} isn't readable", item.filename));
            return;
        };
        let (from, to) = times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
        if from > to {
            return;
        }
        let fps = item.fps.as_f64().max(1.0);
        // The matte on the grade, once, by model: gating the first
        // correction, or cutting the clip out (#73), one node for both.
        // Bound by the clip's media and the model, it follows a relink
        // (#1881).
        let subject = if model == MatteModel::Foreground { klipp_nodes::SubjectMatte::Foreground } else { klipp_nodes::SubjectMatte::Subject };
        if let Some(cut_out) = into.map(|target| target == klipp_core::SubjectMatteUse::CutOut) {
            // Continuing a card's pass never enters this binding path.
            if let Some(why) = cut_out.then(|| alpha_taken(clip.grade(), subject)).flatten() {
                self.state.toast.show(why);
                return;
            }
            if !subject_bound(clip.grade(), subject, cut_out) {
                let chosen = self.state.selected_node_id;
                let edited = super::colour::edit_clip_graph(&mut self.state, clip_id, &[], true, move |graph| bind_requested_subject(graph, subject, chosen, into));
                if matches!(edited, Some(Ok(()))) { *needs_refresh = true; }
            }
        }
        self.start_subject_pass(clip_id, &item, model, dir, from, to + 1.0 / fps);
    }

    /// The matte pass over `from`..`to` of `item`, its frames going to
    /// `dir`, watched for `clip_id`'s sake.
    fn start_subject_pass(&mut self, clip_id: klipp_core::ClipId, item: &klipp_media::MediaItem, model: MatteModel, dir: std::path::PathBuf, from: f64, to: f64) {
        let status = klipp_analysis::service::request_subject_matte(item, model, None, from, to);
        if matches!(status.state, klipp_analysis::service::State::Running | klipp_analysis::service::State::Queued) {
            let seen = klipp_nodes::matte_sequence::frames(&dir);
            self.state.tracking_subjects.insert(clip_id, crate::state::Tracking { media: item.id, folder: dir, seen, started: std::time::Instant::now(), name: item.filename.clone() });
        }
    }

    /// Track the subject a piece transition cuts round (#985 phase 3):
    /// the pass over the frames of its subject side it reads, nothing
    /// bound to the grade. The model's download is offered, not begun.
    fn track_transition_subject(&mut self, transition_id: klipp_core::TransitionId) {
        let seq = self.state.sequence();
        let pool = &self.state.media_pool;
        let Some(ts) = klipp_compose::subject_matte::transition_subjects_with(seq, |c| last_frame_of(pool, c)).into_iter().find(|t| t.transition == transition_id) else {
            self.state.toast.show("Choose how the subject moves in this transition first");
            return;
        };
        let fps_seq = seq.fps.as_f64();
        let Some(clip) = seq.all_clips().find(|c| c.id == ts.clip).cloned() else { return };
        let name = if ts.model == 1 { "foreground" } else { "subject" };
        let model = model_of(name);
        let Some(mut item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)).cloned() else {
            self.state.toast.show("The subject needs a clip with a picture from a file");
            return;
        };
        item.source_rotation = klipp_compose::subject_matte::decode_rotation(&clip, item.source_rotation);
        if item.is_still || item.video_codec.is_empty() {
            self.state.toast.show("The subject needs a video clip");
            return;
        }
        let Some((model_id, weights)) = subject_matte::model_path(model) else {
            self.state.pending_track_subject = Some((AppCommand::TrackTransitionSubject { transition_id }, name.to_string()));
            let id = *model.ids().last().expect("a model id");
            self.request_model_download(id);
            return;
        };
        let bytes = std::fs::metadata(&weights).map(|m| m.len()).unwrap_or(0);
        let Some(dir) = subject_matte::sequence_dir(&item, model_id, bytes, item.source_rotation) else {
            self.state.toast.show(format!("{} isn't readable", item.filename));
            return;
        };
        let (from, to) = ts.times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
        if from > to {
            return;
        }
        let frame = 1.0 / item.fps.as_f64().max(fps_seq).max(1.0);
        self.start_subject_pass(clip.id, &item, model, dir, from, to + frame);
    }

    /// Each moment: a Track Subject waiting on its model started once the
    /// model is there (dropped if the person declined it); running passes
    /// looked at, their new frames shown and their cards and the status
    /// bar's line brought up to date.
    pub(super) fn tick_track_subject(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.state.ai_tools_state.pending_restore_background) {
            if let Some(clip_id) = self.state.selected_clip { self.send_commands(vec![AppCommand::RestoreBackground { clip_id }]); }
        }
        if std::mem::take(&mut self.state.ai_tools_state.pending_track_subject) {
            // The tile's Cut Out asks for the clip cut out round it (#73).
            let cut_out = std::mem::take(&mut self.state.ai_tools_state.pending_cut_out);
            match self.state.selected_clip {
                Some(clip_id) => self.send_commands(vec![AppCommand::TrackSubject { clip_id, model: "subject".into(), into: Some(if cut_out { klipp_core::SubjectMatteUse::CutOut } else { klipp_core::SubjectMatteUse::Grade }) }]),
                None => self.state.toast.show("Select a clip to track its subject"),
            }
        }
        if let Some((cmd, model)) = self.state.pending_track_subject.clone() {
            if subject_matte::model_path(model_of(&model)).is_some() {
                self.state.pending_track_subject = None;
                self.send_commands(vec![cmd]);
            } else if matches!(self.state.model_download, crate::model_download::ModelDownloadState::Idle | crate::model_download::ModelDownloadState::Failed { .. }) {
                self.state.pending_track_subject = None;
            }
        }
        if self.state.track_subject_looked.is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return;
        }
        self.state.track_subject_looked = Some(std::time::Instant::now());
        self.refresh_track_cards();
        let mut finished = Vec::new();
        let mut lines = Vec::new();
        let ids: Vec<_> = self.state.tracking_subjects.keys().copied().collect();
        for clip_id in ids {
            let Some(t) = self.state.tracking_subjects.get(&clip_id).cloned() else { continue };
            let status = klipp_analysis::service::subject_matte_status(t.media);
            let now = klipp_nodes::matte_sequence::frames(&t.folder);
            if now.len() != t.seen.len() {
                // Only the new frames' place in this clip is drawn again.
                let new: Vec<i64> = now.iter().filter(|k| t.seen.binary_search(k).is_err()).copied().collect();
                if let Some(t) = self.state.tracking_subjects.get_mut(&clip_id) {
                    t.seen = now;
                }
                self.redraw_tracked(clip_id, &new);
            }
            match status.as_ref().map(|s| &s.state) {
                Some(klipp_analysis::service::State::Running | klipp_analysis::service::State::Queued) => {
                    let p = status.as_ref().and_then(|s| s.progress).unwrap_or(0.0);
                    lines.push(tracking_line(&t.name, p, t.started.elapsed()));
                }
                _ => {
                    finished.push(clip_id);
                    // One toast, at the end.
                    if let Some(s) = status {
                        match s.state {
                            klipp_analysis::service::State::Failed => {
                                let why = s.failed.first().map(|f| f.1.clone()).unwrap_or_default();
                                self.state.toast.show(format!("Tracking the subject in \u{201c}{}\u{201d} stopped: {why}", s.filename));
                            }
                            klipp_analysis::service::State::Done => self.state.toast.show(format!("Subject tracked in \u{201c}{}\u{201d}", s.filename)),
                            klipp_analysis::service::State::Cancelled => self.state.toast.show(format!("Tracking stopped in \u{201c}{}\u{201d}; the frames made are kept", s.filename)),
                            _ => {}
                        }
                    }
                }
            }
        }
        for id in finished {
            self.state.tracking_subjects.remove(&id);
        }
        self.state.status_tracking = lines;
        if !self.state.tracking_subjects.is_empty() {
            ctx.request_repaint_after(LOOK_EVERY);
        }
    }

    /// The frames of `clip_id` that show the source frames `new` (keys)
    /// dropped from the render cache, and the picture drawn again when
    /// the playhead is on one: nothing else's cached frames are touched.
    fn redraw_tracked(&mut self, clip_id: klipp_core::ClipId, new: &[i64]) {
        let (Some(lo), Some(hi)) = (new.iter().min().copied(), new.iter().max().copied()) else { return };
        let (range, playhead_in) = {
            let seq = self.state.sequence();
            let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return };
            let fps = seq.fps.as_f64();
            let half = (500_000.0 / fps.max(1.0)) as i64;
            let step = klipp_core::RationalTime::from_frames(1, seq.fps.rate());
            let (mut first, mut last) = (None, None);
            // Its transitions' frames too (#985 phase 3).
            let (start, end) = klipp_compose::subject_matte::span(seq, clip);
            let mut t = start;
            while t < end {
                let k = klipp_nodes::matte_sequence::key_of(klipp_compose::render::source_secs(clip, t, fps));
                if k >= lo - half && k <= hi + half {
                    first.get_or_insert(t);
                    last = Some(t + step);
                }
                t += step;
            }
            let Some(range) = first.zip(last) else { return };
            let playhead = self.state.playhead;
            (range, playhead >= range.0 && playhead < range.1)
        };
        let _ = self.cmd_tx.send(AppCommand::ClearRenderCache { from: range.0, to: range.1 });
        if playhead_in {
            self.sync_and_refresh_frame();
        }
    }

    /// The cards of the tracked mattes on the selected clip: their
    /// coverage of what the render reads, and their pass's progress.
    fn refresh_track_cards(&mut self) {
        if let Some((_, tid)) = self.state.selected_transition {
            self.refresh_transition_card(tid);
        }
        let Some(clip_id) = self.state.selected_clip else { return };
        let seq = self.state.sequence();
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return };
        // Bound by path, and by the clip's media and a model (#1881): the
        // latter found from the media as linked now.
        let mut folders: Vec<(std::path::PathBuf, Option<u32>)> = klipp_compose::subject_matte::sequences_of(clip).into_iter().map(|d| (d, None)).collect();
        let item = clip.media_id().and_then(|m| self.state.media_pool.get(&m));
        for model in klipp_compose::subject_matte::subject_models_of(clip) {
            let dir = item.and_then(|i| klipp_compose::subject_matte::media_dir(&i.path, klipp_compose::subject_matte::decode_rotation(clip, i.source_rotation), model)).unwrap_or_default();
            folders.push((dir, Some(model)));
        }
        if folders.is_empty() {
            return;
        }
        let times = klipp_compose::subject_matte::source_times_held(seq, clip, last_frame_of(&self.state.media_pool, clip));
        let running = self.state.tracking_subjects.get(&clip_id).and_then(|t| klipp_analysis::service::subject_matte_status(t.media)).filter(|s| matches!(s.state, klipp_analysis::service::State::Running | klipp_analysis::service::State::Queued));
        for (dir, subject) in folders {
            let (covered, needed) = match (klipp_compose::subject_matte::coverage(&dir, &times), subject) {
                (Some(c), _) => c,
                // Not made for the media as linked now: none of it.
                (None, Some(_)) => (0, times.len()),
                (None, None) => continue,
            };
            let manifest = klipp_nodes::matte_sequence::read_manifest(&dir);
            let lost = if covered > 0 { lost_frames(&dir) } else { 0 };
            // Which correction it gates, by its name.
            let graph = clip.grade();
            let is_it = |n: &&klipp_nodes::graph::Node| n.kind == klipp_nodes::NodeKind::ExternalMatte && match subject {
                Some(m) => n.params.matte_subject.is_some_and(|s| s.index() as u32 == m),
                None => n.params.matte_image_path.as_deref() == Some(dir.as_path()),
            };
            // A correction it gates, by its name, and whether it is the
            // alpha the grade leaves with: it cuts the clip out (#73).
            let matte = graph.nodes.iter().find(is_it).map(|n| n.id);
            let cuts_out = matte.is_some() && graph.input_of(graph.output_node_id, 1) == matte;
            let gates = matte
                .and_then(|m| graph.edges.iter().find(|e| e.from_node == m && e.to_node != graph.output_node_id))
                .and_then(|e| graph.nodes.iter().find(|n| n.id == e.to_node))
                .map(|n| if n.label.is_empty() { format!("{:?}", n.kind) } else { n.label.clone() });
            let card = crate::subject_track::TrackCard {
                gates,
                cuts_out,
                running: running.as_ref().filter(|s| s.artifact.as_deref().is_none_or(|a| std::path::Path::new(a) == dir)).map(|s| s.progress.unwrap_or(0.0)),
                covered,
                needed,
                lost,
                model: match subject {
                    Some(m) => if m == 1 { "foreground" } else { "subject" },
                    None => manifest.as_ref().map_or("subject", |m| crate::subject_track::model_name(&m.model)),
                },
            };
            match subject {
                Some(m) => crate::subject_track::set_subject_card(clip_id, m, card),
                None => crate::subject_track::set_card(&dir, card),
            }
        }
    }
}

impl KlippApp {
    /// The card of the subject a piece transition cuts round (#985 phase
    /// 3): its matte's coverage of the frames it reads, and its pass.
    fn refresh_transition_card(&mut self, tid: klipp_core::TransitionId) {
        let seq = self.state.sequence();
        let pool = &self.state.media_pool;
        let Some(ts) = klipp_compose::subject_matte::transition_subjects_with(seq, |c| last_frame_of(pool, c)).into_iter().find(|t| t.transition == tid) else { return };
        let Some(clip) = seq.all_clips().find(|c| c.id == ts.clip) else { return };
        let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)) else { return };
        let dir = klipp_compose::subject_matte::media_dir(&item.path, klipp_compose::subject_matte::decode_rotation(clip, item.source_rotation), ts.model);
        let (covered, needed) = dir.as_deref().and_then(|d| klipp_compose::subject_matte::coverage(d, &ts.times)).unwrap_or((0, ts.times.len()));
        let lost = dir.as_deref().map_or(0, lost_frames);
        let running = self
            .state
            .tracking_subjects
            .get(&ts.clip)
            .and_then(|t| klipp_analysis::service::subject_matte_status(t.media))
            .filter(|s| matches!(s.state, klipp_analysis::service::State::Running | klipp_analysis::service::State::Queued))
            .map(|s| s.progress.unwrap_or(0.0));
        crate::subject_track::set_transition_card(tid, crate::subject_track::TrackCard { running, covered, needed, lost, model: if ts.model == 1 { "foreground" } else { "subject" }, gates: None, cuts_out: false });
    }
}

/// The time of the last frame of `clip`'s media, where it is read past
/// its end (#985 phase 3).
fn last_frame_of(pool: &klipp_media::MediaPool, clip: &klipp_timeline::Clip) -> Option<f64> {
    let item = pool.get(&clip.media_id()?)?;
    Some(klipp_compose::subject_matte::last_frame_secs(item.duration.to_secs_f64(), item.fps.as_f64()))
}

/// The frames of the sequence in `dir` where the subject wasn't found:
/// published empty.
fn lost_frames(dir: &std::path::Path) -> usize {
    let Some(m) = klipp_nodes::matte_sequence::read_manifest(dir) else { return 0 };
    let raw = klipp_nodes::matte_sequence::raw_dir(dir);
    klipp_nodes::matte_sequence::frames(&raw).into_iter().filter(|k| klipp_nodes::matte_sequence::get(&raw, &m, *k).is_some_and(|a| a.iter().all(|v| *v == 0))).count()
}

/// `graph` with an External Matte named "Subject" reading the matte
/// sequence in `folder`, gating the correction `chosen` (the one picked in
/// the grade view), else the first, made in the same step if it has none;
/// a matte that gated it is replaced.
pub(super) fn bind_subject(graph: &mut klipp_nodes::NodeGraph, model: klipp_nodes::SubjectMatte, chosen: Option<klipp_nodes::NodeId>, cut_out: bool) {
    // One matte node per model, shared by the grade's gate and the cut
    // out (#73). Two readers of it cost nothing more on the GPU: an
    // External Matte is read from disk on the CPU, never drawn into a
    // GPU matte slot, so it takes no slot path a second reader would lose.
    let matte = match subject_node(graph, model) {
        Some(id) => id,
        None => {
            let id = klipp_nodes::NodeId::new_v4();
            // Bound by the clip's media and the model, its folder found at
            // render from the media as linked now (#1881).
            let params = klipp_nodes::NodeParams { matte_subject: Some(model), ..Default::default() };
            graph.nodes.push(klipp_nodes::graph::Node {
                id,
                kind: klipp_nodes::NodeKind::ExternalMatte,
                label: "Subject".into(),
                enabled: true,
                params,
                keyframes: Default::default(),
                output_link: None,
                input_links: Vec::new(),
            });
            id
        }
    };
    // Cut out: the matte is the alpha the grade leaves with (#73, #898),
    // a matte already there replaced.
    let target = if cut_out {
        graph.output_node_id
    } else {
        let is_correction = |n: &&klipp_nodes::graph::Node| n.kind == klipp_nodes::NodeKind::Corrector;
        // Unchosen, never an outside node (#898): its gate takes its key
        // inverted, so the subject would grade everywhere but the subject.
        let inside = |n: &&klipp_nodes::graph::Node| is_correction(n) && n.params.key_invert != Some(true);
        chosen
            .and_then(|id| graph.nodes.iter().filter(is_correction).find(|n| n.id == id))
            .or_else(|| graph.nodes.iter().find(inside))
            .map(|n| n.id)
            // None to gate: a new one, never an outside node (which
            // `ensure_corrector` would hand back as the first Corrector).
            .unwrap_or_else(|| graph.insert_corrector(klipp_nodes::NodeId::new_v4()))
    };
    graph.edges.retain(|e| !(e.to_node == target && e.to_port == 1));
    graph.edges.push(klipp_nodes::graph::Edge { from_node: matte, from_port: 0, to_node: target, to_port: 1 });
}

/// Why `graph`'s Output alpha can't take the subject `model`'s matte, if
/// it can't: another matte is already wired there (a key mixer, a window),
/// named so the person can unwire it; the subject's own is no refusal.
pub(super) fn alpha_taken(graph: &klipp_nodes::NodeGraph, model: klipp_nodes::SubjectMatte) -> Option<String> {
    let there = graph.input_of(graph.output_node_id, 1)?;
    if subject_node(graph, model) == Some(there) {
        return None;
    }
    let node = graph.nodes.iter().find(|n| n.id == there)?;
    let name = if node.label.is_empty() { node.kind.display_name().to_string() } else { node.label.clone() };
    if graph.cut_out_matte().is_some() {
        return Some(format!("The grade's alpha already takes \"{name}\": use Restore Background first to cut the clip out by another subject"));
    }
    Some(format!("The grade's alpha already takes \u{201c}{name}\u{201d}: unwire it from the Output first to cut the clip out by the subject"))
}

/// The External Matte bound to the tracked subject `model` in `graph`.
fn subject_node(graph: &klipp_nodes::NodeGraph, model: klipp_nodes::SubjectMatte) -> Option<klipp_nodes::NodeId> {
    graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::ExternalMatte && n.params.matte_subject == Some(model)).map(|n| n.id)
}

/// Whether `graph`'s subject matte for `model` already does what is
/// asked: cuts the clip out (`cut_out`), or gates a correction.
pub(super) fn subject_bound(graph: &klipp_nodes::NodeGraph, model: klipp_nodes::SubjectMatte, cut_out: bool) -> bool {
    let Some(matte) = subject_node(graph, model) else { return false };
    if cut_out {
        return graph.input_of(graph.output_node_id, 1) == Some(matte);
    }
    graph.edges.iter().any(|e| e.from_node == matte && e.to_port == 1 && graph.nodes.iter().any(|n| n.id == e.to_node && n.kind == klipp_nodes::NodeKind::Corrector))
}

/// A running pass's status-bar line: the file, how far, and, once it has
/// gone far enough to say, about how long is left.
pub(super) fn tracking_line(name: &str, progress: f32, elapsed: std::time::Duration) -> String {
    let mut s = format!("Tracking subject \u{b7} {name} \u{b7} {:.0}%", progress * 100.0);
    if (0.05..1.0).contains(&progress) {
        let left = elapsed.as_secs_f64() * (1.0 - progress as f64) / progress as f64;
        let left = if left >= 90.0 { format!("about {:.0} min", (left / 60.0).round()) } else { format!("about {:.0} s", (left / 5.0).round().max(1.0) * 5.0) };
        s.push_str(&format!(" \u{b7} {left} left"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_background_then_continue_does_not_rebind_the_matte() {
        use klipp_nodes::SubjectMatte::Subject;
        let mut graph = klipp_nodes::NodeGraph::default();
        bind_requested_subject(&mut graph, Subject, None, Some(klipp_core::SubjectMatteUse::CutOut));
        assert!(graph.restore_background());
        let restored = graph.clone();
        bind_requested_subject(&mut graph, Subject, None, None);
        assert_eq!(graph, restored, "continuing adds neither a correction nor an alpha binding");
        bind_requested_subject(&mut graph, Subject, None, Some(klipp_core::SubjectMatteUse::Grade));
        assert_ne!(graph, restored, "an explicit Track Subject still binds the grade");
        assert!(subject_bound(&graph, Subject, false));
    }

    #[test]
    fn restore_background_is_one_undoable_edit() {
        use klipp_nodes::SubjectMatte::Subject;
        for shared in [false, true] {
            let mut state = AppState::default();
            let t = klipp_core::RationalTime::from_secs_f64;
            let mut clip = klipp_timeline::Clip::gap(t(0.0, 24), t(4.0, 24));
            let id = clip.id;
            let mut graph = klipp_nodes::NodeGraph::default();
            bind_subject(&mut graph, Subject, None, true);
            if shared { bind_subject(&mut graph, Subject, None, false); }
            let matte = graph.cut_out_matte().unwrap();
            clip.set_grade(graph.clone());
            let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
            track.clips.push(clip);
            state.sequences[0].tracks = vec![track];
            state.selected_node_id = Some(matte);
            restore_clip_background(&mut state, id).unwrap();
            assert_eq!(state.undo_depth(), 1);
            let restored = state.sequence().all_clips().find(|c| c.id == id).unwrap().grade().clone();
            assert!(restored.cut_out_matte().is_none());
            assert_eq!(restored.nodes.iter().any(|n| n.id == matte), shared);
            assert_eq!(state.selected_node_id, shared.then_some(matte));
            for redo in [false, true] {
                let entry = if redo { state.take_redo() } else { state.take_undo() }.unwrap();
                let mut doc = klipp_document::apply::DocumentView {
                    sequences: &mut state.sequences, active: state.active_sequence_idx,
                    media_pool: &mut state.media_pool, fps: &mut state.fps,
                };
                if redo { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
                assert_eq!(state.sequence().all_clips().find(|c| c.id == id).unwrap().grade(), if redo { &restored } else { &graph });
            }
            assert!(restore_clip_background(&mut state, id).is_err());
            assert_eq!(state.undo_depth(), 1);
        }
    }

    /// The subject matte gates the first correction, replacing a matte
    /// that gated it, as Remove Background's still does.
    #[test]
    fn the_subject_gates_the_first_correction() {
        let mut g = klipp_nodes::NodeGraph::default();
        let corr = g.ensure_corrector();
        let old = klipp_nodes::NodeId::new_v4();
        g.edges.push(klipp_nodes::graph::Edge { from_node: old, from_port: 0, to_node: corr, to_port: 1 });
        bind_subject(&mut g, klipp_nodes::SubjectMatte::Subject, None, false);
        let matte = g.nodes.iter().find(|n| n.label == "Subject").unwrap();
        assert_eq!(matte.kind, klipp_nodes::NodeKind::ExternalMatte);
        // Bound by the model, not a folder's path (#1881).
        assert_eq!((matte.params.matte_subject, matte.params.matte_image_path.as_ref()), (Some(klipp_nodes::SubjectMatte::Subject), None));
        let into: Vec<_> = g.edges.iter().filter(|e| e.to_node == corr && e.to_port == 1).collect();
        assert_eq!(into.len(), 1);
        assert_eq!(into[0].from_node, matte.id);

        // Several: the one picked; none, one made.
        let mut g = klipp_nodes::NodeGraph::default();
        let first = g.ensure_corrector();
        let second = klipp_nodes::NodeId::new_v4();
        g.nodes.push(klipp_nodes::graph::Node { id: second, kind: klipp_nodes::NodeKind::Corrector, label: "Corrector 2".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        bind_subject(&mut g, klipp_nodes::SubjectMatte::Foreground, Some(second), false);
        assert!(g.edges.iter().any(|e| e.to_node == second && e.to_port == 1) && !g.edges.iter().any(|e| e.to_node == first && e.to_port == 1));
        let mut bare = klipp_nodes::NodeGraph::default();
        bare.nodes.retain(|n| n.kind != klipp_nodes::NodeKind::Corrector);
        bind_subject(&mut bare, klipp_nodes::SubjectMatte::Subject, None, false);
        assert!(bare.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Corrector), "a correction made for it");
    }

    /// Unchosen, the subject never gates an outside node (#898), whose gate
    /// takes its key inverted: the first correction that isn't one, or one
    /// made when every correction is.
    #[test]
    fn the_subject_skips_an_outside_node() {
        let mut g = klipp_nodes::NodeGraph::default();
        let corr = g.ensure_corrector();
        g.nodes.iter_mut().find(|n| n.id == corr).unwrap().params.key_invert = Some(true);
        let inside = klipp_nodes::NodeId::new_v4();
        g.nodes.push(klipp_nodes::graph::Node { id: inside, kind: klipp_nodes::NodeKind::Corrector, label: "Inside".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        bind_subject(&mut g, klipp_nodes::SubjectMatte::Subject, None, false);
        assert!(g.input_of(inside, 1).is_some() && g.input_of(corr, 1).is_none(), "the inside correction gated, not the outside one");
        // Chosen, it is the person's pick.
        bind_subject(&mut g, klipp_nodes::SubjectMatte::Subject, Some(corr), false);
        assert!(g.input_of(corr, 1).is_some());
        // The only correction an outside node: a new one is made for it.
        let mut only = klipp_nodes::NodeGraph::default();
        let outside = only.ensure_corrector();
        only.nodes.iter_mut().find(|n| n.id == outside).unwrap().params.key_invert = Some(true);
        bind_subject(&mut only, klipp_nodes::SubjectMatte::Subject, None, false);
        assert!(only.input_of(outside, 1).is_none(), "the outside node left ungated");
        let made: Vec<_> = only.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector && n.id != outside).collect();
        assert_eq!(made.len(), 1, "one made");
        assert!(only.input_of(made[0].id, 1).is_some(), "and gated by the subject");
    }

    /// #73: Cut Out wires the subject's matte into the Output's alpha and
    /// gates no correction; asked for the grade as well, the one matte
    /// node serves both, and each use is seen as bound.
    #[test]
    fn a_cut_out_feeds_the_outputs_alpha_and_shares_the_matte() {
        use klipp_nodes::SubjectMatte::Subject;
        let mut g = klipp_nodes::NodeGraph::default();
        let corr = g.ensure_corrector();
        assert!(!subject_bound(&g, Subject, true) && !subject_bound(&g, Subject, false));
        bind_subject(&mut g, Subject, None, true);
        let matte = subject_node(&g, Subject).expect("a subject matte");
        assert_eq!(g.input_of(g.output_node_id, 1), Some(matte), "the Output's alpha");
        assert!(!g.edges.iter().any(|e| e.to_node == corr && e.to_port == 1), "no correction gated");
        assert!(subject_bound(&g, Subject, true) && !subject_bound(&g, Subject, false));
        bind_subject(&mut g, Subject, None, false);
        assert_eq!(g.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::ExternalMatte).count(), 1, "one matte node for both");
        assert_eq!(g.input_of(corr, 1), Some(matte));
        assert_eq!(g.input_of(g.output_node_id, 1), Some(matte), "still the alpha");
        assert!(subject_bound(&g, Subject, true) && subject_bound(&g, Subject, false));
        assert_eq!(alpha_taken(&g, Subject), None, "the subject's own matte is no refusal");
    }

    /// #73: an Output alpha another matte already takes is refused by
    /// that matte's name, and nothing is rewired.
    #[test]
    fn a_cut_out_refuses_an_alpha_already_taken() {
        let mut g = klipp_nodes::NodeGraph::default();
        let mixer = klipp_nodes::NodeId::new_v4();
        g.nodes.push(klipp_nodes::graph::Node { id: mixer, kind: klipp_nodes::NodeKind::Mask, label: "Sky window".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        g.edges.push(klipp_nodes::graph::Edge { from_node: mixer, from_port: 0, to_node: g.output_node_id, to_port: 1 });
        let why = alpha_taken(&g, klipp_nodes::SubjectMatte::Subject).expect("refused");
        assert!(why.contains("Sky window"), "{why}");
        assert_eq!(g.input_of(g.output_node_id, 1), Some(mixer), "nothing rewired");
    }

    #[test]
    fn the_status_line_says_how_far_and_how_long() {
        assert_eq!(tracking_line("take.mov", 0.0, std::time::Duration::from_secs(1)), "Tracking subject \u{b7} take.mov \u{b7} 0%");
        assert_eq!(tracking_line("take.mov", 0.25, std::time::Duration::from_secs(30)), "Tracking subject \u{b7} take.mov \u{b7} 25% \u{b7} about 2 min left");
        assert_eq!(tracking_line("take.mov", 0.9, std::time::Duration::from_secs(90)), "Tracking subject \u{b7} take.mov \u{b7} 90% \u{b7} about 10 s left");
    }
}
