//! Runtime-discoverable model registry.
//!
//! The UI never imports specific backend crates — it asks the
//! registry for a model by id and gets back a `Box<dyn MaskGenerator>`.
//! Backend crates (`klipp-ml-candle`, `klipp-ml-ort`, future tract /
//! burn) call [`ModelRegistry::register`] at startup to advertise
//! their capabilities.
//!
//! ## Lazy instantiation
//!
//! Registration stores a *factory closure*, not a built model. The
//! actual model (with weights + GPU device + memory pinned) is only
//! materialised when the user first asks for it via
//! [`ModelRegistry::get_or_create`]. This keeps Klipp's startup time
//! small even when many model implementations are compiled in.
//!
//! ## Capability filtering
//!
//! UI code that wants to show only models supporting a particular
//! prompt type uses [`ModelRegistry::list_capable`], e.g.:
//!
//! ```ignore
//! let auto_detect_models = registry.list_capable(MaskCapabilities::AUTO_DETECT);
//! ```

use std::collections::HashMap;
use std::sync::Mutex;

use crate::mask_generator::{MaskCapabilities, MaskError, MaskGenerator};

/// Factory closure stored at registration. Returns a fresh model
/// instance — the registry calls this once per id, caches the
/// resulting Box on first request, and reuses it after.
pub type MaskGeneratorFactory =
    Box<dyn Fn() -> Result<Box<dyn MaskGenerator>, MaskError> + Send + Sync>;

/// Lightweight metadata kept against each registration so capability
/// filtering doesn't require materialising the model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub capabilities: MaskCapabilities,
}

struct Entry {
    info: ModelInfo,
    factory: MaskGeneratorFactory,
    /// Materialised model, populated on first `get_or_create` call.
    /// `Mutex` because `MaskGenerator::generate` takes `&mut self`
    /// and we want shared access from multiple call sites without
    /// each having to lock externally.
    instance: Mutex<Option<Box<dyn MaskGenerator>>>,
}

/// Process-wide registry of `MaskGenerator` implementations. Not a
/// global singleton — Klipp creates one in its app initialization
/// and threads it through. Tests can construct fresh registries
/// without polluting global state.
pub struct ModelRegistry {
    entries: HashMap<String, Entry>,
}

impl ModelRegistry {
    /// Create an empty registry. Backend crates' init code calls
    /// `register` to populate.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Advertise a model. `info` declares stable id, display name,
    /// and capabilities (UI filters on these without instantiating).
    /// `factory` is the actual constructor — called lazily on first
    /// `get_or_create`.
    ///
    /// Re-registering an id replaces the previous entry (warning
    /// logged). This lets a user override a built-in registration
    /// from their own plugin code.
    pub fn register(&mut self, info: ModelInfo, factory: MaskGeneratorFactory) {
        if self.entries.contains_key(&info.id) {
            tracing::warn!(
                "klipp-ml: replacing previously-registered MaskGenerator '{}'",
                info.id,
            );
        }
        let id = info.id.clone();
        self.entries.insert(
            id,
            Entry {
                info,
                factory,
                instance: Mutex::new(None),
            },
        );
    }

    /// Look up the metadata for a registered model without
    /// instantiating it.
    pub fn info(&self, id: &str) -> Option<&ModelInfo> {
        self.entries.get(id).map(|e| &e.info)
    }

    /// Iterate over every registered model's metadata. Used by the
    /// UI's model picker to enumerate options.
    pub fn iter_info(&self) -> impl Iterator<Item = &ModelInfo> {
        self.entries.values().map(|e| &e.info)
    }

    /// Filter registered models by required capabilities.
    /// `required` may be a single flag or an OR-combination — every
    /// returned model's capabilities is a SUPERSET of `required`.
    pub fn list_capable(&self, required: MaskCapabilities) -> Vec<&ModelInfo> {
        self.entries
            .values()
            .filter(|e| e.info.capabilities.contains(required))
            .map(|e| &e.info)
            .collect()
    }

    /// Drop an entry entirely — `info`, factory closure, and any
    /// materialised generator instance all go away. Used by the
    /// custom-model uninstall flow so the panel dropdown reflects
    /// the removal immediately rather than carrying a stale
    /// registration that would only fail on next dispatch (clean
    /// `BackendInit` error, but still surfaces the model in the
    /// picker which is confusing).
    ///
    /// Built-in entries from the manifest stay registered across
    /// per-row Uninstall — only the file is removed in that path,
    /// keeping the entry visible so the dialog can offer Download
    /// again. `clear_instance` is the right primitive there.
    ///
    /// Returns `true` when the entry existed and was removed.
    pub fn unregister(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }

    /// Drop the cached generator for `id`. The next
    /// [`get_or_create`](Self::get_or_create) call will rebuild from
    /// the factory closure. Used by uninstall flows so a freshly
    /// re-downloaded weights file isn't shadowed by an in-memory
    /// generator that's still pointing at a now-deleted path.
    /// Returns `true` if an entry exists under that id, `false`
    /// otherwise. The factory closure stays registered — only the
    /// materialised instance is cleared.
    pub fn clear_instance(&self, id: &str) -> bool {
        let Some(entry) = self.entries.get(id) else {
            return false;
        };
        let mut slot = entry.instance.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
        true
    }

    /// Materialise (or reuse) the model for `id`. Calls the factory
    /// on first request, caches the resulting Box, returns a guard
    /// that the caller can call `generate` through.
    ///
    /// Returns `None` when no model is registered under that id —
    /// callers surface a "no model registered" toast in that case.
    pub fn get_or_create(&self, id: &str) -> Option<Result<MaskGeneratorGuard<'_>, MaskError>> {
        let entry = self.entries.get(id)?;
        let mut slot = entry.instance.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            match (entry.factory)() {
                Ok(model) => *slot = Some(model),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok(MaskGeneratorGuard { guard: slot }))
    }
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII handle returned by [`ModelRegistry::get_or_create`]. Holds
/// the registry's internal mutex while the caller runs `generate`,
/// then releases on drop. Callers chain calls per-frame; the lock
/// only blocks if two threads try to use the SAME model concurrently
/// — different models lock independently.
pub struct MaskGeneratorGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<Box<dyn MaskGenerator>>>,
}

impl<'a> MaskGeneratorGuard<'a> {
    /// Borrow the model mutably. Callers use this to invoke
    /// `generate(req)` once they have the guard.
    pub fn as_mut(&mut self) -> &mut dyn MaskGenerator {
        self.guard
            .as_mut()
            .expect("instance must be Some after get_or_create")
            .as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask_generator::{
        DevicePreference, InputFrame, MaskBuffer, MaskCandidate, MaskRequest, MaskResponse,
    };

    /// Trivial test impl — counts how often `generate` is called so
    /// we can verify the factory is invoked exactly once.
    struct CountingGenerator {
        id: String,
        calls: std::sync::atomic::AtomicU64,
    }

    impl MaskGenerator for CountingGenerator {
        fn id(&self) -> &str {
            &self.id
        }
        fn display_name(&self) -> &str {
            "Counter"
        }
        fn capabilities(&self) -> MaskCapabilities {
            MaskCapabilities::POINT_PROMPT
        }
        fn preferred_device(&self) -> DevicePreference {
            DevicePreference::Cpu
        }
        fn generate(&mut self, _req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(MaskResponse {
                candidates: vec![MaskCandidate {
                    mask: MaskBuffer::empty(1, 1),
                    confidence: 1.0,
                    bbox: None,
                    label: None,
                }],
                inference_ms: 0,
            })
        }
    }

    fn build_registry_with_one_model() -> ModelRegistry {
        let mut r = ModelRegistry::new();
        r.register(
            ModelInfo {
                id: "counter".into(),
                display_name: "Counter".into(),
                capabilities: MaskCapabilities::POINT_PROMPT,
            },
            Box::new(|| {
                Ok(Box::new(CountingGenerator {
                    id: "counter".into(),
                    calls: std::sync::atomic::AtomicU64::new(0),
                }))
            }),
        );
        r
    }

    #[test]
    fn registered_model_is_listed() {
        let r = build_registry_with_one_model();
        assert!(r.info("counter").is_some());
        assert_eq!(r.iter_info().count(), 1);
    }

    #[test]
    fn unregistered_id_returns_none() {
        let r = ModelRegistry::new();
        assert!(r.get_or_create("ghost").is_none());
        assert!(r.info("ghost").is_none());
    }

    #[test]
    fn list_capable_filters_by_required_flags() {
        let mut r = build_registry_with_one_model();
        r.register(
            ModelInfo {
                id: "auto-detect-only".into(),
                display_name: "Auto".into(),
                capabilities: MaskCapabilities::AUTO_DETECT,
            },
            Box::new(|| {
                Ok(Box::new(CountingGenerator {
                    id: "auto-detect-only".into(),
                    calls: std::sync::atomic::AtomicU64::new(0),
                }))
            }),
        );

        let pointers = r.list_capable(MaskCapabilities::POINT_PROMPT);
        assert_eq!(pointers.len(), 1);
        assert_eq!(pointers[0].id, "counter");

        let auto = r.list_capable(MaskCapabilities::AUTO_DETECT);
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].id, "auto-detect-only");
    }

    #[test]
    fn factory_called_lazily_and_only_once() {
        // Registry stores a closure that builds a model with a
        // call counter ON the model. We can't observe how many
        // times the factory ran from outside, so the proxy is:
        // does get_or_create return the same instance on repeated
        // calls? If yes, the factory ran exactly once.
        use std::sync::atomic::{AtomicU64, Ordering};
        let factory_calls = std::sync::Arc::new(AtomicU64::new(0));
        let counter_clone = factory_calls.clone();

        let mut r = ModelRegistry::new();
        r.register(
            ModelInfo {
                id: "lazy".into(),
                display_name: "Lazy".into(),
                capabilities: MaskCapabilities::empty(),
            },
            Box::new(move || {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(CountingGenerator {
                    id: "lazy".into(),
                    calls: AtomicU64::new(0),
                }))
            }),
        );

        // Before any get_or_create, factory hasn't run.
        assert_eq!(factory_calls.load(Ordering::SeqCst), 0);

        // First call materialises.
        {
            let _g = r.get_or_create("lazy").unwrap().unwrap();
        }
        assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

        // Second call reuses — factory still ran once.
        {
            let _g = r.get_or_create("lazy").unwrap().unwrap();
        }
        assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn clear_instance_forces_factory_to_rebuild() {
        // Uninstall flow: drop the cached generator so the next
        // dispatch rebuilds (catching e.g. a freshly re-downloaded
        // weights file or surfacing a clean BackendInit error if
        // the user has just deleted the cache file).
        use std::sync::atomic::{AtomicU64, Ordering};
        let factory_calls = std::sync::Arc::new(AtomicU64::new(0));
        let counter_clone = factory_calls.clone();

        let mut r = ModelRegistry::new();
        r.register(
            ModelInfo {
                id: "rebuildable".into(),
                display_name: "Rebuildable".into(),
                capabilities: MaskCapabilities::empty(),
            },
            Box::new(move || {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(CountingGenerator {
                    id: "rebuildable".into(),
                    calls: AtomicU64::new(0),
                }))
            }),
        );

        // Materialise once.
        {
            let _g = r.get_or_create("rebuildable").unwrap().unwrap();
        }
        assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

        // Clear the cached instance — entry stays registered.
        assert!(r.clear_instance("rebuildable"));
        assert!(r.info("rebuildable").is_some());

        // Next get_or_create rebuilds → factory ran a second time.
        {
            let _g = r.get_or_create("rebuildable").unwrap().unwrap();
        }
        assert_eq!(factory_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clear_instance_returns_false_for_unknown_id() {
        let r = ModelRegistry::new();
        assert!(!r.clear_instance("never-registered"));
    }

    #[test]
    fn unregister_drops_entry_entirely() {
        // Custom-model uninstall: after unregister, the id is gone
        // from info() and from get_or_create(). Distinct from
        // clear_instance, which leaves the entry registered and
        // just drops the materialised Box.
        let mut r = build_registry_with_one_model();
        assert!(r.info("counter").is_some());
        assert!(r.unregister("counter"));
        assert!(r.info("counter").is_none());
        assert!(r.get_or_create("counter").is_none());
    }

    #[test]
    fn unregister_returns_false_for_unknown_id() {
        let mut r = ModelRegistry::new();
        assert!(!r.unregister("never-registered"));
    }

    #[test]
    fn re_register_replaces_with_warning() {
        // Re-registering the same id should replace the entry
        // (and emit a warn log; we don't assert on the log here,
        // just on the behaviour).
        let mut r = build_registry_with_one_model();
        r.register(
            ModelInfo {
                id: "counter".into(),
                display_name: "Counter v2".into(),
                capabilities: MaskCapabilities::AUTO_DETECT,
            },
            Box::new(|| {
                Ok(Box::new(CountingGenerator {
                    id: "counter".into(),
                    calls: std::sync::atomic::AtomicU64::new(0),
                }))
            }),
        );
        assert_eq!(r.iter_info().count(), 1, "still one entry by id");
        assert_eq!(r.info("counter").unwrap().display_name, "Counter v2");
        assert!(r
            .info("counter")
            .unwrap()
            .capabilities
            .contains(MaskCapabilities::AUTO_DETECT));
    }
}
