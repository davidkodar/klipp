//! GPU vendor + libonnxruntime runtime status probes.
//!
//! Why this lives in klipp-ml-ort: vendor detection feeds the AI
//! Tools UI tooltips ("AMD GPU detected — GPU runtime not
//! installed") *and* the eventual auto-download path that fetches
//! the matching custom-built libonnxruntime.so when AMD/Intel
//! support lands. Both consumers live in klipp-ui, but the
//! probes themselves belong to the ort crate boundary because
//! that's where the runtime selection ultimately matters.
//!
//! The checks are deliberately read-only and best-effort. A
//! failed probe never panics — we degrade to `Unknown`/`CpuOnly`
//! and the UI surfaces the conservative state.
//!
//! ## Detection order
//!
//! GPU vendor:
//!   1. `lspci -nn` parsed for VGA/3D class entries — most
//!      reliable when binutils is installed.
//!   2. `/sys/module/{nvidia,amdgpu,i915}/` presence — works in
//!      headless containers without lspci.
//!   3. Falls through to `Unknown`.
//!
//! Runtime status:
//!   1. Honour `ORT_DYLIB_PATH` if set (matches what ort's
//!      load-dynamic feature does at session-init time).
//!   2. Try a few standard install paths
//!      (`~/.local/lib/onnxruntime-*`, `/usr/lib/`).
//!   3. Future: also check
//!      `~/.local/share/klipp/runtimes/<vendor>-<version>/` for
//!      Klipp-shipped builds (reserved path; not yet populated
//!      because no klipp-runtimes repo exists — see
//!      F-97-AMD-ROCM / F-97-INTEL-OPENVINO).

use std::path::{Path, PathBuf};

/// Coarse GPU vendor classification used by the runtime resolver +
/// the AI Tools UI. Doesn't try to distinguish specific GPU
/// models within a vendor — Klipp's runtime selection is per
/// vendor (one libonnxruntime build per vendor), not per model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    /// NVIDIA GPU detected. Klipp uses pyke's prebuilt CUDA
    /// libonnxruntime when the user has CUDA + cuDNN installed.
    Nvidia,
    /// AMD GPU detected. Needs Klipp-shipped MIGraphX-built
    /// libonnxruntime (not yet available; CPU fallback today).
    Amd,
    /// Intel GPU detected (Arc discrete or iGPU). Needs Klipp-
    /// shipped OpenVINO-built libonnxruntime (not yet available;
    /// CPU fallback today).
    Intel,
    /// No GPU detected, OR the detection probe didn't surface
    /// one. Inference falls back to ort's CPU EP.
    None,
    /// Probes failed entirely (no lspci, no /sys access).
    Unknown,
}

impl GpuVendor {
    /// User-facing label for the AI Tools UI. Single short phrase
    /// suitable for inline rendering.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA",
            Self::Amd => "AMD",
            Self::Intel => "Intel",
            Self::None => "No GPU",
            Self::Unknown => "Unknown",
        }
    }
}

/// Runtime acceleration status — the rendered combination of
/// "what GPU vendor does the user have" + "is a matching
/// libonnxruntime available". Drives the UI's
/// "GPU acceleration: X" footer + tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// No GPU detected — inference will run on the CPU EP.
    /// Surfaces as a neutral "CPU only" indicator.
    CpuOnly,
    /// GPU vendor detected AND the libonnxruntime loaded has a GPU
    /// provider this build can use ([`crate::provider::gpu_provider`]:
    /// CUDA, or WebGPU for any vendor). Inference runs on the GPU.
    GpuAccelerationActive(GpuVendor),
    /// GPU vendor detected but no libonnxruntime found. The
    /// dialog suggests installing a GPU runtime.
    GpuAvailableNotInstalled(GpuVendor),
    /// GPU vendor detected and a libonnxruntime found, but one built
    /// without a provider for that GPU (a CPU-only package): inference
    /// runs on the CPU (#2250).
    GpuRuntimeCpuOnly(GpuVendor),
}

impl RuntimeStatus {
    /// Compact label for the AI Models dialog footer cell.
    pub fn label(&self) -> String {
        match self {
            Self::CpuOnly => "CPU only".into(),
            Self::GpuAccelerationActive(v) => format!("GPU ({})", v.display_label()),
            Self::GpuAvailableNotInstalled(v) => {
                format!("{} — runtime missing", v.display_label())
            }
            Self::GpuRuntimeCpuOnly(v) => format!("{} — CPU-only runtime", v.display_label()),
        }
    }

    /// Severity for the UI's colour treatment. Active = neutral;
    /// missing/unsupported = warning; CpuOnly = warning when
    /// there's a GPU we COULD use, neutral otherwise. The panel
    /// can map this to its theme tokens.
    pub fn severity(&self) -> StatusSeverity {
        match self {
            Self::CpuOnly => StatusSeverity::Neutral,
            Self::GpuAccelerationActive(_) => StatusSeverity::Active,
            Self::GpuAvailableNotInstalled(_) | Self::GpuRuntimeCpuOnly(_) => {
                StatusSeverity::Warning
            }
        }
    }
}

/// Three-tier severity the UI maps to theme colours. Pulled out
/// of `RuntimeStatus` so the panel doesn't need a match arm to
/// pick a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSeverity {
    /// Neutral — no acceleration, no problem either.
    Neutral,
    /// GPU acceleration is wired and engaged.
    Active,
    /// GPU is present but not used. Surfaces an action: install
    /// runtime, install CUDA, etc.
    Warning,
}

/// Probe the host for a GPU vendor. Best-effort: returns
/// `GpuVendor::None` when no GPU is found and `GpuVendor::Unknown`
/// when neither probe path is available (e.g. inside a sandbox
/// without `lspci` or `/sys`).
pub fn detect_gpu_vendor() -> GpuVendor {
    if let Some(v) = detect_via_lspci() {
        return v;
    }
    if let Some(v) = detect_via_sys_modules() {
        return v;
    }
    GpuVendor::Unknown
}

/// Resolve the current runtime status — combines vendor
/// detection with what the libonnxruntime found can run on.
///
/// * NVIDIA — the GPU through CUDA (onnxruntime-cuda, or a CUDA build
///   through ORT_DYLIB_PATH), or WebGPU.
/// * AMD, Intel — the GPU through a WebGPU build (#2245).
/// * None — CPU only.
pub fn resolve_runtime_status() -> RuntimeStatus {
    let vendor = detect_gpu_vendor();
    match vendor {
        GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel => {
            if find_libonnxruntime().is_none() {
                return RuntimeStatus::GpuAvailableNotInstalled(vendor);
            }
            // A library is there; what it can run on is asked of it,
            // once (#2250).
            #[cfg(feature = "ort")]
            {
                static GPU: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
                if GPU.get_or_init(crate::provider::gpu_provider).is_some() {
                    return RuntimeStatus::GpuAccelerationActive(vendor);
                }
            }
            RuntimeStatus::GpuRuntimeCpuOnly(vendor)
        }
        GpuVendor::None | GpuVendor::Unknown => RuntimeStatus::CpuOnly,
    }
}

/// Resolve `ORT_DYLIB_PATH` if set, otherwise scan a few
/// standard install paths. Returns the first existing
/// `libonnxruntime.so*` we find. Symbol introspection (CUDA vs
/// MIGraphX vs OpenVINO) is intentionally NOT done here — it
/// requires `nm` and is fragile across binutils versions. The
/// trust is "user has GPU X + libort, libort is the right one".
pub fn find_libonnxruntime() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("ORT_DYLIB_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }
    let candidates: &[&str] = &[
        // pyke's user-side install location after `download-binaries`
        // pulls + extracts the ORT GPU tarball.
        "~/.local/lib/onnxruntime/lib/libonnxruntime.so",
        // Common direct-from-Microsoft tarball install location.
        "~/.local/lib/onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so",
        // System-wide install (some distros package onnxruntime).
        "/usr/lib/libonnxruntime.so",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
    ];
    for raw in candidates {
        let resolved = expand_tilde(raw);
        if resolved.exists() {
            return Some(resolved);
        }
    }
    None
}

/// Fail fast when the ONNX Runtime shared library cannot be loaded.
///
/// `ort` runs in `load-dynamic` mode, and when the library it is pointed
/// at does not exist its lazy initialisation **deadlocks** instead of
/// returning an error — the calling thread and the runtime's init thread
/// both end up parked on a futex indefinitely. That turns a missing
/// optional dependency into a hung process rather than the
/// [`klipp_ml::MaskError::BackendInit`] the API promises, and it is not
/// something we can repair inside a pinned release-candidate dependency.
/// So the precondition gets checked before control reaches `ort`.
///
/// The check mirrors **`ort`'s** resolution order, not ours:
/// `ort` trusts `ORT_DYLIB_PATH` verbatim, so when that variable is set
/// it is the only file `ort` will attempt — even if a perfectly good
/// library sits in one of the fallback locations
/// [`find_libonnxruntime`] would have picked up. Validating our own
/// search order here instead would report success and then hang.
pub fn ensure_runtime_loadable() -> Result<(), String> {
    let explicit = std::env::var("ORT_DYLIB_PATH").ok();
    let explicit = explicit.as_deref().filter(|p| !p.is_empty());
    let explicit_exists = explicit.is_some_and(|p| std::path::Path::new(p).exists());
    runtime_loadable(explicit, explicit_exists, find_libonnxruntime().is_some())
}

/// Decision half of [`ensure_runtime_loadable`], split from the
/// environment read so it can be tested directly.
///
/// Testing the whole thing would mean setting `ORT_DYLIB_PATH`, and the
/// environment is process-wide: a test doing that would race every other
/// test in this binary that builds a session, pointing them at whatever
/// path it had installed. Taking the inputs as arguments keeps the logic
/// under test and the hazard out of the suite.
fn runtime_loadable(
    explicit: Option<&str>,
    explicit_exists: bool,
    fallback_found: bool,
) -> Result<(), String> {
    // An explicit setting wins outright, including when it is wrong:
    // `ort` will not fall back, so neither may we.
    if let Some(path) = explicit {
        return if explicit_exists {
            Ok(())
        } else {
            Err(format!(
                "ONNX Runtime library not found: ORT_DYLIB_PATH is set to \
                 '{path}', which does not exist. Unset it to fall back to the \
                 standard locations, or point it at a real libonnxruntime.so."
            ))
        };
    }
    if fallback_found {
        Ok(())
    } else {
        Err("ONNX Runtime library not found. Install it (Arch: onnxruntime-cpu, \
             or onnxruntime-cuda for the GPU execution provider) or set \
             ORT_DYLIB_PATH to a libonnxruntime.so."
            .to_string())
    }
}

/// Replace a leading `~` with `$HOME`. Standalone helper because
/// our candidate list mixes home-relative + absolute paths.
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Vendor detection via `lspci -nn`. Most reliable on dev/desktop
/// Linux. Returns `None` when lspci isn't installed OR when
/// no VGA/3D class entry is found (very rare on a real machine).
fn detect_via_lspci() -> Option<GpuVendor> {
    let out = std::process::Command::new("lspci")
        .arg("-nn")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    classify_lspci_output(&stdout)
}

/// Pure parser — separated for unit-testing without spawning
/// `lspci`. Looks for VGA-compatible-controller / 3D-controller
/// lines and matches the vendor name (case-insensitive).
fn classify_lspci_output(stdout: &str) -> Option<GpuVendor> {
    let mut found_any = false;
    for line in stdout.lines() {
        let lower = line.to_ascii_lowercase();
        // Class filter — VGA-compatible controller (0300) or
        // 3D controller (0302). Format is "[NNNN]: ".
        let is_gpu_class = lower.contains("vga compatible controller")
            || lower.contains("3d controller")
            || lower.contains("display controller");
        if !is_gpu_class {
            continue;
        }
        found_any = true;
        if lower.contains("nvidia") {
            return Some(GpuVendor::Nvidia);
        }
        // AMD: match the modern strings PCI/lspci use. Don't grep
        // for plain "ati" — it's a substring of "corpor**ati**on"
        // which appears on every Intel/NVIDIA line. Legacy ATI
        // hardware (pre-2010) is ancient enough that it's not in
        // Klipp's target audience.
        if lower.contains("amd") || lower.contains("advanced micro devices") {
            return Some(GpuVendor::Amd);
        }
        if lower.contains("intel") {
            return Some(GpuVendor::Intel);
        }
    }
    if found_any {
        Some(GpuVendor::Unknown)
    } else {
        Some(GpuVendor::None)
    }
}

/// Fallback vendor detection via `/sys/module/`. The kernel
/// loads vendor-specific drivers as modules, and their presence
/// in `/sys/module/` is the most reliable signal that hardware
/// is present + active. Used when lspci isn't installed (e.g.
/// minimal Docker images, Flatpak sandboxes).
fn detect_via_sys_modules() -> Option<GpuVendor> {
    let modules = Path::new("/sys/module");
    if !modules.is_dir() {
        return None;
    }
    if modules.join("nvidia").exists() {
        return Some(GpuVendor::Nvidia);
    }
    if modules.join("amdgpu").exists() {
        return Some(GpuVendor::Amd);
    }
    if modules.join("i915").exists() || modules.join("xe").exists() {
        return Some(GpuVendor::Intel);
    }
    Some(GpuVendor::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_lspci_output ────────────────────────────────

    #[test]
    fn runtime_loadable_accepts_a_valid_explicit_path() {
        assert!(runtime_loadable(Some("/some/real/libonnxruntime.so"), true, false).is_ok());
    }

    #[test]
    fn runtime_loadable_rejects_a_missing_explicit_path() {
        let err = runtime_loadable(Some("/nope/libonnxruntime.so"), false, false)
            .expect_err("a missing explicit path must be rejected");
        assert!(err.contains("/nope/libonnxruntime.so"), "error should name the path: {err}");
    }

    #[test]
    fn a_missing_explicit_path_is_not_rescued_by_the_fallbacks() {
        // The behaviour that matters. `ort` uses ORT_DYLIB_PATH verbatim
        // and never falls back, so a bad explicit setting must fail even
        // when a perfectly good library sits in a standard location —
        // otherwise we report success and ort then deadlocks on load.
        let err = runtime_loadable(Some("/nope/libonnxruntime.so"), false, true)
            .expect_err("explicit setting must win over an available fallback");
        assert!(err.contains("/nope/libonnxruntime.so"), "error should name the path: {err}");
    }

    #[test]
    fn runtime_loadable_uses_fallbacks_when_unset() {
        assert!(runtime_loadable(None, false, true).is_ok());
        assert!(runtime_loadable(None, false, false).is_err());
    }

    #[test]
    fn classifies_nvidia() {
        let out = "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation \
                   AD103 [GeForce RTX 4080] (rev a1)";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Nvidia));
    }

    #[test]
    fn classifies_amd_advanced_micro_devices() {
        let out = "07:00.0 VGA compatible controller [0300]: Advanced Micro Devices, \
                   Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XTX]";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Amd));
    }

    #[test]
    fn classifies_intel_arc() {
        let out = "03:00.0 VGA compatible controller [0300]: Intel Corporation \
                   DG2 [Arc A770] (rev 08)";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Intel));
    }

    #[test]
    fn classifies_intel_igpu_3d_controller() {
        let out = "00:02.0 3D controller [0302]: Intel Corporation Alder Lake-P \
                   GT2 [Iris Xe Graphics] (rev 0c)";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Intel));
    }

    #[test]
    fn ignores_non_gpu_class_lines() {
        let out = "00:1f.0 ISA bridge [0601]: Intel Corporation Z690 Chipset LPC";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::None));
    }

    #[test]
    fn unknown_when_class_matches_but_vendor_doesnt() {
        let out = "01:00.0 VGA compatible controller [0300]: SomeFancyNewVendor Ltd. \
                   Magic GPU 9000";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Unknown));
    }

    #[test]
    fn empty_output_means_no_gpu() {
        assert_eq!(classify_lspci_output(""), Some(GpuVendor::None));
    }

    #[test]
    fn classify_handles_dual_gpu_picks_first_recognized() {
        // Some laptops report integrated + discrete on different
        // class lines. We pick the first hit — the order in
        // lspci output usually has the iGPU first; Klipp's
        // discrete-GPU users will fall through one line and find
        // their NVIDIA / AMD entry. Either answer is OK for
        // routing inference; matching reality.
        let out = "00:02.0 VGA compatible controller [0300]: Intel Corporation\n\
                   01:00.0 3D controller [0302]: NVIDIA Corporation Mobile";
        assert_eq!(classify_lspci_output(out), Some(GpuVendor::Intel));
    }

    // ── RuntimeStatus labels + severities ───────────────────

    #[test]
    fn runtime_status_labels_are_concise() {
        assert_eq!(RuntimeStatus::CpuOnly.label(), "CPU only");
        assert_eq!(
            RuntimeStatus::GpuAccelerationActive(GpuVendor::Nvidia).label(),
            "GPU (NVIDIA)"
        );
        assert_eq!(
            RuntimeStatus::GpuAvailableNotInstalled(GpuVendor::Nvidia).label(),
            "NVIDIA — runtime missing"
        );
        assert_eq!(
            RuntimeStatus::GpuRuntimeCpuOnly(GpuVendor::Amd).label(),
            "AMD — CPU-only runtime"
        );
    }

    #[test]
    fn runtime_status_severity_mapping() {
        assert_eq!(RuntimeStatus::CpuOnly.severity(), StatusSeverity::Neutral);
        assert_eq!(
            RuntimeStatus::GpuAccelerationActive(GpuVendor::Nvidia).severity(),
            StatusSeverity::Active
        );
        assert_eq!(
            RuntimeStatus::GpuAvailableNotInstalled(GpuVendor::Nvidia).severity(),
            StatusSeverity::Warning
        );
        assert_eq!(
            RuntimeStatus::GpuRuntimeCpuOnly(GpuVendor::Intel).severity(),
            StatusSeverity::Warning
        );
    }

    // ── Probe smoke (host-dependent) ─────────────────────────

    #[test]
    fn detect_gpu_vendor_returns_a_known_variant() {
        // Smoke test — we don't assert a specific vendor
        // because tests run on different machines. Just verify
        // the call doesn't panic and returns a valid variant.
        let v = detect_gpu_vendor();
        // Force exhaustive match so a future variant addition
        // makes this test fail loudly until the test is updated.
        match v {
            GpuVendor::Nvidia
            | GpuVendor::Amd
            | GpuVendor::Intel
            | GpuVendor::None
            | GpuVendor::Unknown => {}
        }
    }

    #[test]
    fn resolve_runtime_status_returns_a_known_variant() {
        let s = resolve_runtime_status();
        match s {
            RuntimeStatus::CpuOnly
            | RuntimeStatus::GpuAccelerationActive(_)
            | RuntimeStatus::GpuAvailableNotInstalled(_)
            | RuntimeStatus::GpuRuntimeCpuOnly(_) => {}
        }
    }
}
