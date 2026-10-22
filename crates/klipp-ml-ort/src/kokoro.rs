//! Kokoro-82M text to speech: phoneme ids and a voice's style vector in,
//! 24 kHz mono samples out.
//!
//! The model is `onnx-community/Kokoro-82M-v1.0-ONNX`'s `model_q8f16`,
//! weights Apache-2.0. It takes `input_ids` (int64 `[1, n]`, the phoneme
//! ids wrapped in the pad id 0 at both ends), `style` (float32 `[1, 256]`,
//! one row of the voice's `[510, 1, 256]` pack, chosen by the number of
//! phonemes) and `speed` (float32 `[1]`), and gives `waveform` (float32
//! `[1, samples]`). Turning text into phoneme ids is not done here: that
//! is `klipp_audio::narration`'s, which runs espeak-ng.
//!
//! Without the `ort` feature the type still exists and says it cannot
//! run, so a build without ONNX Runtime refuses a Kokoro voice plainly.

/// The rate the model speaks at.
pub const SAMPLE_RATE: u32 = 24_000;

/// Phonemes the model takes in one go, not counting the two pads.
pub const MAX_PHONEMES: usize = 510;

/// The style row for a take of `phonemes` phonemes, from the raw bytes
/// of a voice file (little-endian float32, 510 rows of 256).
pub fn style_row(voice: &[u8], phonemes: usize) -> Result<Vec<f32>, String> {
    let rows = voice.len() / (256 * 4);
    if rows == 0 || !voice.len().is_multiple_of(256 * 4) {
        return Err(format!("a Kokoro voice is rows of 256 floats; this one is {} bytes", voice.len()));
    }
    let row = phonemes.min(rows - 1);
    Ok(voice[row * 1024..(row + 1) * 1024].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[cfg(feature = "ort")]
mod real {
    use ort::session::builder::GraphOptimizationLevel;

    /// A loaded model, ready to speak.
    pub struct Kokoro {
        session: ort::session::Session,
    }

    impl Kokoro {
        pub fn open(model: &std::path::Path) -> Result<Self, String> {
            crate::runtime_resolve::ensure_runtime_loadable()?;
            #[allow(unused_mut)]
            let mut builder = ort::session::Session::builder()
                .map_err(|e| format!("session builder: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| format!("set optimisation: {e}"))?
                .with_intra_threads(crate::ppocr::SESSION_THREADS)
                .map_err(|e| format!("set threads: {e}"))?;
            // On the GPU where it loads, what it got recorded (#2250).
            let name = model.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            builder = crate::provider::on_device(builder, klipp_ml::DevicePreference::GpuPreferCpu, &name)?;
            let session = builder.commit_from_file(model).map_err(|e| format!("load {}: {e}", model.display()))?;
            Ok(Self { session })
        }

        /// Speak `phonemes` (vocabulary ids, no pads) in the voice whose
        /// style row is `style`, at `speed` (1 as the voice speaks).
        pub fn speak(&mut self, phonemes: &[i64], style: &[f32], speed: f32) -> Result<Vec<f32>, String> {
            if phonemes.is_empty() {
                return Ok(Vec::new());
            }
            if phonemes.len() > super::MAX_PHONEMES {
                return Err(format!("{} phonemes is more than Kokoro takes at once ({})", phonemes.len(), super::MAX_PHONEMES));
            }
            if style.len() != 256 {
                return Err(format!("a style row is 256 floats, not {}", style.len()));
            }
            let mut ids = Vec::with_capacity(phonemes.len() + 2);
            ids.push(0i64);
            ids.extend_from_slice(phonemes);
            ids.push(0);
            let n = ids.len() as i64;
            let input_ids = ort::value::Value::from_array(([1i64, n], ids)).map_err(|e| format!("input_ids: {e}"))?;
            let style = ort::value::Value::from_array(([1i64, 256], style.to_vec())).map_err(|e| format!("style: {e}"))?;
            let speed = ort::value::Value::from_array(([1i64], vec![speed])).map_err(|e| format!("speed: {e}"))?;
            let outputs = self
                .session
                .run(ort::inputs!["input_ids" => input_ids, "style" => style, "speed" => speed])
                .map_err(|e| format!("Kokoro: {e}"))?;
            let (_, wave) = outputs.iter().next().ok_or("Kokoro gave no output")?;
            let (_, samples) = wave.try_extract_tensor::<f32>().map_err(|e| format!("read waveform: {e}"))?;
            Ok(samples.to_vec())
        }
    }
}

#[cfg(feature = "ort")]
pub use real::Kokoro;

/// Stands in for the model in a build without ONNX Runtime.
#[cfg(not(feature = "ort"))]
pub struct Kokoro;

#[cfg(not(feature = "ort"))]
impl Kokoro {
    pub fn open(_model: &std::path::Path) -> Result<Self, String> {
        Err("this build of Klipp has no ONNX Runtime, which Kokoro voices need; a Piper voice works without it".into())
    }

    pub fn speak(&mut self, _phonemes: &[i64], _style: &[f32], _speed: f32) -> Result<Vec<f32>, String> {
        Err("no ONNX Runtime in this build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::style_row;

    #[test]
    fn the_style_row_follows_the_phoneme_count_and_stops_at_the_last() {
        // Three rows whose floats are their row number.
        let mut voice = Vec::new();
        for row in 0..3u32 {
            for _ in 0..256 {
                voice.extend_from_slice(&(row as f32).to_le_bytes());
            }
        }
        assert_eq!(style_row(&voice, 1).unwrap()[0], 1.0);
        assert_eq!(style_row(&voice, 2).unwrap().len(), 256);
        assert_eq!(style_row(&voice, 99).unwrap()[255], 2.0, "past the end: the last row");
        assert!(style_row(&voice[..1000], 0).is_err(), "not whole rows");
    }
}
