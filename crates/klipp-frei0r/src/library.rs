//! A frei0r library opened and closed under the load lock (#1032).
//!
//! Opening a library runs its initialisers and closing it runs its
//! destructors, both with the dynamic linker's lock held; a plug-in that
//! links NVIDIA's OpenGL calls into the driver from either, which
//! deadlocks against a GPU starting on another thread. Holding the library
//! in this type makes every close take the load lock, on every path out:
//! a plug-in that opens and is used, and one rejected for a missing symbol
//! or a failed `f0r_init`.

use std::path::Path;

pub(crate) struct GuardedLibrary(Option<libloading::Library>);

impl GuardedLibrary {
    /// Open `path` under the load lock.
    pub(crate) fn open(path: &Path) -> Result<Self, libloading::Error> {
        // SAFETY: dlopen of an arbitrary .so is inherently unsafe. Same
        // contract the OFX and LADSPA hosts already accept.
        klipp_load_lock::while_loading(|| unsafe { libloading::Library::new(path) }).map(|lib| Self(Some(lib)))
    }

    /// The open library.
    pub(crate) fn get(&self) -> &libloading::Library {
        self.0.as_ref().expect("a GuardedLibrary is open until it drops")
    }
}

impl Drop for GuardedLibrary {
    fn drop(&mut self) {
        if let Some(lib) = self.0.take() {
            let _closing = klipp_load_lock::library_loading();
            drop(lib);
        }
    }
}
