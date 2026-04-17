//! OFX Memory Suite — host-managed allocation for plugins.
//!
//! # Safety conventions
//!
//! `memory_alloc_fn` allocates a zeroed heap block via `std::alloc`
//! and records the matching `Layout` in `ALLOC_TRACKER`, keyed by
//! the returned pointer as usize. `memory_free_fn` looks up the
//! stored layout and calls `dealloc` — this pairs the allocation
//! and deallocation with identical layouts, which is the
//! correctness requirement for `std::alloc`.
//!
//! A plugin that passes a pointer we didn't allocate (or freed
//! already) will miss the HashMap lookup and get
//! `K_OFX_STAT_ERR_BAD_HANDLE` rather than a double-free. A plugin
//! that forgets to free its allocations leaks bytes proportional to
//! the unreleased set — no UB, just a slow leak.

use crate::ffi::*;
use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

/// Track allocations so we can free with the correct layout.
static ALLOC_TRACKER: LazyLock<Mutex<HashMap<usize, Layout>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

unsafe extern "C" fn memory_alloc_fn(
    _handle: *mut c_void,
    n_bytes: usize,
    ptr: *mut *mut c_void,
) -> OfxStatus {
    if n_bytes == 0 {
        *ptr = std::ptr::null_mut();
        return K_OFX_STAT_OK;
    }
    let layout = match Layout::from_size_align(n_bytes, 16) {
        Ok(l) => l,
        Err(_) => return K_OFX_STAT_ERR_MEMORY,
    };
    let p = std::alloc::alloc_zeroed(layout);
    if p.is_null() {
        return K_OFX_STAT_ERR_MEMORY;
    }
    ALLOC_TRACKER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p as usize, layout);
    *ptr = p as *mut c_void;
    K_OFX_STAT_OK
}

unsafe extern "C" fn memory_free_fn(ptr: *mut c_void) -> OfxStatus {
    if ptr.is_null() {
        return K_OFX_STAT_OK;
    }
    let key = ptr as usize;
    if let Some(layout) = ALLOC_TRACKER.lock().unwrap_or_else(|e| e.into_inner()).remove(&key) {
        std::alloc::dealloc(ptr as *mut u8, layout);
        K_OFX_STAT_OK
    } else {
        K_OFX_STAT_ERR_BAD_HANDLE
    }
}

pub static MEMORY_SUITE_V1: OfxMemorySuiteV1 = OfxMemorySuiteV1 {
    memory_alloc: memory_alloc_fn,
    memory_free: memory_free_fn,
};
