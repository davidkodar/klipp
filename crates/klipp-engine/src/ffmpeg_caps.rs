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

    /// The encode plan for an HDR clip's skim: its picture through
    /// `cube`, a 3D LUT baked from Klipp's own HDR to SDR conversion
    /// (`klipp_render::skim_lut`), so the skim shows what the export and
    /// the parked frame show. ffmpeg's tone mappers (tonemap_cuda,
    /// libplacebo, zscale + tonemap) drew it with their own curve: 100-nit
    /// grey at 198 of 255 where the export has 139.
    ///
    /// Decoded on the CPU, scaled to `target_height` in YUV, turned into
    /// full-range BT.2020 R'G'B' at 16 bits (the codes the LUT takes),
    /// through the LUT, back to limited BT.709 Y'CbCr and tagged so: the
    /// engine plays it as the SDR picture it is. NVENC encodes it where
    /// there is one, libx264 elsewhere.
    pub fn pick_hdr_chain(self, target_height: u32, cube: &std::path::Path) -> HdrChain {
        // A path in a filter argument: `:`, `,`, `'` and `\` are its syntax.
        let cube = cube.to_string_lossy().replace('\\', "\\\\").replace(':', "\\:").replace(',', "\\,").replace('\'', "\\'");
        let vf = format!(
            "scale=w=-2:h={target_height}:in_color_matrix=bt2020:in_range=tv:out_range=pc,format=gbrp16le,\
             lut3d=file={cube}:interp=tetrahedral,\
             scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,\
             setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
        );
        let (tier, encoder, encoder_args) = if self.h264_nvenc {
            (HdrChainTier::LutNvenc, "h264_nvenc", vec!["-preset".to_string(), "p1".into(), "-rc".into(), "constqp".into()])
        } else {
            (HdrChainTier::LutCpuEnc, "libx264", vec!["-preset".to_string(), "ultrafast".into()])
        };
        HdrChain { tier, input_args: Vec::new(), vf, encoder: encoder.into(), encoder_args }
    }
}

/// Identifies which tier `pick_hdr_chain` selected. Plumbed through
/// for logging — surfaces in `tracing::info` so the user's session
/// log records which path their HDR clips actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrChainTier {
    LutNvenc,
    LutCpuEnc,
}

impl HdrChainTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LutNvenc => "Klipp's tone map (lut3d) + h264_nvenc",
            Self::LutCpuEnc => "Klipp's tone map (lut3d) + libx264",
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
    fn an_hdr_skim_goes_through_klipps_lut_and_is_tagged_sdr() {
        let cube = std::path::Path::new("/cache/klipp/skim/klipp-skim-pq-1a2b3c4d.cube");
        for (nvenc, tier, encoder) in [(true, HdrChainTier::LutNvenc, "h264_nvenc"), (false, HdrChainTier::LutCpuEnc, "libx264")] {
            let caps = FfmpegCaps { tonemap_cuda: true, libplacebo: true, h264_nvenc: nvenc, h264_vaapi: false, h264_qsv: false };
            let c = caps.pick_hdr_chain(1080, cube);
            assert_eq!((c.tier, c.encoder.as_str()), (tier, encoder));
            assert!(c.vf.contains("lut3d=file=/cache/klipp/skim/klipp-skim-pq-1a2b3c4d.cube"), "{}", c.vf);
            assert!(!c.vf.contains("tonemap") && !c.vf.contains("libplacebo"), "ffmpeg's own tone map is not used: {}", c.vf);
            assert!(c.vf.contains("in_color_matrix=bt2020") && c.vf.contains("color_trc=bt709"), "{}", c.vf);
            assert!(c.vf.contains("h=1080"));
        }
    }

    #[test]
    fn a_cube_path_is_escaped_for_the_filter() {
        let c = FfmpegCaps::default().pick_hdr_chain(720, std::path::Path::new("/home/a:b,c/x.cube"));
        assert!(c.vf.contains("lut3d=file=/home/a\\:b\\,c/x.cube:interp"), "{}", c.vf);
    }
}
