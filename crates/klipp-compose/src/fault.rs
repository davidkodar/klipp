//! A test-only fault (#1276): a clip's grade run made to fail, as a node
//! graph that fails does, so what a render does after a failed run can be
//! held — a scene-referred layer refused part-way that must leave nothing
//! queued for the next, an export frame declined after its work. No
//! shipping clip fails its run on demand. Only the `fault-hook` feature
//! can set one, and only dev-dependencies turn it on
//! (`klipp-nodes/tests/fault_hook_feature.rs` holds every manifest to
//! that); without it, nothing faults.

use klipp_core::ClipId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Whether any fault is set: the cheap test every run makes.
static ANY: AtomicBool = AtomicBool::new(false);
/// The clips whose grade runs fail.
static CLIPS: Mutex<Vec<ClipId>> = Mutex::new(Vec::new());

/// Runs of `clip`'s grade fail until the guard is dropped.
#[cfg(feature = "fault-hook")]
pub fn fail_grade(clip: ClipId) -> Fault {
    let mut clips = CLIPS.lock().unwrap_or_else(|e| e.into_inner());
    clips.push(clip);
    ANY.store(true, Ordering::SeqCst);
    Fault { clip }
}

/// A fault set by [`fail_grade`]; dropped, it's taken off.
#[cfg(feature = "fault-hook")]
pub struct Fault {
    clip: ClipId,
}

#[cfg(feature = "fault-hook")]
impl Drop for Fault {
    fn drop(&mut self) {
        let mut clips = CLIPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = clips.iter().position(|c| *c == self.clip) {
            clips.remove(i);
        }
        ANY.store(!clips.is_empty(), Ordering::SeqCst);
    }
}

/// Whether `clip`'s grade run is to fail.
pub(crate) fn grade_fails(clip: ClipId) -> bool {
    if !ANY.load(Ordering::SeqCst) {
        return false;
    }
    CLIPS.lock().unwrap_or_else(|e| e.into_inner()).contains(&clip)
}
