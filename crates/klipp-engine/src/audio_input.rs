//! Audio recording input — CPAL input stream + WAV writer thread.
//!
//! This module mirrors the shape of `audio_output::AudioOutput` but flips the
//! direction of data flow. A cpal input callback pushes captured f32 samples
//! into a ringbuf producer; a writer thread pops them and writes a standard
//! WAVE/IEEE-float file.
//!
//! Tier 2a scope:
//! - Open a cpal input stream on the requested (or default) device.
//! - Emit a 32-bit float WAV file with a hand-written 44-byte header (we don't
//!   pull in `hound` for one 44-byte blob).
//! - Leave MasterClock timecode alignment to tier 2c (deferred); post-capture
//!   alignment can be done via `AlignToAudioReference` (feature #61).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::io::{Seek, SeekFrom, Write};

#[derive(Debug)]
pub enum AudioInputError {
    /// No input device could be opened (no default and no match by name).
    NoDevice,
    /// cpal refused to build the stream with the requested config.
    BuildStream(String),
    /// cpal refused to start the stream.
    Play(cpal::PlayStreamError),
    /// The device reported no usable configuration.
    Config(String),
}

impl std::fmt::Display for AudioInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioInputError::NoDevice => write!(f, "no audio input device available"),
            AudioInputError::BuildStream(s) => write!(f, "build input stream failed: {s}"),
            AudioInputError::Play(e) => write!(f, "failed to start input stream: {e}"),
            AudioInputError::Config(s) => write!(f, "audio input config error: {s}"),
        }
    }
}

impl std::error::Error for AudioInputError {}

/// A live cpal input stream paired with its sample rate and channel count.
/// Dropping (or calling `stop()`) halts capture.
pub struct AudioInput {
    stream: Option<cpal::Stream>,
    pub sample_rate: u32,
    pub channels: u16,
    /// The loudest sample since the meter last read it, as f32 bits
    /// (#114): levels seen before and while a take is recorded.
    peak: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl AudioInput {
    /// Build an input stream for the named device (or default when `None`).
    ///
    /// Returns the `AudioInput` handle and the consumer side of a ring buffer;
    /// drain the consumer from a writer thread (see `start_recording_thread`).
    /// The ring buffer holds ~4 seconds of audio at the target rate — plenty
    /// of slack for the writer thread's 5 ms poll interval.
    ///
    /// The `sample_rate` argument is a _hint_: we'll ask cpal for that rate,
    /// but fall back to the device's default if the exact rate isn't listed
    /// as supported. The actual rate used is reported on the returned handle.
    pub fn new(
        device_name: Option<&str>,
        sample_rate: u32,
    ) -> Result<(Self, ringbuf::HeapCons<f32>), AudioInputError> {
        crate::audio_output::quiet_alsa();
        let host = cpal::default_host();

        // Try named device first; silently fall back to the default on miss.
        let device = if let Some(name) = device_name {
            host.input_devices()
                .ok()
                .and_then(|mut it| {
                    it.find(|d| d.name().ok().as_deref() == Some(name))
                })
                .or_else(|| host.default_input_device())
                .ok_or(AudioInputError::NoDevice)?
        } else {
            host.default_input_device()
                .ok_or(AudioInputError::NoDevice)?
        };

        let default_cfg = device
            .default_input_config()
            .map_err(|e| AudioInputError::Config(e.to_string()))?;

        // Prefer the requested sample rate if supported, else use the default.
        let chosen_rate = pick_supported_rate(&device, sample_rate)
            .unwrap_or_else(|| default_cfg.sample_rate().0);

        // Channels: keep cpal's native count (usually 1 for a microphone, 2
        // for a line-in). We record whatever the device provides and let the
        // downstream WAV consumer interpret.
        let channels = default_cfg.channels();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(chosen_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        tracing::info!(
            "AudioInput: device={:?}, rate={chosen_rate}, channels={channels}",
            device.name().unwrap_or_default()
        );

        // 4 seconds of float slack — the writer thread polls every 5 ms so the
        // real steady-state usage is ~few kB, but we want a big safety margin
        // for the first few callbacks where the OS hasn't scheduled the writer
        // yet.
        let capacity = (chosen_rate as usize) * channels as usize * 4;
        let (mut producer, consumer) = HeapRb::<f32>::new(capacity.max(8192)).split();

        let err_fn = |e| {
            tracing::error!("cpal input stream error: {e}");
        };

        // Shared dedup flag across the three sample-format variants.
        // Only one branch below actually runs per stream, and a genuine
        // panic deserves exactly one log per process run.
        static INPUT_CALLBACK_PANIC_LOGGED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (p_f32, p_i16, p_u16) = (peak.clone(), peak.clone(), peak.clone());

        // Build for f32 — every mainstream device on every OS supports f32
        // via cpal's auto-conversion.
        let stream = match default_cfg.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    crate::thread_guard::catch_panic_once(
                        "klipp-cpal-input-f32",
                        &INPUT_CALLBACK_PANIC_LOGGED,
                        || {
                            // Best-effort — if the ring is full (writer thread stalled),
                            // we drop samples rather than blocking the audio thread.
                            let _ = producer.push_slice(data);
                            note_peak(&p_f32, data);
                        },
                    );
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    crate::thread_guard::catch_panic_once(
                        "klipp-cpal-input-i16",
                        &INPUT_CALLBACK_PANIC_LOGGED,
                        || {
                            let mut tmp = Vec::with_capacity(data.len());
                            for &s in data {
                                tmp.push(s as f32 / i16::MAX as f32);
                            }
                            let _ = producer.push_slice(&tmp);
                            note_peak(&p_i16, &tmp);
                        },
                    );
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    crate::thread_guard::catch_panic_once(
                        "klipp-cpal-input-u16",
                        &INPUT_CALLBACK_PANIC_LOGGED,
                        || {
                            let mut tmp = Vec::with_capacity(data.len());
                            for &s in data {
                                // u16 is unsigned 0..=65535 with silence at 32768.
                                let centered = s as f32 - 32768.0;
                                tmp.push(centered / 32768.0);
                            }
                            let _ = producer.push_slice(&tmp);
                            note_peak(&p_u16, &tmp);
                        },
                    );
                },
                err_fn,
                None,
            ),
            other => return Err(AudioInputError::Config(format!(
                "unsupported sample format {other:?}"
            ))),
        }
        .map_err(|e| AudioInputError::BuildStream(e.to_string()))?;

        Ok((
            Self {
                stream: Some(stream),
                sample_rate: chosen_rate,
                channels,
                peak,
            },
            consumer,
        ))
    }

    /// Start capturing. Safe to call multiple times.
    pub fn start(&self) -> Result<(), AudioInputError> {
        if let Some(s) = &self.stream {
            s.play().map_err(AudioInputError::Play)?;
        }
        Ok(())
    }

    /// The loudest sample since the last call, 0 to 1 (more when the
    /// device clips past full scale), and the meter starts again.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, std::sync::atomic::Ordering::Relaxed))
    }

    /// Stop capturing and release the device. Further `start()` calls are no-ops.
    pub fn stop(&mut self) {
        // Dropping the cpal::Stream halts and releases the device.
        self.stream = None;
    }
}

/// Raise `peak` to the loudest of `data`, without a lock: the audio
/// thread must not wait.
fn note_peak(peak: &std::sync::atomic::AtomicU32, data: &[f32]) {
    let loudest = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let _ = peak.try_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |was| {
        (loudest > f32::from_bits(was)).then_some(loudest.to_bits())
    });
}

/// Pick the caller's requested sample rate if it's listed as supported.
/// Returns `None` if no config enumeration is available or the rate isn't
/// supported, in which case the caller should fall back to the default config.
fn pick_supported_rate(device: &cpal::Device, want: u32) -> Option<u32> {
    let configs = device.supported_input_configs().ok()?;
    for c in configs {
        let lo = c.min_sample_rate().0;
        let hi = c.max_sample_rate().0;
        if want >= lo && want <= hi {
            return Some(want);
        }
    }
    None
}

/// Enumerate all input devices on the default cpal host.
/// Returns `(device_name, is_default)` pairs. Mirrors
/// `audio_output::enumerate_output_devices` in spirit but with a slimmer
/// payload since the input-device picker doesn't need rate/channel details.
pub fn enumerate_input_devices() -> Vec<(String, bool)> {
    crate::audio_output::quiet_alsa();
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());

    let devices = match host.input_devices() {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for device in devices {
        let name = match device.name() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let is_default = default_name.as_deref() == Some(&name);
        out.push((name, is_default));
    }

    // Push the default to position 0 so UI dropdowns can treat `out[0]` as
    // the natural highlighted entry.
    if let Some(idx) = out.iter().position(|(_, d)| *d) {
        if idx != 0 {
            let d = out.remove(idx);
            out.insert(0, d);
        }
    }

    out
}

/// Spawn a thread that drains the input ring buffer into a WAV file.
///
/// The file is opened immediately with a placeholder 44-byte header; sizes
/// are patched on a clean stop. If the process crashes mid-recording the
/// file is still valid RIFF-wise up to the last flushed block — most
/// players will still play it (the header just understates the length).
///
/// Stop the thread by sending `()` on `stop_rx`. The thread will perform a
/// final drain, patch the header sizes, fsync, and return.
pub fn start_recording_thread(
    mut consumer: ringbuf::HeapCons<f32>,
    path: std::path::PathBuf,
    sample_rate: u32,
    channels: u16,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::spawn(move || -> Result<(), String> {
        let file = std::fs::File::create(&path)
            .map_err(|e| format!("create wav file: {e}"))?;
        let mut w = std::io::BufWriter::new(file);

        // Write header with data-size placeholder = 0; we'll patch both the
        // RIFF total-size (offset 4) and the data-chunk size (offset 40)
        // when we close.
        write_wav_header(&mut w, sample_rate, channels, 0)
            .map_err(|e| format!("write wav header: {e}"))?;

        let mut bytes_written: u64 = 0;
        let mut buf = [0f32; 4096];

        loop {
            let n = consumer.pop_slice(&mut buf);
            if n > 0 {
                write_f32_samples(&mut w, &buf[..n])
                    .map_err(|e| format!("write samples: {e}"))?;
                bytes_written += (n * 4) as u64;
            }

            if stop_rx.try_recv().is_ok() {
                // Drain anything the cpal callback pushed between the last
                // pop and the stop signal — otherwise we'd truncate the tail.
                loop {
                    let m = consumer.pop_slice(&mut buf);
                    if m == 0 {
                        break;
                    }
                    write_f32_samples(&mut w, &buf[..m])
                        .map_err(|e| format!("write final samples: {e}"))?;
                    bytes_written += (m * 4) as u64;
                }
                break;
            }

            if n == 0 {
                // No data this tick — avoid busy-spinning on the ring buffer.
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        // Patch header sizes. BufWriter won't seek-through-buffer cleanly,
        // so we flush first.
        w.flush().map_err(|e| format!("flush: {e}"))?;
        let mut file = w.into_inner().map_err(|e| format!("unwrap bufwriter: {e}"))?;
        patch_wav_sizes(&mut file, bytes_written)
            .map_err(|e| format!("patch wav sizes: {e}"))?;
        file.flush().map_err(|e| format!("final flush: {e}"))?;

        tracing::info!(
            "Recording finished: {} ({} bytes PCM, rate={}, ch={})",
            path.display(), bytes_written, sample_rate, channels
        );
        Ok(())
    })
}

/// Write a 44-byte WAVE header for IEEE 32-bit float PCM.
/// `data_size_bytes` can be 0 as a placeholder and patched later.
fn write_wav_header<W: Write>(
    w: &mut W,
    sample_rate: u32,
    channels: u16,
    data_size_bytes: u32,
) -> std::io::Result<()> {
    let byte_rate: u32 = sample_rate * channels as u32 * 4; // 4 bytes per f32
    let block_align: u16 = channels * 4;
    let bits_per_sample: u16 = 32;
    // RIFF chunk
    w.write_all(b"RIFF")?;
    // Total size - 8 (everything after this field). With 0 data bytes the
    // placeholder is 36 (header remainder). Patched on close.
    w.write_all(&(36u32.wrapping_add(data_size_bytes)).to_le_bytes())?;
    w.write_all(b"WAVE")?;

    // fmt  sub-chunk
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;     // PCM fmt chunk size
    w.write_all(&3u16.to_le_bytes())?;       // AudioFormat = 3 (IEEE float)
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits_per_sample.to_le_bytes())?;

    // data sub-chunk
    w.write_all(b"data")?;
    w.write_all(&data_size_bytes.to_le_bytes())?;
    Ok(())
}

/// Patch the RIFF total-size (offset 4) and data-chunk size (offset 40) once
/// the final recorded byte count is known. Works on an already-written file.
fn patch_wav_sizes(file: &mut std::fs::File, data_bytes: u64) -> std::io::Result<()> {
    // u32 overflow would require a >4 GB WAV file; cap to u32::MAX and log.
    let data_size = if data_bytes > u32::MAX as u64 {
        tracing::warn!("WAV data > 4 GB ({}); size field truncated", data_bytes);
        u32::MAX
    } else {
        data_bytes as u32
    };
    let riff_size = 36u32.saturating_add(data_size);

    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.seek(SeekFrom::Start(40))?;
    file.write_all(&data_size.to_le_bytes())?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

/// Write a slice of `f32` samples as little-endian bytes.
fn write_f32_samples<W: Write>(w: &mut W, samples: &[f32]) -> std::io::Result<()> {
    // Could avoid per-sample conversion with bytemuck::cast_slice, but the
    // 5 ms poll interval means this isn't on any hot path.
    for &s in samples {
        w.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The meter keeps the loudest sample until it is read, and reading
    /// starts it again (#114).
    #[test]
    fn the_peak_holds_the_loudest_until_read() {
        let peak = std::sync::atomic::AtomicU32::new(0);
        super::note_peak(&peak, &[0.1, -0.5, 0.2]);
        super::note_peak(&peak, &[0.3]);
        assert_eq!(f32::from_bits(peak.load(std::sync::atomic::Ordering::Relaxed)), 0.5, "a quieter block does not lower it");
        assert_eq!(f32::from_bits(peak.swap(0, std::sync::atomic::Ordering::Relaxed)), 0.5);
        super::note_peak(&peak, &[0.25]);
        assert_eq!(f32::from_bits(peak.load(std::sync::atomic::Ordering::Relaxed)), 0.25);
    }

    use super::*;

    #[test]
    fn enumerate_input_devices_no_panic() {
        // Just don't crash — in CI there may be no input devices, and that's fine.
        let devices = enumerate_input_devices();
        let _ = devices;
    }

    #[test]
    fn recording_path_format() {
        let path = std::path::Path::new("/tmp/cache")
            .join("recordings")
            .join("rec_12345.wav");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("wav"));
    }

    #[test]
    fn wav_header_is_44_bytes_and_correct_for_48k_stereo_f32() {
        let mut buf: Vec<u8> = Vec::new();
        write_wav_header(&mut buf, 48_000, 2, 0).unwrap();
        assert_eq!(buf.len(), 44, "WAVE/IEEE-float header must be 44 bytes");

        // Spot-check critical fields.
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]), 16);
        assert_eq!(u16::from_le_bytes([buf[20], buf[21]]), 3); // IEEE float
        assert_eq!(u16::from_le_bytes([buf[22], buf[23]]), 2); // stereo
        assert_eq!(u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]), 48_000);
        // ByteRate: 48000 * 2 * 4
        assert_eq!(
            u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
            48_000 * 2 * 4
        );
        // BlockAlign: 2 * 4
        assert_eq!(u16::from_le_bytes([buf[32], buf[33]]), 8);
        assert_eq!(u16::from_le_bytes([buf[34], buf[35]]), 32); // bits
        assert_eq!(&buf[36..40], b"data");
    }

    #[test]
    fn patch_wav_sizes_updates_riff_and_data_fields() {
        let tmp = std::env::temp_dir().join(format!(
            "klipp_wav_patch_test_{}.wav",
            std::process::id()
        ));
        {
            let f = std::fs::File::create(&tmp).unwrap();
            let mut w = std::io::BufWriter::new(f);
            write_wav_header(&mut w, 48_000, 2, 0).unwrap();
            // Append 16 bytes of "pcm" (4 stereo f32 samples worth of zeros).
            w.write_all(&[0u8; 16]).unwrap();
            w.flush().unwrap();
            let mut file = w.into_inner().unwrap();
            patch_wav_sizes(&mut file, 16).unwrap();
        }
        let bytes = std::fs::read(&tmp).unwrap();
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            36 + 16
        );
        assert_eq!(
            u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
            16
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn track_record_armed_default_is_false() {
        let t = klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio);
        assert!(!t.record_armed);
    }
}
