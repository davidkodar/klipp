//! What a finished file is, for checking it against a delivery
//! specification (#2650): its streams read with `ffprobe`, and its
//! loudness and true peak measured with Klipp's own meter (the one the
//! export normalises with) over its sound decoded by `ffmpeg`.

use klipp_core::delivery_spec::{Facts, Transfer};
use klipp_core::events::{ExportAudioCodec, ExportFormat, ExportVideoCodec};
use klipp_core::types::Resolution;
use std::path::Path;

/// The facts of the file at `path`; its loudness too when `loudness`.
pub fn file_facts(path: &Path, loudness: bool) -> Result<Facts, String> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe could not read {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut facts = probe_facts(&String::from_utf8_lossy(&out.stdout), path)?;
    if facts.captions != Some(true) && sidecar_captions(path) {
        facts.captions = Some(true);
    }
    if loudness && facts.sample_rate.is_some() {
        let (lufs, peak) = measure_loudness(path)?;
        facts.integrated_lufs = Some(lufs);
        facts.true_peak_dbtp = Some(peak);
    }
    Ok(facts)
}

/// The facts in `ffprobe`'s JSON for `path` (whose extension tells MP4
/// from MOV, which `ffprobe` names alike).
pub fn probe_facts(json: &str, path: &Path) -> Result<Facts, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("ffprobe's answer is not JSON: {e}"))?;
    let streams = v.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let of = |kind: &str| streams.iter().find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some(kind)).cloned();
    let text = |s: &serde_json::Value, key: &str| s.get(key).and_then(|x| x.as_str()).map(str::to_string);
    let number = |s: &serde_json::Value, key: &str| s.get(key).and_then(|x| x.as_str().and_then(|t| t.parse::<f64>().ok()).or_else(|| x.as_f64()));
    let mut facts = Facts { container: container_of(path), ..Default::default() };
    if let Some(video) = of("video") {
        let name = text(&video, "codec_name").unwrap_or_default();
        facts.video_codec = video_codec_of(&name, text(&video, "profile").as_deref());
        if facts.video_codec.is_none() && !name.is_empty() {
            facts.video_codec_name = Some(name);
        }
        if let (Some(w), Some(h)) = (number(&video, "width"), number(&video, "height")) {
            facts.size = Some(Resolution { width: w as u32, height: h as u32 });
        }
        facts.fps = text(&video, "avg_frame_rate").and_then(|r| ratio(&r)).or_else(|| text(&video, "r_frame_rate").and_then(|r| ratio(&r)));
        facts.bit_depth = text(&video, "pix_fmt").map(|p| bit_depth_of(&p)).or_else(|| text(&video, "bits_per_raw_sample").and_then(|b| b.parse().ok()));
        facts.transfer = Some(match text(&video, "color_transfer").as_deref() {
            Some("smpte2084") => Transfer::Pq,
            Some("arib-std-b67") => Transfer::Hlg,
            _ => Transfer::Sdr,
        });
        facts.video_kbps = number(&video, "bit_rate").or_else(|| v.get("format").and_then(|f| number(f, "bit_rate"))).map(|b| (b / 1000.0).round() as u32);
        // Closed captions carried in the picture (CEA-608 in H.264/H.265).
        if video.get("closed_captions").and_then(|c| c.as_i64()) == Some(1) {
            facts.captions = Some(true);
        }
    }
    if let Some(audio) = of("audio") {
        let name = text(&audio, "codec_name").unwrap_or_default();
        facts.audio_codec = audio_codec_of(&name);
        if facts.audio_codec.is_none() && !name.is_empty() {
            facts.audio_codec_name = Some(name);
        }
        facts.sample_rate = number(&audio, "sample_rate").map(|s| s as u32);
        facts.channels = number(&audio, "channels").map(|c| c as u8);
    }
    if of("subtitle").is_some() {
        facts.captions = Some(true);
    }
    facts.captions.get_or_insert(false);
    Ok(facts)
}

/// A captions file beside `path` with its name: `film.srt`, `film.vtt`,
/// `film.en.srt` and the like.
fn sidecar_captions(path: &Path) -> bool {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else { return false };
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.starts_with(stem) && [".srt", ".vtt", ".ttml", ".scc", ".stl"].iter().any(|x| name.ends_with(x))
    })
}

/// Integrated loudness (LUFS) and true peak (dBTP) of the file's first
/// sound stream, decoded to stereo at 48 kHz: a multichannel mix is read
/// as its stereo downmix.
fn measure_loudness(path: &Path) -> Result<(f32, f32), String> {
    use std::io::Read;
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no sound from ffmpeg")?;
    let mut meter = klipp_audio::lufs::LufsMeter::new(48_000.0, true);
    let mut buf = vec![0u8; 1 << 16];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = stdout.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let whole = carry.len() / 8 * 8;
        let samples: Vec<f32> = carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        meter.process(&samples);
        carry.drain(..whole);
    }
    let _ = child.wait();
    let r = meter.readings();
    if !r.integrated.is_finite() {
        return Err("the sound is silent or too short to measure".into());
    }
    Ok((r.integrated, r.true_peak_dbfs))
}

fn ratio(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

fn bit_depth_of(pix_fmt: &str) -> u8 {
    if pix_fmt.contains("p16") || pix_fmt.contains("64") {
        16
    } else if pix_fmt.contains("p12") {
        12
    } else if pix_fmt.contains("p10") || pix_fmt.contains("v210") {
        10
    } else {
        8
    }
}

fn container_of(path: &Path) -> Option<ExportFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some(ExportFormat::Mp4),
        "mov" => Some(ExportFormat::Mov),
        "mkv" => Some(ExportFormat::Mkv),
        "webm" => Some(ExportFormat::WebM),
        "gif" => Some(ExportFormat::Gif),
        "mp3" => Some(ExportFormat::Mp3),
        "wav" => Some(ExportFormat::Wav),
        "m4a" => Some(ExportFormat::M4a),
        _ => None,
    }
}

fn video_codec_of(name: &str, profile: Option<&str>) -> Option<ExportVideoCodec> {
    Some(match name {
        "h264" => ExportVideoCodec::H264,
        "hevc" => ExportVideoCodec::H265,
        "av1" => ExportVideoCodec::Av1,
        "vp9" => ExportVideoCodec::Vp9,
        "gif" => ExportVideoCodec::Gif,
        "prores" => match profile.unwrap_or("") {
            p if p.contains("HQ") => ExportVideoCodec::ProRes422HQ,
            p if p.contains("4444") => ExportVideoCodec::ProRes4444,
            _ => ExportVideoCodec::ProRes422,
        },
        "dnxhd" => ExportVideoCodec::DNxHD145,
        _ => return None,
    })
}

fn audio_codec_of(name: &str) -> Option<ExportAudioCodec> {
    Some(match name {
        "aac" => ExportAudioCodec::Aac,
        "mp3" => ExportAudioCodec::Mp3,
        "flac" => ExportAudioCodec::Flac,
        "opus" => ExportAudioCodec::Opus,
        "pcm_s16le" | "pcm_s16be" => ExportAudioCodec::Pcm16,
        "pcm_s24le" | "pcm_s24be" => ExportAudioCodec::Pcm24,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1080p 29.97 H.264 HDR10 file with AAC stereo and a subtitle
    /// stream, as ffprobe gives it.
    #[test]
    fn ffprobes_answer_is_read() {
        let json = r#"{"streams": [
            {"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 2160, "avg_frame_rate": "30000/1001", "pix_fmt": "yuv420p10le", "color_transfer": "smpte2084", "bit_rate": "45000000"},
            {"codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2},
            {"codec_type": "subtitle", "codec_name": "mov_text"}
        ], "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "bit_rate": "46000000"}}"#;
        let f = probe_facts(json, Path::new("/x/film.mp4")).unwrap();
        assert_eq!(f.container, Some(ExportFormat::Mp4));
        assert_eq!(f.video_codec, Some(ExportVideoCodec::H265));
        assert_eq!(f.size, Some(Resolution { width: 3840, height: 2160 }));
        assert!((f.fps.unwrap() - 29.97).abs() < 0.01);
        assert_eq!((f.bit_depth, f.transfer, f.video_kbps), (Some(10), Some(Transfer::Pq), Some(45_000)));
        assert_eq!((f.audio_codec, f.sample_rate, f.channels, f.captions), (Some(ExportAudioCodec::Aac), Some(48_000), Some(2), Some(true)));
    }

    /// A ProRes HQ master in a MOV with PCM; no captions.
    #[test]
    fn a_master_is_read() {
        let json = r#"{"streams": [
            {"codec_type": "video", "codec_name": "prores", "profile": "HQ", "width": 1920, "height": 1080, "avg_frame_rate": "24000/1001", "pix_fmt": "yuv422p10le"},
            {"codec_type": "audio", "codec_name": "pcm_s24le", "sample_rate": "48000", "channels": 6}
        ], "format": {"bit_rate": "220000000"}}"#;
        let f = probe_facts(json, Path::new("/x/master.mov")).unwrap();
        assert_eq!((f.container, f.video_codec), (Some(ExportFormat::Mov), Some(ExportVideoCodec::ProRes422HQ)));
        assert_eq!((f.audio_codec, f.channels, f.captions, f.transfer), (Some(ExportAudioCodec::Pcm24), Some(6), Some(false), Some(Transfer::Sdr)));
    }

    /// A real file, made with ffmpeg: its facts read back, and checked
    /// against specs it misses: too small for TikTok, far too loud for
    /// EBU R 128, both said with what to change.
    #[test]
    fn a_finished_file_is_read_and_checked() {
        use klipp_core::delivery_spec::{builtin_specs, check, verdict, Status};
        let dir = std::env::temp_dir().join(format!("klipp-delivery-facts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("loud.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=3", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=3", "-af", "volume=18dB", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "2", "-shortest"])
            .arg(&file)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let facts = file_facts(&file, true).unwrap();
        assert_eq!((facts.container, facts.video_codec, facts.size), (Some(ExportFormat::Mp4), Some(ExportVideoCodec::H264), Some(Resolution { width: 320, height: 180 })));
        assert!((facts.fps.unwrap() - 25.0).abs() < 0.01);
        assert_eq!((facts.audio_codec, facts.sample_rate, facts.channels), (Some(ExportAudioCodec::Aac), Some(48_000), Some(2)));
        let lufs = facts.integrated_lufs.expect("measured");
        // ffmpeg's tone is an eighth of full scale; 18 dB up is near it.
        assert!(lufs > -10.0, "a near full-scale tone is loud: {lufs}");
        let spec = |id: &str| builtin_specs().into_iter().find(|s| s.id == id).unwrap();
        let tiktok = check(&spec("delivery-tiktok"), &facts);
        assert_eq!(tiktok.iter().find(|f| f.what == "Resolution").unwrap().status, Status::Fail, "{tiktok:#?}");
        let ebu = check(&spec("delivery-ebu-r128"), &facts);
        assert_eq!(verdict(&ebu), Status::Fail);
        assert!(ebu.iter().any(|f| f.what == "Loudness" && f.status == Status::Fail && f.fix.contains("Normalise")), "{ebu:#?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
