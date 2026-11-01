//! The 8-bit banding suggestion (#2074) in the export dialog and on the
//! render queue's jobs as the control socket reads them: a few frames of
//! the sequence rendered small on a thread of their own, once per edit,
//! and only while no render runs, so an export never shares the machine
//! with it. A job queued before its sequence was checked is checked once
//! the queue is idle. It suggests; it changes no setting.

use super::*;

/// The sequence, as of an edit, a check ran or runs for: its document
/// revision and which sequence is active.
pub(super) type BandingKey = (klipp_timeline::SequenceRevision, usize);

/// One check at a time and what the last one found, for the sequence as
/// of `K`.
pub(super) struct BandingProbe<K = BandingKey> {
    key: Option<K>,
    /// Whether the sequence at `key` bands at 8 bits, once known.
    found: Option<bool>,
    /// The check running: its answer, `None` once called off, and what it
    /// renders and the flag that stops it, to call it off and run it again.
    running: Option<Running<K>>,
    /// The jobs queued and the sequence each renders.
    jobs: Vec<(uuid::Uuid, K)>,
    /// What waits to be checked once no render runs: a job queued
    /// unchecked, or a check a render called off.
    deferred: Option<(K, klipp_render::SequenceSnapshot)>,
}

struct Running<K> {
    key: K,
    answer: std::sync::mpsc::Receiver<Option<bool>>,
    snapshot: klipp_render::SequenceSnapshot,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<K> Default for BandingProbe<K> {
    fn default() -> Self {
        Self { key: None, found: None, running: None, jobs: Vec::new(), deferred: None }
    }
}

impl<K: Copy + PartialEq> BandingProbe<K> {
    /// Starts a check of the sequence at `key` unless one ran, or is
    /// running, for it, or a render runs (`idle` false): asked again
    /// later. `snapshot` makes what it renders, and is only called to
    /// start one.
    pub(super) fn want(&mut self, key: K, idle: bool, snapshot: impl FnOnce() -> Option<klipp_render::SequenceSnapshot>) {
        if self.key == Some(key) || !idle {
            return;
        }
        self.start(key, snapshot());
    }

    /// Keeps `snapshot`, which a job queued for the sequence at `key`
    /// renders, to check when no render runs, unless it was checked.
    pub(super) fn defer(&mut self, key: K, snapshot: klipp_render::SequenceSnapshot) {
        if self.key != Some(key) {
            self.deferred = Some((key, snapshot));
        }
    }

    fn start(&mut self, key: K, snapshot: Option<klipp_render::SequenceSnapshot>) {
        // Asked once per edit, whatever comes of it: nothing to render, or
        // no thread, is no suggestion, and isn't tried again every frame.
        (self.key, self.found, self.running) = (Some(key), Some(false), None);
        if self.deferred.as_ref().is_some_and(|(k, _)| *k == key) {
            self.deferred = None;
        }
        let Some(snapshot) = snapshot else { return };
        let (tx, answer) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (snap, halt) = (snapshot.clone(), stop.clone());
        let spawned = std::thread::Builder::new().name("banding check".into()).spawn(move || {
            let _ = tx.send(klipp_render::banding::sample_unless(&snap, &halt).ok().map(|b| b.is_some()));
        });
        if spawned.is_ok() {
            self.found = None;
            self.running = Some(Running { key, answer, snapshot, stop });
        }
    }

    /// Takes a finished check's answer. A render running (`idle` false)
    /// calls off a check before its next frame, kept to run again; with
    /// none, the check that waits starts.
    pub(super) fn poll(&mut self, idle: bool) {
        if let Some(run) = &self.running {
            match run.answer.try_recv() {
                Ok(Some(bands)) => (self.found, self.running) = (Some(bands), None),
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self.running = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if !idle {
            if let Some(run) = self.running.take() {
                run.stop.store(true, std::sync::atomic::Ordering::Release);
                (self.key, self.found) = (None, None);
                self.deferred = Some((run.key, run.snapshot));
            }
            return;
        }
        if self.running.is_none() {
            if let Some((key, snap)) = self.deferred.take() {
                if self.key != Some(key) {
                    self.start(key, Some(snap));
                }
            }
        }
    }

    /// Whether the sequence at `key` bands at 8 bits: `None` while its
    /// check runs or waits, or when none ran for it.
    pub(super) fn bands(&self, key: K) -> Option<bool> {
        (self.key == Some(key)).then_some(self.found).flatten()
    }

    /// Remembers that job `id` renders the sequence at `key`.
    pub(super) fn queued(&mut self, id: uuid::Uuid, key: K) {
        self.jobs.retain(|(j, _)| *j != id);
        self.jobs.push((id, key));
    }

    /// What the check says of job `id` writing `settings`: `None` for a
    /// 10-bit target or a job it doesn't know, else "pending" until its
    /// check has run, then "bands" or "smooth".
    pub(super) fn state_for(&self, id: uuid::Uuid, settings: &klipp_core::ExportSettings) -> Option<&'static str> {
        let key = self.jobs.iter().find(|(j, _)| *j == id)?.1;
        if !settings.video_codec.writes_8bit(settings.bit_depth) {
            return None;
        }
        Some(match self.bands(key) {
            None => "pending",
            Some(true) => "bands",
            Some(false) => "smooth",
        })
    }

    /// The suggestion for job `id` writing `settings`: an 8-bit target
    /// of a sequence found to band.
    pub(super) fn note_for(&self, id: uuid::Uuid, settings: &klipp_core::ExportSettings) -> Option<String> {
        (self.state_for(id, settings) == Some("bands")).then(|| klipp_render::banding::SUGGESTION.to_string())
    }

    /// The suggestion for the dialog's target, the sequence at `key`.
    pub(super) fn hint_for(&self, key: K, codec: klipp_core::ExportVideoCodec, bit_depth: u8) -> Option<&'static str> {
        (codec.writes_8bit(bit_depth) && self.bands(key) == Some(true)).then_some(klipp_render::banding::SUGGESTION)
    }
}

impl KlippApp {
    /// The active sequence as of its last edit.
    pub(super) fn banding_key(&self) -> BandingKey {
        (self.sequence.revision(), self.state.active_sequence_idx)
    }

    /// Whether no render runs or waits: the check may have the machine.
    pub(super) fn renders_idle(&self) -> bool {
        !self.state.render_queue.has_active_job() && self.state.render_queue.queued_count() == 0
    }

    /// Starts the check of the active sequence for `settings` unless it
    /// ran for this edit, or a render runs.
    pub(super) fn want_banding_check(&mut self, settings: &klipp_core::ExportSettings) {
        let key = self.banding_key();
        let idle = self.renders_idle();
        let state = &self.state;
        self.export_dialog.banding.want(key, idle, || {
            klipp_export::RenderSnapshot::from_sequence_with_all(
                &state.sequences[state.active_sequence_idx],
                &state.sequences,
                &state.media_pool,
                settings,
                state.timeline_lut(),
                false,
                state.project_working_colorspace,
            )
            .ok()
            .map(|s| s.sequence_snapshot)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eight() -> klipp_core::ExportSettings {
        klipp_core::ExportSettings { video_codec: klipp_core::ExportVideoCodec::H264, bit_depth: 8, ..Default::default() }
    }

    fn empty() -> klipp_render::SequenceSnapshot {
        klipp_render::SequenceSnapshot::from_sequence(&klipp_timeline::Sequence::new_default(), &Default::default(), &Default::default(), None, false, Default::default()).unwrap()
    }

    /// A check running for `key` that answers when `tx` sends.
    fn running(p: &mut BandingProbe<(u64, usize)>, key: (u64, usize)) -> (std::sync::mpsc::Sender<Option<bool>>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let (tx, answer) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (p.key, p.found) = (Some(key), None);
        p.running = Some(Running { key, answer, snapshot: empty(), stop: stop.clone() });
        (tx, stop)
    }

    /// #3255: what an export renders with, taken from the app as every
    /// export takes it. The project's viewing LUT never gets in, on its
    /// own or beside the Timeline LUT; the Timeline LUT does while on.
    #[test]
    fn the_viewing_lut_never_reaches_an_export() {
        let mut state = crate::state::AppState::default();
        let export_lut = |state: &crate::state::AppState| {
            klipp_export::RenderSnapshot::from_sequence_with_all(state.sequence(), &state.sequences, &state.media_pool, &eight(), state.timeline_lut(), false, state.project_working_colorspace)
                .unwrap()
                .sequence_snapshot
                .global_lut_path
        };
        state.project_viewing_lut = Some("/luts/monitor.cube".into());
        assert_eq!(export_lut(&state), None, "a viewing LUT alone");
        state.global_lut_path = Some("/luts/timeline.cube".into());
        state.global_lut_enabled = false;
        assert_eq!(export_lut(&state), None, "the Timeline LUT switched off");
        state.global_lut_enabled = true;
        assert_eq!(export_lut(&state), Some("/luts/timeline.cube".into()), "the Timeline LUT, not the viewing one");
    }

    /// A check's answer reaches the dialog and the jobs of its sequence,
    /// for an 8-bit target only, and not a later edit's: that one is
    /// checked again. Until it answers, a job's check is pending.
    #[test]
    fn a_found_banding_reaches_the_dialog_and_the_jobs_of_its_edit() {
        let mut p = BandingProbe::<(u64, usize)>::default();
        let (tx, _) = running(&mut p, (3, 0));
        let job = uuid::Uuid::new_v4();
        p.queued(job, (3, 0));
        assert_eq!(p.bands((3, 0)), None, "still running");
        assert_eq!((p.state_for(job, &eight()), p.note_for(job, &eight())), (Some("pending"), None));
        tx.send(Some(true)).unwrap();
        p.poll(true);
        assert_eq!(p.bands((3, 0)), Some(true));
        assert_eq!(p.state_for(job, &eight()), Some("bands"));
        assert_eq!(p.note_for(job, &eight()).as_deref(), Some(klipp_render::banding::SUGGESTION));
        let ten = klipp_core::ExportSettings { bit_depth: 10, ..eight() };
        assert_eq!((p.state_for(job, &ten), p.note_for(job, &ten)), (None, None), "a 10-bit target");
        assert_eq!(p.hint_for((3, 0), klipp_core::ExportVideoCodec::H264, 0), Some(klipp_render::banding::SUGGESTION));
        assert_eq!(p.hint_for((3, 0), klipp_core::ExportVideoCodec::ProRes422, 0), None);
        assert_eq!(p.hint_for((4, 0), klipp_core::ExportVideoCodec::H264, 0), None, "another edit");
        assert_eq!(p.note_for(uuid::Uuid::new_v4(), &eight()), None, "a job it doesn't know");
    }

    /// #2074: a render that starts calls off the check running, before
    /// its next frame; it waits, and runs again once no render runs.
    #[test]
    fn a_render_starting_calls_off_a_running_check() {
        let mut p = BandingProbe::<(u64, usize)>::default();
        let (_tx, stop) = running(&mut p, (7, 0));
        p.poll(false);
        assert!(stop.load(std::sync::atomic::Ordering::Acquire), "called off");
        assert!(p.running.is_none() && p.bands((7, 0)).is_none());
        assert_eq!(p.deferred.as_ref().map(|d| d.0), Some((7, 0)), "kept to run again");
        p.poll(false);
        assert!(p.running.is_none(), "not while the render runs");
        p.poll(true);
        assert_eq!(p.key, Some((7, 0)), "run again once idle");
        assert!(p.deferred.is_none());
    }

    /// A check runs once per edit: asked again for the same one, even one
    /// that had nothing to render, it makes no second snapshot.
    #[test]
    fn a_check_runs_once_per_edit() {
        let mut p = BandingProbe::<(u64, usize)>::default();
        let mut made = 0;
        p.want((1, 0), false, || { made += 1; None });
        assert_eq!((made, p.bands((1, 0))), (0, None), "a render runs: not now");
        p.want((1, 0), true, || { made += 1; None });
        assert_eq!((made, p.bands((1, 0))), (1, Some(false)), "nothing to render: no suggestion");
        p.want((1, 0), true, || { made += 1; None });
        assert_eq!(made, 1, "the same edit: not again");
        p.want((2, 0), true, || { made += 1; None });
        assert_eq!(made, 2, "a new edit");
    }

    /// A job queued unchecked leaves its snapshot; the check starts only
    /// once no render runs, and not for an edit already checked.
    #[test]
    fn a_queued_jobs_check_waits_for_the_queue_to_be_idle() {
        let mut p = BandingProbe::<(u64, usize)>::default();
        p.defer((5, 0), empty());
        p.poll(false);
        assert!(p.key.is_none() && p.running.is_none(), "a render runs: not started");
        p.poll(true);
        assert_eq!(p.key, Some((5, 0)), "idle: started");
        assert!(p.deferred.is_none());
        p.defer((5, 0), empty());
        assert!(p.deferred.is_none(), "already checked: nothing kept");
    }
}
