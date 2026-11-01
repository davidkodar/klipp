//! The timeline's strips over footage used twice (#3272): where the
//! active sequence shows the same frames of a file in two places, worked
//! out on a worker after each change. One look runs at a time; a change
//! made while it runs is looked at once it ends, the latest only.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use klipp_document::reuse::Report;

use super::KlippApp;

/// What the marks were worked out for: the engine's copy changes revision
/// with each edit and sequence switch, media read later can bring a
/// fingerprint, and an analysis finishing can bring a file's picture
/// samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub revision: klipp_timeline::SequenceRevision,
    pub sequence: usize,
    pub media: usize,
    pub analysis: u64,
}

#[derive(Default)]
pub(crate) struct ReuseMarks {
    /// The key of the marks the timeline shows.
    shown: Option<Key>,
    running: Option<(Key, Receiver<Report>)>,
}

impl ReuseMarks {
    /// The marks a finished look found, and whether to start one for
    /// `key` (`start` is called only then, and gives the answer's end).
    pub(crate) fn tick(&mut self, key: Key, start: impl FnOnce() -> Option<Receiver<Report>>) -> Option<Report> {
        let mut found = None;
        if let Some((asked, rx)) = &self.running {
            match rx.try_recv() {
                Ok(report) => {
                    self.shown = Some(*asked);
                    found = Some(report);
                    self.running = None;
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => self.running = None,
            }
        }
        // Nothing runs here: a running look returned above.
        if self.shown != Some(key) {
            match start() {
                Some(rx) => self.running = Some((key, rx)),
                // It could not start: not asked again until the next change.
                None => self.shown = Some(key),
            }
        }
        found
    }

    pub(crate) fn busy(&self) -> bool {
        self.running.is_some()
    }
}

impl KlippApp {
    /// Keep `state.reuse_marks` for the active sequence while the strips
    /// are shown; called every frame, a compare while nothing changes.
    pub(super) fn keep_reuse_marks(&mut self, ctx: &egui::Context) {
        if !self.state.show_reused_footage {
            if !self.state.reuse_marks.is_empty() {
                self.state.reuse_marks = Default::default();
            }
            self.reuse_marks = ReuseMarks::default();
            return;
        }
        let key = Key { revision: self.sequence.revision(), sequence: self.state.active_sequence_idx, media: self.state.media_pool.len(), analysis: self.state.analysis_generation };
        let state = &self.state;
        let found = self.reuse_marks.tick(key, || {
            let sequences = state.sequences.clone();
            let pool = state.media_pool.clone();
            let active = state.active_sequence_idx;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("klipp-reuse-marks".into())
                .spawn(move || {
                    let Some(seq) = sequences.get(active) else { return };
                    let looks = klipp_document::reuse::looks_of(seq, &sequences, &pool, |path| {
                        klipp_analysis::visual::load_kept(path).map(|v| klipp_document::reuse::Look { every_secs: v.every_secs, samples: v.samples.iter().map(|s| (s.secs, s.e.clone())).collect() })
                    });
                    let _ = tx.send(klipp_document::reuse::find_with(seq, &sequences, &pool, Some(&looks)));
                })
                .ok()
                .map(|_| rx)
        });
        if let Some(report) = found {
            self.state.reuse_marks = Arc::new(report.marks);
        }
        if self.reuse_marks.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: usize) -> Key {
        Key { revision: klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default()).revision(), sequence: 0, media: n, analysis: 0 }
    }

    /// One look at a time; what changed while it ran is looked at once,
    /// at the latest key; nothing more while nothing changes.
    #[test]
    fn one_look_at_a_time_and_the_latest_change_next() {
        let mut marks = ReuseMarks::default();
        let (a, b, c) = (key(1), key(2), key(3));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut starts = 0;
        assert!(marks.tick(a, || { starts += 1; Some(rx) }).is_none());
        // Two changes while it runs: no second look.
        assert!(marks.tick(b, || { starts += 1; None }).is_none());
        assert!(marks.tick(c, || { starts += 1; None }).is_none());
        assert_eq!(starts, 1);
        tx.send(Report::default()).unwrap();
        let (tx2, rx2) = std::sync::mpsc::channel();
        let mut asked = None;
        assert!(marks.tick(c, || { asked = Some(()); Some(rx2) }).is_some(), "the first look's answer is shown");
        assert!(asked.is_some(), "and the latest key looked at next");
        tx2.send(Report::default()).unwrap();
        assert!(marks.tick(c, || panic!("not asked again")).is_some());
        assert!(marks.tick(c, || panic!("nothing changed")).is_none());
        assert!(!marks.busy());
    }

    /// A look that ends without an answer isn't waited on for ever.
    #[test]
    fn a_look_that_dies_is_asked_again() {
        let mut marks = ReuseMarks::default();
        let a = key(1);
        let (tx, rx) = std::sync::mpsc::channel::<Report>();
        marks.tick(a, || Some(rx));
        drop(tx);
        let mut again = false;
        marks.tick(a, || { again = true; None });
        assert!(again);
    }
}
