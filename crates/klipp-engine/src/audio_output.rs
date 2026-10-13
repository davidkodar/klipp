use crate::audio_decoder::AudioConsumer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Consumer;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{error, info};

/// Consumer side of the skim audio ring buffer, shared with the cpal callback.
pub type SkimConsumer = Arc<Mutex<Option<ringbuf::HeapCons<f32>>>>;

/// Preferred audio device name (set from preferences). None = system default.
pub(crate) static PREFERRED_DEVICE: Mutex<Option<String>> = Mutex::new(None);

/// Set the preferred audio output device name. Call before creating AudioOutput.
/// If the device isn't found at runtime, silently falls back to the system default.
pub fn set_preferred_device(name: Option<String>) {
    *PREFERRED_DEVICE.lock().unwrap_or_else(|e| e.into_inner()) = name;
}

/// Output callbacks while playing that ran out of samples part-way
/// through, after the ring had been feeding them: each is a gap the
/// listener hears as a click or a dropout.
pub static UNDERRUN_CALLBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static LAST_CALLBACK_SHORT: AtomicBool = AtomicBool::new(true);

/// How many gaps have opened mid-stream since the app started.
pub fn underrun_callbacks() -> u64 {
    UNDERRUN_CALLBACKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Flush all samples from a skim consumer (drains to silence).
pub fn flush_skim_consumer(skim: &SkimConsumer) {
    if let Ok(mut guard) = skim.lock() {
        if let Some(ref mut cons) = *guard {
            let mut discard = [0.0f32; 4096];
            while cons.pop_slice(&mut discard) > 0 {}
        }
    }
}

/// Replace the skim consumer contents (None disconnects, Some connects a new consumer).
pub fn set_skim_consumer(skim: &SkimConsumer, consumer: Option<ringbuf::HeapCons<f32>>) {
    if let Ok(mut guard) = skim.lock() {
        *guard = consumer;
    }
}

/// ALSA's "could not open this plugin" messages go to the log, not to
/// stderr (see `alsa_quiet`). Every path that opens or lists a device
/// through cpal calls this first.
pub(crate) fn quiet_alsa() {
    #[cfg(target_os = "linux")]
    crate::alsa_quiet::install();
}


/// The preferred output device's name, if one is set.
fn preferred_device_name() -> Option<String> {
    PREFERRED_DEVICE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The device the output opens: the one named `preferred` when it is
/// present, otherwise the system default. A preferred device that has
/// gone (unplugged since it was chosen) is logged and the default used,
/// so a stale preference never costs the user their sound.
fn resolve_output_device(host: &cpal::Host, preferred: Option<&str>) -> Option<cpal::Device> {
    if let Some(name) = preferred {
        let found = host.output_devices().ok().and_then(|mut devices| devices.find(|d| d.name().ok().as_deref() == Some(name)));
        if found.is_some() {
            return found;
        }
        tracing::warn!("Audio device '{name}' not found, using the default");
    }
    host.default_output_device()
}

/// The device the output stream opens and the config it opens with,
/// decided once so the pipeline's rate and the stream's can never be
/// read from two different devices.
struct OutputChoice {
    device_name: String,
    config: cpal::SupportedStreamConfig,
}

/// U-5: cached in a process-wide OnceLock. cpal's device enumeration is
/// surprisingly expensive — a cold probe on ALSA / PipeWire can take
/// 10-40 ms — and the engine and the UI both ask at startup. Decided for
/// the preferred device, which klipp-app sets before the engine is built;
/// a change of device applies at the next launch.
fn output_choice() -> Option<&'static OutputChoice> {
    static CACHED: std::sync::OnceLock<Option<OutputChoice>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            quiet_alsa();
            let host = cpal::default_host();
            let device = resolve_output_device(&host, preferred_device_name().as_deref())?;
            let config = output_config_for(&device)?;
            Some(OutputChoice { device_name: device.name().unwrap_or_default(), config })
        })
        .as_ref()
}

/// The sample rate the output device opens at, which the playback
/// pipeline is built for. 48000 when there is no device.
pub fn probe_sample_rate() -> u32 {
    output_choice().map(|c| c.config.sample_rate().0).unwrap_or(48000)
}

/// The rate Klipp's playback runs at. Every PCM cache and every effect
/// chain is 48 kHz, so that is the rate to open a device at when it offers
/// it; anything else means converting every sample. A device's *default*
/// config is often not 48 kHz even when it takes it: PipeWire's ALSA
/// bridge reports 44.1 kHz and then converts back to its own 48 kHz graph,
/// which converted each sample twice.
const PREFERRED_OUTPUT_RATE: u32 = 48_000;

/// Pick the output rate: [`PREFERRED_OUTPUT_RATE`] when one of the
/// device's supported ranges covers it with the default config's channel
/// count and the same (or f32) sample format, otherwise the default rate.
/// A range that would also change the channel layout never wins.
pub(crate) fn choose_output_rate(
    default_rate: u32,
    default_channels: u16,
    default_format: cpal::SampleFormat,
    ranges: &[(u16, cpal::SampleFormat, u32, u32)],
) -> u32 {
    let offers_preferred = ranges.iter().any(|&(channels, format, min, max)| {
        channels == default_channels
            && (format == default_format || format == cpal::SampleFormat::F32)
            && (min..=max).contains(&PREFERRED_OUTPUT_RATE)
    });
    if offers_preferred { PREFERRED_OUTPUT_RATE } else { default_rate }
}

/// The config to open `device` with: its default config at the rate
/// [`choose_output_rate`] picks. A rate other than the default is tried
/// by building (not playing) a stream first, since some ALSA plugins list
/// rates they then refuse; on refusal the default config is used, so the
/// choice never costs the user their sound.
fn output_config_for(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    use cpal::traits::DeviceTrait;
    let default = device.default_output_config().ok()?;
    let ranges: Vec<(u16, cpal::SampleFormat, u32, u32)> = device
        .supported_output_configs()
        .map(|it| it.map(|r| (r.channels(), r.sample_format(), r.min_sample_rate().0, r.max_sample_rate().0)).collect())
        .unwrap_or_default();
    let rate = choose_output_rate(default.sample_rate().0, default.channels(), default.sample_format(), &ranges);
    if rate == default.sample_rate().0 {
        return Some(default);
    }
    let candidate = cpal::SupportedStreamConfig::new(
        default.channels(),
        cpal::SampleRate(rate),
        *default.buffer_size(),
        default.sample_format(),
    );
    let trial = cpal::StreamConfig {
        channels: candidate.channels(),
        sample_rate: candidate.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    match device.build_output_stream(&trial, |_: &mut [f32], _: &cpal::OutputCallbackInfo| {}, |_| {}, None) {
        Ok(_) => Some(candidate),
        Err(e) => {
            tracing::warn!("audio device lists {rate} Hz but refused it ({e}); using its default {} Hz", default.sample_rate().0);
            Some(default)
        }
    }
}

/// Check if an audio output device is available.
///
/// U-5: OnceLock-cached, same rationale as `probe_sample_rate`.
pub fn probe_audio_available() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        use cpal::traits::HostTrait;
        quiet_alsa();
        let host = cpal::default_host();
        host.default_output_device().is_some()
    })
}

/// Detailed info about a single output audio device, suitable for showing
/// the user in a "Detected hardware" panel. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    /// Device name as cpal reports it (e.g. `"pipewire"`, `"hw:CARD=PCH,DEV=0"`).
    pub name: String,
    /// True for the system default output device.
    pub is_default: bool,
    /// Native sample rate of the device's default output config (Hz).
    pub default_sample_rate: u32,
    /// Channel count of the default output config.
    pub default_channels: u16,
    /// Sample format label (`"f32"`, `"i16"`, `"u16"`, or `"?"`).
    pub default_format: &'static str,
    /// Min sample rate across all supported configs, or 0 if unknown.
    pub min_sample_rate: u32,
    /// Max sample rate across all supported configs, or 0 if unknown.
    pub max_sample_rate: u32,
    /// Max channel count across all supported configs.
    pub max_channels: u16,
}

/// Identifier of the cpal host backend (e.g. "ALSA", "CoreAudio", "WASAPI").
/// On Linux this is always "ALSA" — even when PipeWire is the active server,
/// cpal speaks to it via the ALSA shim, so the device list will include the
/// "pipewire" virtual device.
pub fn host_name() -> &'static str {
    cpal::default_host().id().name()
}

/// Process-wide cache of the most recent `enumerate_output_devices()` result.
/// Filled lazily by `cached_output_devices()` (or eagerly by
/// `prime_audio_devices_cache()` at startup) and refreshable via
/// `refresh_audio_devices_cache()` so hot-plugged devices can show up.
///
/// Why a cache at all: cpal device enumeration takes 5–50 ms depending on
/// the platform and would hitch UI rendering if called every frame.
///
/// Why not push-based: cpal does not expose any device-change notification
/// API. To get true hot-plug callbacks we'd need platform-specific code
/// (libudev on Linux, AudioObjectAddPropertyListener on macOS,
/// MMNotificationClient on Windows). For now we re-enumerate on demand —
/// when the user opens the Audio HW preferences tab or clicks "Refresh".
static AUDIO_DEVICE_CACHE: std::sync::Mutex<Option<Vec<AudioDeviceInfo>>> =
    std::sync::Mutex::new(None);

/// Return a clone of the cached output device list, populating it from
/// `enumerate_output_devices()` on first call. Cheap on every call after
/// the first. Safe to call from any thread.
pub fn cached_output_devices() -> Vec<AudioDeviceInfo> {
    let mut guard = match AUDIO_DEVICE_CACHE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // recover from poisoning — cache contents are still valid
    };
    if guard.is_none() {
        *guard = Some(enumerate_output_devices());
    }
    guard.clone().unwrap_or_default()
}

/// Force a fresh re-enumeration and replace the cache. Use this when the
/// user explicitly asks to refresh the device list (e.g. after hot-plugging
/// a USB audio interface) or when re-opening the preferences panel.
/// Returns the refreshed list.
pub fn refresh_audio_devices_cache() -> Vec<AudioDeviceInfo> {
    let fresh = enumerate_output_devices();
    if let Ok(mut guard) = AUDIO_DEVICE_CACHE.lock() {
        *guard = Some(fresh.clone());
    }
    fresh
}

/// Eagerly populate the audio device cache. Call this once during app
/// startup (ideally from a background thread) so the first user visit
/// to the Audio HW preferences panel doesn't hit the 5–50 ms enumeration
/// hitch. Idempotent — calling it again does nothing.
pub fn prime_audio_devices_cache() {
    let _ = cached_output_devices();
}

/// Enumerate every available output device with rich info. Returns an empty
/// vec if no host or no devices are available — never panics. The result is
/// suitable for caching: cpal device enumeration takes 5–50 ms depending on
/// the platform and shouldn't be done every UI frame.
///
/// The default device (if any) is marked with `is_default = true`. Order is
/// "default first, then everything else in cpal's enumeration order" so a
/// dropdown built from the result naturally puts the default at the top.
pub fn enumerate_output_devices() -> Vec<AudioDeviceInfo> {
    use cpal::traits::{DeviceTrait, HostTrait};
    quiet_alsa();
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    let devices = match host.output_devices() {
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

        let default_cfg = device.default_output_config().ok();
        let (default_sample_rate, default_channels, default_format) =
            if let Some(cfg) = default_cfg.as_ref() {
                let fmt: &'static str = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => "f32",
                    cpal::SampleFormat::I16 => "i16",
                    cpal::SampleFormat::U16 => "u16",
                    _ => "?",
                };
                (cfg.sample_rate().0, cfg.channels(), fmt)
            } else {
                (0, 0, "?")
            };

        // Walk supported_output_configs to compute the rate range.
        // This iterator is consumed once, so we drain it into a Vec first.
        let mut min_rate = u32::MAX;
        let mut max_rate = 0u32;
        let mut max_channels = 0u16;
        if let Ok(configs) = device.supported_output_configs() {
            for c in configs {
                let lo = c.min_sample_rate().0;
                let hi = c.max_sample_rate().0;
                if lo < min_rate {
                    min_rate = lo;
                }
                if hi > max_rate {
                    max_rate = hi;
                }
                if c.channels() > max_channels {
                    max_channels = c.channels();
                }
            }
        }
        if min_rate == u32::MAX {
            min_rate = 0;
        }

        out.push(AudioDeviceInfo {
            name,
            is_default,
            default_sample_rate,
            default_channels,
            default_format,
            min_sample_rate: min_rate,
            max_sample_rate: max_rate,
            max_channels,
        });
    }

    // Move default to the front so dropdowns built from this list can use
    // out[0] as the natural "selected default" entry.
    if let Some(idx) = out.iter().position(|d| d.is_default) {
        if idx != 0 {
            let d = out.remove(idx);
            out.insert(0, d);
        }
    }

    out
}

pub struct AudioOutput {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    paused: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
    /// Bumped by the engine after a seek. The cpal callback detects the change
    /// and discards all buffered samples to prevent playing stale audio.
    seek_gen: Arc<AtomicU64>,
    /// When true, play ring buffer samples even while paused (for scrub chirps).
    /// Automatically cleared by the cpal callback when the ring buffer empties.
    chirp_active: Arc<AtomicBool>,
    /// Master volume applied in the cpal callback (lock-free, instant response).
    master_volume: Arc<AtomicU32>,
    /// The linear gain that makes playback as loud as the export will
    /// deliver (#1565); 1 when there is no loudness target.
    delivery_gain: Arc<AtomicU32>,
    /// Consumer side of the skim audio ring buffer.
    /// Mixed into the cpal callback output alongside playback audio.
    skim_consumer: SkimConsumer,
}

impl AudioOutput {
    /// Create a cpal output stream that reads from the ring buffer consumer.
    /// Pure consumer — no clock duties. The master clock is external.
    /// `skim_consumer` is shared with the app layer so it can inject skim audio.
    pub fn new(consumer: AudioConsumer, skim_consumer: SkimConsumer) -> Result<Self, AudioOutputError> {
        Self::new_with_device(consumer, skim_consumer, preferred_device_name().as_deref())
    }

    /// Create audio output with an optional device name preference.
    /// Falls back to the system default device if the named device isn't found.
    pub fn new_with_device(mut consumer: AudioConsumer, skim_consumer: SkimConsumer, preferred_device: Option<&str>) -> Result<Self, AudioOutputError> {
        quiet_alsa();
        let host = cpal::default_host();
        let device = resolve_output_device(&host, preferred_device).ok_or(AudioOutputError::NoDevice)?;

        // The config the pipeline's rate was probed with (see
        // `probe_sample_rate`) when this is the device it was probed on;
        // a device chosen since then is probed afresh.
        let name = device.name().unwrap_or_default();
        let supported_config = match output_choice() {
            Some(choice) if choice.device_name == name => choice.config.clone(),
            _ => output_config_for(&device).ok_or_else(|| AudioOutputError::Config("no output config".into()))?,
        };

        let sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        info!(
            "AudioOutput: device={:?}, rate={sample_rate}, channels={channels}",
            device.name().unwrap_or_default()
        );

        let paused = Arc::new(AtomicBool::new(false));
        let paused_flag = paused.clone();
        let flushing = Arc::new(AtomicBool::new(false));
        let flushing_flag = flushing.clone();
        let seek_gen = Arc::new(AtomicU64::new(0));
        let seek_gen_flag = seek_gen.clone();
        let chirp_active = Arc::new(AtomicBool::new(false));
        let chirp_flag = chirp_active.clone();
        let master_volume = Arc::new(AtomicU32::new(f32::to_bits(1.0)));
        let master_vol_flag = master_volume.clone();
        let delivery_gain = Arc::new(AtomicU32::new(f32::to_bits(1.0)));
        let delivery_gain_flag = delivery_gain.clone();
        let skim_consumer_flag = skim_consumer.clone();

        // Static dedup flag for the cpal output callback. cpal fires
        // this at ~90 Hz; a repeating panic through plain catch_panic
        // would drown every other diagnostic. See thread_guard docs.
        static OUTPUT_CALLBACK_PANIC_LOGGED: AtomicBool = AtomicBool::new(false);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // alsa-lib's error handler is per thread, and this is
                    // the thread that recovers from an underrun and would
                    // print "snd_pcm_recover: underrun occurred" to stderr.
                    quiet_alsa();
                    crate::thread_guard::catch_panic_once(
                        "klipp-cpal-output",
                        &OUTPUT_CALLBACK_PANIC_LOGGED,
                        || {
                            // Flush denormals once per
                            // cpal-thread first call. The callback runs
                            // on a dedicated thread (see LAST_GEN below),
                            // so this thread_local once is sufficient.
                            // Defensive — the mixer threads upstream
                            // already flush, but the callback does its
                            // own volume/pan math that can produce
                            // denormals in-place.
                            thread_local! {
                                static FTZ_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
                            }
                            FTZ_INIT.with(|f| {
                                if !f.get() {
                                    klipp_audio::flush_denormals_to_zero();
                                    f.set(true);
                                }
                            });

                            // Check for seek — if generation changed, drain stale buffer
                            let current_gen = seek_gen_flag.load(Ordering::Acquire);
                            // Use a thread-local to track last seen generation
                            // (cpal callbacks always run on the same thread)
                            thread_local! {
                                static LAST_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
                            }
                            LAST_GEN.with(|lg| {
                                if lg.get() != current_gen {
                                    lg.set(current_gen);
                                    // Drain all stale samples from the ring buffer
                                    let mut discard = vec![0.0f32; 8192];
                                    loop {
                                        let n = consumer.pop_slice(&mut discard);
                                        if n == 0 {
                                            break;
                                        }
                                    }
                                    // Output silence for this callback
                                    data.fill(0.0);
                                    return;
                                }

                                let is_paused = paused_flag.load(Ordering::Acquire);
                                let is_chirping = chirp_flag.load(Ordering::Acquire);

                                if is_paused && is_chirping {
                                    // Chirp mode: play ring buffer samples even while paused.
                                    // When buffer empties, chirp is done.
                                    let filled = consumer.pop_slice(data);
                                    data[filled..].fill(0.0);
                                    if filled == 0 {
                                        chirp_flag.store(false, Ordering::Release);
                                    }
                                    return;
                                }
                                if is_paused {
                                    // Paused — output silence, don't drain ring buffer
                                    data.fill(0.0);
                                    return;
                                }
                                if flushing_flag.load(Ordering::Acquire) {
                                    // Flushing — drain ring buffer but output silence
                                    let _ = consumer.pop_slice(data);
                                    data.fill(0.0);
                                    return;
                                }
                                let filled = consumer.pop_slice(data);
                                data[filled..].fill(0.0);
                                // A gap that opens mid-stream is a dropout the
                                // listener hears; the silence before the ring
                                // has filled at the start of playback is not.
                                let short = filled < data.len();
                                if short && !LAST_CALLBACK_SHORT.swap(short, Ordering::Relaxed) && filled > 0 {
                                    UNDERRUN_CALLBACKS.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    LAST_CALLBACK_SHORT.store(short, Ordering::Relaxed);
                                }
                                // Master volume times the delivery gain (lock-free).
                                // Each clip's volume, gain, pan and channel config
                                // are applied to its own source in the mix thread.
                                // A change ramps across one callback, so a
                                // delivery gain that lands mid-play doesn't click
                                // (#1565).
                                let vol = f32::from_bits(master_vol_flag.load(Ordering::Relaxed))
                                    * f32::from_bits(delivery_gain_flag.load(Ordering::Relaxed));
                                thread_local! {
                                    static APPLIED_GAIN: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
                                }
                                APPLIED_GAIN.with(|g| {
                                    apply_gain_ramp(&mut data[..filled], g.get(), vol);
                                    g.set(vol);
                                });
                                // NO clock update — master clock is external
                            });

                            // Mix in skim audio (independent of playback state).
                            // Uses thread-local buffer to avoid heap allocation in real-time callback.
                            //
                            // Diagnostic counters track whether the skim consumer
                            // is connected and how many samples per call get
                            // mixed in. Printed once per second from a separate
                            // path. Use SKIM_DIAG_LAST to throttle.
                            thread_local! {
                                static SKIM_DIAG_FRAMES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
                                static SKIM_DIAG_NONZERO: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
                                static SKIM_DIAG_DISCONNECTED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
                                static SKIM_DIAG_LAST: std::cell::Cell<std::time::Instant> = std::cell::Cell::new(std::time::Instant::now());
                            }
                            let mut skim_read_this_call: usize = 0;
                            let mut skim_connected = false;
                            if let Ok(mut skim_guard) = skim_consumer_flag.try_lock() {
                                if let Some(ref mut skim_cons) = *skim_guard {
                                    skim_connected = true;
                                    thread_local! {
                                        static SKIM_BUF: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
                                    }
                                    SKIM_BUF.with(|buf| {
                                        let mut buf = buf.borrow_mut();
                                        buf.resize(data.len(), 0.0);
                                        let read = skim_cons.pop_slice(&mut buf[..data.len()]);
                                        skim_read_this_call = read;
                                        for i in 0..read {
                                            data[i] = (data[i] + buf[i]).clamp(-1.0, 1.0);
                                        }
                                    });
                                }
                            }
                            SKIM_DIAG_FRAMES.with(|c| c.set(c.get() + 1));
                            if skim_read_this_call > 0 {
                                SKIM_DIAG_NONZERO.with(|c| c.set(c.get() + 1));
                            }
                            if !skim_connected {
                                SKIM_DIAG_DISCONNECTED.with(|c| c.set(c.get() + 1));
                            }
                            SKIM_DIAG_LAST.with(|last| {
                                let now = std::time::Instant::now();
                                if now.duration_since(last.get()) >= std::time::Duration::from_secs(1) {
                                    let frames = SKIM_DIAG_FRAMES.with(|c| c.replace(0));
                                    let nonzero = SKIM_DIAG_NONZERO.with(|c| c.replace(0));
                                    let disc = SKIM_DIAG_DISCONNECTED.with(|c| c.replace(0));
                                    // Only when the skim path is doing
                                    // something. "frames > 0" is true
                                    // whenever the audio callback runs,
                                    // so this line was emitted once a
                                    // second for the whole life of the
                                    // process, at INFO, in release —
                                    // 260 copies of
                                    // "disconnected=38, nothing read"
                                    // in a four-minute session, which
                                    // is the ordinary idle state and
                                    // says nothing. It was written to
                                    // chase skim flicker, so it fires
                                    // when skim actually read audio, or
                                    // when only some callbacks saw a
                                    // disconnected source — the mixed
                                    // case that a flicker looks like.
                                    if frames > 0 && (nonzero > 0 || (disc > 0 && disc != frames)) {
                                        tracing::info!(
                                            "cpal_skim_diag: frames={} nonzero={} disconnected={} last_read={}",
                                            frames, nonzero, disc, skim_read_this_call,
                                        );
                                    }
                                    last.set(now);
                                }
                            });
                        },
                    );
                },
                move |err| {
                    error!("cpal stream error: {err}");
                },
                None,
            )
            .map_err(|e| AudioOutputError::Build(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioOutputError::Play(e.to_string()))?;

        Ok(Self {
            _stream: stream,
            sample_rate,
            channels,
            paused,
            flushing,
            seek_gen,
            chirp_active,
            master_volume,
            delivery_gain,
            skim_consumer,
        })
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        self.flushing.store(false, Ordering::Release);
        self.paused.store(false, Ordering::Release);
    }

    /// Start flushing: drain ring buffer silently.
    pub fn start_flush(&self) {
        self.flushing.store(true, Ordering::Release);
    }

    /// Signal a seek happened — cpal callback will drain stale samples.
    pub fn notify_seek(&self) {
        self.seek_gen.fetch_add(1, Ordering::Release);
    }

    /// Enable chirp mode — play ring buffer samples even while paused.
    /// Automatically deactivates when the ring buffer empties.
    pub fn start_chirp(&self) {
        self.chirp_active.store(true, Ordering::Release);
    }

    /// Set master volume (applied instantly in cpal callback, no buffer lag).
    pub fn set_master_volume(&self, vol: f32) {
        self.master_volume.store(f32::to_bits(vol), Ordering::Relaxed);
    }

    /// Set the delivery gain, linear (#1565): applied after the master
    /// volume, ramped over one callback.
    pub fn set_delivery_gain(&self, gain: f32) {
        self.delivery_gain.store(f32::to_bits(gain), Ordering::Relaxed);
    }

    /// Wire a skim audio consumer into the cpal callback for mixing.
    pub fn set_skim_consumer(&self, consumer: Option<ringbuf::HeapCons<f32>>) {
        if let Ok(mut guard) = self.skim_consumer.lock() {
            *guard = consumer;
        }
    }

    /// Flush (drain) the skim ring buffer — called when skim position changes
    /// or session ends, to prevent stale audio from playing.
    pub fn flush_skim_buffer(&self) {
        if let Ok(mut guard) = self.skim_consumer.lock() {
            if let Some(ref mut cons) = *guard {
                let mut discard = [0.0f32; 4096];
                while cons.pop_slice(&mut discard) > 0 {}
            }
        }
    }
}

#[derive(Debug)]
pub enum AudioOutputError {
    NoDevice,
    Config(String),
    Build(String),
    Play(String),
}

impl std::fmt::Display for AudioOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no audio output device"),
            Self::Config(e) => write!(f, "audio config: {e}"),
            Self::Build(e) => write!(f, "build stream: {e}"),
            Self::Play(e) => write!(f, "play stream: {e}"),
        }
    }
}

/// Scale `data` by a gain that moves from `from` to `to` across it, so a
/// change of volume is a short fade rather than a step; untouched at
/// unity.
fn apply_gain_ramp(data: &mut [f32], from: f32, to: f32) {
    if (from - 1.0).abs() <= 0.001 && (to - 1.0).abs() <= 0.001 {
        return;
    }
    let n = data.len().max(1) as f32;
    for (i, x) in data.iter_mut().enumerate() {
        *x *= from + (to - from) * (i + 1) as f32 / n;
    }
}

#[cfg(test)]
mod gain_ramp_tests {
    use super::apply_gain_ramp;

    /// #1565: a delivery gain that lands mid-play fades in over the
    /// buffer, no sample jumps by more than its share of the change, and
    /// the buffer ends at the new gain.
    #[test]
    fn a_gain_change_ramps_across_the_buffer() {
        let mut data = vec![1.0f32; 480];
        apply_gain_ramp(&mut data, 1.0, 1.6);
        assert!((data[0] - 1.0).abs() < 0.01, "starts at the old gain: {}", data[0]);
        assert!((data[479] - 1.6).abs() < 1e-5, "ends at the new: {}", data[479]);
        assert!(data.windows(2).all(|w| w[1] - w[0] < 0.6 / 479.0 + 1e-6), "no step");
        let mut same = vec![0.5f32; 4];
        apply_gain_ramp(&mut same, 1.0, 1.0);
        assert_eq!(same, [0.5; 4], "unity leaves the samples alone");
        let mut held = vec![1.0f32; 4];
        apply_gain_ramp(&mut held, 0.5, 0.5);
        assert_eq!(held, [0.5; 4], "a steady gain is applied in full");
    }
}

#[cfg(test)]
mod device_enum_tests {

    //! Tests for the audio device enumeration helpers. These run against
    //! whatever cpal sees on the host machine, so we keep the assertions
    //! shape-based rather than depending on specific device names. CI
    //! machines may legitimately have zero devices — that case is
    //! exercised explicitly.
    use super::*;

    #[test]
    fn host_name_is_non_empty() {
        let name = host_name();
        assert!(!name.is_empty(), "cpal host should have a non-empty name");
    }

    #[test]
    fn enumerate_returns_a_vec_without_panicking() {
        // The list may be empty (CI / sandbox) but enumeration must not panic.
        let _ = enumerate_output_devices();
    }

    #[test]
    fn at_most_one_device_marked_default() {
        let devices = enumerate_output_devices();
        let default_count = devices.iter().filter(|d| d.is_default).count();
        assert!(
            default_count <= 1,
            "expected at most one default device, got {}",
            default_count
        );
    }

    #[test]
    fn default_device_is_first_when_present() {
        let devices = enumerate_output_devices();
        if let Some(default_idx) = devices.iter().position(|d| d.is_default) {
            assert_eq!(
                default_idx, 0,
                "default device should be first in the list"
            );
        }
    }

    #[test]
    fn enumerate_device_fields_are_well_formed() {
        let devices = enumerate_output_devices();
        for d in &devices {
            assert!(!d.name.is_empty(), "device name should not be empty");
            // Sample rate of 0 is allowed (means "couldn't read default
            // config"), but if non-zero it should be a sane value.
            if d.default_sample_rate != 0 {
                assert!(
                    d.default_sample_rate >= 8_000 && d.default_sample_rate <= 768_000,
                    "device {} has implausible default rate {}",
                    d.name,
                    d.default_sample_rate
                );
            }
            // If we got a min and max range, min must not exceed max.
            if d.min_sample_rate != 0 && d.max_sample_rate != 0 {
                assert!(
                    d.min_sample_rate <= d.max_sample_rate,
                    "device {} has min_sample_rate {} > max_sample_rate {}",
                    d.name,
                    d.min_sample_rate,
                    d.max_sample_rate
                );
            }
            // Format label is one of our known strings.
            assert!(
                matches!(d.default_format, "f32" | "i16" | "u16" | "?"),
                "device {} has unexpected format label {:?}",
                d.name,
                d.default_format
            );
        }
    }

    #[test]
    #[ignore = "cpal+ALSA dmix/dsnoop contention under parallel test execution; run with --ignored --test-threads=1 (TEST-CPAL-PARALLEL-HANG)"]
    fn cached_output_devices_returns_consistent_results() {
        // Two consecutive calls without explicit refresh should return
        // the same set of device names (the cache is stable between
        // refresh calls).
        let first = cached_output_devices();
        let second = cached_output_devices();
        let first_names: Vec<&str> = first.iter().map(|d| d.name.as_str()).collect();
        let second_names: Vec<&str> = second.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(first_names, second_names);
    }

    #[test]
    #[ignore = "cpal+ALSA dmix/dsnoop contention under parallel test execution; run with --ignored --test-threads=1 (TEST-CPAL-PARALLEL-HANG)"]
    fn refresh_audio_devices_cache_returns_a_vec() {
        // Same shape contract as enumerate_output_devices — the result
        // may be empty but must not panic and must update the cache.
        let refreshed = refresh_audio_devices_cache();
        let cached = cached_output_devices();
        assert_eq!(
            refreshed.iter().map(|d| &d.name).collect::<Vec<_>>(),
            cached.iter().map(|d| &d.name).collect::<Vec<_>>(),
            "refresh result should match the new cache contents"
        );
    }

    #[test]
    #[ignore = "cpal+ALSA dmix/dsnoop contention under parallel test execution; run with --ignored --test-threads=1 (TEST-CPAL-PARALLEL-HANG)"]
    fn prime_is_idempotent_and_does_not_panic() {
        prime_audio_devices_cache();
        prime_audio_devices_cache();
        let _ = cached_output_devices();
    }

    #[test]
    fn output_rate_prefers_48k_without_changing_the_layout() {
        use cpal::SampleFormat::{F32, I16};
        // PipeWire's ALSA bridge: default 44.1 kHz, takes anything.
        assert_eq!(choose_output_rate(44_100, 2, F32, &[(2, F32, 8_000, 192_000)]), 48_000);
        // Only a mono or 8-channel 48k: keep the default stereo rate.
        assert_eq!(choose_output_rate(44_100, 2, F32, &[(1, F32, 48_000, 48_000), (8, F32, 48_000, 48_000)]), 44_100);
        // 48k only in another integer format while the default is f32: keep.
        assert_eq!(choose_output_rate(44_100, 2, F32, &[(2, I16, 48_000, 48_000)]), 44_100);
        // An integer default may move to f32 at 48k (the stream is f32).
        assert_eq!(choose_output_rate(44_100, 2, I16, &[(2, F32, 48_000, 48_000)]), 48_000);
        // A device that cannot do 48k at all stays at its default.
        assert_eq!(choose_output_rate(44_100, 2, F32, &[(2, F32, 44_100, 44_100)]), 44_100);
        // Already 48k.
        assert_eq!(choose_output_rate(48_000, 2, F32, &[]), 48_000);
    }
}
