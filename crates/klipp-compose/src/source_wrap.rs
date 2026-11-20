//! One place a frame source is wrapped before a render reads it: how a
//! clip shown through a Veylvik composition gets its picture in every
//! render (the viewer, export, stills, scopes), with no call site of its
//! own to forget. Installed once, by the app; a render without one, or
//! with nothing for it to do, reads the source as it is.

use std::cell::Cell;
use std::sync::{Arc, OnceLock};

use klipp_core::WorkingColorspace;

use crate::FrameSource;

/// What wraps a render's frame source.
pub trait SourceWrap: Send + Sync {
    /// Whether renders now need wrapping: `false` and the source is read
    /// as it is, at no cost.
    fn active(&self) -> bool;
    /// `inner` as a render in `working` reads it.
    fn wrap<'a>(&self, inner: &'a mut dyn FrameSource, working: WorkingColorspace) -> Box<dyn FrameSource + 'a>;
}

static WRAP: OnceLock<Arc<dyn SourceWrap>> = OnceLock::new();

/// Wrap every render's frame source with `wrap` from now on. Once per
/// process: a second call is refused, with `false`.
pub fn install(wrap: Arc<dyn SourceWrap>) -> bool {
    WRAP.set(wrap).is_ok()
}

thread_local! {
    /// A render inside a render (a track layer, `render` over
    /// `render_inspected`) reads the source already wrapped: wrapped twice,
    /// a composition's picture would be taken for its footage again.
    static WRAPPING: Cell<bool> = const { Cell::new(false) };
}

/// A render that wraps its source, until it ends.
pub(crate) struct Wrapping(Arc<dyn SourceWrap>);

impl Wrapping {
    /// The wrapper, when this render should wrap its source: one is
    /// installed and active, and this render isn't inside another.
    pub(crate) fn begin() -> Option<Self> {
        let wrap = WRAP.get().filter(|w| w.active())?;
        if WRAPPING.with(Cell::get) {
            return None;
        }
        WRAPPING.with(|c| c.set(true));
        Some(Self(Arc::clone(wrap)))
    }

    pub(crate) fn wrap<'a>(&self, inner: &'a mut dyn FrameSource, working: WorkingColorspace) -> Box<dyn FrameSource + 'a> {
        self.0.wrap(inner, working)
    }
}

impl Drop for Wrapping {
    fn drop(&mut self) {
        WRAPPING.with(|c| c.set(false));
    }
}

/// `$src` as this render reads it: wrapped, when it should be, for the
/// rest of the enclosing block.
macro_rules! wrapped_source {
    ($self:ident, $src:ident) => {
        let wrapping = crate::source_wrap::Wrapping::begin();
        let mut held;
        let $src: &mut dyn FrameSource = match &wrapping {
            Some(w) => {
                held = w.wrap($src, $self.working);
                &mut *held
            }
            None => $src,
        };
    };
}
pub(crate) use wrapped_source;
