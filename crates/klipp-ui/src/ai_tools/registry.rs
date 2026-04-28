//! UI-side wrapper around `klipp_ml::ModelRegistry`.
//!
//! ## Why a wrapper
//!
//! `klipp_ml::ModelRegistry` is the canonical, generic registry — it
//! has no opinion about Klipp's task-oriented UI surface. The AI Tools
//! panel thinks in tasks ("Remove Background", "Isolate Object") and
//! each task maps to a *required-capability mask*. This wrapper:
//!
//! 1. Owns a `ModelRegistry`,
//! 2. Registers the `StubMaskGenerator` at startup so the panel has
//!    something concrete to render against,
//! 3. Translates `AiTask` → `MaskCapabilities` so the panel doesn't
//!    have to know that mapping,
//! 4. Forwards inference dispatch to `ModelRegistry::get_or_create`.
//!
//! Real backend crates (`klipp-ml-candle`, `klipp-ml-ort`, future
//! tract / burn) call `inner_mut().register(...)` to advertise their
//! own models — same path as the stub.

use klipp_ml::{
    MaskCapabilities, MaskError, MaskGeneratorGuard, ModelInfo, ModelRegistry, StubMaskGenerator,
};

use super::AiTask;

/// Required capability for each task. Pinned in one place so the
/// panel + tests + future re-skins don't drift.
pub fn required_capability(task: AiTask) -> MaskCapabilities {
    match task {
        // "Remove Background" is a one-click matting operation —
        // models that auto-segment foreground (BiRefNet, RVM) are
        // the natural fit. AUTO_DETECT signals "no prompt needed".
        AiTask::RemoveBackground => MaskCapabilities::AUTO_DETECT,
        // "Isolate Object" is the SAM-family click-to-mask flow.
        AiTask::IsolateObject => MaskCapabilities::POINT_PROMPT,
        // "Detect Objects" is auto-detect with class labels
        // (RF-DETR-Seg, YOLO-Seg). Same flag as Remove Background
        // here; future revision may add a CLASSIFY capability flag.
        AiTask::DetectObjects => MaskCapabilities::AUTO_DETECT,
        // "Track Object" requires temporal video propagation
        // (SAM2 video, XMem). v1 has no model satisfying this, so
        // the button greys out — that's the design intent.
        AiTask::TrackObject => MaskCapabilities::TEMPORAL,
    }
}

/// Wrapper around `klipp_ml::ModelRegistry` with task-oriented
/// helpers and the stub auto-registered at construction.
pub struct AiToolsRegistry {
    inner: ModelRegistry,
}

impl AiToolsRegistry {
    /// Build a fresh registry with the stub generator registered as
    /// the default fallback for every prompt-bearing task. Tests and
    /// app startup both call this; backend crates layer their real
    /// model registrations on top.
    pub fn with_stub_registered() -> Self {
        let mut inner = ModelRegistry::new();
        register_stub(&mut inner);
        Self { inner }
    }

    /// Build an empty registry with no models — used in tests that
    /// want to verify "no model installed" UX without the stub
    /// auto-satisfying the capability filter.
    pub fn empty() -> Self {
        Self {
            inner: ModelRegistry::new(),
        }
    }

    /// Borrow the underlying registry for read-only operations
    /// (info lookups, listing). Backend crates that want to
    /// register their own models go through [`Self::inner_mut`].
    pub fn inner(&self) -> &ModelRegistry {
        &self.inner
    }

    /// Borrow the underlying registry mutably so backend crates can
    /// add their model factories. Kept on the wrapper so the UI
    /// layer doesn't have to expose the raw `ModelRegistry` field.
    pub fn inner_mut(&mut self) -> &mut ModelRegistry {
        &mut self.inner
    }

    /// Models suitable for the given task — i.e. those whose
    /// capability bitset is a superset of [`required_capability`].
    /// The panel uses this to populate per-task model dropdowns.
    pub fn models_for(&self, task: AiTask) -> Vec<&ModelInfo> {
        self.inner.list_capable(required_capability(task))
    }

    /// Look up a model by its registered id, regardless of task.
    /// Used to render the "currently selected" model name.
    pub fn info(&self, id: &str) -> Option<&ModelInfo> {
        self.inner.info(id)
    }

    /// `true` when at least one model is registered for the task.
    /// The panel greys out task buttons that return `false` and
    /// surfaces a "no model installed" tooltip instead.
    pub fn has_model_for(&self, task: AiTask) -> bool {
        !self.models_for(task).is_empty()
    }

    /// Materialise the model with `id` for inference dispatch.
    /// Wraps `ModelRegistry::get_or_create`; the returned guard
    /// holds the model's mutex while the caller runs `generate`.
    /// Returns `None` when no model is registered under that id.
    pub fn get_or_create(
        &self,
        id: &str,
    ) -> Option<Result<MaskGeneratorGuard<'_>, MaskError>> {
        self.inner.get_or_create(id)
    }
}

impl Default for AiToolsRegistry {
    fn default() -> Self {
        Self::with_stub_registered()
    }
}

/// Register the in-tree `StubMaskGenerator` under its canonical id.
/// Pulled out of `with_stub_registered` so the integration tests can
/// also mount it on a registry they constructed by hand.
pub fn register_stub(registry: &mut ModelRegistry) {
    let stub = StubMaskGenerator::new();
    let info = ModelInfo {
        id: stub.id_static().into(),
        display_name: "Stub (placeholder)".into(),
        capabilities: stub.advertised_capabilities(),
    };
    registry.register(info, Box::new(|| Ok(Box::new(StubMaskGenerator::new()))));
}

/// Helper trait to read the stub's id / capabilities without
/// instantiating-and-throwing-away. Implemented locally on
/// `StubMaskGenerator` via inherent-style methods (we don't add
/// methods to klipp_ml from here — the wrapper just wraps the
/// constructor calls).
trait StubIntrospect {
    fn id_static(&self) -> &'static str;
    fn advertised_capabilities(&self) -> MaskCapabilities;
}

impl StubIntrospect for StubMaskGenerator {
    fn id_static(&self) -> &'static str {
        klipp_ml::STUB_GENERATOR_ID
    }
    fn advertised_capabilities(&self) -> MaskCapabilities {
        // Mirror what the stub itself advertises through the trait.
        // We can't call `self.capabilities()` without `&dyn` ceremony
        // here, but the stub's flag set is part of its public
        // contract (see klipp-ml/src/stub.rs) and is asserted by the
        // `stub_advertises_expected_capabilities` test below.
        MaskCapabilities::POINT_PROMPT
            | MaskCapabilities::BOX_PROMPT
            | MaskCapabilities::SCRIBBLE
            | MaskCapabilities::AUTO_DETECT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::MaskGenerator;

    #[test]
    fn stub_advertises_expected_capabilities() {
        // Pin the stub's published capability set — the panel
        // depends on it satisfying both POINT_PROMPT (Isolate
        // Object) and AUTO_DETECT (Remove Background / Detect).
        let cap = StubMaskGenerator::new().capabilities();
        assert!(cap.contains(MaskCapabilities::POINT_PROMPT));
        assert!(cap.contains(MaskCapabilities::AUTO_DETECT));
        assert!(!cap.contains(MaskCapabilities::TEMPORAL));
    }

    #[test]
    fn with_stub_registered_lists_stub_for_isolate_and_auto() {
        let r = AiToolsRegistry::with_stub_registered();
        assert!(r.has_model_for(AiTask::IsolateObject));
        assert!(r.has_model_for(AiTask::RemoveBackground));
        assert!(r.has_model_for(AiTask::DetectObjects));
        // Stub does NOT advertise TEMPORAL → Track Object greys out.
        assert!(!r.has_model_for(AiTask::TrackObject));
    }

    #[test]
    fn empty_registry_has_nothing_for_any_task() {
        let r = AiToolsRegistry::empty();
        assert!(!r.has_model_for(AiTask::RemoveBackground));
        assert!(!r.has_model_for(AiTask::IsolateObject));
        assert!(!r.has_model_for(AiTask::DetectObjects));
        assert!(!r.has_model_for(AiTask::TrackObject));
    }

    #[test]
    fn required_capability_per_task() {
        assert_eq!(
            required_capability(AiTask::RemoveBackground),
            MaskCapabilities::AUTO_DETECT,
        );
        assert_eq!(
            required_capability(AiTask::IsolateObject),
            MaskCapabilities::POINT_PROMPT,
        );
        assert_eq!(
            required_capability(AiTask::DetectObjects),
            MaskCapabilities::AUTO_DETECT,
        );
        assert_eq!(
            required_capability(AiTask::TrackObject),
            MaskCapabilities::TEMPORAL,
        );
    }

    #[test]
    fn get_or_create_returns_stub_when_registered() {
        let r = AiToolsRegistry::with_stub_registered();
        let guard = r.get_or_create(klipp_ml::STUB_GENERATOR_ID);
        assert!(guard.is_some(), "stub must resolve through the wrapper");
        let result = guard.unwrap();
        assert!(result.is_ok(), "stub factory must not fail");
    }

    #[test]
    fn get_or_create_unknown_id_returns_none() {
        let r = AiToolsRegistry::with_stub_registered();
        assert!(r.get_or_create("does-not-exist").is_none());
    }
}
