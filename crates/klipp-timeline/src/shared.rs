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
//! from that snapshot holding nothing. A writer takes the same short lock
//! and changes the sequence copy-on-write: if a reader still holds the old
//! snapshot, the writer gets its own copy rather than waiting for it, and
//! the reader finishes its frame from the sequence as it was. Neither ever
//! waits for the other's work.
//!
//! The methods keep the shape of `RwLock`'s (`read`, `write`, `try_read`,
//! `try_write`, results with a poison error the caller can recover from),
//! so the code that used the lock reads the same. A poisoned lock is
//! recovered at once: the snapshot itself cannot be half-written.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult};

use crate::Sequence;

/// See the module docs.
#[derive(Debug)]
pub struct SharedSequence {
    current: Mutex<Arc<Sequence>>,
}

/// The sequence as it was when read: holds no lock.
#[derive(Debug, Clone)]
pub struct SequenceSnapshot(Arc<Sequence>);

impl Deref for SequenceSnapshot {
    type Target = Sequence;
    fn deref(&self) -> &Sequence {
        &self.0
    }
}

impl SequenceSnapshot {
    /// The snapshot as an `Arc`, to keep or hand on.
    pub fn into_arc(self) -> Arc<Sequence> {
        self.0
    }
}

/// The sequence open for a change: holds the short lock, and copies the
/// sequence on its first change if a reader still holds the snapshot.
#[derive(Debug)]
pub struct SequenceWrite<'a>(MutexGuard<'a, Arc<Sequence>>);

impl Deref for SequenceWrite<'_> {
    type Target = Sequence;
    fn deref(&self) -> &Sequence {
        &self.0
    }
}

impl DerefMut for SequenceWrite<'_> {
    fn deref_mut(&mut self) -> &mut Sequence {
        Arc::make_mut(&mut self.0)
    }
}

impl SharedSequence {
    pub fn new(seq: Sequence) -> Self {
        Self { current: Mutex::new(Arc::new(seq)) }
    }

    fn lock(&self) -> MutexGuard<'_, Arc<Sequence>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The sequence as it stands, held without a lock.
    pub fn snapshot(&self) -> Arc<Sequence> {
        Arc::clone(&self.lock())
    }

    /// [`Self::snapshot`] in `RwLock::read`'s shape. Never an error.
    pub fn read(&self) -> LockResult<SequenceSnapshot> {
        Ok(SequenceSnapshot(self.snapshot()))
    }

    /// As [`Self::read`]: a read never has to wait for a writer's work, so
    /// there is nothing to try.
    pub fn try_read(&self) -> TryLockResult<SequenceSnapshot> {
        Ok(SequenceSnapshot(self.snapshot()))
    }

    /// The sequence for a change. Waits only for another writer.
    pub fn write(&self) -> LockResult<SequenceWrite<'_>> {
        Ok(SequenceWrite(self.lock()))
    }

    /// [`Self::write`], or `WouldBlock` while another writer has it.
    pub fn try_write(&self) -> TryLockResult<SequenceWrite<'_>> {
        match self.current.try_lock() {
            Ok(g) => Ok(SequenceWrite(g)),
            Err(TryLockError::Poisoned(e)) => Ok(SequenceWrite(e.into_inner())),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// Put `seq` in place whole: what a publish of an edited sequence does.
    pub fn replace(&self, seq: Sequence) {
        *self.lock() = Arc::new(seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
