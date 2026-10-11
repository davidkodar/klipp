//! A test-only fault (M4 step 10's readiness cases): a built-in effect's
//! run made to fail, so the render-failure decline can be held. A built-in
//! kernel has no failure of its own to provoke; with a fault set, the node
//! passes its picture on without the effect and declines as a render that
//! failed, which an export refuses. Only the `fault-hook` feature can set
//! one, and only dev-dependencies and the readiness crate turn it on
//! (`tests/fault_hook_feature.rs`); without it, nothing faults.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Whether any fault is set: the cheap test every run makes.
static ANY: AtomicBool = AtomicBool::new(false);
/// The effect tags (`"deflicker"`, `"speed_ramp"`) whose runs fail.
static TAGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Runs of the effect tagged `tag` fail until the guard is dropped.
#[cfg(feature = "fault-hook")]
pub fn fail(tag: &str) -> Fault {
    let mut tags = TAGS.lock().unwrap_or_else(|e| e.into_inner());
    tags.push(tag.to_owned());
    ANY.store(true, Ordering::SeqCst);
    Fault { tag: tag.to_owned() }
}

/// A fault set by [`fail`]; dropped, it's taken off.
#[cfg(feature = "fault-hook")]
pub struct Fault {
    tag: String,
}

#[cfg(feature = "fault-hook")]
impl Drop for Fault {
    fn drop(&mut self) {
        let mut tags = TAGS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = tags.iter().position(|t| *t == self.tag) {
            tags.remove(i);
        }
        ANY.store(!tags.is_empty(), Ordering::SeqCst);
    }
}

/// Whether `effect`'s run is to fail.
pub(crate) fn faults(effect: &klipp_effects::BuiltinEffect) -> bool {
    if !ANY.load(Ordering::SeqCst) {
        return false;
    }
    let tag = serde_json::to_value(effect).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned));
    let tags = TAGS.lock().unwrap_or_else(|e| e.into_inner());
    tag.is_some_and(|t| tags.contains(&t))
}
