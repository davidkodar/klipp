//! Where a feature's jobs stand, kept the same way for every feature: a
//! job begun is running, is told how far it has come, and ends done
//! (with what it made), failed (with why) or cancelled; a cancel is
//! asked for through its flag, which the work reads between its steps.
//! The jobs that ended are kept, the newest few, for a reader that asks
//! late, and the app's job list and an agent's `job_status` read them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Where a job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    /// The word the job list uses.
    pub fn word(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Done => "completed",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// A job as last told.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub id: String,
    /// What it is about, in the feature's words (a language, a file).
    pub about: String,
    pub state: JobState,
    /// The step it is on, or how it ended.
    pub phase: &'static str,
    /// The step's share done, 0 to 1.
    pub progress: f32,
    /// Units done, of `total`.
    pub done: usize,
    pub total: usize,
    pub error: Option<String>,
    /// What it made, once done.
    pub result: Option<String>,
}

struct Entry {
    job: JobRecord,
    cancel: Arc<AtomicBool>,
}

/// One feature's jobs. Made `const`, so a feature keeps it in a static.
pub struct Tracker {
    entries: Mutex<Vec<Entry>>,
    /// How many ended jobs are kept.
    kept: usize,
    /// The feature's name for its jobs, in what a cancel says.
    name: &'static str,
}

impl Tracker {
    pub const fn new(name: &'static str, kept: usize) -> Self {
        Self { entries: Mutex::new(Vec::new()), kept, name }
    }

    fn entries(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Job `id` begun, about `about`, of `total` units: its cancel flag.
    /// A job of the same id is replaced, and the oldest ended jobs past
    /// the ones kept go.
    pub fn begin(&self, id: &str, about: &str, total: usize) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut jobs = self.entries();
        jobs.retain(|e| e.job.id != id);
        let ended = jobs.iter().filter(|e| e.job.state != JobState::Running).count();
        let mut over = ended.saturating_sub(self.kept.saturating_sub(1));
        jobs.retain(|e| {
            let gone = over > 0 && e.job.state != JobState::Running;
            over -= usize::from(gone);
            !gone
        });
        jobs.push(Entry {
            job: JobRecord { id: id.into(), about: about.into(), state: JobState::Running, phase: "starting", progress: 0.0, done: 0, total, error: None, result: None },
            cancel: cancel.clone(),
        });
        cancel
    }

    /// Job `id` changed by `f`, while it runs.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut JobRecord)) {
        if let Some(e) = self.entries().iter_mut().find(|e| e.job.id == id && e.job.state == JobState::Running) {
            f(&mut e.job);
        }
    }

    /// Job `id` stopped by a cancel, nothing made.
    pub fn cancelled(&self, id: &str) {
        self.update(id, |j| {
            j.state = JobState::Cancelled;
            j.phase = "cancelled";
        });
    }

    /// Job `id` ended: what it made, or why it made nothing.
    pub fn finish(&self, id: &str, result: Result<String, String>) {
        self.update(id, |j| match result {
            Ok(made) => {
                j.state = JobState::Done;
                j.phase = "done";
                j.progress = 1.0;
                j.result = Some(made);
            }
            Err(why) => {
                j.state = JobState::Failed;
                j.phase = "failed";
                j.error = Some(why);
            }
        });
    }

    /// Job `id` refused before it could start: kept as failed with `why`,
    /// so a reader given its id learns what happened. One that did start
    /// is ended with `why` instead.
    pub fn refused(&self, id: &str, about: &str, why: &str) {
        if self.get(id).is_none() {
            self.begin(id, about, 0);
        }
        self.finish(id, Err(why.to_string()));
    }

    /// Whether job `id` was asked to stop.
    pub fn cancel_requested(&self, id: &str) -> bool {
        self.entries().iter().any(|e| e.job.id == id && e.cancel.load(Ordering::Relaxed))
    }

    /// Every job kept, oldest first.
    pub fn all(&self) -> Vec<JobRecord> {
        self.entries().iter().map(|e| e.job.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<JobRecord> {
        self.entries().iter().find(|e| e.job.id == id).map(|e| e.job.clone())
    }

    /// The job running now, if one is.
    pub fn running(&self) -> Option<JobRecord> {
        self.entries().iter().find(|e| e.job.state == JobState::Running).map(|e| e.job.clone())
    }

    /// Job `id` asked to stop: its work stops at its next step. An error
    /// when it is not running.
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let jobs = self.entries();
        let e = jobs.iter().find(|e| e.job.id == id).ok_or_else(|| format!("no {} job {id}", self.name))?;
        if e.job.state != JobState::Running {
            return Err(format!("{} {id} has ended already ({}); nothing cancelled", self.name, e.job.phase));
        }
        e.cancel.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job runs, is told how far it has come, and ends; a cancel is
    /// asked through its flag and refused once it has ended; ended jobs
    /// past the ones kept go, oldest first, and a running one never does.
    #[test]
    fn a_job_runs_ends_and_is_kept_for_a_while() {
        let t = Tracker::new("test", 2);
        let flag = t.begin("a", "German", 4);
        t.update("a", |j| j.done = 2);
        assert_eq!(t.get("a").unwrap().done, 2);
        assert!(t.cancel("a").is_ok() && flag.load(Ordering::Relaxed) && t.cancel_requested("a"));
        t.cancelled("a");
        assert_eq!(t.get("a").unwrap().state, JobState::Cancelled);
        assert!(t.cancel("a").is_err(), "ended");
        t.update("a", |j| j.done = 4);
        assert_eq!(t.get("a").unwrap().done, 2, "an ended job is no longer told");

        t.begin("b", "", 1);
        t.finish("b", Ok("made".into()));
        t.begin("run", "", 1);
        t.begin("c", "", 1);
        t.finish("c", Err("why".into()));
        t.begin("d", "", 1);
        let ids: Vec<String> = t.all().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, ["run", "c", "d"], "two ended kept at most, the running ones always");
        assert_eq!(t.running().unwrap().id, "run");
        t.refused("e", "", "no model");
        assert_eq!((t.get("e").unwrap().state, t.get("e").unwrap().error.as_deref()), (JobState::Failed, Some("no model")));
        assert!(t.cancel("nothing").is_err());
    }
}
