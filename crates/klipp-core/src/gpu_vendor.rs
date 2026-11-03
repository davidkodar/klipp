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
                match Self::from_pci_id(&contents) {
                    GpuVendor::Unknown => continue,
                    vendor => return vendor,
                }
            }
        }
        GpuVendor::Unknown
    }

    /// The vendor a sysfs `vendor` file names.
    fn from_pci_id(id: &str) -> Self {
        match id.trim() {
            "0x10de" => GpuVendor::Nvidia,
            "0x1002" => GpuVendor::Amd,
            "0x8086" => GpuVendor::Intel,
            _ => GpuVendor::Unknown,
        }
    }

    /// Every DRM render node, `/dev/dri/renderD*`, with its GPU's vendor,
    /// in node order.
    ///
    /// Where [`Self::detect`] answers for the first card, this lets a
    /// caller pick a device by vendor: a laptop with an Intel iGPU and an
    /// NVIDIA dGPU has one render node for each.
    pub fn render_nodes() -> Vec<(std::path::PathBuf, Self)> {
        Self::render_nodes_in(std::path::Path::new("/sys/class/drm"), std::path::Path::new("/dev/dri"))
    }

    pub(crate) fn render_nodes_in(sys: &std::path::Path, dev: &std::path::Path) -> Vec<(std::path::PathBuf, Self)> {
        let mut nodes: Vec<(u32, std::path::PathBuf, Self)> = std::fs::read_dir(sys)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let n: u32 = name.strip_prefix("renderD")?.parse().ok()?;
                let vendor = std::fs::read_to_string(entry.path().join("device").join("vendor"))
                    .map(|id| Self::from_pci_id(&id))
                    .unwrap_or(GpuVendor::Unknown);
                Some((n, dev.join(&name), vendor))
            })
            .collect();
        nodes.sort_by_key(|(n, ..)| *n);
        nodes.into_iter().map(|(_, path, vendor)| (path, vendor)).collect()
    }

    /// The render nodes a VAAPI decoder may open a device on: every one
    /// but NVIDIA's, in node order.
    ///
    /// NVIDIA's VAAPI driver initialises CUDA from its constructor, while
    /// the dynamic loader's lock is held. A thread already in its first
    /// `cuInit` (a CUDA decoder or NVENC opening) takes that lock in turn,
    /// and the process deadlocks: every later library load waits behind
    /// it. CUDA is that card's decoder anyway.
    pub fn vaapi_nodes() -> Vec<std::path::PathBuf> {
        Self::vaapi_nodes_of(Self::render_nodes())
    }

    fn vaapi_nodes_of(nodes: Vec<(std::path::PathBuf, Self)>) -> Vec<std::path::PathBuf> {
        nodes.into_iter().filter(|(_, vendor)| *vendor != GpuVendor::Nvidia).map(|(node, _)| node).collect()
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

    /// Whether this machine has a GPU of a make that encodes video: what
    /// the export form can say before anything has tried to open an encoder
    /// (#3431). Apple's, under Asahi, reads as Unknown and doesn't.
    pub fn gpu_encoder_likely() -> bool {
        static LIKELY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *LIKELY.get_or_init(|| Self::gpu_encoder_among(&Self::render_nodes()))
    }

    fn gpu_encoder_among(nodes: &[(std::path::PathBuf, Self)]) -> bool {
        nodes.iter().any(|(_, vendor)| matches!(vendor, GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel))
    }

    /// What a decoder logs, once, when VAAPI did not open and it goes on in
    /// software.
    pub const VAAPI_FALLBACK_LINE: &'static str =
        "Hardware decode: no VAAPI connection opened on this machine; decoding in software, without trying VAAPI again this session";

    /// What startup can say about VAAPI without opening a connection:
    /// where the first clip will try it, or that there is nowhere to.
    pub fn vaapi_startup_line(nodes: &[std::path::PathBuf]) -> String {
        if nodes.is_empty() {
            return "Hardware decode: no render node for VAAPI on this machine".to_owned();
        }
        let nodes: Vec<String> = nodes.iter().map(|n| n.display().to_string()).collect();
        format!("Hardware decode: the first clip tries VAAPI on {}", nodes.join(", "))
    }

    /// Whether an NVIDIA driver is loaded, without which CUDA can't open: its
    /// control device, there with or without a display on the card. Opening
    /// CUDA elsewhere fails with a warning for each decoder (#3432).
    pub fn nvidia_driver_loaded() -> bool {
        static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *LOADED.get_or_init(|| std::path::Path::new("/dev/nvidiactl").exists())
    }

    /// Whether a VAAPI connection already failed on every render node in
    /// this process, so a decoder doesn't try it again.
    pub fn vaapi_unavailable() -> bool {
        VAAPI_FAILED.has_failed()
    }

    /// A VAAPI connection failed on every render node: not tried again
    /// this session. `true` the first time, for the one line that says so.
    pub fn mark_vaapi_unavailable() -> bool {
        VAAPI_FAILED.fail()
    }
}

/// A failure that holds for the rest of the process once seen. A machine
/// whose GPU has no VAAPI driver (Asahi) failed the connection the same
/// way at every clip open, an error line each time.
#[derive(Debug, Default)]
pub struct FailedOnce(std::sync::atomic::AtomicBool);

impl FailedOnce {
    pub const fn new() -> Self {
        Self(std::sync::atomic::AtomicBool::new(false))
    }

    pub fn has_failed(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record the failure: `true` the first time only.
    pub fn fail(&self) -> bool {
        !self.0.swap(true, std::sync::atomic::Ordering::Relaxed)
    }
}

static VAAPI_FAILED: FailedOnce = FailedOnce::new();

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn a_failure_is_kept_and_said_once() {
        let vaapi = FailedOnce::new();
        assert!(!vaapi.has_failed(), "tried at first");
        assert!(vaapi.fail(), "the first failure is said");
        assert!(vaapi.has_failed(), "and not tried again");
        assert!(!vaapi.fail(), "a later one is not said again");
        assert!(vaapi.has_failed());
    }

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
    fn render_nodes_are_listed_in_order_with_their_vendors() {
        let base = tmp_base("render_nodes");
        for (node, id) in [("renderD129", "0x10de"), ("renderD128", "0x8086"), ("renderD130", "0xbeef")] {
            let dir = base.join(node).join("device");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("vendor"), format!("{id}\n")).unwrap();
        }
        // A card entry is not a render node.
        write_vendor(&base, 0, "0x1002");
        let dev = std::path::Path::new("/dev/dri");
        assert_eq!(
            GpuVendor::render_nodes_in(&base, dev),
            vec![
                (dev.join("renderD128"), GpuVendor::Intel),
                (dev.join("renderD129"), GpuVendor::Nvidia),
                (dev.join("renderD130"), GpuVendor::Unknown),
            ]
        );
        let _ = fs::remove_dir_all(&base);
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

    /// #3407: startup names what the machine has, and does not call VAAPI
    /// available before anything has opened it.
    #[test]
    fn startup_says_where_vaapi_will_be_tried() {
        let nodes = [std::path::PathBuf::from("/dev/dri/renderD128"), std::path::PathBuf::from("/dev/dri/renderD129")];
        assert_eq!(GpuVendor::vaapi_startup_line(&nodes), "Hardware decode: the first clip tries VAAPI on /dev/dri/renderD128, /dev/dri/renderD129");
        assert_eq!(GpuVendor::vaapi_startup_line(&[]), "Hardware decode: no render node for VAAPI on this machine");
    }

    /// #3431: an NVIDIA, AMD or Intel GPU may encode; none, or one of a make
    /// not known here, as Apple's under Asahi, is the CPU's work.
    #[test]
    fn a_gpu_encoder_is_likely_only_with_a_known_make_of_gpu() {
        let node = |v| (std::path::PathBuf::from("/dev/dri/renderD128"), v);
        assert!(GpuVendor::gpu_encoder_among(&[node(GpuVendor::Nvidia)]));
        assert!(GpuVendor::gpu_encoder_among(&[node(GpuVendor::Unknown), node(GpuVendor::Amd)]));
        assert!(!GpuVendor::gpu_encoder_among(&[node(GpuVendor::Unknown)]));
        assert!(!GpuVendor::gpu_encoder_among(&[]));
    }

    #[test]
    fn vaapi_never_opens_an_nvidia_node() {
        let nodes = vec![
            (PathBuf::from("/dev/dri/renderD128"), GpuVendor::Intel),
            (PathBuf::from("/dev/dri/renderD129"), GpuVendor::Nvidia),
            (PathBuf::from("/dev/dri/renderD130"), GpuVendor::Unknown),
        ];
        assert_eq!(GpuVendor::vaapi_nodes_of(nodes), [PathBuf::from("/dev/dri/renderD128"), PathBuf::from("/dev/dri/renderD130")]);
        assert!(GpuVendor::vaapi_nodes_of(vec![(PathBuf::from("/dev/dri/renderD128"), GpuVendor::Nvidia)]).is_empty());
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
