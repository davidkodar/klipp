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

use super::resample::Resampler;
use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::collections::VecDeque;
use klipp_deepfilter::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::Array2;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

/// Model id in `assets/models.toml`.
pub const MODEL_ID: &str = "deepfilternet3";
/// DeepFilterNet3 runs at 48 kHz. At other rates (a 44.1 kHz sound card)
/// the effect resamples to 48 kHz and back around the model.
pub const MODEL_RATE: u32 = 48_000;
/// Samples the rate bridge's output queue starts with, so it never runs
/// dry while the two resamplers' outputs settle into step.
const BRIDGE_PRIME: usize = 2;
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
    /// Hops still to wait after a reset before the engine's output
    /// reflects the new position; until then the delayed dry sound plays.
    settling_hops: usize,
    /// Conversion to and from 48 kHz when the chain runs at another rate.
    bridge: Option<Bridge>,
}

impl Stream {
    /// A seek: forget the old position without rebuilding the engine
    /// (a ~45 ms stall on the audio path). The engine keeps running and
    /// hears the new position; the delayed dry sound covers the hops until
    /// its output has caught up.
    fn restart(&mut self) {
        for line in &mut self.dry {
            line.fill(0.0);
        }
        self.dry_pos = 0;
        self.filled = 0;
        self.out.fill(0.0);
        if let Some((noisy, _)) = self.buffers.as_mut() {
            noisy.fill(0.0);
        }
        self.settling_hops = self.model_delay.div_ceil(self.hop) + 1;
        if let Some(bridge) = self.bridge.as_mut() {
            let channels = bridge.up.len();
            let rate = bridge.rate;
            if let Some(fresh) = Bridge::new(rate, channels, self.hop + self.model_delay) {
                *bridge = fresh;
            }
        }
    }
}

/// Resamplers around the 48 kHz model, per channel, and the queue that
/// hands the result back one sample per input sample.
struct Bridge {
    rate: u32,
    up: Vec<Resampler>,
    down: Vec<Resampler>,
    queue: Vec<VecDeque<f32>>,
    planar: Vec<Vec<f32>>,
    at_48k: Vec<f32>,
}

impl Bridge {
    fn new(rate: u32, channels: usize, model_frames: usize) -> Option<Self> {
        let (_, steps) = Self::plan(rate, model_frames)?;
        let up = (0..channels).map(|_| Resampler::new(rate, MODEL_RATE)).collect::<Option<Vec<_>>>()?;
        let down = (0..channels)
            .map(|_| Resampler::new(MODEL_RATE, rate).map(|r| r.with_extra_delay(steps)))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            rate,
            up,
            down,
            queue: vec![VecDeque::from(vec![0.0; BRIDGE_PRIME]); channels],
            planar: vec![Vec::new(); channels],
            at_48k: Vec::new(),
        })
    }

    /// The whole delay at `rate` around a model delay of `model_frames` at
    /// 48 kHz, and the extra delay steps the down-resampler takes so that
    /// delay is a whole number of samples: up, model and down, then the
    /// queue's priming.
    fn plan(rate: u32, model_frames: usize) -> Option<(usize, u64)> {
        let up = Resampler::new(rate, MODEL_RATE)?.delay();
        let down = Resampler::new(MODEL_RATE, rate)?;
        let to_rate = rate as f64 / MODEL_RATE as f64;
        let at_rate = up + (model_frames as f64 + down.delay()) * to_rate;
        let whole = at_rate.ceil();
        let steps = ((whole - at_rate) / (down.delay_step() * to_rate)).round() as u64;
        Some((whole as usize + BRIDGE_PRIME, steps))
    }

    fn latency(rate: u32, model_frames: usize) -> Option<usize> {
        Self::plan(rate, model_frames).map(|(latency, _)| latency)
    }
}

/// How the 48 kHz loop mixes its output.
#[derive(Clone, Copy)]
struct Mix {
    remove_voice: bool,
    voice_gain: f32,
    full: bool,
    untouched: bool,
    limit_db: f32,
}

/// Run interleaved 48 kHz audio (`stride` samples a frame, the first
/// `channels` processed) through the engine, in place. False if the
/// engine thread has gone.
fn run_at_model_rate(s: &mut Stream, buffer: &mut [f32], stride: usize, channels: usize, mix: Mix) -> bool {
    for frame in buffer.chunks_exact_mut(stride) {
        let Some((noisy, _)) = s.buffers.as_mut() else { return false };
        for ch in 0..channels {
            let input = frame[ch];
            noisy[[ch, s.filled]] = input;
            let enhanced = s.out[[ch, s.filled]];
            let dry = &mut s.dry[ch];
            let delayed = dry[s.dry_pos];
            dry[s.dry_pos] = input;
            frame[ch] = if mix.untouched || s.settling_hops > 0 {
                delayed
            } else if mix.remove_voice {
                let gain = if mix.full { 1.0 } else { mix.voice_gain };
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
                .send(Hop { noisy, enhanced, limit_db: mix.limit_db })
                .ok()
                .and_then(|_| s.engine.from_engine.recv().ok());
            match back {
                Some(hop) => {
                    s.out.assign(&hop.enhanced);
                    s.buffers = Some((hop.noisy, hop.enhanced));
                    s.settling_hops = s.settling_hops.saturating_sub(1);
                }
                None => return false,
            }
        }
    }
    true
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

    /// Whether audio is being processed (the model is present).
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
        let bridge = if self.sample_rate == MODEL_RATE {
            None
        } else {
            let model_frames = model_params().and_then(|p| Some(p.hop_size()? + p.latency_samples()?)).unwrap_or(0);
            match Bridge::new(self.sample_rate, channels, model_frames) {
                Some(b) => Some(b),
                None => {
                    tracing::warn!("Voice Isolation: cannot convert {} Hz to 48 kHz; passing audio through", self.sample_rate);
                    return;
                }
            }
        };
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
            settling_hops: 0,
            bridge,
        });
        self.apply_limit();
    }

    /// Input-to-output delay. Known before the engine starts (it starts on
    /// the first block), because export and playback delay compensation
    /// ask for it before processing anything.
    fn total_latency(&self) -> usize {
        let model_frames = match &self.stream {
            Some(s) => s.hop + s.model_delay,
            None => match model_params().and_then(|p| Some(p.hop_size()? + p.latency_samples()?)) {
                Some(frames) => frames,
                None => return 0,
            },
        };
        if self.sample_rate == MODEL_RATE {
            model_frames
        } else {
            Bridge::latency(self.sample_rate, model_frames).unwrap_or(0)
        }
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
        let mix = Mix {
            remove_voice: self.remove_voice,
            voice_gain: 1.0 - 10f32.powf(-self.reduction_db / 20.0),
            full: self.reduction_db >= FULL_REDUCTION_DB,
            untouched: self.reduction_db <= 0.0,
            limit_db: self.engine_limit_db,
        };
        let Some(s) = self.stream.as_mut() else { return };
        let stride = ctx.channels as usize;
        let alive = match s.bridge.take() {
            None => run_at_model_rate(s, buffer, stride, channels, mix),
            Some(mut bridge) => {
                // Up to 48 kHz, through the model, and back down.
                let frames = buffer.len() / stride;
                for ch in 0..channels {
                    let input: Vec<f32> = (0..frames).map(|i| buffer[i * stride + ch]).collect();
                    bridge.planar[ch].clear();
                    bridge.up[ch].push(&input, &mut bridge.planar[ch]);
                }
                let n48 = bridge.planar.iter().map(Vec::len).min().unwrap_or(0);
                bridge.at_48k.clear();
                for i in 0..n48 {
                    for ch in 0..channels {
                        bridge.at_48k.push(bridge.planar[ch][i]);
                    }
                }
                let alive = run_at_model_rate(s, &mut bridge.at_48k, channels, channels, mix);
                for ch in 0..channels {
                    let at_48k: Vec<f32> = (0..n48).map(|i| bridge.at_48k[i * channels + ch]).collect();
                    let mut back = Vec::with_capacity(frames + 1);
                    bridge.down[ch].push(&at_48k, &mut back);
                    bridge.queue[ch].extend(back);
                    for i in 0..frames {
                        buffer[i * stride + ch] = bridge.queue[ch].pop_front().unwrap_or(0.0);
                    }
                }
                s.bridge = Some(bridge);
                alive
            }
        };
        if !alive {
            tracing::warn!("Voice Isolation: the engine stopped; passing audio through");
            self.stream = None;
        }
    }
    fn reset(&mut self) {
        if let Some(s) = self.stream.as_mut() {
            s.restart();
        }
    }
    fn prepare(&mut self, channels: usize) {
        self.ensure_stream(channels.min(MAX_CHANNELS));
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

    /// Output of `fx` for `input` at `rate`, mono.
    fn run_at(rate: u32, input: &[f32], reduction: f32, mode: f32) -> (Vec<f32>, usize, usize) {
        let mut fx = VoiceIsolation::new(rate);
        fx.set_param("reduction", reduction);
        fx.set_param("mode", mode);
        let before = fx.latency_samples() as usize;
        let mut buf = input.to_vec();
        for block in buf.chunks_mut(1000) {
            fx.process(block, &AudioProcessContext { sample_rate: rate, channels: 1, clip_time_secs: 0.0 });
        }
        (buf, before, fx.latency_samples() as usize)
    }

    fn residual_db(out: &[f32], input: &[f32], latency: usize, from: usize) -> f32 {
        let (mut err, mut sig) = (0.0f32, 0.0f32);
        for i in from..out.len() {
            err += (out[i] - input[i - latency]).powi(2);
            sig += input[i - latency].powi(2);
        }
        10.0 * (err / sig).log10()
    }

    #[test]
    fn works_at_44_1_khz_with_the_same_alignment() {
        if !model_installed() {
            // Without the model the effect leaves the sound as it is.
            let input = noise(44_100, 4);
            let (out, _, latency) = run_at(44_100, &input, 30.0, 0.0);
            assert_eq!((out, latency), (input, 0));
            return;
        }
        let rate = 44_100;
        let tone = |hz: f32| -> Vec<f32> {
            (0..rate as usize * 2).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.3).collect()
        };
        let settled = rate as usize / 2;
        // 0 dB: the input, delayed by exactly the reported latency, across
        // the band (a sub-sample slip would show at the top first).
        for hz in [200.0f32, 1000.0, 5000.0, 12_000.0] {
            let input = tone(hz);
            let (out, before, latency) = run_at(rate, &input, 0.0, 0.0);
            assert!(latency > 0);
            assert_eq!(before, latency, "latency is known before the first block at 44.1 kHz too");
            let db = residual_db(&out, &input, latency, settled);
            assert!(db < -50.0, "{hz} Hz should come back on time, residual {db:.1} dB");
        }
        // Isolate and Remove still add back up to the input.
        let input: Vec<f32> = tone(300.0).iter().zip(tone(2200.0)).map(|(a, b)| a + b).collect();
        let (isolated, _, latency) = run_at(rate, &input, FULL_REDUCTION_DB, 0.0);
        let (removed, _, _) = run_at(rate, &input, FULL_REDUCTION_DB, 1.0);
        let sum: Vec<f32> = isolated.iter().zip(&removed).map(|(a, b)| a + b).collect();
        let db = residual_db(&sum, &input, latency, settled);
        assert!(db < -50.0, "isolate + remove should be the input, residual {db:.1} dB");
    }

    #[test]
    fn a_seek_keeps_the_engine_and_plays_nothing_from_before_it() {
        if !model_installed() {
            eprintln!("skipped: {MODEL_ID} not downloaded");
            return;
        }
        let mut fx = VoiceIsolation::new(MODEL_RATE);
        fx.set_param("reduction", FULL_REDUCTION_DB);
        fx.prepare(1);
        assert!(fx.is_active(), "prepare starts the engine before the first block");
        let loud: Vec<f32> = (0..MODEL_RATE as usize).map(|i| (i as f32 * 0.05).sin() * 0.8 + ((i * 7919) % 97) as f32 / 97.0 * 0.2).collect();
        let mut buf = loud.clone();
        fx.process(&mut buf, &ctx(1));
        let started = std::time::Instant::now();
        fx.reset();
        assert!(started.elapsed() < std::time::Duration::from_millis(10), "a seek must not rebuild the engine");
        let mut after = vec![0.0f32; MODEL_RATE as usize / 2];
        for block in after.chunks_mut(512) {
            fx.process(block, &ctx(1));
        }
        let peak = after.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1e-3, "sound from before the seek leaked out: peak {peak}");
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
