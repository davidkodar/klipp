use crate::events::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportVideoCodec, RateControl};
use crate::time::Fps;
use crate::types::Resolution;
use serde::{Deserialize, Serialize};

pub type PresetId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportPreset {
    pub id: PresetId,
    pub name: String,
    pub built_in: bool,
    pub settings: ExportPresetSettings,
}

/// Export settings without output_path — path is chosen at export time, not stored in preset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportPresetSettings {
    pub format: ExportFormat,
    pub video_codec: ExportVideoCodec,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub crf: u32,
    #[serde(default)]
    pub preset: EncoderPreset,
    pub audio_codec: ExportAudioCodec,
    pub audio_bitrate_kbps: Option<u32>,
    /// Video bitrate in kbps (for CBR/VBR modes). None = use CRF instead.
    #[serde(default)]
    pub video_bitrate_kbps: Option<u32>,
    /// Rate control mode: CRF (default), CBR, or VBR.
    #[serde(default)]
    pub rate_control: RateControl,
    /// Target integrated loudness in LUFS for EBU R128 normalization. None = disabled.
    #[serde(default)]
    pub target_lufs: Option<f32>,
    /// The true-peak ceiling in dBTP; None is -1. See
    /// `ExportSettings::true_peak_dbtp`.
    #[serde(default)]
    pub true_peak_dbtp: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyResolution {
    Quarter,
    #[default]
    Half,
    W960,
    W1280,
}

impl ProxyResolution {
    pub fn ffmpeg_scale_arg(&self) -> &'static str {
        match self {
            Self::Quarter => "scale=iw/4:-2",
            Self::Half => "scale=iw/2:-2",
            Self::W960 => "scale=960:-2",
            Self::W1280 => "scale=1280:-2",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Quarter => "Quarter resolution (÷4)",
            Self::Half => "Half resolution (÷2)",
            Self::W960 => "960px wide",
            Self::W1280 => "1280px wide",
        }
    }
}

/// The built-in presets, with deterministic UUIDs so a project that
/// names one keeps it across versions.
pub fn builtin_presets() -> Vec<ExportPreset> {
    vec![
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000001".into(),
            name: "YouTube 1080p".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1920,
                    height: 1080,
                }),
                fps: None,
                crf: 18,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000002".into(),
            name: "YouTube 4K".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: Some(Resolution {
                    width: 3840,
                    height: 2160,
                }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000003".into(),
            name: "Vimeo 1080p".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1920,
                    height: 1080,
                }),
                fps: None,
                crf: 16,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000004".into(),
            name: "Twitter/X".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1280,
                    height: 720,
                }),
                fps: None,
                crf: 23,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(128),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000005".into(),
            name: "Instagram Square".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1080,
                    height: 1080,
                }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000006".into(),
            name: "Instagram Vertical".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1080,
                    height: 1920,
                }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000007".into(),
            name: "ProRes 422".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mov,
                video_codec: ExportVideoCodec::ProRes422,
                resolution: None,
                fps: None,
                crf: 0,
                preset: EncoderPreset::default(),
                // PCM, not `Copy`: that one skips the audio stream
                // entirely (there is nothing to copy from an in-process
                // mixdown), so this preset used to write a silent
                // master — see #156. A mastering deliverable wants
                // uncompressed audio anyway.
                audio_codec: ExportAudioCodec::Pcm24,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000008".into(),
            name: "DNxHD 115".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mov,
                video_codec: ExportVideoCodec::DNxHD115,
                resolution: Some(Resolution {
                    width: 1920,
                    height: 1080,
                }),
                fps: None,
                crf: 0,
                preset: EncoderPreset::default(),
                // As with ProRes above: `Copy` wrote no audio at all
                // (#156), which made the Avid interchange master silent.
                audio_codec: ExportAudioCodec::Pcm24,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000009".into(),
            name: "H.265 4K".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: Some(Resolution {
                    width: 3840,
                    height: 2160,
                }),
                fps: None,
                crf: 22,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000b".into(),
            name: "H.265 HQ (GPU)".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: None,
                fps: None,
                crf: 16,
                preset: EncoderPreset::Slow,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000a".into(),
            name: "Audio Only MP3".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: None,
                fps: None,
                crf: 28,
                preset: EncoderPreset::Fast,
                audio_codec: ExportAudioCodec::Mp3,
                audio_bitrate_kbps: Some(320),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
        // Animated GIF — two-pass palette workflow. 480p / 15fps is
        // the social-media sweet spot; users override resolution / fps
        // via the export dialog when they want larger or slower.
        // Audio settings are nominally present but ignored by the GIF
        // encoder path — the dialog also greys them out.
        // ── Deliveries that state a loudness ──
        //
        // The export dialogue has offered loudness targets by name for
        // a while, and the two-pass normaliser behind them is finished
        // and correct. What was missing was a preset that pairs one
        // with a format, so delivering to a spec is a choice from a
        // list rather than remembering to tick a box and pick a number
        // every time.
        //
        // A preset that names a spec has to hold its true-peak limit
        // as well as its loudness. The renderer's ceiling is -1 dBTP
        // unless `true_peak_dbtp` says otherwise, which is right for EBU
        // R128 and the -14 streaming convention. ATSC A/85 and Netflix
        // want -2, so a preset for either must set it (#122).
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000d".into(),
            name: "Broadcast master (EBU R128)".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mov,
                video_codec: ExportVideoCodec::ProRes422,
                resolution: None,
                fps: None,
                crf: 0,
                preset: EncoderPreset::default(),
                audio_codec: ExportAudioCodec::Pcm24,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                // -23 LUFS integrated, true peak at or under -1 dBTP.
                // The number a European broadcaster's automated QC
                // measures, and the one it rejects a file over.
                target_lufs: Some(-23.0),
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000e".into(),
            name: "Web 1080p (-14 LUFS)".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution {
                    width: 1920,
                    height: 1080,
                }),
                fps: None,
                crf: 18,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                // What the streaming platforms normalise to anyway.
                // Delivering at it means they leave the mix alone
                // rather than turning it down on the way in.
                target_lufs: Some(-14.0),
                true_peak_dbtp: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000c".into(),
            name: "Animated GIF (480p, 15fps)".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Gif,
                video_codec: ExportVideoCodec::Gif,
                resolution: Some(Resolution {
                    width: 854,
                    height: 480,
                }),
                fps: Some(Fps::new(15, 1)),
                crf: 0,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac, // ignored
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_preset_is_present_and_well_formed() {
        let presets = builtin_presets();
        let gif = presets
            .iter()
            .find(|p| p.settings.format == ExportFormat::Gif)
            .expect("GIF preset must be in builtin_presets");
        assert_eq!(gif.settings.video_codec, ExportVideoCodec::Gif);
        // 480p / 15fps default — the social-media sweet spot. Pinned
        // here so an accidental tweak doesn't quietly bloat exported
        // file sizes.
        let res = gif
            .settings
            .resolution
            .expect("GIF preset must pin resolution");
        assert_eq!(res.height, 480);
        let fps = gif.settings.fps.expect("GIF preset must pin fps");
        assert_eq!(fps.num, 15);
    }

    #[test]
    fn no_built_in_preset_writes_a_silent_file() {
        // `ExportAudioCodec::Copy` does not copy: the encoder skips the
        // audio stream entirely, because an in-process mixdown has
        // nothing to copy from. The ProRes and DNxHD mastering presets
        // both selected it, so the two exports a professional reaches
        // for when the file is a deliverable came out silent (#156).
        for p in builtin_presets() {
            if p.settings.format == ExportFormat::Gif {
                continue; // no sound in a GIF, and none expected
            }
            assert_ne!(
                p.settings.audio_codec,
                ExportAudioCodec::Copy,
                "preset \"{}\" would export no audio at all",
                p.name,
            );
        }
    }

    #[test]
    fn the_delivery_presets_state_a_loudness_the_renderer_can_hold() {
        // A preset that names a standard has to be able to meet it:
        // its loudness, and the true-peak ceiling that standard asks.
        let presets = builtin_presets();
        let with_target: Vec<_> = presets
            .iter()
            .filter(|p| p.settings.target_lufs.is_some())
            .collect();
        assert!(!with_target.is_empty(), "there should be delivery presets");

        for p in &with_target {
            let lufs = p.settings.target_lufs.unwrap();
            let ceiling = p.settings.true_peak_dbtp.unwrap_or(-1.0);
            // ATSC A/85 (-24 LKFS) and Netflix want -2 dBTP; the others
            // the renderer's -1 already meets.
            let wants = if (lufs - -24.0).abs() < 0.01 || p.name.contains("Netflix") {
                -2.0
            } else {
                -1.0
            };
            assert!(
                ceiling <= wants + 1e-6,
                "preset \"{}\" targets {lufs} LUFS under a {ceiling} dBTP ceiling; its spec asks {wants}",
                p.name,
            );
            assert_ne!(
                p.settings.audio_codec,
                ExportAudioCodec::Copy,
                "preset \"{}\" asks for a loudness and writes no audio",
                p.name,
            );
        }

        let r128 = presets
            .iter()
            .find(|p| p.name.contains("R128"))
            .expect("an R128 preset");
        assert_eq!(r128.settings.target_lufs, Some(-23.0));
        assert_eq!(
            r128.settings.audio_codec,
            ExportAudioCodec::Pcm24,
            "a master wants PCM"
        );
    }

    #[test]
    fn preset_ids_are_unique() {
        // Defensive against copy-paste id duplicates when adding a
        // new preset (the GIF entry got a fresh "...000c" suffix).
        let presets = builtin_presets();
        let mut seen = std::collections::HashSet::new();
        for p in &presets {
            assert!(seen.insert(p.id.clone()), "duplicate preset id: {}", p.id);
        }
    }
}
