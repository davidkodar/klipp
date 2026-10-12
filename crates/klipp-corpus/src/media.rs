//! Integer-only sources: encoded bytes may change with FFmpeg, decoded samples may not.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 360;
pub const FRAMES: usize = 48;

#[derive(Clone, Copy)]
pub enum Pattern {
    Motion,
    Flicker,
    Noise,
}

pub const VIDEOS: &[(&str, Pattern)] = &[
    ("motion.mkv", Pattern::Motion),
    ("flicker.mkv", Pattern::Flicker),
    ("noise.mkv", Pattern::Noise),
];

fn ffmpeg() -> Command {
    let mut c = Command::new("ffmpeg");
    c.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "1",
    ]);
    c
}

/// A ramp, coloured patches, fine detail and a moving hard edge in legal-range 709.
/// Fill native YUV planes so generation never depends on a colour converter.
fn frame(at: usize) -> Vec<u8> {
    let mut out = vec![0; WIDTH * HEIGHT * 3 / 2];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let moving = (x + WIDTH - (at * 7 % WIDTH)) % WIDTH < 48;
            let v = if y < 120 {
                16 + x * 219 / (WIDTH - 1)
            } else if y < 240 {
                if moving {
                    220
                } else {
                    32 + ((x / 8 + y / 8) % 2) * 100
                }
            } else {
                // A fixed grain pattern: no RNG state or floating point.
                48 + ((x * 17 + y * 31 + (x ^ y) * 7) % 128)
            };
            out[y * WIDTH + x] = v as u8;
        }
    }
    let plane = WIDTH * HEIGHT / 4;
    for y in 0..HEIGHT / 2 {
        for x in 0..WIDTH / 2 {
            out[WIDTH * HEIGHT + y * WIDTH / 2 + x] = (32 + x * 192 / (WIDTH / 2 - 1)) as u8;
            out[WIDTH * HEIGHT + plane + y * WIDTH / 2 + x] =
                (32 + y * 192 / (HEIGHT / 2 - 1)) as u8;
        }
    }
    out
}

pub fn generate(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    for &(name, pattern) in VIDEOS {
        video(&root.join(name), pattern)?;
    }
    write_audio(&root.join("sync.wav"))
}

fn patterned_frame(pattern: Pattern, at: usize) -> Vec<u8> {
    let mut pixels = frame(at);
    match pattern {
        Pattern::Motion => {}
        Pattern::Flicker => {
            // Deliberate whole-frame pumping, so a deflicker has work to do.
            let gain = [70, 100, 85][at % 3];
            for y in &mut pixels[..WIDTH * HEIGHT] {
                *y = (16 + (u32::from(*y) - 16) * gain / 100) as u8;
            }
        }
        Pattern::Noise => {
            // Grain changes each frame, below the denoiser's motion threshold.
            // The static source's grain alone would make temporal NR an identity.
            for (i, y) in pixels[..WIDTH * HEIGHT].iter_mut().enumerate() {
                let mut state =
                    (i as u32).wrapping_mul(0x9e37_79b9) ^ (at as u32).wrapping_mul(0x85eb_ca6b);
                state ^= state >> 16;
                state = state.wrapping_mul(0x7feb_352d);
                state ^= state >> 15;
                let noise = (state % 17) as i32 - 8;
                *y = (i32::from(*y) + noise).clamp(16, 235) as u8;
            }
        }
    }
    pixels
}

fn video(path: &Path, pattern: Pattern) -> Result<(), String> {
    let mut child = ffmpeg()
        .args([
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv420p",
            "-video_size",
            "640x360",
            "-framerate",
            "24",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            "setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709",
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-threads",
            "1",
            "-flags",
            "+bitexact",
            "-fflags",
            "+bitexact",
            "-map_metadata",
            "-1",
            "-color_range",
            "tv",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start ffmpeg: {e}"))?;
    let write_result = (|| {
        let mut input = child.stdin.take().ok_or("ffmpeg stdin missing")?;
        for at in 0..FRAMES {
            input
                .write_all(&patterned_frame(pattern, at))
                .map_err(|e| format!("frame {at}: {e}"))?;
        }
        Ok::<_, String>(())
    })();
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "encode {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    write_result
}

fn write_audio(path: &Path) -> Result<(), String> {
    // Exact 400/600 Hz triangle tones and a click at each half-second.
    let samples = 96_000u32;
    let bytes = samples * 4;
    let mut wav = Vec::with_capacity(bytes as usize + 44);
    wav.extend(b"RIFF");
    wav.extend((bytes + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(48_000u32.to_le_bytes());
    wav.extend(192_000u32.to_le_bytes());
    wav.extend(4u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    for i in 0..samples {
        for period in [120, 80] {
            let phase = (i % period) as i32;
            let half = period as i32 / 2;
            let triangle = (phase.min(period as i32 - phase) * 2 - half) * 6000 / half;
            let value = if i % 24_000 < 24 { 24_000 } else { triangle };
            wav.extend((value as i16).to_le_bytes());
        }
    }
    std::fs::write(path, wav).map_err(|e| e.to_string())
}

/// Each decoded frame includes its timestamps and sample count; headers contain
/// FFmpeg versions and are deliberately excluded. Require the expected frame count.
pub fn decoded_video(path: &Path) -> Result<Vec<String>, String> {
    let output = ffmpeg()
        .arg("-i")
        .arg(path)
        .args([
            "-map", "0:v:0", "-an", "-pix_fmt", "yuv420p", "-f", "framemd5", "-",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "decode {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let rows: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|s| !s.starts_with('#') && !s.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if rows.len() != FRAMES {
        return Err(format!(
            "{}: expected {FRAMES} decoded frames, got {}",
            path.display(),
            rows.len()
        ));
    }
    Ok(rows)
}

pub fn decoded_audio(path: &Path) -> Result<String, String> {
    let output = ffmpeg()
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() || output.stdout.len() != 384_000 {
        return Err(format!(
            "decode {}: expected 384000 PCM bytes; got {}: {}",
            path.display(),
            output.stdout.len(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(crate::sha256(&output.stdout))
}

pub fn metadata(path: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("ffprobe").args([
        "-v", "error", "-show_entries",
        "stream=codec_name,width,height,pix_fmt,r_frame_rate,sample_rate,channels,color_range,color_space,color_transfer,color_primaries",
        "-of", "json",
    ]).arg(path).output().map_err(|e| format!("start ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "probe {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let streams = value["streams"]
        .as_array()
        .ok_or("ffprobe returned no streams")?;
    if streams.len() != 1 {
        return Err(format!("{}: expected one stream", path.display()));
    }
    Ok(streams[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analysis_and_temporal_inputs_are_not_static() {
        let luma = |p: &[u8]| {
            p[..WIDTH * HEIGHT]
                .iter()
                .map(|&v| u64::from(v))
                .sum::<u64>()
        };
        let dark = patterned_frame(Pattern::Flicker, 0);
        let light = patterned_frame(Pattern::Flicker, 1);
        assert!(luma(&light) > luma(&dark) * 12 / 10);
        let a = patterned_frame(Pattern::Noise, 0);
        let b = patterned_frame(Pattern::Noise, 1);
        assert_ne!(&a[..WIDTH * 120], &b[..WIDTH * 120]);
        assert_eq!(
            &a[WIDTH * HEIGHT..],
            &b[WIDTH * HEIGHT..],
            "chroma is unchanged"
        );
        assert_eq!(a, patterned_frame(Pattern::Noise, 0));
    }
    #[test]
    fn motion_changes_only_the_middle_band() {
        let a = frame(0);
        let b = frame(1);
        assert_eq!(a.len(), WIDTH * HEIGHT * 3 / 2);
        assert_eq!(&a[..WIDTH * 120], &b[..WIDTH * 120]);
        assert_ne!(&a[WIDTH * 120..WIDTH * 240], &b[WIDTH * 120..WIDTH * 240]);
        assert_eq!(&a[WIDTH * 240..], &b[WIDTH * 240..]);
    }
}
