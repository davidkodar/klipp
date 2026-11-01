//! The render script: a shell script that renders the timeline with ffmpeg, its
//! times and its quoting (#890).

use super::*;

/// Format absolute seconds as ffmpeg-compatible `HH:MM:SS.mmm`. Negative
/// inputs are clamped to zero. Used by the headless render-script generator
/// for `-ss` / `-to` arguments.
pub(super) fn format_ffmpeg_time(total_secs: f64) -> String {
    let secs = total_secs.max(0.0);
    let h = (secs / 3600.0) as u64;
    let m = ((secs % 3600.0) / 60.0) as u64;
    let s = (secs % 60.0) as u64;
    let ms = ((secs - (h as f64 * 3600.0 + m as f64 * 60.0 + s as f64)) * 1000.0).round() as u64;
    // Clamp potential rounding overflow (e.g. 0.9999 -> 1000) into the next second.
    let (s, ms) = if ms >= 1000 { (s + 1, 0) } else { (s, ms) };
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Shell-quote a string for bash: wrap in single quotes, escaping embedded
/// single quotes via the standard `'\''` dance. Safe for paths containing
/// spaces, parens, quotes, dollar signs, etc.
pub(super) fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Generate a bash script that invokes ffmpeg to render the given Sequence
/// headlessly. **Cuts only** — effects, grades, transitions, keyframes,
/// volume/pan automation, and title/generator clips are NOT serialized.
/// The script begins with a prominent WARNING comment explaining this.
///
/// MVP: operates on the first Video track only. Gap clips and non-Media
/// clips on that track are skipped.
///
/// - Zero media clips → the script echoes an error and exits 1.
/// - One media clip → direct transcode with `-ss`/`-to`/`-i` + encoder args.
/// - Many media clips → two-pass: extract each segment to `$TMPDIR/segNNNN.<ext>`,
///   then `ffmpeg -f concat -safe 0 -i list.txt -c copy OUTPUT`.
pub(super) fn generate_render_script(
    seq: &Sequence,
    media_pool: &klipp_media::MediaPool,
    settings: &ExportSettings,
    output_path: &str,
) -> String {
    // Collect source segments from the first video track.
    struct Segment {
        source_path: String,
        in_secs: f64,
        out_secs: f64,
    }
    let mut segs: Vec<Segment> = Vec::new();
    if let Some(track) = seq
        .tracks
        .iter()
        .find(|t| t.kind == klipp_timeline::TrackKind::Video)
    {
        for clip in &track.clips {
            if !clip.enabled {
                continue;
            }
            if let klipp_timeline::ClipKind::Media { media_id } = &clip.kind {
                if let Some(item) = media_pool.get(media_id) {
                    let in_secs = clip.source_in.to_secs_f64();
                    let out_secs = clip.source_out.to_secs_f64();
                    if out_secs > in_secs {
                        segs.push(Segment {
                            source_path: item.path.to_string_lossy().to_string(),
                            in_secs,
                            out_secs,
                        });
                    }
                }
            }
        }
    }

    let ext = settings.format.extension();
    let video_codec = settings.video_codec.ffmpeg_codec();
    let audio_args_vec = settings.audio_codec.ffmpeg_args(settings.audio_bitrate_kbps);
    let audio_args = audio_args_vec
        .iter()
        .map(|a| shell_single_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    // Resolution/fps filter chain — only include if overrides present.
    let res = settings
        .resolution
        .unwrap_or(seq.resolution);
    let fps_val = settings
        .fps
        .map(|f| f.as_f64())
        .unwrap_or_else(|| seq.fps.as_f64());
    let vf = format!("scale={}:{},fps={}", res.width, res.height, fps_val);

    // Quality args: CRF for CRF-capable codecs, else bitrate.
    let quality_args = if settings.video_codec.uses_crf()
        && settings.rate_control == klipp_core::RateControl::Crf
    {
        format!("-crf {}", settings.crf)
    } else if let Some(bk) = settings.video_bitrate_kbps {
        format!("-b:v {}k", bk)
    } else {
        String::new()
    };
    let preset_args = if settings.video_codec.supports_preset() {
        format!("-preset {}", settings.preset.ffmpeg_value())
    } else {
        String::new()
    };

    let output_q = shell_single_quote(output_path);

    let mut s = String::new();
    s.push_str("#!/usr/bin/env bash\n");
    s.push_str("set -euo pipefail\n");
    s.push('\n');
    s.push_str("# =============================================================================\n");
    s.push_str("# WARNING: Klipp headless render script — CUTS ONLY.\n");
    s.push_str("#\n");
    s.push_str("# This script reproduces the timeline CUTS of the current sequence by calling\n");
    s.push_str("# ffmpeg directly. It does NOT apply:\n");
    s.push_str("#   * Effects (blur, sharpen, chroma-key, transform, etc.)\n");
    s.push_str("#   * Colour grades / node graphs\n");
    s.push_str("#   * Transitions (crossfades, wipes, etc.)\n");
    s.push_str("#   * Keyframes / automation (opacity, volume, pan, transform)\n");
    s.push_str("#   * Title, generator, or adjustment-layer clips\n");
    s.push_str("#   * Audio tracks, mixer, or loudness normalization\n");
    s.push_str("#   * Subtitles or chapters\n");
    s.push_str("#\n");
    s.push_str("# For a full-quality render that honours every edit, use Klipp's built-in\n");
    s.push_str("# Export dialog (File > Export... or Ctrl+E) instead.\n");
    s.push_str("#\n");
    s.push_str("# This script is intended for headless / CI / batch pipelines where only\n");
    s.push_str("# rough-cut output is needed.\n");
    s.push_str("# =============================================================================\n");
    s.push('\n');
    s.push_str(&format!("OUTPUT={}\n", output_q));
    s.push('\n');

    if segs.is_empty() {
        s.push_str("echo \"Error: no media clips on the first video track — nothing to render.\" >&2\n");
        s.push_str("exit 1\n");
        return s;
    }

    if segs.len() == 1 {
        let seg = &segs[0];
        let src_q = shell_single_quote(&seg.source_path);
        let ss = format_ffmpeg_time(seg.in_secs);
        let to = format_ffmpeg_time(seg.out_secs);
        s.push_str("# Single-clip timeline: direct transcode.\n");
        s.push_str("ffmpeg -y \\\n");
        s.push_str(&format!("  -ss {} -to {} -i {} \\\n", ss, to, src_q));
        s.push_str(&format!("  -vf {} \\\n", shell_single_quote(&vf)));
        s.push_str(&format!("  -c:v {}", video_codec));
        if !preset_args.is_empty() {
            s.push(' ');
            s.push_str(&preset_args);
        }
        if !quality_args.is_empty() {
            s.push(' ');
            s.push_str(&quality_args);
        }
        s.push_str(" \\\n");
        s.push_str(&format!("  {} \\\n", audio_args));
        s.push_str("  \"$OUTPUT\"\n");
        s.push('\n');
        s.push_str("echo \"Done: $OUTPUT\"\n");
        return s;
    }

    // Multi-clip: two-pass extract-and-concat. Extract each segment into TMPDIR
    // using the same codec settings as the final output so `-c copy` concat
    // works cleanly without re-encoding.
    s.push_str("# Multi-clip timeline: extract segments then concat.\n");
    s.push_str("TMPDIR=\"$(mktemp -d -t klipp-render-XXXXXX)\"\n");
    s.push_str("trap 'rm -rf \"$TMPDIR\"' EXIT\n");
    s.push_str("LIST=\"$TMPDIR/concat.txt\"\n");
    s.push_str(": > \"$LIST\"\n");
    s.push('\n');

    for (i, seg) in segs.iter().enumerate() {
        let src_q = shell_single_quote(&seg.source_path);
        let ss = format_ffmpeg_time(seg.in_secs);
        let to = format_ffmpeg_time(seg.out_secs);
        let seg_name = format!("seg{:04}.{}", i, ext);
        s.push_str(&format!("# Segment {} of {}\n", i + 1, segs.len()));
        s.push_str(&format!("SEG={}\n", shell_single_quote(&format!("$TMPDIR/{}", seg_name))));
        s.push_str("ffmpeg -y \\\n");
        s.push_str(&format!("  -ss {} -to {} -i {} \\\n", ss, to, src_q));
        s.push_str(&format!("  -vf {} \\\n", shell_single_quote(&vf)));
        s.push_str(&format!("  -c:v {}", video_codec));
        if !preset_args.is_empty() {
            s.push(' ');
            s.push_str(&preset_args);
        }
        if !quality_args.is_empty() {
            s.push(' ');
            s.push_str(&quality_args);
        }
        s.push_str(" \\\n");
        s.push_str(&format!("  {} \\\n", audio_args));
        s.push_str("  \"$SEG\"\n");
        s.push_str("printf \"file '%s'\\n\" \"$SEG\" >> \"$LIST\"\n");
        s.push('\n');
    }

    s.push_str("# Concat pass (stream copy — no re-encode).\n");
    s.push_str("ffmpeg -y -f concat -safe 0 -i \"$LIST\" -c copy \"$OUTPUT\"\n");
    s.push('\n');
    s.push_str("echo \"Done: $OUTPUT\"\n");
    s
}

#[cfg(test)]
mod render_script_tests {
    use super::*;

    #[test]
    fn format_ffmpeg_time_basic() {
        assert_eq!(format_ffmpeg_time(0.0), "00:00:00.000");
        assert_eq!(format_ffmpeg_time(3661.5), "01:01:01.500");
        assert_eq!(format_ffmpeg_time(83.456), "00:01:23.456");
    }

    #[test]
    fn format_ffmpeg_time_sub_second() {
        assert_eq!(format_ffmpeg_time(0.001), "00:00:00.001");
        assert_eq!(format_ffmpeg_time(0.999), "00:00:00.999");
    }

    #[test]
    fn format_ffmpeg_time_ninety_seconds() {
        // Smoke-test used by the wider render-script assertion block —
        // 90.0 seconds must round-trip through the HH:MM:SS.mmm formatter
        // cleanly, no 59.999-style rounding artefacts.
        assert_eq!(format_ffmpeg_time(90.0), "00:01:30.000");
    }

    #[test]
    fn render_script_has_warning_comment() {
        // Minimal sequence + default export settings exercising the
        // zero-clip path (still emits the WARNING block + shebang).
        let seq = Sequence::new_default();
        let media_pool = klipp_media::MediaPool::default();
        let settings = ExportSettings {
            pan_law: Default::default(),
            output_path: std::path::PathBuf::from("out.mp4"),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::Medium,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true,
            embed_subtitles: false,
            target_lufs: None,
            true_peak_dbtp: None,
            embed_chapters: false, captions_608: false, hdr10plus: false,
            bit_depth: 0,
            alpha: false,
            hdr_mode: klipp_core::HdrMode::Sdr,
            tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer,
            mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
        };
        let script = generate_render_script(&seq, &media_pool, &settings, "/tmp/out.mp4");
        assert!(script.starts_with("#!/usr/bin/env bash\n"), "missing shebang: {script}");
        assert!(script.contains("WARNING"), "missing WARNING in: {script}");
        assert!(
            script.contains("CUTS ONLY") || script.contains("Cuts only") || script.contains("cuts only"),
            "WARNING block should mention cuts-only rendering: {script}"
        );
        assert!(
            script.contains("Ctrl+E") || script.contains("Export dialog"),
            "WARNING block should point users to the Export dialog: {script}"
        );
        // Zero-clip path: script must exit non-zero with an error message.
        assert!(script.contains("exit 1"), "zero-clip path should exit 1: {script}");
    }
}
