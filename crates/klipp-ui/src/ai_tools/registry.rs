//! UI-side wrapper around `klipp_ml::ModelRegistry`.
//!
//! ## Why a wrapper
//!
//! `klipp_ml::ModelRegistry` is the canonical, generic registry — it
//! has no opinion about Klipp's task-oriented UI surface. The AI Tools
//! panel thinks in tasks ("Remove Background", "Isolate Object") and
//! each task maps to a *required-capability mask*. This wrapper:
//!
//! 1. Holds a shared `Arc<RwLock<ModelRegistry>>` (same Arc the
//!    worker-side [`super::inference::RegistryHandle`] points at, so
//!    runtime install / uninstall / variant-switch reflects on both
//!    sides immediately),
//! 2. Translates `AiTask` → `MaskCapabilities` so the panel doesn't
//!    have to know that mapping,
//! 3. Returns owned `ModelInfo` from lookups so call sites don't fight
//!    the read-guard's lifetime.
//!
//! Real backend crates (`klipp-ml-candle`, `klipp-ml-ort`, future
//! tract / burn) take a `&mut ModelRegistry` write-lock and
//! `register(...)` against it — same path the stub takes at startup.

use std::sync::{Arc, RwLock};

use klipp_ml::{MaskCapabilities, ModelInfo, ModelRegistry, StubMaskGenerator};

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

/// Panel-facing view over the shared registry. Cheap to clone (just
/// bumps the Arc) so AppState can pass it through to render code by
/// value. Mutations go through the Arc directly via [`Self::shared`].
#[derive(Clone)]
pub struct AiToolsRegistry {
    inner: Arc<RwLock<ModelRegistry>>,
}

impl AiToolsRegistry {
    /// Wrap an already-shared registry. Used by `state::build_*`
    /// helpers so the panel and the worker point at the same Arc.
    pub fn from_shared(inner: Arc<RwLock<ModelRegistry>>) -> Self {
        Self { inner }
    }

    /// Build a fresh standalone registry with the stub generator
    /// registered. Convenience constructor for tests + early app
    /// startup; the production path constructs the shared Arc once
    /// in `state::build_shared_registry` and hands views to both
    /// sides.
    pub fn with_stub_registered() -> Self {
        let mut r = ModelRegistry::new();
        register_stub(&mut r);
        Self {
            inner: Arc::new(RwLock::new(r)),
        }
    }

    /// Build an empty registry with no models — used in tests that
    /// want to verify "no model installed" UX without the stub
    /// auto-satisfying the capability filter.
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ModelRegistry::new())),
        }
    }

    /// Clone the shared Arc — backend crates use this to register
    /// their own model factories (write-lock + `register`) and
    /// future install/uninstall flows mutate through it.
    pub fn shared(&self) -> Arc<RwLock<ModelRegistry>> {
        Arc::clone(&self.inner)
    }

    /// Models suitable for the given task — i.e. those whose
    /// capability bitset is a superset of [`required_capability`].
    /// Returns owned [`ModelInfo`] copies so callers don't carry
    /// the read guard around.
    pub fn models_for(&self, task: AiTask) -> Vec<ModelInfo> {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        g.list_capable(required_capability(task))
            .into_iter()
            .cloned()
            .collect()
    }

    /// Look up a model by its registered id, regardless of task.
    /// Used to render the "currently selected" model name.
    pub fn info(&self, id: &str) -> Option<ModelInfo> {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        g.info(id).cloned()
    }

    /// `true` when at least one model is registered for the task.
    /// The panel greys out task buttons that return `false` and
    /// surfaces a "no model installed" tooltip instead.
    pub fn has_model_for(&self, task: AiTask) -> bool {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        g.list_capable(required_capability(task))
            .into_iter()
            .next()
            .is_some()
    }

    /// `true` when an entry with this id exists. Cheap presence
    /// check that avoids cloning the [`ModelInfo`].
    pub fn has_id(&self, id: &str) -> bool {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        g.info(id).is_some()
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
    let info = klipp_ml::ModelInfo {
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
    fn has_id_returns_true_for_registered_stub() {
        let r = AiToolsRegistry::with_stub_registered();
        assert!(r.has_id(klipp_ml::STUB_GENERATOR_ID));
    }

    #[test]
    fn has_id_returns_false_for_unknown() {
        let r = AiToolsRegistry::with_stub_registered();
        assert!(!r.has_id("does-not-exist"));
    }

    #[test]
    fn from_shared_views_see_each_others_writes() {
        // The whole point of the refactor: a write through one view
        // becomes visible through another that shares the same Arc.
        let arc = Arc::new(RwLock::new(ModelRegistry::new()));
        let a = AiToolsRegistry::from_shared(Arc::clone(&arc));
        let b = AiToolsRegistry::from_shared(arc);

        assert!(!a.has_id(klipp_ml::STUB_GENERATOR_ID));
        assert!(!b.has_id(klipp_ml::STUB_GENERATOR_ID));

        // Mutate through view `a`'s shared Arc; observe through `b`.
        {
            let shared = a.shared();
            let mut g = shared.write().unwrap_or_else(|p| p.into_inner());
            register_stub(&mut g);
        }

        assert!(a.has_id(klipp_ml::STUB_GENERATOR_ID));
        assert!(b.has_id(klipp_ml::STUB_GENERATOR_ID));
    }
}
