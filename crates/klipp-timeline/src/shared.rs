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
            self.touched = true;
        }
        Arc::make_mut(&mut self.current.sequence)
    }
}

impl SharedSequence {
    pub fn new(seq: Sequence) -> Self {
        Self { current: Mutex::new(Current { sequence: Arc::new(seq), revision: SequenceRevision::next(), preview: None }) }
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

    /// The sequence for a change. Waits only for another writer.
    pub fn write(&self) -> LockResult<SequenceWrite<'_>> {
        Ok(SequenceWrite { current: self.lock(), touched: false })
    }

    /// [`Self::write`], or `WouldBlock` while another writer has it.
    pub fn try_write(&self) -> TryLockResult<SequenceWrite<'_>> {
        match self.current.try_lock() {
            Ok(g) => Ok(SequenceWrite { current: g, touched: false }),
            Err(TryLockError::Poisoned(e)) => Ok(SequenceWrite { current: e.into_inner(), touched: false }),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// Put `seq` in place whole: what a publish of an edited sequence does.
    pub fn replace(&self, seq: Sequence) {
        *self.lock() = Current { sequence: Arc::new(seq), revision: SequenceRevision::next(), preview: None };
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
