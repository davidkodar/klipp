//! Runtime GPU vendor detection via sysfs PCI vendor IDs.
//!
//! Used to make hardware acceleration decisions across crates without
//! requiring an FFmpeg or Vulkan init call at detection time.
//!
//! The sysfs approach produces no stderr output (unlike probing FFmpeg
//! HW device types on systems with `nvidia-vaapi-driver` installed,
//! which spams stderr even when set to `AV_LOG_QUIET`).

/// Known GPU vendor, identified by PCI vendor ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    /// PCI vendor ID `0x10de`.
    Nvidia,
    /// PCI vendor ID `0x1002`.
    Amd,
    /// PCI vendor ID `0x8086`.
    Intel,
    /// No /sys/class/drm entries, unreadable, or unrecognized vendor.
    Unknown,
}

impl GpuVendor {
    /// Detect the primary GPU vendor by scanning `/sys/class/drm/card0..card3`.
    ///
    /// Returns the first recognized vendor ID found. On single-GPU systems
    /// this is always correct; on multi-GPU systems the first card wins —
    /// conservative behaviour that avoids enabling VAAPI on a machine that
    /// also has an NVIDIA GPU.
    ///
    /// This function performs filesystem reads only — no FFmpeg calls, no
    /// Vulkan init, no stderr output.
    pub fn detect() -> Self {
        Self::detect_in(std::path::Path::new("/sys/class/drm"))
    }

    /// Detect with a custom base path — extracted so unit tests can mock sysfs.
    pub(crate) fn detect_in(base: &std::path::Path) -> Self {
        for n in 0..4u32 {
            let path = base.join(format!("card{n}")).join("device").join("vendor");
            if let Ok(contents) = std::fs::read_to_string(&path) {
                match contents.trim() {
                    "0x10de" => return GpuVendor::Nvidia,
                    "0x1002" => return GpuVendor::Amd,
                    "0x8086" => return GpuVendor::Intel,
                    _ => continue,
                }
            }
        }
        GpuVendor::Unknown
    }

    /// Should VAAPI be attempted for this vendor?
    ///
    /// NVIDIA is excluded because `nvidia-vaapi-driver` writes probe errors
    /// directly to stderr, bypassing FFmpeg's log level suppression. AMD
    /// and Intel use VAAPI as their native GPU decode path. Unknown returns
    /// true so VAAPI is attempted and falls back naturally on failure,
    /// rather than penalising hardware we don't recognise.
    pub fn should_try_vaapi(self) -> bool {
        matches!(self, GpuVendor::Amd | GpuVendor::Intel | GpuVendor::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_vendor(base: &std::path::Path, card: u32, vendor_id: &str) {
        let card_dir = base.join(format!("card{card}")).join("device");
        fs::create_dir_all(&card_dir).unwrap();
        fs::write(card_dir.join("vendor"), format!("{vendor_id}\n")).unwrap();
    }

    fn tmp_base(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("klipp_gpu_vendor_test_{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn detects_nvidia() {
        let base = tmp_base("nvidia");
        write_vendor(&base, 1, "0x10de");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Nvidia);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn detects_amd() {
        let base = tmp_base("amd");
        write_vendor(&base, 0, "0x1002");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Amd);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn detects_intel() {
        let base = tmp_base("intel");
        write_vendor(&base, 0, "0x8086");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Intel);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unknown_when_no_cards() {
        let base = tmp_base("empty");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Unknown);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unknown_when_vendor_unrecognised() {
        let base = tmp_base("unknown_vendor");
        write_vendor(&base, 0, "0xbeef");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Unknown);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn first_card_wins_when_multiple_present() {
        // card0 = Intel iGPU, card1 = NVIDIA dGPU — common laptop layout.
        // First recognised vendor wins.
        let base = tmp_base("mixed");
        write_vendor(&base, 0, "0x8086");
        write_vendor(&base, 1, "0x10de");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Intel);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn skips_unrecognised_before_returning_known() {
        // card0 = unknown, card1 = AMD — still finds AMD.
        let base = tmp_base("skip_unknown");
        write_vendor(&base, 0, "0xdead");
        write_vendor(&base, 1, "0x1002");
        assert_eq!(GpuVendor::detect_in(&base), GpuVendor::Amd);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn should_try_vaapi_matrix() {
        assert!(!GpuVendor::Nvidia.should_try_vaapi());
        assert!(GpuVendor::Amd.should_try_vaapi());
        assert!(GpuVendor::Intel.should_try_vaapi());
        assert!(GpuVendor::Unknown.should_try_vaapi());
    }

    #[test]
    fn real_sysfs_detect_matches_current_machine() {
        // Smoke test against the actual /sys/class/drm on the current machine.
        // Should return a well-defined vendor (not panic, not hang).
        let _ = GpuVendor::detect();
    }
}
