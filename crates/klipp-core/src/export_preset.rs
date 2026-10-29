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
    /// The true-peak ceiling in dBTP; None is -1. See
    /// `ExportSettings::true_peak_dbtp`.
    #[serde(default)]
    pub true_peak_dbtp: Option<f32>,
    /// 10 for a 10-bit encode where the codec has a choice (H.264,
    /// H.265, AV1), else 0: `ExportSettings::bit_depth`. ProRes is 10-bit
    /// whatever this says.
    #[serde(default)]
    pub bit_depth: u8,
}

impl ExportPresetSettings {
    /// Whether the preset writes 10-bit video (#2074): ProRes, or a codec
    /// with the choice at 10. Its label in the list says so.
    pub fn is_10bit(&self) -> bool {
        self.video_codec != ExportVideoCodec::Gif && !self.video_codec.writes_8bit(self.bit_depth)
    }

    /// An export to `output_path` with the preset's settings, the rest
    /// the defaults.
    pub fn settings_for(&self, output_path: std::path::PathBuf) -> crate::ExportSettings {
        crate::ExportSettings {
            output_path,
            format: self.format,
            video_codec: self.video_codec,
            resolution: self.resolution,
            fps: self.fps,
            crf: self.crf,
            preset: self.preset,
            audio_codec: self.audio_codec,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
            video_bitrate_kbps: self.video_bitrate_kbps,
            rate_control: self.rate_control,
            target_lufs: self.target_lufs,
            true_peak_dbtp: self.true_peak_dbtp,
            bit_depth: self.bit_depth,
            ..Default::default()
        }
    }
}

/// A format a delivery offers in one click (#2724): its name, its aspect
/// as width and height parts, the suffix its file takes, and the built-in
/// export preset it is written with, by id.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryTemplate {
    pub name: String,
    pub aspect: (u32, u32),
    pub suffix: String,
    pub preset: String,
}

/// Why delivery templates can't be used: a name empty or twice, an
/// aspect part of 0 or past 100, a suffix that is empty, longer than 16
/// or anything but letters, digits, `-` and `_` (it goes into a file's
/// name), or a preset that isn't one of Klipp's.
fn delivery_fault(templates: &[DeliveryTemplate]) -> Option<String> {
    let presets = builtin_presets();
    templates.iter().enumerate().find_map(|(i, t)| {
        if t.name.trim().is_empty() || templates[..i].iter().any(|o| o.name == t.name) {
            Some(format!("template {}: a name, said once", i + 1))
        } else if !(1..=100).contains(&t.aspect.0) || !(1..=100).contains(&t.aspect.1) {
            Some(format!("\"{}\": an aspect of 1 to 100 parts each way", t.name))
        } else if t.suffix.is_empty() || t.suffix.len() > 16 || !t.suffix.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            Some(format!("\"{}\": a suffix of letters, digits, - and _, up to 16", t.name))
        } else if !presets.iter().any(|p| p.id == t.preset) {
            Some(format!("\"{}\": {} is not one of Klipp's export presets", t.name, t.preset))
        } else {
            None
        }
    })
}

fn read_delivery_templates(text: &str) -> Result<Vec<DeliveryTemplate>, String> {
    let templates = crate::preset_file::read::<DeliveryTemplate>(text, "delivery_template", 1, "template")?;
    delivery_fault(&templates).map_or(Ok(templates), Err)
}

/// The delivery templates, `assets/presets/delivery_templates/
/// builtin.toml`, read once; a file that doesn't read leaves none, said,
/// never a crash.
pub fn delivery_templates() -> &'static [DeliveryTemplate] {
    static TEMPLATES: std::sync::OnceLock<Vec<DeliveryTemplate>> = std::sync::OnceLock::new();
    TEMPLATES.get_or_init(|| {
        read_delivery_templates(include_str!("../../../assets/presets/delivery_templates/builtin.toml")).unwrap_or_else(|why| {
            tracing::error!("the built-in delivery templates don't read: {why}");
            Vec::new()
        })
    })
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

/// The loudness the web platforms play at: YouTube, Instagram, X and
/// Vimeo turn louder uploads down to about −14 LUFS integrated and leave
/// quieter ones quiet, so a web delivery is normalised there by default.
pub const WEB_LOUDNESS_LUFS: f32 = -14.0;

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
                resolution: Some(Resolution { width: 1920, height: 1080 }),
                fps: None,
                crf: 18,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
            },
        },
        // The feed's tallest post (#2724).
        ExportPreset {
            id: "00000000-0000-4000-a000-000000000010".into(),
            name: "Instagram Feed 4:5".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H264,
                resolution: Some(Resolution { width: 1080, height: 1350 }),
                fps: None,
                crf: 20,
                preset: EncoderPreset::Medium,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(192),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: Some(WEB_LOUDNESS_LUFS),
                true_peak_dbtp: None,
                bit_depth: 0,
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
                bit_depth: 0,
            },
        },
        // A 10-bit master for delivery to a platform or a grade (#2074):
        // HEVC Main 10 keeps a dark, soft gradient from stepping, where
        // an 8-bit file needs the gradient's grain to hide it.
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000f".into(),
            name: "HEVC Main10 master".into(),
            built_in: true,
            settings: ExportPresetSettings {
                format: ExportFormat::Mp4,
                video_codec: ExportVideoCodec::H265,
                resolution: None,
                fps: None,
                crf: 16,
                preset: EncoderPreset::Slow,
                audio_codec: ExportAudioCodec::Aac,
                audio_bitrate_kbps: Some(320),
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
                bit_depth: 10,
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
                // As with ProRes above: `Copy` wrote no audio at all
                // (#156), which made the Avid interchange master silent.
                audio_codec: ExportAudioCodec::Pcm24,
                audio_bitrate_kbps: None,
                video_bitrate_kbps: None,
                rate_control: RateControl::Crf,
                target_lufs: None,
                true_peak_dbtp: None,
                bit_depth: 0,
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
                true_peak_dbtp: None,
                bit_depth: 0,
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
                bit_depth: 0,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000a".into(),
            name: "Audio Only MP3".into(),
            built_in: true,
            // The mix alone, no video stream (#1813); the video fields
            // are not used.
            settings: ExportPresetSettings {
                format: ExportFormat::Mp3,
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
                bit_depth: 0,
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
                bit_depth: 0,
            },
        },
        ExportPreset {
            id: "00000000-0000-4000-a000-00000000000e".into(),
            name: "Web 1080p (-14 LUFS)".into(),
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
                // What the streaming platforms normalise to anyway.
                // Delivering at it means they leave the mix alone
                // rather than turning it down on the way in.
                target_lufs: Some(-14.0),
                true_peak_dbtp: None,
                bit_depth: 0,
            },
        },
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
                true_peak_dbtp: None,
                bit_depth: 0,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_preset_writes_a_container_ffmpeg_accepts() {
        for p in builtin_presets() {
            let s = &p.settings;
            assert!(s.format.holds_video(s.video_codec), "{}: {:?} in {:?}", p.name, s.video_codec, s.format);
            assert!(s.format.holds_audio(s.audio_codec), "{}: {:?} in {:?}", p.name, s.audio_codec, s.format);
        }
    }

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
        let with_target: Vec<_> = presets.iter().filter(|p| p.settings.target_lufs.is_some()).collect();
        assert!(!with_target.is_empty(), "there should be delivery presets");

        for p in &with_target {
            let lufs = p.settings.target_lufs.unwrap();
            let ceiling = p.settings.true_peak_dbtp.unwrap_or(-1.0);
            // ATSC A/85 (-24 LKFS) and Netflix want -2 dBTP; the others
            // the renderer's -1 already meets.
            let wants = if (lufs - -24.0).abs() < 0.01 || p.name.contains("Netflix") { -2.0 } else { -1.0 };
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

        let r128 = presets.iter().find(|p| p.name.contains("R128")).expect("an R128 preset");
        assert_eq!(r128.settings.target_lufs, Some(-23.0));
        assert_eq!(r128.settings.audio_codec, ExportAudioCodec::Pcm24, "a master wants PCM");
    }

    /// #2074: the built-in list offers 10-bit masters, and says which they
    /// are: ProRes (the 422 master and the broadcast one) and HEVC Main10.
    /// The delivery presets for the web are 8-bit.
    #[test]
    fn ten_bit_masters_are_offered_and_known() {
        let presets = builtin_presets();
        let ten: Vec<&str> = presets.iter().filter(|p| p.settings.is_10bit()).map(|p| p.name.as_str()).collect();
        assert_eq!(ten, ["ProRes 422", "HEVC Main10 master", "Broadcast master (EBU R128)"]);
        let main10 = presets.iter().find(|p| p.name == "HEVC Main10 master").unwrap();
        assert_eq!((main10.settings.video_codec, main10.settings.bit_depth), (ExportVideoCodec::H265, 10));
        assert!(!presets.iter().find(|p| p.name == "YouTube 1080p").unwrap().settings.is_10bit());
        // A preset saved before the field reads as 8-bit.
        let old = r#"{"format":"Mp4","video_codec":"H265","resolution":null,"fps":null,"crf":20,"audio_codec":"Aac","audio_bitrate_kbps":192}"#;
        let s: ExportPresetSettings = serde_json::from_str(old).unwrap();
        assert_eq!(s.bit_depth, 0);
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

    /// A web delivery comes out at the loudness the platforms play at; a
    /// master (ProRes, DNxHR, lossless) keeps its level, and the
    /// broadcast preset keeps its own R128 target.
    #[test]
    fn web_presets_normalise_and_masters_do_not() {
        for p in builtin_presets() {
            let web = ["YouTube", "Vimeo", "Twitter", "Instagram", "Web"].iter().any(|w| p.name.contains(w));
            if web {
                assert_eq!(p.settings.target_lufs, Some(WEB_LOUDNESS_LUFS), "{}", p.name);
            } else if !p.name.contains("Broadcast") {
                assert_eq!(p.settings.target_lufs, None, "{}", p.name);
            }
        }
    }
}

#[cfg(test)]
mod delivery_template_tests {
    use super::*;

    /// The one-click delivery formats read from Klipp's file as the code
    /// listed them (#2887): names, aspects, suffixes and presets, in order.
    #[test]
    fn the_delivery_templates_read_as_they_were() {
        let got: Vec<(&str, (u32, u32), &str, &str)> = delivery_templates().iter().map(|t| (t.name.as_str(), t.aspect, t.suffix.as_str(), t.preset.as_str())).collect();
        assert_eq!(got, [
            ("YouTube 16:9", (16, 9), "16x9", "00000000-0000-4000-a000-000000000001"),
            ("Shorts, Reels, TikTok 9:16", (9, 16), "9x16", "00000000-0000-4000-a000-000000000006"),
            ("Square 1:1", (1, 1), "1x1", "00000000-0000-4000-a000-000000000005"),
            ("Instagram feed 4:5", (4, 5), "4x5", "00000000-0000-4000-a000-000000000010"),
        ]);
    }

    /// A template that can't be offered is refused, saying why: a suffix
    /// that would climb out of the output folder or isn't plain, a preset
    /// Klipp hasn't got, an aspect of nothing, a name twice, an unknown
    /// field.
    #[test]
    fn a_bad_delivery_template_is_refused() {
        let file = |body: &str| format!("kind = \"delivery_template\"\nformat_version = 1\n{body}");
        let one = |name: &str, aspect: &str, suffix: &str, preset: &str| format!("[[template]]\nname = \"{name}\"\naspect = {aspect}\nsuffix = \"{suffix}\"\npreset = \"{preset}\"\n");
        let yt = "00000000-0000-4000-a000-000000000001";
        assert!(read_delivery_templates(&file(&one("Wide", "[2, 1]", "2x1", yt))).is_ok());
        for (body, says) in [
            (one("Escape", "[1, 1]", "../../x", yt), "a suffix of letters"),
            (one("Spaced", "[1, 1]", "a b", yt), "a suffix of letters"),
            (one("Long", "[1, 1]", "aaaaaaaaaaaaaaaaa", yt), "up to 16"),
            (one("Unknown", "[1, 1]", "x", "00000000-0000-4000-a000-000000000999"), "not one of Klipp's export presets"),
            (one("Flat", "[0, 1]", "x", yt), "an aspect of 1 to 100"),
            (one("Twice", "[1, 1]", "x", yt) + &one("Twice", "[2, 1]", "y", yt), "a name, said once"),
            (one("Extra", "[1, 1]", "x", yt) + "fps = 30\n", "unknown field"),
        ] {
            let why = read_delivery_templates(&file(&body)).err().unwrap_or_default();
            assert!(why.contains(says), "{body}: {why}");
        }
    }
}
