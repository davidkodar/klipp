//! Voice Isolation: DeepFilterNet3 speech enhancement as an audio effect.
//!
//! A neural network separates speech from everything else (wind,
//! traffic, room tone, hum), which classic spectral denoising cannot do
//! for noise that moves. Two modes share one model:
//!
//! - **Isolate voice** keeps the speech and lowers the rest by up to the
//!   Reduction amount, so some ambience can stay.
//! - **Remove voice** keeps the rest and lowers the speech: the input
//!   minus the isolated voice, delay-aligned.
//!
//! The model is not bundled. It downloads on request through the
//! verified model cache (`deepfilternet3` in `assets/models.toml`); until
//! it is present the effect passes audio through untouched.

use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use klipp_deepfilter::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::Array2;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

/// Model id in `assets/models.toml`.
pub const MODEL_ID: &str = "deepfilternet3";
/// DeepFilterNet3 runs at 48 kHz only; at other rates the effect passes
/// audio through.
pub const MODEL_RATE: u32 = 48_000;
const MAX_CHANNELS: usize = 2;
/// Reduction at or beyond this is "no limit".
pub const FULL_REDUCTION_DB: f32 = 60.0;

/// Bumped when the model is installed, so effects built before that
/// retry loading it instead of passing through for the rest of the chain.
static MODEL_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The model, loaded once. Engines are built from it per effect.
static MODEL: Mutex<Option<DfParams>> = Mutex::new(None);

/// Where the model lives in the model cache.
pub fn model_path() -> Option<PathBuf> {
    klipp_ml::ModelCache::open().ok().map(|c| c.path_for_id(MODEL_ID))
}

/// Whether the model has been downloaded.
pub fn model_installed() -> bool {
    model_path().is_some_and(|p| p.is_file())
}

/// Tell running effects the model is now installed.
pub fn notify_model_installed() {
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Load the model ahead of first use, so the first chain that needs it
/// does not read and unpack it on the audio path.
pub fn prewarm() {
    let _ = model_params();
}

fn model_params() -> Option<DfParams> {
    let mut guard = MODEL.lock().ok()?;
    if guard.is_none() {
        let path = model_path().filter(|p| p.is_file())?;
        match DfParams::new(path) {
            Ok(p) => *guard = Some(p),
            Err(e) => {
                tracing::warn!("Voice Isolation: could not load the model: {e:#}");
                return None;
            }
        }
    }
    guard.clone()
}

/// One hop of audio travelling to the engine thread and back. The two
/// buffers go back and forth, so nothing is allocated per hop.
struct Hop {
    noisy: Array2<f32>,
    enhanced: Array2<f32>,
    /// Attenuation limit in dB for the engine (100 = none).
    limit_db: f32,
}

/// The engine's shape, reported once it is built.
struct Ready {
    hop: usize,
    model_delay: usize,
}

/// A DeepFilterNet engine on its own thread. The engine keeps its state in
/// types that may not cross threads, so it is built on this thread and
/// never leaves it; the effect trades plain audio buffers with it. The
/// thread ends when the effect drops its side of the channel.
struct Engine {
    to_engine: Sender<Hop>,
    from_engine: Receiver<Hop>,
}

impl Engine {
    fn start(channels: usize) -> Option<(Engine, Ready)> {
        let params = model_params()?;
        let (to_engine, jobs) = channel::<Hop>();
        let (results, from_engine) = channel::<Hop>();
        let (ready_tx, ready_rx) = channel::<Option<Ready>>();
        std::thread::Builder::new()
            .name("klipp-voice-isolation".into())
            .spawn(move || {
                // The command-line tool's thresholds and a max-reduced
                // mask, the configuration Klipp's denoiser benchmark measured.
                let runtime = RuntimeParams::default_with_ch(channels)
                    .with_thresholds(-15.0, 35.0, 35.0)
                    .with_mask_reduce(ReduceMask::MAX)
                    .with_atten_lim(100.0);
                let mut df = match DfTract::new(params, &runtime) {
                    Ok(df) => df,
                    Err(e) => {
                        tracing::warn!("Voice Isolation: could not build the engine: {e:#}");
                        let _ = ready_tx.send(None);
                        return;
                    }
                };
                let model_delay = df.fft_size - df.hop_size + df.lookahead * df.hop_size;
                let _ = ready_tx.send(Some(Ready { hop: df.hop_size, model_delay }));
                let mut limit = 100.0f32;
                while let Ok(mut hop) = jobs.recv() {
                    if hop.limit_db != limit {
                        limit = hop.limit_db;
                        df.set_atten_lim(limit);
                    }
                    if df.process(hop.noisy.view(), hop.enhanced.view_mut()).is_err() {
                        hop.enhanced.fill(0.0);
                    }
                    if results.send(hop).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        let ready = ready_rx.recv().ok().flatten()?;
        Some((Engine { to_engine, from_engine }, ready))
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "reduction", label: "Reduction (dB)", default: 30.0, min: 0.0, max: FULL_REDUCTION_DB, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "mode", label: "Mode", default: 0.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
];

/// Streaming state around one engine: the engine works in fixed hops, the
/// chain delivers any block size.
struct Stream {
    engine: Engine,
    channels: usize,
    hop: usize,
    /// Model delay: STFT overlap plus lookahead.
    model_delay: usize,
    /// The buffers of the hop being gathered; away at the engine while
    /// it processes, so `None` only if the engine thread died.
    buffers: Option<(Array2<f32>, Array2<f32>)>,
    /// Samples gathered towards the next hop, per channel.
    filled: usize,
    /// Enhanced output of the last hop, handed out while the next fills.
    out: Array2<f32>,
    /// Dry input delayed to line up with the output, for Remove voice.
    dry: Vec<Vec<f32>>,
    dry_pos: usize,
}

pub struct VoiceIsolation {
    sample_rate: u32,
    reduction_db: f32,
    remove_voice: bool,
    /// Attenuation limit passed to the engine, in dB.
    engine_limit_db: f32,
    stream: Option<Stream>,
    /// Model generation last tried, so a missing model is retried once
    /// it has been installed, not on every block.
    tried_generation: Option<u64>,
}

impl VoiceIsolation {
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate, reduction_db: 30.0, remove_voice: false, engine_limit_db: 30.0, stream: None, tried_generation: None }
    }

    /// Whether audio is being processed (model present, 48 kHz).
    pub fn is_active(&self) -> bool {
        self.stream.is_some()
    }

    fn apply_limit(&mut self) {
        // Remove voice needs the full voice estimate; its amount is
        // applied when subtracting. Below 0.01 dB the engine short-cuts
        // to its input without its model delay, which would jump the
        // output ~30 ms early; 0 dB is handled in `process` instead.
        self.engine_limit_db = if self.remove_voice || self.reduction_db >= FULL_REDUCTION_DB {
            100.0
        } else {
            self.reduction_db.max(0.1)
        };
    }

    fn ensure_stream(&mut self, channels: usize) {
        if self.sample_rate != MODEL_RATE {
            return;
        }
        if let Some(stream) = &self.stream {
            if stream.channels == channels {
                return;
            }
            self.stream = None;
            self.tried_generation = None;
        }
        let generation = MODEL_GENERATION.load(Ordering::Relaxed);
        if self.tried_generation == Some(generation) {
            return;
        }
        self.tried_generation = Some(generation);
        let Some((engine, ready)) = Engine::start(channels) else { return };
        let (hop, model_delay) = (ready.hop, ready.model_delay);
        let total = hop + model_delay;
        self.stream = Some(Stream {
            engine,
            channels,
            hop,
            model_delay,
            buffers: Some((Array2::zeros((channels, hop)), Array2::zeros((channels, hop)))),
            filled: 0,
            out: Array2::zeros((channels, hop)),
            dry: vec![vec![0.0; total]; channels],
            dry_pos: 0,
        });
        self.apply_limit();
    }

    /// Input-to-output delay. Known before the engine starts (it starts on
    /// the first block), because export and playback delay compensation
    /// ask for it before processing anything.
    fn total_latency(&self) -> usize {
        if let Some(s) = &self.stream {
            return s.hop + s.model_delay;
        }
        if self.sample_rate != MODEL_RATE {
            return 0;
        }
        model_params()
            .and_then(|p| Some(p.hop_size()? + p.latency_samples()?))
            .unwrap_or(0)
    }
}

impl AudioEffect for VoiceIsolation {
    fn type_id(&self) -> &'static str { "voice_isolation" }
    fn name(&self) -> &'static str { "Voice Isolation" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }
    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "reduction" => Some(self.reduction_db),
            "mode" => Some(if self.remove_voice { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "reduction" => self.reduction_db = value.clamp(0.0, FULL_REDUCTION_DB),
            "mode" => self.remove_voice = value > 0.5,
            _ => return,
        }
        self.apply_limit();
    }
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = (ctx.channels as usize).min(MAX_CHANNELS);
        if channels == 0 {
            return;
        }
        self.ensure_stream(channels);
        let remove_voice = self.remove_voice;
        let voice_gain = 1.0 - 10f32.powf(-self.reduction_db / 20.0);
        let full = self.reduction_db >= FULL_REDUCTION_DB;
        let untouched = self.reduction_db <= 0.0;
        let limit_db = self.engine_limit_db;
        let Some(s) = self.stream.as_mut() else { return };
        let stride = ctx.channels as usize;
        let mut engine_lost = false;
        for frame in buffer.chunks_exact_mut(stride) {
            let Some((noisy, _)) = s.buffers.as_mut() else {
                engine_lost = true;
                break;
            };
            for ch in 0..channels {
                let input = frame[ch];
                noisy[[ch, s.filled]] = input;
                let enhanced = s.out[[ch, s.filled]];
                let dry = &mut s.dry[ch];
                let delayed = dry[s.dry_pos];
                dry[s.dry_pos] = input;
                frame[ch] = if untouched {
                    delayed
                } else if remove_voice {
                    let gain = if full { 1.0 } else { voice_gain };
                    delayed - gain * enhanced
                } else {
                    enhanced
                };
            }
            s.dry_pos = (s.dry_pos + 1) % s.dry[0].len();
            s.filled += 1;
            if s.filled == s.hop {
                s.filled = 0;
                let (noisy, enhanced) = s.buffers.take().expect("checked above");
                let back = s
                    .engine
                    .to_engine
                    .send(Hop { noisy, enhanced, limit_db })
                    .ok()
                    .and_then(|_| s.engine.from_engine.recv().ok());
                match back {
                    Some(hop) => {
                        s.out.assign(&hop.enhanced);
                        s.buffers = Some((hop.noisy, hop.enhanced));
                    }
                    None => {
                        engine_lost = true;
                        break;
                    }
                }
            }
        }
        if engine_lost {
            tracing::warn!("Voice Isolation: the engine stopped; passing audio through");
            self.stream = None;
        }
    }
    fn reset(&mut self) {
        if let Some(s) = self.stream.take() {
            self.tried_generation = None;
            self.ensure_stream(s.channels);
        }
    }
    fn latency_samples(&self) -> u32 {
        self.total_latency() as u32
    }
    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("reduction".into(), self.reduction_db),
            ("mode".into(), if self.remove_voice { 1.0 } else { 0.0 }),
        ]
    }
    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (k, v) in params {
            self.set_param(k, *v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(channels: u32) -> AudioProcessContext {
        AudioProcessContext { sample_rate: MODEL_RATE, channels, clip_time_secs: 0.0 }
    }

    #[test]
    fn other_sample_rates_pass_through() {
        let mut fx = VoiceIsolation::new(44_100);
        let input: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        let mut buf = input.clone();
        fx.process(&mut buf, &ctx(1));
        assert_eq!(buf, input);
        assert_eq!(fx.latency_samples(), 0);
    }

    #[test]
    fn params_round_trip() {
        let mut fx = VoiceIsolation::new(MODEL_RATE);
        fx.set_param("reduction", 18.0);
        fx.set_param("mode", 1.0);
        let mut rebuilt = VoiceIsolation::new(MODEL_RATE);
        rebuilt.deserialize_params(&fx.serialize_params());
        assert_eq!(rebuilt.get_param("reduction"), Some(18.0));
        assert_eq!(rebuilt.get_param("mode"), Some(1.0));
    }

    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.1
            })
            .collect()
    }

    fn run(input: &[f32], reduction: f32, mode: f32) -> (Vec<f32>, usize) {
        let mut fx = VoiceIsolation::new(MODEL_RATE);
        fx.set_param("reduction", reduction);
        fx.set_param("mode", mode);
        let mut buf = input.to_vec();
        fx.process(&mut buf, &ctx(1));
        assert!(fx.is_active(), "the installed model should load");
        (buf, fx.latency_samples() as usize)
    }

    // What counts as speech is the model's call and is measured on real
    // recordings by the denoiser benchmark; these tests pin the plumbing
    // around it. They need the downloaded model and skip without it.

    #[test]
    fn isolate_and_remove_add_back_up_to_the_input() {
        if !model_installed() {
            eprintln!("skipped: {MODEL_ID} not downloaded");
            return;
        }
        let input = noise(MODEL_RATE as usize * 2, 3);
        let (isolated, latency) = run(&input, FULL_REDUCTION_DB, 0.0);
        let (removed, _) = run(&input, FULL_REDUCTION_DB, 1.0);
        assert!(latency > 0);
        for i in latency..input.len() {
            let sum = isolated[i] + removed[i];
            assert!((sum - input[i - latency]).abs() < 1e-4, "sample {i}: {sum} vs {}", input[i - latency]);
        }
    }

    #[test]
    fn latency_is_known_before_the_first_block() {
        if !model_installed() {
            eprintln!("skipped: {MODEL_ID} not downloaded");
            return;
        }
        // Export and playback compensation ask before processing.
        let mut fx = VoiceIsolation::new(MODEL_RATE);
        let before = fx.latency_samples();
        let mut buf = noise(MODEL_RATE as usize / 10, 9);
        fx.process(&mut buf, &ctx(2));
        assert!(before > 0, "latency must be reported before the engine starts");
        assert_eq!(before, fx.latency_samples(), "and match the running engine");
    }

    #[test]
    fn zero_reduction_returns_the_delayed_input() {
        if !model_installed() {
            eprintln!("skipped: {MODEL_ID} not downloaded");
            return;
        }
        let input = noise(MODEL_RATE as usize * 2, 5);
        let (out, latency) = run(&input, 0.0, 0.0);
        let err: f32 = (latency..input.len()).map(|i| (out[i] - input[i - latency]).powi(2)).sum();
        let total: f32 = input.iter().map(|v| v * v).sum();
        assert!(err < total * 1e-3, "0 dB reduction should leave the audio as it was");
    }
}
