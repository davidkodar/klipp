//! Route ALSA's own error messages into tracing instead of stderr.
//!
//! libasound prints straight to stderr whenever a PCM plugin it tries
//! cannot open — `pcm_dmix.c: unable to open slave`, `pcm_oss.c: Cannot
//! open device /dev/dsp` — which happens every time a device is
//! enumerated while another process holds the card. Those are expected
//! outcomes of asking, not faults: the enumeration goes on to whatever
//! does open. Printed raw they drown a test run's output and, in the app,
//! land on a stderr nobody reads while the log records nothing.
//!
//! ALSA offers two handler hooks. `snd_lib_error_set_handler` takes a
//! variadic function, which stable Rust cannot define. `snd_lib_error_set_local`
//! takes one with a `va_list` argument instead, which it can: the list is
//! an opaque pointer here, handed straight on to `vsnprintf`, which is
//! what libasound's own default handler does with it.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::cell::Cell;

type LocalHandler = unsafe extern "C" fn(*const c_char, c_int, *const c_char, c_int, *const c_char, *mut c_void);

extern "C" {
    fn snd_lib_error_set_local(func: Option<LocalHandler>) -> Option<LocalHandler>;
    fn vsnprintf(buf: *mut c_char, size: usize, fmt: *const c_char, args: *mut c_void) -> c_int;
    fn snd_strerror(errnum: c_int) -> *const c_char;
}

unsafe extern "C" fn to_tracing(file: *const c_char, line: c_int, func: *const c_char, err: c_int, fmt: *const c_char, args: *mut c_void) {
    let text = |p: *const c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
    let mut buf = [0 as c_char; 512];
    let n = if fmt.is_null() { 0 } else { vsnprintf(buf.as_mut_ptr(), buf.len(), fmt, args) };
    let message = if n > 0 { CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned() } else { String::new() };
    let reason = if err != 0 { format!(" ({})", text(snd_strerror(err))) } else { String::new() };
    tracing::debug!(target: "alsa", "{}:{} {}: {message}{reason}", text(file), line, text(func));
}

/// Install the handler on the calling thread, once per thread. The local
/// handler is thread-local inside alsa-lib (`__thread` in error.c), so it
/// has to be set on every thread that enumerates or opens a device — cpal
/// does both on the thread that asks. A process-wide `Once` installed it
/// on the first thread only, and every other thread's messages still went
/// to stderr, which is exactly what a test run showed.
pub fn install() {
    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    INSTALLED.with(|done| {
        if !done.replace(true) {
            unsafe {
                snd_lib_error_set_local(Some(to_tracing));
            }
        }
    });
}
