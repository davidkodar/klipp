//! Shared values read after a panic elsewhere (#3181): the media pool's
//! writers replace it whole, so a lock poisoned by a panic still holds a
//! whole pool. A reader takes it as it is; it used to answer an agent with
//! an error, or with nothing ("no clips"), on every call until a restart.

/// A lock's value whether or not a panic poisoned it.
pub(crate) trait Recover<G> {
    fn recovered(self) -> G;
}

impl<G> Recover<G> for std::sync::LockResult<G> {
    fn recovered(self) -> G {
        self.unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
