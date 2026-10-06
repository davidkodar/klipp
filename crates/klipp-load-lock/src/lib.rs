#![forbid(unsafe_code)]
//! One lock around loading a shared library and starting a GPU (#1032).
//!
//! A crate of its own, below every crate that loads a library: the
//! frei0r host sits under `klipp-core`, so the lock cannot live there.
//!
//! Loading a plug-in runs its library's start-up code with the dynamic
//! linker's lock held. A frei0r plug-in that links NVIDIA's OpenGL calls
//! into the driver from there, and the driver waits on a lock of its own.
//! Creating a Vulkan instance on another thread at the same moment holds
//! that driver lock and asks the linker for a symbol, so each thread
//! waits on the other and the process hangs, without a word, at launch on
//! a cold plug-in cache or wherever the two meet.
//!
//! Every plug-in or driver library Klipp opens, and every GPU it starts,
//! does so holding [`library_loading`], so the two never overlap. The UI's
//! own GPU is started by the windowing toolkit before any plug-in loads.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(());
static TAKEN: AtomicUsize = AtomicUsize::new(0);

/// Hold this while opening or closing a plug-in or driver library (closing
/// runs its destructors with the linker's lock held, as opening runs its
/// initialisers), or while creating a GPU instance, adapter or device. Not
/// re-entrant: take it around the load itself, not around code that may
/// load something else.
pub fn library_loading() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TAKEN.fetch_add(1, Ordering::Relaxed);
    guard
}

/// How many times the lock has been taken in this process, so a test can
/// tell that a path takes it.
pub fn times_taken() -> usize {
    TAKEN.load(Ordering::Relaxed)
}

/// Run `load` holding [`library_loading`].
pub fn while_loading<T>(load: impl FnOnce() -> T) -> T {
    let _loading = library_loading();
    load()
}
