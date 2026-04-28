//! Worker-thread dispatch for AI Tools mask inference.
//!
//! Mirrors the [`model_download`](crate::model_download) pattern
//! verbatim: caller hands us an owned request and an `egui::Context`,
//! we spawn a `std::thread`, run the (sync, blocking) `MaskGenerator`
//! call on it, push events back through a `crossbeam-channel`, and
//! call `ctx.request_repaint()` so the UI refreshes the moment a
//! response or failure arrives.
//!
//! ## Why an OwnedMaskRequest
//!
//! `klipp_ml::MaskRequest<'a>` borrows the input frame because the
//! per-frame inference path is hot enough to care about zero-copy.
//! UI dispatch isn't on that path — the user clicks, we run inference
//! once, mask comes back. Worker threads cannot hold a borrow into
//! the UI thread's frame buffer (lifetime would have to be `'static`),
//! so we copy the linear-RGBA frame into an `Arc<Vec<f32>>` and pass
//! ownership across. The Arc lets us cheaply hand the buffer down
//! into the borrowed `MaskRequest` view inside the worker.
//!
//! ## Cancellation
//!
//! Same shape as `model_download`: the handle exposes an
//! `Arc<AtomicBool>`. Setting it to `true` is a soft signal — the
//! current `MaskGenerator::generate` call is sync and can't be
//! preempted, but the worker checks the flag before sending events
//! and a future model-side cooperative cancel can hook into the same
//! flag. For now the worst case is "the user gets one stale mask
//! they then discard"; that's acceptable for v1.
//!
//! ## INTEGRATION_NOTES
//!
//! This module is a leaf — it does NOT touch `AppState`, `KlippApp`,
//! or any panel. The integration site (deferred to review) needs:
//!
//! 1. A new field `state.ai_tools_state: AiToolsState` (caller
//!    defines the struct), holding at least:
//!    - `prompts: super::prompt_input::PromptCapture`
//!    - `inference: InferenceState` (the enum below)
//!    - `latest_mask: Option<klipp_ml::MaskBuffer>` for the overlay
//!    - `selected_model_id: String` (e.g. `"stub"`)
//!    - `mask_overlay_color: egui::Color32` (default: orange 50%)
//! 2. A field `state.ml_registry: super::inference::RegistryHandle`
//!    constructed at app startup with at least the stub generator
//!    registered.
//! 3. A poll site in `app::update` that calls
//!    [`InferenceState::poll`] each frame, drains events, and writes
//!    the resulting mask into `state.ai_tools_state.latest_mask`.
//! 4. A keypress / button handler that builds an
//!    [`OwnedMaskRequest`] from the current viewer frame +
//!    `prompts.build()` and calls [`spawn_inference`].
//! 5. The viewer panel (out of scope for this PR) wires
//!    `PromptCapture::record_click` / `start_drag` / etc. to its
//!    pointer event flow.
//!
//! The `RegistryHandle` newtype below exists specifically so it can
//! be cloned cheaply onto the worker thread without leaking
//! `Arc<ModelRegistry>` into every caller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use klipp_ml::{
    InputFrame, MaskBuffer, MaskCandidate, MaskError, MaskPrompts, MaskRequest, MaskResponse,
    ModelRegistry, TemporalHint,
};

/// Cheaply-cloneable handle to the process-wide [`ModelRegistry`].
///
/// The registry itself is `Send + Sync` (its only interior mutability
/// is a `Mutex` on each model entry), so wrapping in `Arc` is enough.
/// We use a newtype rather than exposing the bare `Arc<_>` so the
/// integration site has one canonical name to thread through, and
/// future additions (per-process model warmup, telemetry) live on the
/// handle without touching every caller.
#[derive(Clone)]
pub struct RegistryHandle {
    inner: Arc<ModelRegistry>,
}

impl RegistryHandle {
    /// Wrap an existing registry. Caller usually builds + registers
    /// once at app startup, then clones this handle into AppState.
    pub fn new(registry: ModelRegistry) -> Self {
        Self {
            inner: Arc::new(registry),
        }
    }

    /// Wrap an already-Arc'd registry — useful when the registry is
    /// shared with another subsystem (e.g. command-palette autocomplete
    /// listing registered models).
    pub fn from_arc(arc: Arc<ModelRegistry>) -> Self {
        Self { inner: arc }
    }

    /// Borrow the inner registry. Used by the worker to look up the
    /// model by id without taking an additional clone of the Arc.
    pub fn registry(&self) -> &ModelRegistry {
        &self.inner
    }
}

impl std::fmt::Debug for RegistryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryHandle")
            .field("models", &self.inner.iter_info().count())
            .finish()
    }
}

/// Owned-copy of a `MaskRequest`. Constructed on the UI thread,
/// shipped across the channel into the worker, then re-borrowed via
/// [`as_borrowed`](Self::as_borrowed) into a real `MaskRequest`
/// before the model is called.
#[derive(Clone)]
pub struct OwnedMaskRequest {
    /// Linear-light RGBA, 4 floats per pixel. `Arc` so the worker
    /// thread can hold ownership while the UI thread keeps a
    /// reference for diagnostics or for re-issuing the request after
    /// a model change.
    pub frame_pixels: Arc<Vec<f32>>,
    pub frame_width: u32,
    pub frame_height: u32,
    pub prompts: MaskPrompts,
    pub temporal: Option<TemporalHint>,
}

impl OwnedMaskRequest {
    /// Borrow this owned request as a real `MaskRequest`. The
    /// returned struct holds a shared reference into `frame_pixels`,
    /// so it lives only as long as `self`.
    pub fn as_borrowed(&self) -> MaskRequest<'_> {
        MaskRequest {
            frame: InputFrame {
                width: self.frame_width,
                height: self.frame_height,
                linear_rgba: self.frame_pixels.as_slice(),
            },
            prompts: self.prompts.clone(),
            temporal: self.temporal.clone(),
        }
    }

    /// Convenience for tests + the auto-detect dispatch path: build
    /// a request with no prompts.
    pub fn auto_detect(frame_pixels: Arc<Vec<f32>>, width: u32, height: u32) -> Self {
        Self {
            frame_pixels,
            frame_width: width,
            frame_height: height,
            prompts: MaskPrompts::default(),
            temporal: None,
        }
    }
}

impl std::fmt::Debug for OwnedMaskRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedMaskRequest")
            .field("frame_width", &self.frame_width)
            .field("frame_height", &self.frame_height)
            .field("frame_pixels", &format_args!("[{} f32]", self.frame_pixels.len()))
            .field("prompts", &self.prompts)
            .field("temporal", &self.temporal.is_some())
            .finish()
    }
}

/// Events sent from the worker thread to the UI thread. Matches the
/// shape of `model_download::DownloadEvent` so the integration site
/// can poll uniformly.
#[derive(Debug)]
pub enum InferenceEvent {
    /// Worker has begun. Useful for showing a "thinking…" indicator
    /// in the AI Tools panel; the UI doesn't need to do anything with
    /// it for correctness.
    Started,
    /// Inference completed with a response. Worker thread exits after
    /// sending this.
    Completed(MaskResponse),
    /// Inference failed. The error string is suitable for direct user
    /// display. Worker thread exits after sending.
    Failed(String),
}

/// Handle to an in-flight inference. Mirror of `DownloadHandle`.
pub struct InferenceHandle {
    pub recv: Receiver<InferenceEvent>,
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for InferenceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceHandle")
            .field("cancelled", &self.cancel.load(Ordering::Relaxed))
            .finish()
    }
}

/// Spawn an inference on a worker thread. The UI thread keeps the
/// returned [`InferenceHandle`] and polls `recv` each frame to drain
/// events.
///
/// Panics only on [`std::thread::Builder::spawn`] failure (system out
/// of threads), matching the same behaviour as
/// `model_download::spawn_download`.
pub fn spawn_inference(
    model_id: String,
    request: OwnedMaskRequest,
    registry: RegistryHandle,
    ctx: egui::Context,
) -> InferenceHandle {
    let (tx, rx) = crossbeam_channel::unbounded::<InferenceEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_worker = cancel.clone();

    let model_id_for_log = model_id.clone();
    std::thread::Builder::new()
        .name(format!("klipp-ml-infer-{}", model_id_for_log))
        .spawn(move || {
            run_inference(
                model_id,
                request,
                registry,
                ctx,
                tx,
                cancel_for_worker,
            );
        })
        .expect("spawn klipp-ml inference thread");

    InferenceHandle { recv: rx, cancel }
}

/// Worker-thread body. Extracted from the spawn closure so it can be
/// covered by tests that drive a synthetic registry without the
/// `std::thread` indirection.
fn run_inference(
    model_id: String,
    request: OwnedMaskRequest,
    registry: RegistryHandle,
    ctx: egui::Context,
    tx: Sender<InferenceEvent>,
    cancel: Arc<AtomicBool>,
) {
    let _ = tx.send(InferenceEvent::Started);
    ctx.request_repaint();

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(InferenceEvent::Failed("cancelled".into()));
        ctx.request_repaint();
        return;
    }

    let event = run_inference_blocking(&model_id, &request, &registry);

    if cancel.load(Ordering::Relaxed) {
        // User cancelled mid-flight. Drop the result on the floor —
        // sending it would race with the UI's own cancel-handling
        // path.
        let _ = tx.send(InferenceEvent::Failed("cancelled".into()));
        ctx.request_repaint();
        return;
    }

    let _ = tx.send(event);
    ctx.request_repaint();
}

/// Pure-blocking helper: looks up the model, runs `generate`,
/// returns the resulting event. No threading, no channels. Tests
/// drive this directly to avoid spawning real threads.
fn run_inference_blocking(
    model_id: &str,
    request: &OwnedMaskRequest,
    registry: &RegistryHandle,
) -> InferenceEvent {
    let Some(get_or_create) = registry.registry().get_or_create(model_id) else {
        return InferenceEvent::Failed(format!(
            "no model registered under id '{model_id}'"
        ));
    };
    let mut guard = match get_or_create {
        Ok(g) => g,
        Err(e) => return InferenceEvent::Failed(format_mask_error(&e)),
    };
    let req = request.as_borrowed();
    match guard.as_mut().generate(req) {
        Ok(resp) => InferenceEvent::Completed(resp),
        Err(e) => InferenceEvent::Failed(format_mask_error(&e)),
    }
}

/// User-facing rendering of a [`MaskError`]. Errors that already carry
/// good context (model id, expected shape) pass through; the
/// `Backend*` and `Inference` variants get a "the model failed:"
/// preamble so the toast reads as a model issue rather than a Klipp
/// bug.
fn format_mask_error(e: &MaskError) -> String {
    match e {
        MaskError::Inference(_) | MaskError::BackendInit(_) => {
            format!("the model failed: {e}")
        }
        _ => format!("{e}"),
    }
}

/// Lightweight first candidate accessor — the AI Tools UI usually
/// shows just one mask preview, picks the highest-confidence
/// candidate. Lives here (rather than being inlined at the call site)
/// so the integration site has one canonical predicate.
pub fn first_candidate(resp: &MaskResponse) -> Option<&MaskCandidate> {
    resp.candidates.first()
}

/// State machine the UI keeps in `AppState`. Mirrors
/// `ModelDownloadState`: Idle → Running → Done | Failed → (clear) →
/// Idle.
#[derive(Debug, Default)]
pub enum InferenceState {
    /// No inference in flight, no mask shown. Default.
    #[default]
    Idle,
    /// Worker thread is processing a request. UI shows "thinking…"
    /// indicator + Cancel button.
    Running {
        /// Stable id of the model running the inference.
        model_id: String,
        /// Frame size — convenient for the UI to compose the overlay
        /// rect even before the response lands.
        frame_width: u32,
        frame_height: u32,
        handle: InferenceHandle,
    },
    /// Worker finished successfully. The latest mask candidate is
    /// available for the overlay; the UI also shows accept / reject
    /// affordances.
    Done {
        model_id: String,
        response: MaskResponse,
    },
    /// Worker failed. UI shows the error message until dismissed.
    Failed {
        model_id: String,
        error: String,
    },
}

impl InferenceState {
    /// Drain pending events and update `self` accordingly. Returns
    /// `true` when the state changed materially this frame (so the
    /// caller can refresh the overlay / show a toast / etc.). Mirrors
    /// `model_download::poll_download`.
    pub fn poll(&mut self) -> bool {
        loop {
            let Self::Running { handle, model_id, .. } = self else {
                return false;
            };
            match handle.recv.try_recv() {
                Ok(InferenceEvent::Started) => {
                    // Just an early ping; no state change. Loop to
                    // see if there's a Completed event already.
                    continue;
                }
                Ok(InferenceEvent::Completed(resp)) => {
                    let model_id = model_id.clone();
                    *self = Self::Done {
                        model_id,
                        response: resp,
                    };
                    return true;
                }
                Ok(InferenceEvent::Failed(error)) => {
                    let model_id = model_id.clone();
                    *self = Self::Failed { model_id, error };
                    return true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return false,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    let model_id = model_id.clone();
                    *self = Self::Failed {
                        model_id,
                        error: "inference worker disconnected unexpectedly".into(),
                    };
                    return true;
                }
            }
        }
    }

    /// `true` while a worker is in flight.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Currently-shown mask, if the state is `Done`. `None` for
    /// every other state. Convenience for the overlay renderer.
    pub fn current_mask(&self) -> Option<&MaskBuffer> {
        match self {
            Self::Done { response, .. } => first_candidate(response).map(|c| &c.mask),
            _ => None,
        }
    }

    /// Cancel an in-flight inference. No-op outside `Running`.
    pub fn cancel(&mut self) {
        if let Self::Running { handle, .. } = self {
            handle.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Drop any held state and return to `Idle`. Used by the AI
    /// Tools "Clear" button, by the Reject affordance, and on clip /
    /// frame change so a stale mask doesn't paint over a different
    /// frame.
    pub fn reset(&mut self) {
        if let Self::Running { handle, .. } = self {
            handle.cancel.store(true, Ordering::Relaxed);
        }
        *self = Self::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::{
        ClickPrompt, MaskCapabilities, ModelInfo, StubMaskGenerator, STUB_GENERATOR_ID,
    };

    fn dummy_pixels(w: u32, h: u32) -> Arc<Vec<f32>> {
        Arc::new(vec![0.0; (w as usize) * (h as usize) * 4])
    }

    fn registry_with_stub() -> RegistryHandle {
        let mut r = ModelRegistry::new();
        r.register(
            ModelInfo {
                id: STUB_GENERATOR_ID.into(),
                display_name: "Stub".into(),
                capabilities: MaskCapabilities::POINT_PROMPT
                    | MaskCapabilities::BOX_PROMPT
                    | MaskCapabilities::SCRIBBLE
                    | MaskCapabilities::AUTO_DETECT,
            },
            Box::new(|| Ok(Box::new(StubMaskGenerator::new()))),
        );
        RegistryHandle::new(r)
    }

    #[test]
    fn owned_request_borrowed_view_matches_originals() {
        let pixels = dummy_pixels(8, 6);
        let owned = OwnedMaskRequest {
            frame_pixels: pixels.clone(),
            frame_width: 8,
            frame_height: 6,
            prompts: MaskPrompts {
                clicks: vec![ClickPrompt {
                    x: 0.5,
                    y: 0.5,
                    positive: true,
                }],
                ..Default::default()
            },
            temporal: None,
        };
        let view = owned.as_borrowed();
        assert_eq!(view.frame.width, 8);
        assert_eq!(view.frame.height, 6);
        assert_eq!(view.frame.linear_rgba.len(), 8 * 6 * 4);
        assert_eq!(view.prompts.clicks.len(), 1);
    }

    #[test]
    fn auto_detect_constructor_produces_empty_prompts() {
        let pixels = dummy_pixels(4, 4);
        let req = OwnedMaskRequest::auto_detect(pixels, 4, 4);
        assert!(req.prompts.is_empty());
        assert!(req.temporal.is_none());
    }

    #[test]
    fn run_inference_blocking_with_stub_returns_completed() {
        // Drive the helper directly so we don't need a real worker
        // thread. Verifies the registry plumbing + stub call.
        let registry = registry_with_stub();
        let pixels = dummy_pixels(64, 64);
        let req = OwnedMaskRequest {
            frame_pixels: pixels,
            frame_width: 64,
            frame_height: 64,
            prompts: MaskPrompts {
                clicks: vec![ClickPrompt {
                    x: 0.5,
                    y: 0.5,
                    positive: true,
                }],
                ..Default::default()
            },
            temporal: None,
        };

        let event = run_inference_blocking(STUB_GENERATOR_ID, &req, &registry);
        match event {
            InferenceEvent::Completed(resp) => {
                assert_eq!(resp.candidates.len(), 1);
                let mask = &resp.candidates[0].mask;
                assert_eq!(mask.width, 64);
                assert_eq!(mask.height, 64);
                // Stub paints a circle around the click; centre pixel
                // should be lit.
                assert_eq!(mask.at(32, 32), 255);
                // A pixel near the corner should be dark.
                assert_eq!(mask.at(0, 0), 0);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_inference_blocking_unknown_model_fails_gracefully() {
        let registry = registry_with_stub();
        let pixels = dummy_pixels(16, 16);
        let req = OwnedMaskRequest::auto_detect(pixels, 16, 16);
        let event = run_inference_blocking("does-not-exist", &req, &registry);
        match event {
            InferenceEvent::Failed(msg) => {
                assert!(msg.contains("no model registered"));
                assert!(msg.contains("does-not-exist"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn spawn_inference_with_stub_delivers_completed_event() {
        // End-to-end through a real worker thread. Uses the stub +
        // an offscreen egui::Context.
        let registry = registry_with_stub();
        let pixels = dummy_pixels(32, 32);
        let req = OwnedMaskRequest {
            frame_pixels: pixels,
            frame_width: 32,
            frame_height: 32,
            prompts: MaskPrompts {
                clicks: vec![ClickPrompt {
                    x: 0.5,
                    y: 0.5,
                    positive: true,
                }],
                ..Default::default()
            },
            temporal: None,
        };

        let ctx = egui::Context::default();
        let handle = spawn_inference(
            STUB_GENERATOR_ID.to_string(),
            req,
            registry,
            ctx,
        );

        // Drain events with a generous timeout — stub is sub-ms but
        // CI can be slow.
        let mut got_started = false;
        let mut got_completed: Option<MaskResponse> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match handle.recv.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(InferenceEvent::Started) => got_started = true,
                Ok(InferenceEvent::Completed(resp)) => {
                    got_completed = Some(resp);
                    break;
                }
                Ok(InferenceEvent::Failed(e)) => panic!("unexpected Failed: {e}"),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(got_started, "Started event not received");
        let resp = got_completed.expect("Completed event not received");
        assert_eq!(resp.candidates.len(), 1);
        // Stub centres a circle around the click — verify that.
        let mask = &resp.candidates[0].mask;
        assert_eq!(mask.at(16, 16), 255);
    }

    #[test]
    fn poll_idle_state_is_no_op() {
        let mut s = InferenceState::Idle;
        assert!(!s.poll());
        assert!(matches!(s, InferenceState::Idle));
    }

    #[test]
    fn poll_drains_completed_event() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = InferenceState::Running {
            model_id: STUB_GENERATOR_ID.into(),
            frame_width: 4,
            frame_height: 4,
            handle: InferenceHandle { recv: rx, cancel },
        };
        let resp = MaskResponse {
            candidates: vec![MaskCandidate {
                mask: MaskBuffer::solid(4, 4, 255),
                confidence: 0.9,
                bbox: None,
                label: None,
            }],
            inference_ms: 1,
        };
        tx.send(InferenceEvent::Started).unwrap();
        tx.send(InferenceEvent::Completed(resp)).unwrap();
        let transitioned = state.poll();
        assert!(transitioned);
        match state {
            InferenceState::Done { ref model_id, ref response } => {
                assert_eq!(model_id, STUB_GENERATOR_ID);
                assert_eq!(response.candidates.len(), 1);
            }
            _ => panic!("expected Done after Completed"),
        }
    }

    #[test]
    fn poll_drains_failed_event() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = InferenceState::Running {
            model_id: "bad".into(),
            frame_width: 4,
            frame_height: 4,
            handle: InferenceHandle { recv: rx, cancel },
        };
        tx.send(InferenceEvent::Failed("boom".into())).unwrap();
        let transitioned = state.poll();
        assert!(transitioned);
        match state {
            InferenceState::Failed { ref error, .. } => assert_eq!(error, "boom"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn poll_disconnected_channel_marks_failed() {
        let (tx, rx) = crossbeam_channel::unbounded::<InferenceEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = InferenceState::Running {
            model_id: "x".into(),
            frame_width: 1,
            frame_height: 1,
            handle: InferenceHandle { recv: rx, cancel },
        };
        drop(tx);
        let transitioned = state.poll();
        assert!(transitioned);
        match state {
            InferenceState::Failed { ref error, .. } => {
                assert!(error.contains("disconnected"));
            }
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn current_mask_some_only_when_done() {
        let mut state = InferenceState::Idle;
        assert!(state.current_mask().is_none());

        let (_tx, rx) = crossbeam_channel::unbounded();
        state = InferenceState::Running {
            model_id: "x".into(),
            frame_width: 1,
            frame_height: 1,
            handle: InferenceHandle {
                recv: rx,
                cancel: Arc::new(AtomicBool::new(false)),
            },
        };
        assert!(state.current_mask().is_none());

        state = InferenceState::Done {
            model_id: "x".into(),
            response: MaskResponse {
                candidates: vec![MaskCandidate {
                    mask: MaskBuffer::solid(2, 2, 200),
                    confidence: 0.5,
                    bbox: None,
                    label: None,
                }],
                inference_ms: 0,
            },
        };
        let m = state.current_mask().expect("Done state has a mask");
        assert_eq!(m.alpha[0], 200);
    }

    #[test]
    fn cancel_running_state_sets_flag() {
        let (_tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = InferenceState::Running {
            model_id: "x".into(),
            frame_width: 1,
            frame_height: 1,
            handle: InferenceHandle {
                recv: rx,
                cancel: cancel.clone(),
            },
        };
        state.cancel();
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn reset_returns_to_idle() {
        let mut state = InferenceState::Done {
            model_id: "x".into(),
            response: MaskResponse {
                candidates: vec![],
                inference_ms: 0,
            },
        };
        state.reset();
        assert!(matches!(state, InferenceState::Idle));
    }

    #[test]
    fn registry_handle_clone_is_cheap_and_observable() {
        let h = registry_with_stub();
        let h2 = h.clone();
        assert!(h.registry().info(STUB_GENERATOR_ID).is_some());
        assert!(h2.registry().info(STUB_GENERATOR_ID).is_some());
    }

    #[test]
    fn first_candidate_returns_none_for_empty_response() {
        let resp = MaskResponse {
            candidates: vec![],
            inference_ms: 0,
        };
        assert!(first_candidate(&resp).is_none());
    }

    #[test]
    fn first_candidate_returns_first_when_present() {
        let resp = MaskResponse {
            candidates: vec![
                MaskCandidate {
                    mask: MaskBuffer::solid(1, 1, 10),
                    confidence: 0.1,
                    bbox: None,
                    label: None,
                },
                MaskCandidate {
                    mask: MaskBuffer::solid(1, 1, 20),
                    confidence: 0.9,
                    bbox: None,
                    label: None,
                },
            ],
            inference_ms: 0,
        };
        let c = first_candidate(&resp).unwrap();
        assert!((c.confidence - 0.1).abs() < 1e-5);
    }
}
