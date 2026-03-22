//! OFX Message + Progress Suites — plugin notifications logged via tracing.

use crate::ffi::*;
use std::ffi::{c_char, c_double, c_void, CStr};

// ── Message Suite ────────────────────────────────────────────────────

unsafe extern "C" fn message_fn(
    _handle: *mut c_void,
    msg_type: *const c_char,
    _msg_id: *const c_char,
    format: *const c_char,
) -> OfxStatus {
    let type_str = CStr::from_ptr(msg_type).to_string_lossy();
    let msg_str = CStr::from_ptr(format).to_string_lossy();
    tracing::info!("[OFX Plugin {}] {}", type_str, msg_str);
    K_OFX_STAT_OK
}

pub static MESSAGE_SUITE_V1: OfxMessageSuiteV1 = OfxMessageSuiteV1 {
    message: message_fn,
};

// ── Progress Suite ───────────────────────────────────────────────────

unsafe extern "C" fn progress_start_fn(
    _handle: *mut c_void,
    label: *const c_char,
) -> OfxStatus {
    let label = CStr::from_ptr(label).to_string_lossy();
    tracing::debug!("[OFX Progress] Start: {}", label);
    K_OFX_STAT_OK
}

unsafe extern "C" fn progress_update_fn(
    _handle: *mut c_void,
    progress: c_double,
) -> OfxStatus {
    tracing::debug!("[OFX Progress] {:.0}%", progress * 100.0);
    K_OFX_STAT_OK
}

unsafe extern "C" fn progress_end_fn(_handle: *mut c_void) -> OfxStatus {
    tracing::debug!("[OFX Progress] End");
    K_OFX_STAT_OK
}

pub static PROGRESS_SUITE_V1: OfxProgressSuiteV1 = OfxProgressSuiteV1 {
    progress_start: progress_start_fn,
    progress_update: progress_update_fn,
    progress_end: progress_end_fn,
};
