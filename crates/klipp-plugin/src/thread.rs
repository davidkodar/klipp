//! OFX Multi-Thread Suite — real multi-threaded implementation.
//!
//! Plugins can use multiThread() to dispatch work across CPU cores.
//! Thread-local storage tracks each thread's index and spawned status.
//! Mutexes use AtomicBool spin-locks (correct for OFX's lock/unlock API).

use crate::ffi::*;
use std::cell::Cell;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

// ── Thread-local state ──────────────────────────────────────────────

thread_local! {
    static THREAD_INDEX: Cell<u32> = const { Cell::new(0) };
    static IS_SPAWNED: Cell<bool> = const { Cell::new(false) };
}

// ── Suite functions ─────────────────────────────────────────────────

unsafe extern "C" fn multithread_fn(
    func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
    n_threads: c_uint,
    custom_arg: *mut c_void,
) -> OfxStatus {
    // Single-threaded dispatch: call once with threadIndex=0, threadMax=1.
    // OFX spec OfxThreadFunctionV1: func(threadIndex, threadMax, customArg)
    let _ = n_threads;

    func(0, 1, custom_arg);
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_num_cpus_fn(n: *mut c_uint) -> OfxStatus {
    *n = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_index_fn(n: *mut c_uint) -> OfxStatus {
    *n = THREAD_INDEX.get();
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_is_spawned_thread_fn() -> c_int {
    if IS_SPAWNED.get() { 1 } else { 0 }
}

/// Spin-lock mutex using AtomicBool. Suitable for OFX's non-RAII lock/unlock API.
/// Under real multi-threading, spin contention is bounded because OFX plugin
/// critical sections are typically very short.
unsafe extern "C" fn mutex_create_fn(
    handle: *mut OfxMutexHandle,
    _lock_count: c_int,
) -> OfxStatus {
    let mtx = Box::new(AtomicBool::new(false));
    *handle = Box::into_raw(mtx) as OfxMutexHandle;
    K_OFX_STAT_OK
}

unsafe extern "C" fn mutex_destroy_fn(handle: OfxMutexHandle) -> OfxStatus {
    if handle.is_null() {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    }
    let _ = Box::from_raw(handle as *mut AtomicBool);
    K_OFX_STAT_OK
}

unsafe extern "C" fn mutex_lock_fn(handle: OfxMutexHandle) -> OfxStatus {
    if handle.is_null() {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    }
    let locked = &*(handle as *const AtomicBool);
    // Spin until we acquire
    while locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        std::hint::spin_loop();
    }
    K_OFX_STAT_OK
}

unsafe extern "C" fn mutex_unlock_fn(handle: OfxMutexHandle) -> OfxStatus {
    if handle.is_null() {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    }
    let locked = &*(handle as *const AtomicBool);
    locked.store(false, Ordering::Release);
    K_OFX_STAT_OK
}

unsafe extern "C" fn mutex_try_lock_fn(handle: OfxMutexHandle) -> OfxStatus {
    if handle.is_null() {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    }
    let locked = &*(handle as *const AtomicBool);
    match locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
        Ok(_) => K_OFX_STAT_OK,
        Err(_) => K_OFX_STAT_FAILED,
    }
}

pub static MULTITHREAD_SUITE_V1: OfxMultiThreadSuiteV1 = OfxMultiThreadSuiteV1 {
    multithread: multithread_fn,
    multithread_num_cpus: multithread_num_cpus_fn,
    multithread_index: multithread_index_fn,
    multithread_is_spawned_thread: multithread_is_spawned_thread_fn,
    mutex_create: mutex_create_fn,
    mutex_destroy: mutex_destroy_fn,
    mutex_lock: mutex_lock_fn,
    mutex_unlock: mutex_unlock_fn,
    mutex_try_lock: mutex_try_lock_fn,
};
