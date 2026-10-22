//! PANNs Cnn10 (#2216): AudioSet's 527 sound classes for a stretch of
//! sound, the small tagger behind audio role classification.
//!
//! Kong et al., "PANNs: Large-Scale Pretrained Audio Neural Networks for
//! Audio Pattern Recognition" (2020); weights CC-BY-4.0, from Zenodo
//! record 3987831, as safetensors. The file carries the network's own
//! STFT and mel filter weights, so its log-mel front end is computed here
//! exactly as it was trained: a reflect-padded 1024-point STFT every 320
//! samples at 32 kHz, power, 64 mel bands, 10·log10.

/// The model's id in the manifest.
pub const PANNS_CNN10_MODEL_ID: &str = "panns_cnn10";

/// The rate the model hears at.
pub const SAMPLE_RATE: u32 = 32_000;

/// AudioSet's class count.
pub const CLASSES: usize = 527;

const N_FFT: usize = 1024;
const HOP: usize = 320;

#[cfg(feature = "candle")]
mod net {
    use super::{CLASSES, HOP, N_FFT};
    use candle_core::{DType, Device, Module, ModuleT, Tensor, D};
    use candle_nn::{BatchNorm, Conv2d, Linear, VarBuilder};

    struct Block {
        conv1: Conv2d,
        bn1: BatchNorm,
        conv2: Conv2d,
        bn2: BatchNorm,
    }

    impl Block {
        fn new(vb: VarBuilder, cin: usize, cout: usize) -> candle_core::Result<Self> {
            let cfg = candle_nn::Conv2dConfig { padding: 1, ..Default::default() };
            let bn = |name: &str| candle_nn::batch_norm(cout, candle_nn::BatchNormConfig { eps: 1e-5, ..Default::default() }, vb.pp(name));
            Ok(Self {
                conv1: candle_nn::conv2d_no_bias(cin, cout, 3, cfg, vb.pp("conv1"))?,
                bn1: bn("bn1")?,
                conv2: candle_nn::conv2d_no_bias(cout, cout, 3, cfg, vb.pp("conv2"))?,
                bn2: bn("bn2")?,
            })
        }

        fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
            let x = self.bn1.forward_t(&self.conv1.forward(x)?, false)?.relu()?;
            let x = self.bn2.forward_t(&self.conv2.forward(&x)?, false)?.relu()?;
            x.avg_pool2d((2, 2))
        }
    }

    pub struct Cnn10 {
        device: Device,
        stft_real: Tensor,
        stft_imag: Tensor,
        mel: Tensor,
        bn0: BatchNorm,
        blocks: [Block; 4],
        fc1: Linear,
        fc_audioset: Linear,
    }

    impl Cnn10 {
        pub fn new(bytes: Vec<u8>, device: Device) -> candle_core::Result<Self> {
            let vb = VarBuilder::from_buffered_safetensors(bytes, DType::F32, &device)?;
            let vb = vb.pp("backbone");
            let bins = N_FFT / 2 + 1;
            let stft = vb.pp("spectrogram_extractor").pp("stft");
            Ok(Self {
                stft_real: stft.get((bins, 1, N_FFT), "conv_real.weight")?,
                stft_imag: stft.get((bins, 1, N_FFT), "conv_imag.weight")?,
                mel: vb.pp("logmel_extractor").get((bins, 64), "melW")?,
                bn0: candle_nn::batch_norm(64, candle_nn::BatchNormConfig { eps: 1e-5, ..Default::default() }, vb.pp("bn0"))?,
                blocks: [
                    Block::new(vb.pp("conv_block1"), 1, 64)?,
                    Block::new(vb.pp("conv_block2"), 64, 128)?,
                    Block::new(vb.pp("conv_block3"), 128, 256)?,
                    Block::new(vb.pp("conv_block4"), 256, 512)?,
                ],
                fc1: candle_nn::linear(512, 512, vb.pp("fc1"))?,
                fc_audioset: candle_nn::linear(512, CLASSES, vb.pp("fc_audioset"))?,
                device,
            })
        }

        /// Each of `windows` (32 kHz mono, all one length) scored against
        /// every class, 0 to 1.
        pub fn forward(&self, windows: &[Vec<f32>]) -> candle_core::Result<Vec<Vec<f32>>> {
            let padded: Vec<Vec<f32>> = windows.iter().map(|w| reflect_pad(w, N_FFT / 2)).collect();
            let len = padded.first().map_or(0, Vec::len);
            let flat: Vec<f32> = padded.into_iter().flatten().collect();
            let x = Tensor::from_vec(flat, (windows.len(), 1, len), &self.device)?;
            let real = x.conv1d(&self.stft_real, 0, HOP, 1, 1)?;
            let imag = x.conv1d(&self.stft_imag, 0, HOP, 1, 1)?;
            // (batch, bins, frames) power, to (batch, frames, mel).
            let power = (real.sqr()? + imag.sqr()?)?.transpose(1, 2)?;
            let mel = power.broadcast_matmul(&self.mel)?;
            let db = (mel.maximum(1e-10f32)?.log()? * (10.0 / std::f64::consts::LN_10))?;
            // (batch, 1, frames, mel); bn0 normalises each mel band.
            let x = db.unsqueeze(1)?;
            let x = self.bn0.forward_t(&x.transpose(1, 3)?, false)?.transpose(1, 3)?;
            let mut x = x;
            for block in &self.blocks {
                x = block.forward(&x)?;
            }
            // Mean over frequency, then max plus mean over time.
            let x = x.mean(D::Minus1)?;
            let x = (x.max(D::Minus1)? + x.mean(D::Minus1)?)?;
            let x = self.fc1.forward(&x)?.relu()?;
            let p = candle_nn::ops::sigmoid(&self.fc_audioset.forward(&x)?)?;
            p.to_vec2::<f32>()
        }
    }

    /// `x` padded by `n` on each side with its own mirror, not repeating
    /// the edge sample, as `torch.nn.functional.pad(mode="reflect")`.
    pub(super) fn reflect_pad(x: &[f32], n: usize) -> Vec<f32> {
        if x.len() <= n {
            let mut out = vec![0.0; n];
            out.extend_from_slice(x);
            out.resize(x.len() + 2 * n, 0.0);
            return out;
        }
        let mut out = Vec::with_capacity(x.len() + 2 * n);
        out.extend((1..=n).rev().map(|i| x[i]));
        out.extend_from_slice(x);
        out.extend((1..=n).map(|i| x[x.len() - 1 - i]));
        out
    }
}

/// The loaded tagger.
pub struct Cnn10 {
    #[cfg(feature = "candle")]
    net: net::Cnn10,
}

impl Cnn10 {
    /// The weights at `path`, on the GPU when candle has one, else the CPU.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        #[cfg(feature = "candle")]
        {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let device = candle_core::Device::cuda_if_available(0).unwrap_or(candle_core::Device::Cpu);
            let net = net::Cnn10::new(bytes, device).map_err(|e| format!("loading the sound tagger: {e}"))?;
            Ok(Self { net })
        }
        #[cfg(not(feature = "candle"))]
        {
            let _ = path;
            Err("this build has no candle backend (build with ai-candle)".into())
        }
    }

    /// Each window of 32 kHz mono sound (all one length) scored against
    /// AudioSet's classes, 0 to 1, in its index order.
    pub fn classify(&self, windows: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        if windows.iter().any(|w| w.len() != windows[0].len() || w.len() < N_FFT) {
            return Err("windows must share one length of at least 1024 samples".into());
        }
        #[cfg(feature = "candle")]
        {
            self.net.forward(windows).map_err(|e| format!("tagging sound: {e}"))
        }
        #[cfg(not(feature = "candle"))]
        {
            Err("this build has no candle backend (build with ai-candle)".into())
        }
    }
}

#[cfg(all(test, feature = "candle"))]
mod tests {
    use super::*;

    #[test]
    fn reflect_padding_mirrors_without_the_edge() {
        assert_eq!(net::reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2), vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]);
    }

    /// With the weights at `KLIPP_PANNS_CNN10` (the 25 MB file), a 1 kHz
    /// tone and silence read as what they are.
    #[test]
    fn a_tone_reads_as_a_tone() {
        let Ok(path) = std::env::var("KLIPP_PANNS_CNN10") else {
            eprintln!("skipping: set KLIPP_PANNS_CNN10 to the weights");
            return;
        };
        let model = Cnn10::load(std::path::Path::new(&path)).unwrap();
        let n = SAMPLE_RATE as usize * 2;
        let tone: Vec<f32> = (0..n).map(|i| 0.3 * (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / SAMPLE_RATE as f32).sin()).collect();
        let p = model.classify(&[tone, vec![0.0; n]]).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].len(), CLASSES);
        let top = |v: &[f32]| v.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        // 501 "Sine wave"; 500 "Silence".
        assert!([501, 502, 503].contains(&top(&p[0])), "a tone tops {}", top(&p[0]));
        assert_eq!(top(&p[1]), 500, "silence");
    }
}
