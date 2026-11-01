//! The sequence the UI, the engine and the control socket share (#690).
//!
//! It used to be an `RwLock<Sequence>`. The engine takes a read lock for
//! every frame it renders, for as long as the frame takes, and on Linux a
//! read-write lock lets readers in ahead of a waiting writer. The UI's
//! write, publishing an edit, waited behind a whole heavy frame, and once,
//! behind a run of them, for over a minute.
//!
//! Here the sequence is an immutable snapshot behind a short lock. A reader
//! takes the lock only to clone the `Arc`, a few nanoseconds, then renders
//! from that snapshot holding nothing. A writer holds the lock while it
//! changes the sequence, copy-on-write: if a reader still holds the old
//! snapshot, the writer gets its own copy rather than waiting for it, and
//! the reader finishes its frame from the sequence as it was. A writer
//! never waits for a reader's frame; a reader waits only for a writer's
//! edit, which is quick, and `try_read` does not wait for that.
//!
//! The methods keep the shape of `RwLock`'s (`read`, `write`, `try_read`,
//! `try_write`, results with a poison error the caller can recover from),
//! so the code that used the lock reads the same. A poisoned lock is
//! recovered at once. That does not make the sequence whole: a writer
//! changes it in place (through `Arc::make_mut`), and one that panicked
//! part way through a change of several steps leaves it half changed.
//! Readers then see that sequence until a writer replaces it; the
//! engine's copy is replaced whole at the UI's next sync.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Sequence;

/// See the module docs.
#[derive(Debug)]
pub struct SharedSequence {
    current: Mutex<Current>,
}

/// Identity of a published sequence revision, without retaining its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceRevision(u64);

impl SequenceRevision {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug)]
struct Current {
    sequence: Arc<Sequence>,
    revision: SequenceRevision,
    /// What the viewer shows in place of `sequence` while an entry is
    /// hovered in a browser (#3210): the sequence with it applied, made
    /// for this revision and gone with it, so an edit ends it whatever
    /// else happens. Never the document's: nothing saves or undoes it.
    preview: Option<Arc<Sequence>>,
    /// While a take of an audition is previewed (#3209): the document,
    /// kept here while `sequence` is the copy with the take in its slot,
    /// which the engine draws and plays as it does any sequence. What
    /// writes a file reads [`SharedSequence::document`]; a change ends it
    /// first, so an edit is always the document's.
    stand_in: Option<StandIn>,
}

#[derive(Debug)]
struct StandIn {
    document: Arc<Sequence>,
    what: StandInFor,
}

/// What a stand-in sequence shows (#3209): a take of the audition in a
/// clip's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandInFor {
    pub clip: klipp_core::ClipId,
    pub take: klipp_core::TakeId,
}

/// The sequence as it was when read: holds no lock.
#[derive(Debug, Clone)]
pub struct SequenceRead(Arc<Sequence>);

impl Deref for SequenceRead {
    type Target = Sequence;
    fn deref(&self) -> &Sequence {
        &self.0
    }
}

impl SequenceRead {
    /// The snapshot as an `Arc`, to keep or hand on.
    pub fn into_arc(self) -> Arc<Sequence> {
        self.0
    }
}

/// The sequence open for a change: holds the short lock, and copies the
/// sequence on its first change if a reader still holds the snapshot.
#[derive(Debug)]
pub struct SequenceWrite<'a> {
    current: MutexGuard<'a, Current>,
    touched: bool,
}

impl Deref for SequenceWrite<'_> {
    type Target = Sequence;
    fn deref(&self) -> &Sequence {
        &self.current.sequence
    }
}

impl DerefMut for SequenceWrite<'_> {
    fn deref_mut(&mut self) -> &mut Sequence {
        if !self.touched {
            self.current.revision = SequenceRevision::next();
            self.current.preview = None;
            debug_assert!(self.current.stand_in.is_none(), "a stand-in is ended before a write");
            self.touched = true;
        }
        Arc::make_mut(&mut self.current.sequence)
    }
}

impl SharedSequence {
    pub fn new(seq: Sequence) -> Self {
        Self { current: Mutex::new(Current { sequence: Arc::new(seq), revision: SequenceRevision::next(), preview: None, stand_in: None }) }
    }

    fn lock(&self) -> MutexGuard<'_, Current> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The sequence as it stands, held without a lock.
    pub fn snapshot(&self) -> Arc<Sequence> {
        Arc::clone(&self.lock().sequence)
    }

    /// Read the snapshot and its token under the same lock. Keep the
    /// snapshot only while rendering; caches can retain just the token.
    pub fn snapshot_with_revision(&self) -> (Arc<Sequence>, SequenceRevision) {
        let current = self.lock();
        (Arc::clone(&current.sequence), current.revision)
    }

    pub fn revision(&self) -> SequenceRevision {
        self.lock().revision
    }

    /// [`Self::snapshot`] in `RwLock::read`'s shape. Never an error.
    pub fn read(&self) -> LockResult<SequenceRead> {
        Ok(SequenceRead(self.snapshot()))
    }

    /// [`Self::read`], or `WouldBlock` while a writer is making its change.
    pub fn try_read(&self) -> TryLockResult<SequenceRead> {
        match self.current.try_lock() {
            Ok(current) => Ok(SequenceRead(Arc::clone(&current.sequence))),
            Err(TryLockError::Poisoned(e)) => Ok(SequenceRead(Arc::clone(&e.into_inner().sequence))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// The sequence for a change. Waits only for another writer. A take
    /// previewed in its slot is ended first: the change is the
    /// document's.
    pub fn write(&self) -> LockResult<SequenceWrite<'_>> {
        let mut current = self.lock();
        end_stand_in(&mut current);
        Ok(SequenceWrite { current, touched: false })
    }

    /// [`Self::write`], or `WouldBlock` while another writer has it.
    pub fn try_write(&self) -> TryLockResult<SequenceWrite<'_>> {
        let mut current = match self.current.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(TryLockError::WouldBlock),
        };
        end_stand_in(&mut current);
        Ok(SequenceWrite { current, touched: false })
    }

    /// Put `seq` in place whole: what a publish of an edited sequence does.
    /// It is the document, so a take previewed is ended with it.
    pub fn replace(&self, seq: Sequence) {
        *self.lock() = Current { sequence: Arc::new(seq), revision: SequenceRevision::next(), preview: None, stand_in: None };
    }

    /// Hand the engine `seq` in place of the document while a take is
    /// previewed (#3209), keeping the document beside it; previewing
    /// another take keeps the same document. Its revision.
    pub fn stand_in(&self, seq: Sequence, what: StandInFor) -> SequenceRevision {
        let mut current = self.lock();
        let document = match current.stand_in.take() {
            Some(s) => s.document,
            None => Arc::clone(&current.sequence),
        };
        current.sequence = Arc::new(seq);
        current.revision = SequenceRevision::next();
        current.preview = None;
        current.stand_in = Some(StandIn { document, what });
        current.revision
    }

    /// The document back in place of the take previewed, under a revision
    /// of its own, so what drew the take draws it again. Whether a take
    /// was previewed.
    pub fn end_stand_in(&self) -> bool {
        end_stand_in(&mut self.lock())
    }

    /// The take previewed in its slot, if one is.
    pub fn standing_in(&self) -> Option<StandInFor> {
        self.lock().stand_in.as_ref().map(|s| s.what)
    }

    /// The document: what a save, an export or anything else writing a
    /// file reads, never a take previewed in its slot.
    pub fn document(&self) -> Arc<Sequence> {
        let current = self.lock();
        match &current.stand_in {
            Some(s) => Arc::clone(&s.document),
            None => Arc::clone(&current.sequence),
        }
    }

    /// Show `preview` in place of the sequence (#3210), if the sequence is
    /// still at `base`, the revision it was made from: one made from an
    /// older sequence would show the edits since then undone. Whether it
    /// was taken.
    pub fn set_preview(&self, base: SequenceRevision, preview: Arc<Sequence>) -> bool {
        let mut current = self.lock();
        let taken = current.revision == base;
        if taken {
            current.preview = Some(preview);
        }
        taken
    }

    /// Show the sequence itself again. Whether a preview was up.
    pub fn clear_preview(&self) -> bool {
        self.lock().preview.take().is_some()
    }

    /// [`Self::snapshot_with_revision`], and the preview to show in its
    /// place, if one is up.
    pub fn view_with_revision(&self) -> (Arc<Sequence>, SequenceRevision, Option<Arc<Sequence>>) {
        let current = self.lock();
        (Arc::clone(&current.sequence), current.revision, current.preview.clone())
    }
}

fn end_stand_in(current: &mut Current) -> bool {
    let Some(s) = current.stand_in.take() else { return false };
    current.sequence = s.document;
    current.revision = SequenceRevision::next();
    current.preview = None;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3210: a preview is shown only for the revision it was made from,
    /// and any change to the sequence, or a sequence put in whole, ends
    /// it; the sequence itself is never changed by one.
    #[test]
    fn a_preview_lasts_only_as_long_as_the_sequence_it_was_made_from() {
        let shared = SharedSequence::new(Sequence::new_default());
        let base = shared.revision();
        let mut previewed = Sequence::new_default();
        previewed.name = "previewed".into();
        let previewed = Arc::new(previewed);
        assert!(shared.set_preview(base, Arc::clone(&previewed)));
        let (seq, revision, preview) = shared.view_with_revision();
        assert_eq!((revision, preview.map(|p| p.name.clone())), (base, Some("previewed".into())));
        assert_ne!(seq.name, "previewed", "the sequence itself is as it was");
        assert!(shared.clear_preview() && !shared.clear_preview(), "cleared once");
        assert!(shared.set_preview(base, Arc::clone(&previewed)));
        shared.write().unwrap().name = "edited".into();
        assert!(shared.view_with_revision().2.is_none(), "an edit ends it");
        assert!(!shared.set_preview(base, Arc::clone(&previewed)), "nor is one made before the edit taken");
        assert!(shared.set_preview(shared.revision(), Arc::clone(&previewed)));
        shared.replace(Sequence::new_default());
        assert!(shared.view_with_revision().2.is_none(), "a sequence put in whole ends it");
    }

    /// #3209: a take stood in shows in place of the document, which is
    /// kept whole beside it and is what `document` gives; previewing
    /// another keeps the same document; ending gives it back as it was,
    /// under a new revision; a write ends it first and changes the
    /// document, never the take; a publish ends it.
    #[test]
    fn a_take_stands_in_for_the_document_until_ended_or_written() {
        let mut doc = Sequence::new_default();
        doc.name = "document".into();
        let shared = SharedSequence::new(doc);
        let before = shared.revision();
        let take = |name: &str| {
            let mut s = Sequence::new_default();
            s.name = name.into();
            s
        };
        let what = StandInFor { clip: klipp_core::ClipId::new(), take: klipp_core::TakeId::new() };
        let r = shared.stand_in(take("take 2"), what);
        assert_ne!(r, before);
        assert_eq!(shared.snapshot().name, "take 2", "the engine draws the take");
        assert_eq!(shared.document().name, "document", "a save reads the document");
        assert_eq!(shared.standing_in(), Some(what));
        shared.stand_in(take("take 3"), what);
        assert_eq!(shared.document().name, "document", "the same document under the next take");
        assert!(shared.end_stand_in());
        assert_eq!(shared.snapshot().name, "document");
        assert!(shared.revision() != r && shared.revision() != before, "a revision of its own");
        assert!(!shared.end_stand_in(), "nothing to end");

        shared.stand_in(take("take 2"), what);
        shared.write().unwrap().name = "edited".into();
        assert_eq!(shared.standing_in(), None, "a write ends it");
        assert_eq!(shared.snapshot().name, "edited");
        assert_eq!(shared.document().name, "edited", "and changes the document");

        shared.stand_in(take("take 2"), what);
        shared.replace(take("published"));
        assert_eq!((shared.standing_in(), shared.document().name.as_str()), (None, "published"));
    }

    #[test]
    fn revisions_track_mutation_without_retaining_a_snapshot() {
        let shared = SharedSequence::new(Sequence::new_default());
        let revision = shared.revision();
        let pointer = Arc::as_ptr(&shared.snapshot());
        drop(shared.write().unwrap());
        drop(shared.try_write().unwrap());
        assert_eq!(shared.revision(), revision, "an unused write guard is not a change");
        shared.write().unwrap().name = "edited".into();
        assert_ne!(shared.revision(), revision);
        assert_eq!(Arc::as_ptr(&shared.snapshot()), pointer, "a token does not force copy-on-write");
        let (snapshot, at_render) = shared.snapshot_with_revision();
        assert_eq!(at_render, shared.revision());
        shared.try_write().unwrap().name = "later edit".into();
        assert_eq!(snapshot.name, "edited");
        assert_ne!(shared.revision(), at_render);
        let before_replace = shared.revision();
        shared.replace(Sequence::new_default());
        assert_ne!(shared.revision(), before_replace);
        assert_ne!(shared.revision(), SharedSequence::new(Sequence::new_default()).revision(), "another project has a distinct token");
    }

    /// #690: a reader holding the sequence through a long frame never keeps
    /// a writer waiting, and finishes from the sequence it read.
    #[test]
    fn a_reader_never_holds_up_a_writer() {
        let shared = Arc::new(SharedSequence::new(Sequence::new_default()));
        let held = shared.read().unwrap();
        let before = held.name.clone();
        // A writer, on another thread, while the snapshot is held.
        let writer = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                shared.write().unwrap().name = "edited".into();
                t.elapsed()
            })
        };
        let waited = writer.join().unwrap();
        assert!(waited < std::time::Duration::from_millis(50), "the writer waited {waited:?}");
        assert_eq!(held.name, before, "the reader keeps the sequence it read");
        assert_eq!(shared.read().unwrap().name, "edited", "a later read sees the edit");
        // And a try_write succeeds while a snapshot is held.
        assert!(shared.try_write().is_ok());
    }

    /// `try_read` does not wait for a writer making its change.
    #[test]
    fn try_read_gives_way_to_a_writer() {
        let shared = SharedSequence::new(Sequence::new_default());
        let edit = shared.write().unwrap();
        assert!(matches!(shared.try_read(), Err(TryLockError::WouldBlock)));
        drop(edit);
        assert!(shared.try_read().is_ok());
    }

    /// A writer copies only when a reader still holds the snapshot: with
    /// none held, the change is made in place.
    #[test]
    fn a_write_copies_only_when_a_snapshot_is_held() {
        let shared = SharedSequence::new(Sequence::new_default());
        let before = Arc::as_ptr(&shared.snapshot());
        shared.write().unwrap().name = "a".into();
        assert_eq!(Arc::as_ptr(&shared.snapshot()), before, "in place");
        let held = shared.snapshot();
        shared.write().unwrap().name = "b".into();
        assert_ne!(Arc::as_ptr(&shared.snapshot()), Arc::as_ptr(&held), "copied");
        assert_eq!(held.name, "a");
    }
}
