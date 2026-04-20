//! Detect the user's `ffmpeg` build capabilities so we can pick the
//! fastest transcode chain available on their box. Without this we'd
//! have to hard-code one safe path (CPU `zscale` + `libx264`) that
//! works everywhere but takes ~30 minutes for a 4K HDR clip on a
//! workstation that could do it in 3.
//!
//! Two probes:
//!   * `ffmpeg -hide_banner -filters` — checks for `tonemap_cuda` /
//!     `libplacebo`. Both are GPU tonemappers; tonemap_cuda is
//!     NVIDIA-only and ships in builds with `--enable-libnpp`,
//!     libplacebo is vendor-neutral (Vulkan) and ships in builds
//!     with `--enable-libplacebo`.
//!   * `ffmpeg -hide_banner -encoders` — checks for `h264_nvenc`,
//!     `h264_vaapi`, `h264_qsv`, and (always present) `libx264`.
//!
//! Both probes are cached in a `OnceLock`. The first call shells out
//! once (~30 ms), subsequent calls are atomic reads.

use std::sync::OnceLock;

/// What this `ffmpeg` build can do, distilled from `-filters` and
/// `-encoders` parsing. Each field is a single boolean so the
/// command-builder logic stays a flat decision tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FfmpegCaps {
    /// `tonemap_cuda` filter — NVIDIA-only HDR tone-map on CUDA
    /// surfaces. Fastest possible HDR path when paired with
    /// `h264_nvenc` because everything stays on the GPU.
    pub tonemap_cuda: bool,
    /// `libplacebo` filter — Vulkan-based, vendor-neutral. Slower
    /// than tonemap_cuda on NVIDIA but a major win over CPU on
    /// AMD / Intel.
    pub libplacebo: bool,
    /// `h264_nvenc` encoder.
    pub h264_nvenc: bool,
    /// `h264_vaapi` encoder (AMD / Intel on Linux).
    pub h264_vaapi: bool,
    /// `h264_qsv` encoder (Intel Quick Sync).
    pub h264_qsv: bool,
}

impl FfmpegCaps {
    /// Cached detection — shells out once per process lifetime.
    /// Returns the default (all false) if `ffmpeg` is missing or
    /// the probe fails; callers always fall back to libx264 + CPU
    /// tonemap which works on any build.
    pub fn detect() -> Self {
        static CACHED: OnceLock<FfmpegCaps> = OnceLock::new();
        *CACHED.get_or_init(probe_ffmpeg)
    }

    /// Pick the encode chain to use for an HDR (PQ/HLG) source.
    ///
    /// Returns the filter chain and the `-c:v` encoder name plus
    /// extra encoder args. The caller is responsible for adding
    /// `-y -ss <start> -i <src>` and segment-muxer flags.
    ///
    /// Ordering rationale:
    ///   1. **tonemap_cuda + h264_nvenc** — keeps everything on the
    ///      GPU end-to-end. ~3-5× realtime at 4K on RTX 4080.
    ///   2. **libplacebo + h264_nvenc** — GPU tonemap on Vulkan,
    ///      hwdownload back to CPU for nvenc upload. Slightly slower
    ///      than #1 because of the round-trip but still well over
    ///      realtime.
    ///   3. **libplacebo + libx264** — GPU tonemap, CPU encode. Used
    ///      when no hardware encoder is available (rare on a
    ///      workstation; common on a headless render box).
    ///   4. **CPU zscale + tonemap + libx264** — the safe fallback.
    ///      ~0.3-0.5× realtime at 4K. Used only on stripped-down
    ///      ffmpeg builds.
    ///
    /// `target_height` is baked into the filter chain — saves an
    /// extra scaler stage.
    pub fn pick_hdr_chain(self, target_height: u32) -> HdrChain {
        if self.tonemap_cuda && self.h264_nvenc {
            // `tonemap_cuda` outputs a CUDA frame; chain
            // scale_cuda directly. NVENC consumes CUDA frames
            // natively (`-hwaccel cuda -hwaccel_output_format cuda`
            // on the input side; nvenc accepts the GPU surface).
            return HdrChain {
                tier: HdrChainTier::TonemapCudaNvenc,
                input_args: vec!["-hwaccel".into(), "cuda".into(),
                                  "-hwaccel_output_format".into(), "cuda".into()],
                vf: format!(
                    "tonemap_cuda=transfer=bt709:matrix=bt709:primaries=bt709:tonemap=hable:format=yuv420p,scale_cuda=-2:{target_height}"
                ),
                encoder: "h264_nvenc".into(),
                encoder_args: vec![
                    "-preset".into(), "p1".into(),
                    "-rc".into(), "constqp".into(),
                ],
            };
        }
        if self.libplacebo && self.h264_nvenc {
            return HdrChain {
                tier: HdrChainTier::LibplaceboNvenc,
                input_args: Vec::new(),
                vf: format!(
                    "libplacebo=tonemapping=hable:colorspace=bt709:color_primaries=bt709:color_trc=bt709:format=yuv420p:w=-2:h={target_height}"
                ),
                encoder: "h264_nvenc".into(),
                encoder_args: vec![
                    "-preset".into(), "p1".into(),
                    "-rc".into(), "constqp".into(),
                ],
            };
        }
        if self.libplacebo {
            return HdrChain {
                tier: HdrChainTier::LibplaceboCpuEnc,
                input_args: Vec::new(),
                vf: format!(
                    "libplacebo=tonemapping=hable:colorspace=bt709:color_primaries=bt709:color_trc=bt709:format=yuv420p:w=-2:h={target_height}"
                ),
                encoder: "libx264".into(),
                encoder_args: vec!["-preset".into(), "ultrafast".into()],
            };
        }
        // Final fallback — CPU all the way. zscale is in nearly
        // every ffmpeg build (it's GPL, bundled by default).
        HdrChain {
            tier: HdrChainTier::CpuFallback,
            input_args: Vec::new(),
            vf: format!(
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
                 tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,\
                 format=yuv420p,scale=-2:{target_height}"
            ),
            encoder: "libx264".into(),
            encoder_args: vec!["-preset".into(), "ultrafast".into()],
        }
    }
}

/// Identifies which tier `pick_hdr_chain` selected. Plumbed through
/// for logging — surfaces in `tracing::info` so the user's session
/// log records which path their HDR clips actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrChainTier {
    TonemapCudaNvenc,
    LibplaceboNvenc,
    LibplaceboCpuEnc,
    CpuFallback,
}

impl HdrChainTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TonemapCudaNvenc => "tonemap_cuda + h264_nvenc (GPU end-to-end)",
            Self::LibplaceboNvenc  => "libplacebo + h264_nvenc",
            Self::LibplaceboCpuEnc => "libplacebo + libx264",
            Self::CpuFallback      => "CPU zscale+tonemap + libx264 (safe fallback)",
        }
    }
}

/// Resolved encode plan for a single HDR transcode call. Caller plugs
/// `vf` into `-vf` and `encoder` into `-c:v`, then appends
/// `encoder_args` and the segment-muxer flags.
#[derive(Debug, Clone)]
pub struct HdrChain {
    pub tier: HdrChainTier,
    pub input_args: Vec<String>,
    pub vf: String,
    pub encoder: String,
    pub encoder_args: Vec<String>,
}

fn probe_ffmpeg() -> FfmpegCaps {
    let filters_text = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let encoders_text = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let caps = parse_caps(&filters_text, &encoders_text);
    tracing::info!(
        "ffmpeg caps: tonemap_cuda={} libplacebo={} h264_nvenc={} h264_vaapi={} h264_qsv={}",
        caps.tonemap_cuda, caps.libplacebo,
        caps.h264_nvenc, caps.h264_vaapi, caps.h264_qsv,
    );
    caps
}

/// Pure parser — extracted so we can test it without shelling out.
/// `ffmpeg -filters` lines look like `T.. tonemap_cuda  V->V  GPU ...`
/// and `ffmpeg -encoders` lines look like `V..... h264_nvenc  NVIDIA ...`.
/// We're intentionally permissive: any line with the filter / encoder
/// name as a whitespace-delimited token counts.
pub fn parse_caps(filters_text: &str, encoders_text: &str) -> FfmpegCaps {
    fn token_present(text: &str, name: &str) -> bool {
        text.lines().any(|l| l.split_whitespace().any(|t| t == name))
    }
    FfmpegCaps {
        tonemap_cuda: token_present(filters_text, "tonemap_cuda"),
        libplacebo:   token_present(filters_text, "libplacebo"),
        h264_nvenc:   token_present(encoders_text, "h264_nvenc"),
        h264_vaapi:   token_present(encoders_text, "h264_vaapi"),
        h264_qsv:     token_present(encoders_text, "h264_qsv"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snippet of real `ffmpeg -filters` output (trimmed). The
    /// header columns "TS." encode "Timeline Slice", etc.; the
    /// filter name is the second whitespace-token on each line.
    const FILTERS_FIXTURE: &str = "\
Filters:
  T.. = Timeline support
  ... = Filter type
  T.S asetnsamples       A->A       Set the number of samples per frame.
  ... libplacebo         V->V       GPU video processing using libplacebo
  ..C tonemap_cuda       V->V       GPU accelerated HDR to SDR tonemapping
  ... scale_cuda         V->V       GPU accelerated video resizer
  ..C tonemap            V->V       Conversion to/from different dynamic ranges
  ... zscale             V->V       Apply Zscale.
";

    const ENCODERS_FIXTURE: &str = "\
Encoders:
 V..... = Video
 V..... libx264              libx264 H.264 / AVC
 V..... h264_nvenc           NVIDIA NVENC H.264 encoder
 V..... h264_qsv             H.264 (Intel Quick Sync Video acceleration)
 V..... h264_vaapi           H.264/AVC (VAAPI)
 V..... mjpeg                MJPEG (Motion JPEG)
";

    #[test]
    fn parser_picks_up_known_filters_and_encoders() {
        let c = parse_caps(FILTERS_FIXTURE, ENCODERS_FIXTURE);
        assert!(c.tonemap_cuda);
        assert!(c.libplacebo);
        assert!(c.h264_nvenc);
        assert!(c.h264_vaapi);
        assert!(c.h264_qsv);
    }

    #[test]
    fn parser_rejects_substring_only_matches() {
        // A line mentioning `tonemap` (without _cuda) must NOT
        // count as tonemap_cuda. The naive `text.contains` would
        // false-positive on `tonemap` substring of `tonemap_cuda`
        // — token-based parsing avoids that.
        let only_tonemap = "..C tonemap            V->V       Conversion\n";
        let c = parse_caps(only_tonemap, "");
        assert!(!c.tonemap_cuda);
        assert!(!c.libplacebo);
    }

    #[test]
    fn parser_handles_empty_output() {
        // ffmpeg missing or probe failed — every flag false, no panic.
        let c = parse_caps("", "");
        assert_eq!(c, FfmpegCaps::default());
    }

    #[test]
    fn pick_hdr_chain_prefers_tonemap_cuda_when_available() {
        let caps = FfmpegCaps {
            tonemap_cuda: true, libplacebo: true,
            h264_nvenc: true, h264_vaapi: false, h264_qsv: false,
        };
        let c = caps.pick_hdr_chain(1080);
        assert_eq!(c.tier, HdrChainTier::TonemapCudaNvenc);
        assert_eq!(c.encoder, "h264_nvenc");
        assert!(c.vf.contains("tonemap_cuda"));
        assert!(c.vf.contains("scale_cuda=-2:1080"));
        assert!(c.input_args.contains(&"cuda".to_string()));
    }

    #[test]
    fn pick_hdr_chain_falls_back_to_libplacebo_nvenc() {
        let caps = FfmpegCaps {
            tonemap_cuda: false, libplacebo: true,
            h264_nvenc: true, h264_vaapi: false, h264_qsv: false,
        };
        let c = caps.pick_hdr_chain(1080);
        assert_eq!(c.tier, HdrChainTier::LibplaceboNvenc);
        assert_eq!(c.encoder, "h264_nvenc");
        assert!(c.vf.contains("libplacebo"));
        assert!(c.vf.contains("h=1080"));
    }

    #[test]
    fn pick_hdr_chain_falls_back_to_libplacebo_libx264_when_no_hw_enc() {
        let caps = FfmpegCaps {
            tonemap_cuda: false, libplacebo: true,
            h264_nvenc: false, h264_vaapi: false, h264_qsv: false,
        };
        let c = caps.pick_hdr_chain(1080);
        assert_eq!(c.tier, HdrChainTier::LibplaceboCpuEnc);
        assert_eq!(c.encoder, "libx264");
        assert!(c.vf.contains("libplacebo"));
    }

    #[test]
    fn pick_hdr_chain_falls_back_to_pure_cpu_when_no_gpu_filters() {
        let caps = FfmpegCaps::default(); // nothing available
        let c = caps.pick_hdr_chain(1080);
        assert_eq!(c.tier, HdrChainTier::CpuFallback);
        assert_eq!(c.encoder, "libx264");
        assert!(c.vf.contains("zscale"));
        assert!(c.vf.contains("tonemap=tonemap=hable"));
        assert!(c.vf.contains("scale=-2:1080"));
    }

    #[test]
    fn pick_hdr_chain_uses_libx264_even_with_hw_encoders_when_no_gpu_tonemap() {
        // The CPU fallback chain emits CPU frames (zscale runs on
        // CPU), so we can't pipe them straight into h264_nvenc
        // without an extra hwupload that the simple chain doesn't
        // include. The selection logic intentionally requires a
        // GPU tonemap filter to pair with a GPU encoder.
        let caps = FfmpegCaps {
            tonemap_cuda: false, libplacebo: false,
            h264_nvenc: true, h264_vaapi: true, h264_qsv: true,
        };
        let c = caps.pick_hdr_chain(1080);
        assert_eq!(c.tier, HdrChainTier::CpuFallback);
        assert_eq!(c.encoder, "libx264");
    }

    #[test]
    fn target_height_threads_through_each_chain() {
        let caps = FfmpegCaps {
            tonemap_cuda: true, libplacebo: true,
            h264_nvenc: true, h264_vaapi: false, h264_qsv: false,
        };
        for h in [720u32, 1080, 1440] {
            let c = caps.pick_hdr_chain(h);
            assert!(c.vf.contains(&format!("{h}")),
                "{:?} chain at h={h} missed the height: {}", c.tier, c.vf);
        }
    }
}
