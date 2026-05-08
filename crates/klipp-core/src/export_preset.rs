use crate::events::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportVideoCodec, RateControl};
use crate::types::Resolution;
use crate::time::Fps;
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

/// Returns the 10 built-in presets with deterministic UUIDs.
pub fn builtin_presets() -> Vec<ExportPreset> {
    vec![
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000001".into(),
            name: "YouTube 1080p".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1920, height: 1080 }),
                fps: None,
                crf: 18,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000002".into(),
            name: "YouTube 4K".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: Some(Resolution { width: 3840, height: 2160 }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000003".into(),
            name: "Vimeo 1080p".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1920, height: 1080 }),
                fps: None,
                crf: 16,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000004".into(),
            name: "Twitter/X".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1280, height: 720 }),
                fps: None,
                crf: 23,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(128),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000005".into(),
            name: "Instagram Square".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1080, height: 1080 }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000006".into(),
            name: "Instagram Vertical".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1080, height: 1920 }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
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
                audio_codec: ExportAudioCodec::Copy,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000008".into(),
            name: "DNxHD 115".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mov,
                video_codec: ExportVideoCodec::DNxHD115,
                resolution: Some(Resolution { width: 1920, height: 1080 }),
                fps: None,
                crf: 0,
                preset: EncoderPreset::default(),
                audio_codec: ExportAudioCodec::Copy,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000009".into(),
            name: "H.265 4K".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: Some(Resolution { width: 3840, height: 2160 }),
                fps: None,
                crf: 22,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(256),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
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
            },
        },
        // Animated GIF — two-pass palette workflow. 480p / 15fps is
        // the social-media sweet spot; users override resolution / fps
        // via the export dialog when they want larger or slower.
        // Audio settings are nominally present but ignored by the GIF
        // encoder path — the dialog also greys them out.
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000c".into(),
            name: "Animated GIF (480p, 15fps)".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Gif,
                video_codec: ExportVideoCodec::Gif,
                resolution: Some(Resolution { width: 854, height: 480 }),
                fps: Some(Fps::new(15, 1)),
                crf: 0,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac, // ignored
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
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
        let res = gif.settings.resolution.expect("GIF preset must pin resolution");
        assert_eq!(res.height, 480);
        let fps = gif.settings.fps.expect("GIF preset must pin fps");
        assert_eq!(fps.num, 15);
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
