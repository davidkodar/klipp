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
    /// The inputs changed this frame: stems no longer current must stop
    /// playing at once.
    pub changed: bool,
    /// A stem was drawn: tell the engine the stems again.
    pub stems: bool,
    /// A job ended: what is held changed.
    pub ended: bool,
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
    /// Each job's frames in the order it draws them, the sequence's rate,
    /// and how many it had drawn when its stale spans were last worked out.
    orders: HashMap<TrackId, (Vec<klipp_core::Timecode>, klipp_core::Fps, usize)>,
    /// What the engine was last told.
    told: Option<(Option<PathBuf>, Vec<TrackId>)>,
    /// What frozen tracks hold of the cache budget, as the engine was last
    /// told it.
    reserved: Option<u64>,
    /// Frozen sound tracks' stems that are current, as the engine was
    /// told them, and the file each stem job is drawing.
    stems: Vec<(TrackId, PathBuf)>,
    stem_pending: HashMap<TrackId, PathBuf>,
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
        self.stem_pending.clear();
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
                out.ended = true;
                if tally.full {
                    out.full.push(track);
                }
                if let Some(path) = self.stem_pending.remove(&track) {
                    if tally.stored > 0 {
                        self.stems.retain(|(t, _)| *t != track);
                        self.stems.push((track, path));
                        out.stems = true;
                    }
                }
                if let Some(s) = self.statuses.get_mut(&track) {
                    // Not reached, or not drawn: stale; reading other
                    // tracks: live.
                    if let Some((order, fps, _)) = self.orders.get(&track) {
                        let mut stale: Vec<klipp_core::Timecode> = order.get(tally.reached..).unwrap_or(&[]).to_vec();
                        stale.extend(tally.failed_frames.iter().copied());
                        s.stale = klipp_engine::freeze::spans(&stale, *fps);
                        s.live = klipp_engine::freeze::spans(&tally.live_frames, *fps);
                    }
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
                _ => {
                    self.seen = Some((inputs.clone(), now));
                    out.changed = true;
                }
            }
        }

        for (track, job) in &self.jobs {
            if let Some(s) = self.statuses.get_mut(track) {
                let p = job.progress();
                s.progress = Some(p);
                // What it hasn't reached yet is stale, worked out again
                // only as it moves on.
                if let Some((order, fps, seen)) = self.orders.get_mut(track) {
                    let done = ((p * order.len() as f32).round() as usize).min(order.len());
                    if done != *seen {
                        *seen = done;
                        s.stale = klipp_engine::freeze::spans(&order[done..], *fps);
                    }
                }
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
    /// The open project's cache folder, `<project>/cache`, where frozen
    /// tracks keep what is drawn of them: `None` without a saved, writable
    /// project.
    pub(super) fn freeze_cache_folder(&self) -> Option<PathBuf> {
        self.state.project_path.clone().filter(|p| !crate::project_paths::is_untitled(Some(p)) && self.read_only_project.as_ref() != Some(p)).map(|p| p.join("cache"))
    }

    /// The open project's frozen frames (`sound` false) or frozen sound
    /// cleared (#3207): the tracks frozen so in the open sequence played
    /// live again, one undo step, and the files removed.
    pub(super) fn clear_frozen(&mut self, sound: bool) {
        let kind = if sound { klipp_timeline::TrackKind::Audio } else { klipp_timeline::TrackKind::Video };
        let idx = self.state.active_sequence_idx;
        if let Some(step) = unfreeze_all(&mut self.state.sequences[idx], kind) {
            self.state.push_history(step);
            self.state.dirty = true;
        }
        let fj = &mut self.freeze_jobs;
        fj.stop();
        if let Some(cache) = self.freeze_cache_folder() {
            let dir = cache.join(if sound { "stems" } else { "freeze" });
            if sound {
                self.freeze_jobs.stems.clear();
                let _ = self.cmd_tx.send(AppCommand::SetFrozenStems(Vec::new()));
            } else {
                // The store forgets what it held; opened again when wanted.
                self.freeze_jobs.store = None;
                self.freeze_jobs.opening = None;
            }
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Clearing {}: {e}", dir.display());
                }
            }
        }
        self.freeze_jobs.started_for = None;
        self.freeze_jobs.seen = None;
        self.freeze_jobs.reserved = None;
    }

    /// Tell the engine what of the cache budget frozen tracks hold, when
    /// it changed: their frames and stems are kept first, and the render
    /// cache keeps to the rest (#3207). The frame store keeps to what the
    /// stems leave.
    fn reserve_for_frozen(&mut self, folder: Option<&std::path::Path>) {
        let budget = u64::from(self.prefs.media_cache.max_cache_size_gb) << 30;
        let stems = folder.map_or(0, |f| crate::cache_manager::scan_tree_size(&f.join("stems")).0);
        let frames = self.freeze_jobs.store.as_ref().map_or(0, |(_, s)| {
            let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
            s.set_cap(budget.saturating_sub(stems));
            s.total_bytes()
        });
        let held = frames + stems;
        if self.freeze_jobs.reserved != Some(held) {
            self.freeze_jobs.reserved = Some(held);
            let _ = self.cmd_tx.send(AppCommand::SetRenderCacheReserved(held));
        }
    }

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
        let folder = self.freeze_cache_folder();
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
        // A stem the change made stale stops playing at once (#3207): the
        // track plays live until its stem is drawn again.
        if tick.changed {
            let pool = &self.state.media_pool;
            let before = self.freeze_jobs.stems.len();
            self.freeze_jobs.stems.retain(|(t, path)| inputs.tracks.contains(t) && stem_path(&seq, pool, inputs.folder.as_deref(), *t).ok().as_ref() == Some(path));
            if self.freeze_jobs.stems.len() != before {
                let _ = self.cmd_tx.send(AppCommand::SetFrozenStems(self.freeze_jobs.stems.clone()));
            }
        }
        if tick.stems {
            let _ = self.cmd_tx.send(AppCommand::SetFrozenStems(self.freeze_jobs.stems.clone()));
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
        // What they hold of the budget, again once jobs end or start.
        if tick.start || tick.stems || tick.ended || self.freeze_jobs.reserved.is_none() {
            self.reserve_for_frozen(inputs.folder.as_deref());
        }
        klipp_engine::freeze::set_statuses(self.freeze_jobs.statuses.clone());
    }

    fn start_freeze_jobs(&mut self, seq: &klipp_timeline::Sequence, inputs: &Inputs) {
        let is_sound = |t: &TrackId| seq.tracks.iter().any(|x| x.id == *t && x.kind == klipp_timeline::TrackKind::Audio);
        let (sound, pictures): (Vec<TrackId>, Vec<TrackId>) = inputs.tracks.iter().copied().partition(is_sound);
        let mut statuses = self.start_stem_jobs(seq, inputs, &sound);
        let playhead = self.state.playhead;
        let pool = &self.state.media_pool;
        let fj = &mut self.freeze_jobs;
        let done = fj.done.get_or_insert_with(mpsc::channel).0.clone();
        let Some((_, store)) = fj.store.clone().filter(|_| inputs.folder.is_some()) else {
            for t in &pictures {
                let index = seq.tracks.iter().position(|x| x.id == *t);
                let frames = index.map_or_else(Vec::new, |i| klipp_engine::freeze::track_frames(seq, i, playhead));
                let stale = klipp_engine::freeze::spans(&frames, seq.fps);
                statuses.insert(*t, FreezeStatus { frames: frames.len(), unsaved: true, stale, ..Default::default() });
            }
            fj.statuses = statuses;
            return;
        };
        {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            for t in &pictures {
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
        for t in &pictures {
            let Some(index) = seq.tracks.iter().position(|x| x.id == *t) else { continue };
            let frames = klipp_engine::freeze::track_frames(seq, index, playhead);
            let previous = fj.statuses.get(t).and_then(|s| s.tally.clone());
            statuses.insert(*t, FreezeStatus { frames: frames.len(), progress: Some(0.0), tally: previous, ..Default::default() });
            fj.orders.insert(*t, (frames.clone(), seq.fps, usize::MAX));
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
            let files = Arc::clone(&files);
            let (id, end) = reported(done.clone(), *t);
            let job = klipp_engine::freeze::spawn_freeze(req, Arc::clone(&store), move || klipp_render::ExportFrameSource::shared(files, fps, working), end);
            let _ = id.set(job.id());
            self.job_scheduler.submit(Arc::clone(&job) as Arc<dyn BackgroundJob>);
            fj.jobs.insert(*t, job);
        }
        fj.statuses = statuses;
    }

    /// The frozen sound tracks (#3207): each stem current under its key is
    /// told the engine; one not drawn is drawn by a job, played live until
    /// it is. Their statuses.
    fn start_stem_jobs(&mut self, seq: &klipp_timeline::Sequence, inputs: &Inputs, sound: &[TrackId]) -> HashMap<TrackId, FreezeStatus> {
        let mut statuses = HashMap::new();
        let mut current: Vec<(TrackId, PathBuf)> = Vec::new();
        let done = self.freeze_jobs.done.get_or_insert_with(mpsc::channel).0.clone();
        for t in sound {
            let base = FreezeStatus { sound: true, frames: 1, ..Default::default() };
            let path = match stem_path(seq, &self.state.media_pool, inputs.folder.as_deref(), *t) {
                Err(_) if inputs.folder.is_none() => {
                    statuses.insert(*t, FreezeStatus { unsaved: true, ..base });
                    continue;
                }
                Err(why) => {
                    statuses.insert(*t, FreezeStatus { live_reason: Some(why), tally: Some(FreezeTally::default()), ..base });
                    continue;
                }
                Ok(path) => path,
            };
            if path.exists() {
                current.push((*t, path));
                statuses.insert(*t, FreezeStatus { tally: Some(FreezeTally { current: 1, reached: 1, ..Default::default() }), ..base });
                continue;
            }
            let snap = match klipp_render::stem::stem_snapshot(seq, &[], &self.state.media_pool, *t) {
                Ok(Some(snap)) => snap,
                Ok(None) | Err(_) => {
                    statuses.insert(*t, FreezeStatus { tally: Some(FreezeTally { failed: 1, ..Default::default() }), ..base });
                    continue;
                }
            };
            let name = seq.tracks.iter().find(|x| x.id == *t).map(|x| x.name.clone()).unwrap_or_default();
            let (id, end) = reported(done.clone(), *t);
            let into = path.clone();
            let job = ThreadJob::spawn(format!("Freeze sound of {name}"), klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, move |pause, progress| {
                let mut go_on = |p: f32| {
                    pause.wait_while_paused();
                    progress.set(p);
                    !pause.is_cancelled()
                };
                let tally = match klipp_render::stem::render_stem(&snap, &mut go_on) {
                    Ok(None) => FreezeTally { cancelled: true, ..Default::default() },
                    Ok(Some(samples)) => match klipp_render::stem::write_stem(&into, &samples) {
                        Ok(()) => {
                            // Older stems of the track, under keys gone by.
                            for e in std::fs::read_dir(into.parent().unwrap_or(&into)).into_iter().flatten().flatten() {
                                if e.path() != into {
                                    let _ = std::fs::remove_file(e.path());
                                }
                            }
                            FreezeTally { stored: 1, reached: 1, ..Default::default() }
                        }
                        Err(e) => {
                            tracing::warn!("freeze: the stem of {name} wasn't written: {e}");
                            FreezeTally { failed: 1, reached: 1, ..Default::default() }
                        }
                    },
                    Err(e) => {
                        tracing::warn!("freeze: the stem of {name} wasn't drawn: {e}");
                        FreezeTally { failed: 1, reached: 1, ..Default::default() }
                    }
                };
                end(tally);
            });
            let _ = id.set(job.id());
            self.job_scheduler.submit(Arc::clone(&job) as Arc<dyn BackgroundJob>);
            let fj = &mut self.freeze_jobs;
            fj.jobs.insert(*t, job);
            fj.stem_pending.insert(*t, path);
            statuses.insert(*t, FreezeStatus { progress: Some(0.0), ..base });
        }
        // Those current now, an undo's among them, played from their stems.
        let fj = &mut self.freeze_jobs;
        let as_set = |v: &[(TrackId, PathBuf)]| v.iter().cloned().collect::<std::collections::HashSet<_>>();
        if as_set(&fj.stems) != as_set(&current) {
            fj.stems = current;
            let _ = self.cmd_tx.send(AppCommand::SetFrozenStems(fj.stems.clone()));
        }
        statuses
    }
}

/// Every track of `kind` frozen in `seq` played live again, unlocked if it
/// was locked while frozen: the undo step that does it, `None` with none
/// frozen.
fn unfreeze_all(seq: &mut klipp_timeline::Sequence, kind: klipp_timeline::TrackKind) -> Option<klipp_document::history::EditCommand> {
    let mut edits = Vec::new();
    for (ti, t) in seq.tracks.iter_mut().enumerate().filter(|(_, t)| t.frozen && t.kind == kind) {
        let old = t.clone();
        t.set_frozen(false);
        edits.push(klipp_document::history::EditCommand::SetTrack { description: "Clear Freeze Cache", track_index: ti, old: Box::new(old), new: Box::new(t.clone()) });
    }
    (!edits.is_empty()).then_some(klipp_document::history::EditCommand::Batch(edits))
}

/// A job's end, reported on `done` for `track` with the job's id, which
/// is set in the cell once the job is made: a job with nothing to do can
/// end before then, so the end waits a moment for it.
fn reported(done: mpsc::Sender<Finished>, track: TrackId) -> (Arc<std::sync::OnceLock<JobId>>, impl FnOnce(FreezeTally) + Send + 'static) {
    let id: Arc<std::sync::OnceLock<JobId>> = Arc::default();
    let at_end = Arc::clone(&id);
    let end = move |tally: FreezeTally| {
        let started = Instant::now();
        while at_end.get().is_none() && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(1));
        }
        if let Some(id) = at_end.get() {
            let _ = done.send((track, *id, tally));
        }
    };
    (id, end)
}

/// Where sound track `track`'s stem is kept under its current key
/// (#3207): `<project>/cache/stems/<track>/<key>.pcm`; or why it plays
/// live.
fn stem_path(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, folder: Option<&std::path::Path>, track: TrackId) -> Result<PathBuf, &'static str> {
    let folder = folder.ok_or("the project isn't saved")?;
    if let Some(why) = klipp_render::stem::live_reason(seq, track) {
        return Err(why);
    }
    let media = |id: &klipp_core::MediaId| pool.get(id).map(klipp_engine::freeze::media_identity);
    let key = klipp_render::stem::stem_key(seq, &[], track, &media).ok_or("it has no sound to freeze")?;
    Ok(folder.join("stems").join(track.to_string()).join(format!("{key:016x}.pcm")))
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

    /// While a job draws, the frames it hasn't reached are stale; once it
    /// ends, those it never reached and those it couldn't draw are, and
    /// those that read other tracks are live.
    #[test]
    fn stale_and_live_spans_follow_the_job() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let t = TrackId::new();
        let fps = klipp_core::Fps::new(25, 1);
        let f = |n: i64| klipp_core::Timecode { value: n, rate: 25 };
        let order: Vec<klipp_core::Timecode> = (10..20).chain(0..10).map(f).collect();
        let mut fj = FreezeJobs::default();
        let a = inputs(None, revision(&shared), vec![t]);
        let (tx, rx) = mpsc::channel();
        fj.done = Some((tx.clone(), rx));
        fj.started_for = Some(a.clone());
        fj.orders.insert(t, (order.clone(), fps, usize::MAX));
        fj.statuses.insert(t, FreezeStatus { frames: 20, progress: Some(0.0), ..Default::default() });
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let job = ThreadJob::spawn("drawing", klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, move |_, progress| {
            progress.set(0.5);
            let _ = go_rx.recv();
        });
        let id = job.id();
        fj.jobs.insert(t, Arc::clone(&job));
        while job.progress() < 0.5 {
            std::thread::sleep(Duration::from_millis(1));
        }
        fj.tick(&a, Instant::now());
        assert_eq!(fj.statuses[&t].stale, vec![(f(0), f(10))], "the half not reached: frames 0-9");

        go_tx.send(()).unwrap();
        job.join();
        let tally = FreezeTally { stored: 15, live: 2, failed: 1, reached: 18, live_frames: vec![f(12), f(13)], failed_frames: vec![f(15)], ..Default::default() };
        tx.send((t, id, tally)).unwrap();
        fj.tick(&a, Instant::now());
        let s = &fj.statuses[&t];
        assert_eq!(s.stale, vec![(f(8), f(10)), (f(15), f(16))], "not reached (8, 9) and not drawn (15)");
        assert_eq!(s.live, vec![(f(12), f(14))]);
    }

    /// A change is said once, when first seen: the stems are checked then,
    /// not every frame of a drag.
    #[test]
    fn a_change_is_said_when_first_seen() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let mut fj = FreezeJobs::default();
        let t0 = Instant::now();
        let a = inputs(None, revision(&shared), vec![TrackId::new()]);
        assert!(fj.tick(&a, t0).changed);
        assert!(!fj.tick(&a, t0 + SETTLE / 4).changed);
        shared.replace(klipp_timeline::Sequence::new_default());
        let b = inputs(None, revision(&shared), a.tracks.clone());
        assert!(fj.tick(&b, t0 + SETTLE / 2).changed);
        assert!(!fj.tick(&b, t0 + SETTLE).changed);
    }

    /// A stem job that drew its stem adds it to those told the engine;
    /// one that failed doesn't.
    #[test]
    fn a_drawn_stem_is_told_the_engine() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let (t, u) = (TrackId::new(), TrackId::new());
        let mut fj = FreezeJobs::default();
        let a = inputs(None, revision(&shared), vec![t, u]);
        let (tx, rx) = mpsc::channel();
        fj.done = Some((tx.clone(), rx));
        fj.started_for = Some(a.clone());
        let job = |n: &'static str| ThreadJob::spawn(n, klipp_jobs::JobPriority::Background, klipp_jobs::ResourceClass::Cpu, |_, _| {});
        let (jt, ju) = (job("t"), job("u"));
        tx.send((t, jt.id(), FreezeTally { stored: 1, reached: 1, ..Default::default() })).unwrap();
        tx.send((u, ju.id(), FreezeTally { failed: 1, reached: 1, ..Default::default() })).unwrap();
        fj.jobs.insert(t, jt);
        fj.jobs.insert(u, ju);
        fj.stem_pending.insert(t, PathBuf::from("/p/cache/stems/t/1.pcm"));
        fj.stem_pending.insert(u, PathBuf::from("/p/cache/stems/u/2.pcm"));
        let tick = fj.tick(&a, Instant::now());
        assert!(tick.stems);
        assert_eq!(fj.stems, vec![(t, PathBuf::from("/p/cache/stems/t/1.pcm"))]);
    }

    /// A stem's place follows its key: an edit to the track moves it, the
    /// fader doesn't; without a saved project, or with an outside plug-in,
    /// there is none.
    #[test]
    fn a_stem_s_place_follows_its_sound() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/voice.wav".into(), None);
        item.video_codec.clear();
        let media = pool.add(item);
        let at = |f: i64| klipp_core::Timecode { value: f, rate: 24 };
        let mut track = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        track.clips = vec![klipp_timeline::Clip::media(media, at(0), at(48), at(0), at(48))];
        let id = track.id;
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks = vec![track];
        let folder = std::path::Path::new("/p/cache");
        let place = stem_path(&seq, &pool, Some(folder), id).unwrap();
        assert!(place.starts_with("/p/cache/stems") && place.extension().is_some_and(|e| e == "pcm"), "{}", place.display());
        let mut louder = seq.clone();
        louder.tracks[0].volume = 0.3;
        assert_eq!(stem_path(&louder, &pool, Some(folder), id).unwrap(), place, "the fader is live");
        let mut trimmed = seq.clone();
        trimmed.tracks[0].clips[0].timeline_out = at(24);
        assert_ne!(stem_path(&trimmed, &pool, Some(folder), id).unwrap(), place, "a trim is a new stem");
        assert!(stem_path(&seq, &pool, None, id).is_err(), "nowhere to keep it");
        let mut outside = seq.clone();
        outside.tracks[0].effects.audio = vec![klipp_core::AudioEffectInstance { effect_type: "clap:org.example.x".into(), enabled: true, wet_dry: 1.0, params: vec![] }];
        assert!(stem_path(&outside, &pool, Some(folder), id).is_err());
    }

    /// Clearing the frozen frames plays the frozen video tracks live,
    /// unlocking one locked while frozen, as one step named for it, which
    /// undone freezes them again; the frozen sound tracks are left.
    #[test]
    fn clearing_frozen_frames_plays_those_tracks_live_in_one_step() {
        use klipp_timeline::{Track, TrackKind};
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.set_lock_while_frozen(true);
        v1.set_frozen(true);
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.frozen = true;
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.frozen = true;
        seq.tracks = vec![v1, v2, a1, Track::new("V3", TrackKind::Video)];
        let before = seq.clone();
        let step = unfreeze_all(&mut seq, TrackKind::Video).expect("a step");
        assert_eq!(step.label(), "Clear Freeze Cache");
        assert!(!seq.tracks[0].frozen && !seq.tracks[0].locked, "live, unlocked");
        assert!(!seq.tracks[1].frozen);
        assert!(seq.tracks[2].frozen, "the sound kept");
        let mut seqs = vec![seq.clone()];
        let (mut pool, mut fps) = (klipp_media::MediaPool::new(), seq.fps);
        klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&step);
        assert_eq!(serde_json::to_value(&seqs[0].tracks).unwrap(), serde_json::to_value(&before.tracks).unwrap(), "undone, frozen again");
        assert!(unfreeze_all(&mut seq, TrackKind::Video).is_none(), "nothing frozen left");
    }
}
