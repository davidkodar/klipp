//! OFX Multi-Thread Suite — single-threaded implementation.
//!
//! OFX guarantees single-threaded action dispatch. We report 1 CPU and
//! call plugin thread functions once with index 0. Mutexes use real
//! Rust Mutexes behind opaque handles for correctness.

use crate::ffi::*;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

unsafe extern "C" fn multithread_fn(
    func: unsafe extern "C" fn(*mut c_void, c_uint, c_uint),
    _n_threads: c_uint,
    custom_arg: *mut c_void,
) -> OfxStatus {
    // Single-threaded: call once with threadIndex=0, threadMax=1
    // OFX spec: func(customArg, threadIndex, threadMax)
    func(custom_arg, 0, 1);
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_num_cpus_fn(n: *mut c_uint) -> OfxStatus {
    *n = 1;
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_index_fn(n: *mut c_uint) -> OfxStatus {
    *n = 0;
    K_OFX_STAT_OK
}

unsafe extern "C" fn multithread_is_spawned_thread_fn() -> c_int {
    0 // never spawned — always main thread
}

/// Simple spin-lock mutex using AtomicBool. OFX is single-threaded
/// during action dispatch so this is sufficient and avoids guard lifetime issues.
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
    // Spin until we acquire (single-threaded, so this is immediate)
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
