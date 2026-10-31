//! Frozen tracks (#3207): their frames drawn ahead into the project's
//! cache, a background job a track, and the engine told which tracks are
//! frozen and where their frames are kept.
//!
//! Nothing here is told about an edit. Each frame the frozen tracks, the
//! sequence's revision and what a frame's key takes are compared with what
//! the jobs were started for ([`FreezeJobs::tick`]); on a change the jobs
//! stop, and once the change has held still a moment they start again. A
//! frame already stored under its key is passed over in microseconds, so
//! a job started again draws only what the change made stale.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use klipp_core::TrackId;
use klipp_engine::cache_store::CacheStore;
use klipp_engine::freeze::{FreezeStatus, FreezeTally};
use klipp_jobs::{BackgroundJob, JobId, ThreadJob};

use super::*;

/// How long a change holds still before the jobs start again: a drag
/// changes the revision every frame.
const SETTLE: Duration = Duration::from_millis(600);

/// What the jobs are started for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Inputs {
    /// The project's cache folder; `None` without a saved, writable project.
    pub folder: Option<PathBuf>,
    pub revision: klipp_timeline::SequenceRevision,
    pub tracks: Vec<TrackId>,
    pub size: (u32, u32),
    pub environment: u64,
}

/// A job's end: its track, the job and what it found.
type Finished = (TrackId, JobId, FreezeTally);

/// What a frame's look asks of the app.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Tick {
    /// Tell the engine the folder to read frozen frames from (`None` with
    /// nothing to read) and the frozen tracks.
    pub tell: Option<(Option<PathBuf>, Vec<TrackId>)>,
    /// Open the project's cache store in this folder.
    pub open: Option<PathBuf>,
    /// Start the jobs for the inputs.
    pub start: bool,
    /// Tracks whose job found the cache full.
    pub full: Vec<TrackId>,
}

#[derive(Default)]
pub(crate) struct FreezeJobs {
    /// The project's cache: its folder and the store, opened off the UI
    /// thread (it counts every file in it).
    store: Option<(PathBuf, Arc<Mutex<CacheStore>>)>,
    opening: Option<(PathBuf, mpsc::Receiver<CacheStore>)>,
    started_for: Option<Inputs>,
    seen: Option<(Inputs, Instant)>,
    jobs: HashMap<TrackId, Arc<ThreadJob>>,
    done: Option<(mpsc::Sender<Finished>, mpsc::Receiver<Finished>)>,
    statuses: HashMap<TrackId, FreezeStatus>,
    /// What the engine was last told.
    told: Option<(Option<PathBuf>, Vec<TrackId>)>,
}

impl FreezeJobs {
    /// Whether there is nothing frozen and nothing to undo: no look needed.
    fn idle(&self) -> bool {
        self.started_for.is_none() && self.told.is_none()
    }

    fn stop(&mut self) {
        for job in self.jobs.values() {
            job.cancel();
        }
        self.jobs.clear();
    }

    /// One frame's look at `inputs` (see the module's doc), at `now`.
    pub(crate) fn tick(&mut self, inputs: &Inputs, now: Instant) -> Tick {
        let mut out = Tick::default();
        let tell = (inputs.folder.clone().filter(|_| !inputs.tracks.is_empty()), inputs.tracks.clone());
        if self.told.as_ref() != Some(&tell) {
            out.tell = Some(tell.clone());
            self.told = Some(tell);
        }

        // The store, opened once per folder.
        if let Some((path, rx)) = &self.opening {
            if let Ok(store) = rx.try_recv() {
                self.store = Some((path.clone(), Arc::new(Mutex::new(store))));
                self.opening = None;
            }
        }
        if let Some(f) = &inputs.folder {
            let have = self.store.as_ref().is_some_and(|(p, _)| p == f) || self.opening.as_ref().is_some_and(|(p, _)| p == f);
            if !have && !inputs.tracks.is_empty() {
                out.open = Some(f.clone());
            }
        }

        // Finished jobs; one stopped for a change has been replaced, and
        // says nothing of its track.
        if let Some((_, rx)) = &self.done {
            while let Ok((track, id, tally)) = rx.try_recv() {
                if tally.cancelled || self.jobs.get(&track).map(|j| j.id()) != Some(id) {
                    continue;
                }
                if tally.full {
                    out.full.push(track);
                }
                if let Some(s) = self.statuses.get_mut(&track) {
                    s.progress = None;
                    s.tally = Some(tally);
                }
                self.jobs.remove(&track);
            }
        }

        if self.started_for.as_ref() != Some(inputs) {
            // Changed: what runs is drawing for what was.
            self.stop();
            match &self.seen {
                Some((seen, at)) if seen == inputs && now.duration_since(*at) >= SETTLE => {
                    let ready = inputs.folder.is_none() || self.store.as_ref().is_some_and(|(p, _)| Some(p) == inputs.folder.as_ref());
                    if ready {
                        self.started_for = Some(inputs.clone());
                        out.start = true;
                    }
                }
                Some((seen, _)) if seen == inputs => {}
                _ => self.seen = Some((inputs.clone(), now)),
            }
        }

        for (track, job) in &self.jobs {
            if let Some(s) = self.statuses.get_mut(track) {
                s.progress = Some(job.progress());
            }
        }
        out
    }

    /// Open the store in `folder` on a thread of its own; [`Self::tick`]
    /// takes it once it's open.
    fn open_store(&mut self, folder: PathBuf, cap: u64) {
        let (tx, rx) = mpsc::channel();
        let root = folder.clone();
        std::thread::Builder::new()
            .name("freeze-store".into())
            .spawn(move || {
                let _ = tx.send(CacheStore::open(root, cap));
            })
            .ok();
        self.opening = Some((folder, rx));
    }
}

impl KlippApp {
    /// Keep the frozen tracks' frames drawn and the engine told (see the
    /// module's doc). Called every frame; costs a walk over the tracks
    /// while nothing changes.
    pub(super) fn keep_frozen_tracks(&mut self) {
        // Frozen as the project says; drawn from the copy the engine plays,
        // whose tracks are the same.
        let seq = self.sequence.snapshot();
        let tracks: Vec<TrackId> = self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.tracks.iter().filter(|t| t.frozen && seq.tracks.iter().any(|x| x.id == t.id)).map(|t| t.id).collect()).unwrap_or_default();
        if tracks.is_empty() && self.freeze_jobs.idle() {
            return;
        }
        let folder = self.state.project_path.clone().filter(|p| !crate::project_paths::is_untitled(Some(p)) && self.read_only_project.as_ref() != Some(p)).map(|p| p.join("cache"));
        let working = klipp_engine::video_decoder::working_colorspace();
        let inputs = Inputs {
            folder,
            revision: self.sequence.revision(),
            tracks,
            size: klipp_engine::freeze::playback_size((seq.resolution.width, seq.resolution.height)),
            environment: klipp_engine::freeze::environment(working, self.state.use_proxies),
        };
        let tick = self.freeze_jobs.tick(&inputs, Instant::now());
        if let Some((folder, tracks)) = tick.tell {
            let _ = self.cmd_tx.send(AppCommand::SetFreezeFolder(folder));
            let _ = self.cmd_tx.send(AppCommand::SetFrozenTracks(tracks));
        }
        if let Some(folder) = tick.open {
            self.freeze_jobs.open_store(folder, u64::from(self.prefs.media_cache.max_cache_size_gb) << 30);
        }
        for track in tick.full {
            let name = seq.tracks.iter().find(|t| t.id == track).map(|t| t.name.clone()).unwrap_or_default();
            self.state.toast.show(format!("The cache is full, so part of {name} plays live. A larger cache in Preferences > Media Cache holds it all."));
        }
        if tick.start {
            self.start_freeze_jobs(&seq, &inputs);
        }
        klipp_engine::freeze::set_statuses(self.freeze_jobs.statuses.clone());
    }

    fn start_freeze_jobs(&mut self, seq: &klipp_timeline::Sequence, inputs: &Inputs) {
        let playhead = self.state.playhead;
        let pool = &self.state.media_pool;
        let fj = &mut self.freeze_jobs;
        let done = fj.done.get_or_insert_with(mpsc::channel).0.clone();
        let mut statuses = HashMap::new();
        let Some((_, store)) = fj.store.clone().filter(|_| inputs.folder.is_some()) else {
            for t in &inputs.tracks {
                let index = seq.tracks.iter().position(|x| x.id == *t);
                let frames = index.map_or(0, |i| klipp_engine::freeze::track_frames(seq, i, playhead).len());
                statuses.insert(*t, FreezeStatus { frames, unsaved: true, ..Default::default() });
            }
            fj.statuses = statuses;
            return;
        };
        {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            for t in &inputs.tracks {
                store.pin(&klipp_engine::freeze::namespace(*t));
            }
            for t in fj.statuses.keys().filter(|t| !inputs.tracks.contains(t)) {
                store.unpin(&klipp_engine::freeze::namespace(*t));
            }
        }
        let media = Arc::new(klipp_engine::freeze::media_identities(pool));
        let files: Arc<HashMap<klipp_core::MediaId, klipp_render::MediaFile>> = Arc::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect());
        let seq_arc = Arc::new(seq.clone());
        let working = klipp_engine::video_decoder::working_colorspace();
        let fps = seq.fps.as_f64();
        for t in &inputs.tracks {
            let Some(index) = seq.tracks.iter().position(|x| x.id == *t) else { continue };
            let frames = klipp_engine::freeze::track_frames(seq, index, playhead);
            let previous = fj.statuses.get(t).and_then(|s| s.tally.clone());
            statuses.insert(*t, FreezeStatus { frames: frames.len(), progress: Some(0.0), tally: previous, unsaved: false });
            let req = klipp_engine::freeze::FreezeRequest {
                seq: Arc::clone(&seq_arc),
                // As the engine keys: a precomposition's scene is on its clip.
                sequences: Arc::new(Vec::new()),
                track: index,
                track_id: *t,
                frames,
                size: inputs.size,
                working,
                environment: inputs.environment,
                media: Arc::clone(&media),
            };
            let (files, done, track) = (Arc::clone(&files), done.clone(), *t);
            // The job's id is known once it's made; its end reads it then.
            let id: Arc<std::sync::OnceLock<JobId>> = Arc::default();
            let id_at_end = Arc::clone(&id);
            let job = klipp_engine::freeze::spawn_freeze(req, Arc::clone(&store), move || klipp_render::ExportFrameSource::shared(files, fps, working), move |tally| {
                // A job with nothing to draw ends at once: its id is set
                // as soon as `spawn_freeze` returns.
                let started = Instant::now();
                while id_at_end.get().is_none() && started.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if let Some(id) = id_at_end.get() {
                    let _ = done.send((track, *id, tally));
                }
            });
            let _ = id.set(job.id());
            self.job_scheduler.submit(Arc::clone(&job) as Arc<dyn BackgroundJob>);
            fj.jobs.insert(*t, job);
        }
        fj.statuses = statuses;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(shared: &klipp_timeline::SharedSequence) -> klipp_timeline::SequenceRevision {
        shared.revision()
    }

    fn inputs(folder: Option<&str>, revision: klipp_timeline::SequenceRevision, tracks: Vec<TrackId>) -> Inputs {
        Inputs { folder: folder.map(PathBuf::from), revision, tracks, size: (960, 540), environment: 7 }
    }

    fn ready_store(fj: &mut FreezeJobs, folder: &str) {
        let dir = std::env::temp_dir().join(format!("klipp-freeze-jobs-{}", std::process::id()));
        fj.store = Some((PathBuf::from(folder), Arc::new(Mutex::new(CacheStore::open(dir, 1 << 30)))));
    }

    /// The engine is told once per change; the store is asked for once;
    /// the jobs start once the change holds still and the store is open,
    /// once, and again after an edit has held still.
    #[test]
    fn jobs_start_once_a_change_holds_still() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let t = TrackId::new();
        let mut fj = FreezeJobs::default();
        let t0 = Instant::now();
        let a = inputs(Some("/p/cache"), revision(&shared), vec![t]);

        let first = fj.tick(&a, t0);
        assert_eq!(first.tell, Some((Some(PathBuf::from("/p/cache")), vec![t])), "the engine told the folder and the track");
        assert_eq!(first.open, Some(PathBuf::from("/p/cache")), "the store asked for");
        assert!(!first.start);
        let later = fj.tick(&a, t0 + SETTLE * 2);
        assert!(later.tell.is_none(), "told once");
        assert!(!later.start, "not before the store is open");
        ready_store(&mut fj, "/p/cache");
        assert!(fj.tick(&a, t0 + SETTLE * 2).start, "the store open and the change still: start");
        assert!(!fj.tick(&a, t0 + SETTLE * 3).start, "started once");

        // An edit: nothing starts until it has held still.
        shared.replace(klipp_timeline::Sequence::new_default());
        let b = inputs(Some("/p/cache"), revision(&shared), vec![t]);
        let t1 = t0 + SETTLE * 4;
        assert!(!fj.tick(&b, t1).start);
        assert!(!fj.tick(&b, t1 + SETTLE / 2).start, "a drag still going");
        assert!(fj.tick(&b, t1 + SETTLE).start, "held still");
    }

    /// Without a saved project the engine is told there is nothing to read,
    /// and the jobs "start" at once, which records the tracks as played
    /// live until it is saved; unfrozen, the engine is told no track is.
    #[test]
    fn an_unsaved_project_reads_nothing_and_unfreezing_tells_the_engine() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let t = TrackId::new();
        let mut fj = FreezeJobs::default();
        let t0 = Instant::now();
        let a = inputs(None, revision(&shared), vec![t]);
        let first = fj.tick(&a, t0);
        assert_eq!(first.tell, Some((None, vec![t])));
        assert_eq!(first.open, None);
        assert!(fj.tick(&a, t0 + SETTLE).start);
        let none = inputs(None, revision(&shared), vec![]);
        assert_eq!(fj.tick(&none, t0 + SETTLE * 2).tell, Some((None, vec![])), "no track frozen");
    }

    /// A finished job's tally is the track's status; one that found the
    /// cache full is said; a cancelled one, or one replaced since, says
    /// nothing.
    #[test]
    fn a_finished_job_is_its_track_s_status_and_a_replaced_one_is_not() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let t = TrackId::new();
        let mut fj = FreezeJobs::default();
        let a = inputs(None, revision(&shared), vec![t]);
        let now = Instant::now();
        let (tx, rx) = mpsc::channel();
        fj.done = Some((tx.clone(), rx));
        let job = ThreadJob::spawn("test", klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, |_, _| {});
        let id = job.id();
        fj.jobs.insert(t, job);
        fj.started_for = Some(a.clone());
        fj.statuses.insert(t, FreezeStatus { frames: 10, progress: Some(0.5), ..Default::default() });
        let other = ThreadJob::spawn("old", klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, |_, _| {});
        // The replaced job's end alone: nothing changes.
        tx.send((t, other.id(), FreezeTally { stored: 1, full: true, ..Default::default() })).unwrap();
        let tick = fj.tick(&a, now);
        assert!(tick.full.is_empty(), "a replaced job says nothing");
        assert_eq!(fj.statuses[&t].tally, None);
        assert!(fj.statuses[&t].progress.is_some(), "still drawing");
        assert_eq!(fj.jobs.get(&t).map(|j| j.id()), Some(id), "the current job kept");
        tx.send((t, id, FreezeTally { stored: 4, full: true, ..Default::default() })).unwrap();
        let tick = fj.tick(&a, now);
        assert_eq!(tick.full, vec![t], "the full cache said");
        let s = &fj.statuses[&t];
        assert_eq!(s.tally.as_ref().map(|t| t.stored), Some(4), "the current job's tally, not the replaced one's");
        assert_eq!(s.progress, None);
        assert!(fj.jobs.is_empty());
    }
}
