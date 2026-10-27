//! Custom-URL paste flow for adding user-supplied AI models.
//!
//! ## State machine
//!
//! ```text
//! Idle ─── click "Add custom model…" ───▶ Form { url, adapter, display, id, errors }
//!                                              │
//!                                              │ click "Download"
//!                                              ▼
//!                                         Downloading { entry, recv, cancel,
//!                                                       downloaded, total }
//!                                              │
//!                                              ├── recv Ok(sha, size) ───▶ Confirming
//!                                              │                          { entry+sha+size }
//!                                              │
//!                                              └── recv Err(e) ──▶ Form { with error }
//!                                                                  (back to top)
//!
//! Confirming ── click Confirm ──▶ register + persist + Idle (toast)
//!         │
//!         └── click Cancel ──▶ delete file + Idle
//! ```
//!
//! Worker thread fetches the URL via [`klipp_ml::download_url_uncertified`],
//! which streams the body to a `<id>.bin.partial` and atomic-renames
//! to `<id>.bin` on success — same on-disk shape as the verified
//! download path. The computed `(sha256, size_bytes)` come back over
//! a crossbeam channel.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crossbeam_channel::Receiver;

use super::persistence::{CustomAdapter, CustomModelEntry};

/// Hardcoded ceiling on a single custom-model download. Defends
/// against a hostile server streaming forever; the legitimate
/// upper bound for SAM-family / matting / detection models on disk
/// today is around 1 GB. Bump if/when much larger checkpoints land.
pub const CUSTOM_DOWNLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Form fields the user fills before kicking off a download. Each
/// field carries an in-progress validation error for inline
/// surfacing.
#[derive(Debug, Default, Clone)]
pub struct CustomInstallForm {
    pub url: String,
    pub display_name: String,
    pub id: String,
    pub adapter: Option<CustomAdapter>,
    pub error: Option<String>,
}

/// Result delivered from the worker thread when the download
/// finishes (success or failure).
#[derive(Debug)]
pub enum CustomDownloadResult {
    Ok { sha256: String, size_bytes: u64 },
    Err(String),
}

/// One in-flight download. Held in `CustomInstallState::Downloading`
/// alongside the form data so the modal can render progress + the
/// completion handler has the metadata it needs to build the final
/// `CustomModelEntry`.
pub struct CustomDownloadHandle {
    pub recv: Receiver<CustomDownloadEvent>,
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for CustomDownloadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomDownloadHandle")
            .field("cancelled", &self.cancel.load(std::sync::atomic::Ordering::Relaxed))
            .finish()
    }
}

/// Worker → UI events. Mirrors the existing `model_download::DownloadEvent`
/// shape but distinct so the two flows can't accidentally share a
/// channel.
#[derive(Debug)]
pub enum CustomDownloadEvent {
    Progress { downloaded: u64, total: u64 },
    Completed(CustomDownloadResult),
}

/// Top-level state machine. AppState carries one of these.
#[derive(Debug)]
#[derive(Default)]
pub enum CustomInstallState {
    /// Modal closed — the dialog footer's Add custom model… button
    /// transitions Idle → Form.
    #[default]
    Idle,
    /// Modal open with the form fields. Submit transitions to
    /// Downloading; Cancel returns to Idle.
    Form(CustomInstallForm),
    /// Worker thread fetching bytes. UI shows a progress bar.
    Downloading {
        form: CustomInstallForm,
        handle: CustomDownloadHandle,
        downloaded: u64,
        total: u64,
    },
    /// Download finished successfully. Modal switches to a confirm
    /// view showing the computed SHA + size. User can register or
    /// cancel (cancel deletes the just-downloaded file).
    Confirming {
        form: CustomInstallForm,
        sha256: String,
        size_bytes: u64,
    },
}


impl CustomInstallState {
    /// Drain any pending events from the worker thread and update
    /// `self`. Returns `true` if the state changed materially this
    /// frame (so the caller can request_repaint + show a toast on
    /// completion). Mirrors `model_download::poll_download` and
    /// `InferenceState::poll`.
    pub fn poll(&mut self) -> bool {
        loop {
            let Self::Downloading {
                form,
                handle,
                downloaded,
                total,
            } = self
            else {
                return false;
            };
            match handle.recv.try_recv() {
                Ok(CustomDownloadEvent::Progress {
                    downloaded: d,
                    total: t,
                }) => {
                    *downloaded = d;
                    *total = t;
                    // Don't return true on every chunk — too chatty.
                    continue;
                }
                Ok(CustomDownloadEvent::Completed(CustomDownloadResult::Ok {
                    sha256,
                    size_bytes,
                })) => {
                    let form = form.clone();
                    *self = Self::Confirming {
                        form,
                        sha256,
                        size_bytes,
                    };
                    return true;
                }
                Ok(CustomDownloadEvent::Completed(CustomDownloadResult::Err(msg))) => {
                    let mut f = form.clone();
                    f.error = Some(msg);
                    *self = Self::Form(f);
                    return true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return false,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    let mut f = form.clone();
                    f.error = Some("download worker disconnected".into());
                    *self = Self::Form(f);
                    return true;
                }
            }
        }
    }
}

/// Build a `CustomModelEntry` from a confirmed form + computed
/// hash/size. Pure function — UI calls this when the user clicks
/// Confirm in the Confirming view, then hands the result to
/// `register_custom_model` + persistence.
pub fn finalize_entry(
    form: &CustomInstallForm,
    sha256: String,
    size_bytes: u64,
) -> Option<CustomModelEntry> {
    let adapter = form.adapter?;
    Some(CustomModelEntry {
        id: form.id.clone(),
        display_name: form.display_name.clone(),
        url: form.url.clone(),
        sha256,
        size_bytes,
        adapter,
    })
}

/// Validate the form before kicking off a download. Returns
/// `Ok(())` when every field has a usable value; `Err(reason)` for
/// the first problem found so the modal can surface it inline.
pub fn validate_form(
    form: &CustomInstallForm,
    existing_ids: &[String],
) -> Result<(), String> {
    if form.url.trim().is_empty() {
        return Err("URL is required".into());
    }
    // Over https only, as the built-in models are fetched.
    if !form.url.trim().starts_with("https://") {
        return Err("URL must start with https://".into());
    }
    if form.display_name.trim().is_empty() {
        return Err("Display name is required".into());
    }
    let id_trim = form.id.trim();
    if id_trim.is_empty() {
        return Err("Model id is required".into());
    }
    if !id_trim
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(
            "Model id must be ASCII alphanumeric, underscore, or hyphen".into(),
        );
    }
    if existing_ids.iter().any(|x| x == id_trim) {
        return Err(format!("Model id '{id_trim}' is already in use"));
    }
    if form.adapter.is_none() {
        return Err("Pick an adapter (matting / SAM / RF-DETR-Seg)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    /// A model is fetched over https only.
    #[test]
    fn a_plain_http_url_is_refused() {
        let form = CustomInstallForm { url: "http://example.com/m.onnx".into(), display_name: "M".into(), id: "m".into(), ..Default::default() };
        assert!(validate_form(&form, &[]).unwrap_err().contains("https://"));
    }

    use super::*;

    fn fresh_form() -> CustomInstallForm {
        CustomInstallForm {
            url: "https://example.com/model.onnx".into(),
            display_name: "My Custom Model".into(),
            id: "my_custom_v1".into(),
            adapter: Some(CustomAdapter::BiRefNet),
            error: None,
        }
    }

    #[test]
    fn validate_form_accepts_a_clean_form() {
        let f = fresh_form();
        let r = validate_form(&f, &[]);
        assert!(r.is_ok(), "got {r:?}");
    }

    #[test]
    fn validate_form_rejects_empty_url() {
        let mut f = fresh_form();
        f.url.clear();
        assert!(validate_form(&f, &[]).is_err());
    }

    #[test]
    fn validate_form_rejects_non_http_url() {
        let mut f = fresh_form();
        f.url = "ftp://example.com/model.onnx".into();
        let err = validate_form(&f, &[]).unwrap_err();
        assert!(err.contains("https://"));
    }

    #[test]
    fn validate_form_rejects_missing_display_name() {
        let mut f = fresh_form();
        f.display_name.clear();
        assert!(validate_form(&f, &[]).is_err());
    }

    #[test]
    fn validate_form_rejects_invalid_id_chars() {
        let mut f = fresh_form();
        f.id = "my model with spaces".into();
        let err = validate_form(&f, &[]).unwrap_err();
        assert!(err.contains("alphanumeric"));
    }

    #[test]
    fn validate_form_rejects_collision_with_existing_id() {
        let f = fresh_form();
        let err = validate_form(&f, &["my_custom_v1".into()]).unwrap_err();
        assert!(err.contains("already in use"));
    }

    #[test]
    fn validate_form_rejects_missing_adapter() {
        let mut f = fresh_form();
        f.adapter = None;
        assert!(validate_form(&f, &[]).is_err());
    }

    #[test]
    fn finalize_entry_builds_full_record() {
        let f = fresh_form();
        let e = finalize_entry(&f, "deadbeef".into(), 1024).expect("Some");
        assert_eq!(e.id, "my_custom_v1");
        assert_eq!(e.display_name, "My Custom Model");
        assert_eq!(e.url, "https://example.com/model.onnx");
        assert_eq!(e.sha256, "deadbeef");
        assert_eq!(e.size_bytes, 1024);
        assert_eq!(e.adapter, CustomAdapter::BiRefNet);
    }

    #[test]
    fn finalize_entry_returns_none_without_adapter() {
        let mut f = fresh_form();
        f.adapter = None;
        assert!(finalize_entry(&f, "abc".into(), 0).is_none());
    }

    #[test]
    fn idle_state_poll_returns_false() {
        let mut s = CustomInstallState::Idle;
        assert!(!s.poll());
    }
}
