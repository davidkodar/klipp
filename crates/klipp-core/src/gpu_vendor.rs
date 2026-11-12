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
        Self::described_render_nodes_in(sys, dev).into_iter().map(|n| (n.path, n.vendor)).collect()
    }

    fn described_render_nodes_in(sys: &std::path::Path, dev: &std::path::Path) -> Vec<RenderNode> {
        let mut nodes: Vec<(u32, RenderNode)> = std::fs::read_dir(sys)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let n: u32 = name.strip_prefix("renderD")?.parse().ok()?;
                let device = entry.path().join("device");
                let vendor = std::fs::read_to_string(device.join("vendor"))
                    .map(|id| Self::from_pci_id(&id))
                    .unwrap_or(GpuVendor::Unknown);
                // Apple's GPU under Asahi is a platform device: no PCI
                // vendor, its driver named "asahi".
                let apple = std::fs::read_link(device.join("driver"))
                    .ok()
                    .is_some_and(|driver| driver.file_name().is_some_and(|d| d == "asahi"));
                Some((n, RenderNode { path: dev.join(&name), vendor, apple }))
            })
            .collect();
        nodes.sort_by_key(|(n, _)| *n);
        nodes.into_iter().map(|(_, node)| node).collect()
    }

    /// The render nodes a VAAPI decoder may open a device on, in node
    /// order: none while VAAPI is off ([`Self::vaapi_use`]), else every one
    /// but NVIDIA's, and but Apple's unless forced.
    ///
    /// NVIDIA's VAAPI driver initialises CUDA from its constructor, while
    /// the dynamic loader's lock is held. A thread already in its first
    /// `cuInit` (a CUDA decoder or NVENC opening) takes that lock in turn,
    /// and the process deadlocks: every later library load waits behind
    /// it. CUDA is that card's decoder anyway.
    ///
    /// Apple's decoder under Asahi opens, but with each frame copied back
    /// from it Klipp decoded at about half the speed of software (4K HEVC
    /// on an M1, 16 vs 30 fps), so it is left out, said once, unless
    /// `KLIPP_VAAPI=force`.
    pub fn vaapi_nodes() -> Vec<std::path::PathBuf> {
        let (nodes, apple) = Self::vaapi_nodes_for(
            Self::vaapi_use(),
            Self::described_render_nodes_in(std::path::Path::new("/sys/class/drm"), std::path::Path::new("/dev/dri")),
        );
        if !apple.is_empty() && APPLE_SKIP_SAID.fail() {
            tracing::info!("{}", Self::apple_skip_line(&apple));
        }
        nodes
    }

    /// The nodes VAAPI may open under `vaapi`, and the Apple ones it
    /// passed over.
    fn vaapi_nodes_for(vaapi: VaapiUse, nodes: Vec<RenderNode>) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        if vaapi == VaapiUse::Off {
            return (Vec::new(), Vec::new());
        }
        let (apple, nodes): (Vec<RenderNode>, Vec<RenderNode>) = nodes
            .into_iter()
            .filter(|n| n.vendor != GpuVendor::Nvidia)
            .partition(|n| n.apple && vaapi != VaapiUse::Force);
        (nodes.into_iter().map(|n| n.path).collect(), apple.into_iter().map(|n| n.path).collect())
    }

    /// What is said, once, when Apple's decoder is passed over.
    fn apple_skip_line(nodes: &[std::path::PathBuf]) -> String {
        let nodes: Vec<String> = nodes.iter().map(|n| n.display().to_string()).collect();
        format!(
            "Hardware decode: not trying VAAPI on {}, Apple's GPU: copying its frames back is slower here than decoding in software (KLIPP_VAAPI=force tries it)",
            nodes.join(", ")
        )
    }

    /// Whether VAAPI may be tried in this process: the preference set
    /// with [`Self::set_vaapi_preference`] (off until then), or
    /// `KLIPP_VAAPI` over it.
    pub fn vaapi_use() -> VaapiUse {
        VaapiUse::of(VAAPI_PREFERENCE.load(std::sync::atomic::Ordering::Relaxed), std::env::var("KLIPP_VAAPI").ok().as_deref())
    }

    /// The person's VA-API preference (Preferences, System): whether
    /// decoders and export encoders may try VAAPI.
    pub fn set_vaapi_preference(on: bool) {
        VAAPI_PREFERENCE.store(on, std::sync::atomic::Ordering::Relaxed);
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
    /// (#3431). Apple's, under Asahi, reads as Unknown and doesn't; AMD's
    /// and Intel's encode through VAAPI, so only while it is on.
    pub fn gpu_encoder_likely() -> bool {
        static NODES: std::sync::OnceLock<Vec<(std::path::PathBuf, GpuVendor)>> = std::sync::OnceLock::new();
        Self::gpu_encoder_among(NODES.get_or_init(Self::render_nodes), Self::vaapi_use() != VaapiUse::Off)
    }

    fn gpu_encoder_among(nodes: &[(std::path::PathBuf, Self)], vaapi: bool) -> bool {
        nodes.iter().any(|(_, vendor)| match vendor {
            GpuVendor::Nvidia => true,
            GpuVendor::Amd | GpuVendor::Intel => vaapi,
            GpuVendor::Unknown => false,
        })
    }

    /// What a decoder logs, once, when VAAPI did not open and it goes on in
    /// software.
    pub const VAAPI_FALLBACK_LINE: &'static str =
        "Hardware decode: no VAAPI connection opened on this machine; decoding in software, without trying VAAPI again this session";

    /// What startup can say about VAAPI without opening a connection:
    /// that it is off, where the first clip will try it, or that there is
    /// nowhere to.
    pub fn vaapi_startup_line(vaapi: VaapiUse, nodes: &[std::path::PathBuf]) -> String {
        if vaapi == VaapiUse::Off {
            return "Hardware decode: VAAPI off (Preferences, System, or KLIPP_VAAPI=1 turns it on)".to_owned();
        }
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

/// Apple's node was passed over, and said so.
static APPLE_SKIP_SAID: FailedOnce = FailedOnce::new();

/// The VA-API preference; off until the person turns it on.
static VAAPI_PREFERENCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A DRM render node and what drives it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderNode {
    path: std::path::PathBuf,
    vendor: GpuVendor,
    /// Apple's GPU, driven by Asahi's `asahi` driver.
    apple: bool,
}

/// Whether VAAPI may be tried: off unless the person turned it on. Its
/// drivers have not yet been tested on AMD and Intel GPUs here, and
/// Apple's decoder is slower than software as Klipp copies frames back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaapiUse {
    /// Not tried: decoders and encoders go on as with no VAAPI driver.
    Off,
    /// Tried on every render node but NVIDIA's and Apple's.
    On,
    /// `KLIPP_VAAPI=force`: on Apple's node too. Never NVIDIA's.
    Force,
}

impl VaapiUse {
    /// From the preference and `KLIPP_VAAPI`: `1` or `on` turns VAAPI on
    /// and `0` or `off` turns it off whatever the preference says, `force`
    /// also tries Apple's decoder; unset or anything else leaves it to the
    /// preference.
    pub fn of(preference: bool, env: Option<&str>) -> Self {
        match env.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("force") => Self::Force,
            Some("1" | "on" | "true" | "yes") => Self::On,
            Some("0" | "off" | "false" | "no") => Self::Off,
            _ if preference => Self::On,
            _ => Self::Off,
        }
    }
}

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
    /// available before anything has opened it; nor tried, while it is off.
    #[test]
    fn startup_says_where_vaapi_will_be_tried() {
        let nodes = [std::path::PathBuf::from("/dev/dri/renderD128"), std::path::PathBuf::from("/dev/dri/renderD129")];
        assert_eq!(GpuVendor::vaapi_startup_line(VaapiUse::On, &nodes), "Hardware decode: the first clip tries VAAPI on /dev/dri/renderD128, /dev/dri/renderD129");
        assert_eq!(GpuVendor::vaapi_startup_line(VaapiUse::On, &[]), "Hardware decode: no render node for VAAPI on this machine");
        assert_eq!(GpuVendor::vaapi_startup_line(VaapiUse::Off, &[]), "Hardware decode: VAAPI off (Preferences, System, or KLIPP_VAAPI=1 turns it on)");
    }

    /// #3431: an NVIDIA GPU may encode, and AMD's or Intel's while VAAPI is
    /// on; none, or one of a make not known here, as Apple's under Asahi, is
    /// the CPU's work.
    #[test]
    fn a_gpu_encoder_is_likely_only_with_a_known_make_of_gpu() {
        let node = |v| (std::path::PathBuf::from("/dev/dri/renderD128"), v);
        assert!(GpuVendor::gpu_encoder_among(&[node(GpuVendor::Nvidia)], false));
        assert!(GpuVendor::gpu_encoder_among(&[node(GpuVendor::Unknown), node(GpuVendor::Amd)], true));
        assert!(!GpuVendor::gpu_encoder_among(&[node(GpuVendor::Unknown), node(GpuVendor::Amd)], false), "AMD encodes through VAAPI, off");
        assert!(!GpuVendor::gpu_encoder_among(&[node(GpuVendor::Intel)], false), "Intel too");
        assert!(!GpuVendor::gpu_encoder_among(&[node(GpuVendor::Unknown)], true));
        assert!(!GpuVendor::gpu_encoder_among(&[], true));
    }

    fn node(n: u32, vendor: GpuVendor, apple: bool) -> RenderNode {
        RenderNode { path: PathBuf::from(format!("/dev/dri/renderD{n}")), vendor, apple }
    }

    #[test]
    fn vaapi_never_opens_an_nvidia_node() {
        let nodes = vec![node(128, GpuVendor::Intel, false), node(129, GpuVendor::Nvidia, false), node(130, GpuVendor::Unknown, false)];
        for vaapi in [VaapiUse::On, VaapiUse::Force] {
            assert_eq!(GpuVendor::vaapi_nodes_for(vaapi, nodes.clone()).0, [PathBuf::from("/dev/dri/renderD128"), PathBuf::from("/dev/dri/renderD130")]);
            assert!(GpuVendor::vaapi_nodes_for(vaapi, vec![node(128, GpuVendor::Nvidia, false)]).0.is_empty(), "{vaapi:?}");
        }
    }

    /// Off by default: no node is opened, on any make of GPU.
    #[test]
    fn vaapi_off_opens_no_node() {
        let nodes = vec![node(128, GpuVendor::Amd, false), node(129, GpuVendor::Intel, false), node(130, GpuVendor::Unknown, true)];
        assert_eq!(GpuVendor::vaapi_nodes_for(VaapiUse::Off, nodes), (Vec::new(), Vec::new()));
    }

    /// Apple's decoder is passed over, and named, unless forced.
    #[test]
    fn apples_node_is_passed_over_unless_forced() {
        let nodes = vec![node(128, GpuVendor::Unknown, true), node(129, GpuVendor::Amd, false)];
        assert_eq!(
            GpuVendor::vaapi_nodes_for(VaapiUse::On, nodes.clone()),
            (vec![PathBuf::from("/dev/dri/renderD129")], vec![PathBuf::from("/dev/dri/renderD128")])
        );
        assert_eq!(
            GpuVendor::vaapi_nodes_for(VaapiUse::Force, nodes),
            (vec![PathBuf::from("/dev/dri/renderD128"), PathBuf::from("/dev/dri/renderD129")], Vec::new())
        );
        assert_eq!(
            GpuVendor::apple_skip_line(&[PathBuf::from("/dev/dri/renderD128")]),
            "Hardware decode: not trying VAAPI on /dev/dri/renderD128, Apple's GPU: copying its frames back is slower here than decoding in software (KLIPP_VAAPI=force tries it)"
        );
    }

    /// The preference decides, unless `KLIPP_VAAPI` says otherwise.
    #[test]
    fn klipp_vaapi_overrules_the_preference() {
        for pref in [false, true] {
            for on in ["1", "on", " ON ", "true", "yes"] {
                assert_eq!(VaapiUse::of(pref, Some(on)), VaapiUse::On, "{on:?}");
            }
            for off in ["0", "off", "false", "no"] {
                assert_eq!(VaapiUse::of(pref, Some(off)), VaapiUse::Off, "{off:?}");
            }
            assert_eq!(VaapiUse::of(pref, Some("force")), VaapiUse::Force);
        }
        assert_eq!(VaapiUse::of(false, None), VaapiUse::Off, "off by default");
        assert_eq!(VaapiUse::of(true, None), VaapiUse::On);
        assert_eq!(VaapiUse::of(true, Some("")), VaapiUse::On, "an empty or unknown value leaves it to the preference");
        assert_eq!(VaapiUse::of(false, Some("maybe")), VaapiUse::Off);
    }

    /// Apple's GPU is known by its driver, having no PCI vendor.
    #[test]
    fn apples_gpu_is_known_by_its_asahi_driver() {
        let base = tmp_base("asahi_driver");
        let drivers = base.join("bus");
        for (node, driver, vendor) in [("renderD128", "asahi", None), ("renderD129", "amdgpu", Some("0x1002"))] {
            let dir = base.join(node).join("device");
            fs::create_dir_all(&dir).unwrap();
            fs::create_dir_all(drivers.join(driver)).unwrap();
            std::os::unix::fs::symlink(drivers.join(driver), dir.join("driver")).unwrap();
            if let Some(id) = vendor {
                fs::write(dir.join("vendor"), format!("{id}\n")).unwrap();
            }
        }
        assert_eq!(
            GpuVendor::described_render_nodes_in(&base, std::path::Path::new("/dev/dri")),
            vec![node(128, GpuVendor::Unknown, true), node(129, GpuVendor::Amd, false)]
        );
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
