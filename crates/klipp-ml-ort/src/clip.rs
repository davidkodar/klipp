//! CLIP ViT-B/32 (#2212): pictures and words in one space, so frames can
//! be found by describing them. OpenAI's weights (MIT), as the picture
//! and words encoders exported to ONNX, run on ONNX Runtime: on the GPU
//! where the runtime has a provider for it (#2245). Its byte-level BPE
//! tokenizer is read here from the model's `tokenizer.json`.

/// The picture encoder's, the words encoder's and the tokenizer's ids in
/// the manifest.
pub const CLIP_VISION_ID: &str = "clip_vit_b32_vision";
pub const CLIP_TEXT_ID: &str = "clip_vit_b32_text";
pub const CLIP_TOKENIZER_ID: &str = "clip_vit_b32_tokenizer";

/// The id the model had as one candle safetensors file, before the ONNX
/// encoders replaced it: its file in the model cache is Klipp's to
/// remove once they are there.
pub const CLIP_CANDLE_ID: &str = "clip_vit_b32";

/// The side of the square picture the model sees.
pub const IMAGE_SIZE: u32 = 224;

/// An embedding's length.
pub const DIM: usize = 512;

const CONTEXT: usize = 77;
const START: u32 = 49406;
const END: u32 = 49407;

/// CLIP's tokenizer: words split as its pre-tokenizer splits them, each
/// word's UTF-8 bytes as GPT-2's printable characters with `</w>` on the
/// last, merged pair by pair in rank order.
pub struct Tokenizer {
    vocab: std::collections::HashMap<String, u32>,
    ranks: std::collections::HashMap<(String, String), usize>,
    bytes: [char; 256],
}

impl Tokenizer {
    /// From a `tokenizer.json` (a BPE model with string merges).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("tokenizer: {e}"))?;
        let model = &v["model"];
        let vocab: std::collections::HashMap<String, u32> = serde_json::from_value(model["vocab"].clone()).map_err(|e| format!("tokenizer vocab: {e}"))?;
        let merges = model["merges"].as_array().ok_or("tokenizer: no merges")?;
        let mut ranks = std::collections::HashMap::with_capacity(merges.len());
        for (rank, m) in merges.iter().enumerate() {
            let pair = match m {
                serde_json::Value::String(s) => s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())),
                serde_json::Value::Array(a) => match (a.first().and_then(|x| x.as_str()), a.get(1).and_then(|x| x.as_str())) {
                    (Some(x), Some(y)) => Some((x.to_string(), y.to_string())),
                    _ => None,
                },
                _ => None,
            };
            if let Some(p) = pair {
                ranks.insert(p, rank);
            }
        }
        Ok(Self { vocab, ranks, bytes: byte_chars() })
    }

    /// `text`'s ids, between the start and end tokens, cut to the
    /// model's 77 and padded with 0 (pooling reads the end token, the
    /// highest id, so padding must stay below it).
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids = vec![START];
        for word in words(&text.to_lowercase()) {
            let mut parts: Vec<String> = word.bytes().map(|b| self.bytes[b as usize].to_string()).collect();
            if let Some(last) = parts.last_mut() {
                last.push_str("</w>");
            }
            loop {
                let best = parts.windows(2).enumerate().filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|r| (*r, i))).min();
                let Some((_, i)) = best else { break };
                let joined = format!("{}{}", parts[i], parts[i + 1]);
                parts.splice(i..i + 2, [joined]);
            }
            ids.extend(parts.iter().filter_map(|p| self.vocab.get(p)));
        }
        ids.truncate(CONTEXT - 1);
        ids.push(END);
        ids.resize(CONTEXT, 0);
        ids
    }
}

/// GPT-2's bytes as printable characters: the printable ones as
/// themselves, the rest from U+0100 on.
fn byte_chars() -> [char; 256] {
    let mut out = ['\0'; 256];
    let mut next = 256u32;
    for b in 0..256u32 {
        let printable = (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        out[b as usize] = if printable {
            char::from_u32(b).unwrap_or('?')
        } else {
            let c = char::from_u32(next).unwrap_or('?');
            next += 1;
            c
        };
    }
    out
}

/// The words CLIP's pre-tokenizer makes of lowercased text: a contraction
/// ('s 't 're 've 'm 'll 'd), a run of letters, a single digit, or a run
/// of anything else that is not a space.
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' {
            let rest: String = chars[i + 1..chars.len().min(i + 3)].iter().collect();
            if let Some(n) = ["re", "ve", "ll", "s", "t", "m", "d"].iter().find(|s| rest.starts_with(**s)).map(|s| s.len()) {
                out.push(chars[i..=i + n].iter().collect());
                i += n + 1;
                continue;
            }
        }
        let start = i;
        if c.is_alphabetic() {
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
        } else if c.is_numeric() {
            i += 1;
        } else {
            while i < chars.len() && !chars[i].is_whitespace() && !chars[i].is_alphabetic() && !chars[i].is_numeric() {
                i += 1;
            }
        }
        out.push(chars[start..i].iter().collect());
    }
    out
}

/// The model's two halves on ONNX Runtime: the picture encoder and the
/// words encoder, each giving the projected embedding.
#[cfg(feature = "ort")]
mod net {
    use super::*;
    use std::sync::Mutex;

    const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
    const STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

    pub struct Clip {
        vision: Mutex<ort::session::Session>,
        text: Mutex<ort::session::Session>,
        vision_model: String,
        text_model: String,
    }

    fn session(weights: &std::path::Path, device: klipp_ml::DevicePreference) -> Result<(ort::session::Session, String), String> {
        use ort::session::builder::GraphOptimizationLevel;
        crate::runtime_resolve::ensure_runtime_loadable()?;
        let builder = ort::session::Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("set optimisation: {e}"))?;
        // On the GPU where it loads, what it got recorded (#2245).
        let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let session = crate::provider::on_device(builder, device, &model)?
            .commit_from_file(weights)
            .map_err(|e| format!("load {}: {e}", weights.display()))?;
        Ok((session, model))
    }

    /// Each row of a `(rows, DIM)` output scaled to unit length.
    fn unit_rows(outputs: &ort::session::SessionOutputs<'_>, name: &str) -> Result<Vec<Vec<f32>>, String> {
        let v = outputs.get(name).ok_or_else(|| format!("the picture search model gave no {name}"))?;
        let (_, data) = v.try_extract_tensor::<f32>().map_err(|e| format!("read {name}: {e}"))?;
        Ok(data
            .chunks(DIM)
            .map(|row| {
                let n = row.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::MIN_POSITIVE);
                row.iter().map(|x| x / n).collect()
            })
            .collect())
    }

    impl Clip {
        pub fn new(vision: &std::path::Path, text: &std::path::Path, device: klipp_ml::DevicePreference) -> Result<Self, String> {
            let (vision, vision_model) = session(vision, device)?;
            let (text, text_model) = session(text, device)?;
            Ok(Self { vision: Mutex::new(vision), text: Mutex::new(text), vision_model, text_model })
        }

        pub fn images(&self, rgb: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String> {
            let n = (IMAGE_SIZE * IMAGE_SIZE) as usize;
            let mut chw = Vec::with_capacity(rgb.len() * 3 * n);
            for img in rgb {
                for (c, (m, s)) in MEAN.iter().zip(STD).enumerate() {
                    chw.extend((0..n).map(|p| (img[p * 3 + c] as f32 / 255.0 - m) / s));
                }
            }
            let s = IMAGE_SIZE as i64;
            let x = ort::value::Value::from_array(([rgb.len() as i64, 3, s, s], chw)).map_err(|e| format!("picture input: {e}"))?;
            let started = std::time::Instant::now();
            let mut vision = self.vision.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = vision.run(ort::inputs!["pixel_values" => x]).map_err(|e| format!("picture run: {e}"))?;
            let e = unit_rows(&outputs, "image_embeds")?;
            crate::provider::timed(&self.vision_model, rgb.len(), started.elapsed());
            Ok(e)
        }

        pub fn text(&self, ids: &[u32]) -> Result<Vec<f32>, String> {
            let ids: Vec<i64> = ids.iter().map(|&i| i as i64).collect();
            let x = ort::value::Value::from_array(([1i64, ids.len() as i64], ids)).map_err(|e| format!("words input: {e}"))?;
            let started = std::time::Instant::now();
            let mut text = self.text.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = text.run(ort::inputs!["input_ids" => x]).map_err(|e| format!("words run: {e}"))?;
            let e = unit_rows(&outputs, "text_embeds")?.remove(0);
            crate::provider::timed(&self.text_model, 1, started.elapsed());
            Ok(e)
        }
    }
}

/// The loaded model and its tokenizer.
pub struct Clip {
    tokenizer: Tokenizer,
    #[cfg(feature = "ort")]
    net: net::Clip,
}

impl Clip {
    /// The picture and words encoders at `vision` and `text` and the
    /// tokenizer at `tokenizer`, on `device`: the GPU where the runtime
    /// has a provider for it, else the CPU.
    pub fn load(vision: &std::path::Path, text: &std::path::Path, tokenizer: &std::path::Path, device: klipp_ml::DevicePreference) -> Result<Self, String> {
        let tokenizer = Tokenizer::from_json(&std::fs::read_to_string(tokenizer).map_err(|e| format!("{}: {e}", tokenizer.display()))?)?;
        #[cfg(feature = "ort")]
        {
            let net = net::Clip::new(vision, text, device).map_err(|e| format!("loading the picture search model: {e}"))?;
            Ok(Self { tokenizer, net })
        }
        #[cfg(not(feature = "ort"))]
        {
            let _ = (vision, text, tokenizer, device);
            Err("this build of Klipp has no ONNX Runtime, which picture search needs".into())
        }
    }

    /// Each picture (224×224 RGB, row by row) as a unit-length embedding.
    pub fn embed_images(&self, rgb: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String> {
        let n = (IMAGE_SIZE * IMAGE_SIZE * 3) as usize;
        if rgb.iter().any(|p| p.len() != n) {
            return Err(format!("pictures must be {IMAGE_SIZE}x{IMAGE_SIZE} RGB"));
        }
        if rgb.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(feature = "ort")]
        {
            self.net.images(rgb).map_err(|e| format!("embedding pictures: {e}"))
        }
        #[cfg(not(feature = "ort"))]
        Err("this build of Klipp has no ONNX Runtime, which picture search needs".into())
    }

    /// `text` as a unit-length embedding in the pictures' space.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>, String> {
        let ids = self.tokenizer.encode(text);
        #[cfg(feature = "ort")]
        {
            self.net.text(&ids).map_err(|e| format!("embedding words: {e}"))
        }
        #[cfg(not(feature = "ort"))]
        {
            let _ = ids;
            Err("this build of Klipp has no ONNX Runtime, which picture search needs".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_as_clip_splits_them() {
        assert_eq!(words("a dog's 2 balls, on a beach!!"), vec!["a", "dog", "'s", "2", "balls", ",", "on", "a", "beach", "!!"]);
        assert_eq!(words("close-up of hands"), vec!["close", "-", "up", "of", "hands"]);
    }

    #[test]
    fn bytes_map_to_printable_characters() {
        let b = byte_chars();
        assert_eq!(b[b'a' as usize], 'a');
        assert_eq!(b[b' ' as usize], '\u{120}');
        let unique: std::collections::HashSet<char> = b.iter().copied().collect();
        assert_eq!(unique.len(), 256);
    }

    /// A file of the model in the app's model cache, when it is there.
    fn cached(id: &str) -> Option<std::path::PathBuf> {
        Some(klipp_ml::ModelCache::open().ok()?.path_for_id(id)).filter(|p| p.is_file())
    }

    /// With CLIP's `tokenizer.json` in the model cache, words come out as
    /// the reference tokenizer gives them.
    #[test]
    fn words_encode_as_the_reference_tokenizer() {
        let Some(path) = cached(CLIP_TOKENIZER_ID) else {
            eprintln!("skipping: the CLIP tokenizer is not downloaded");
            return;
        };
        let t = Tokenizer::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let ids = t.encode("a photo of a dog");
        // From the reference tokenizer.
        assert_eq!(&ids[..7], &[49406, 320, 1125, 539, 320, 1929, 49407]);
        assert_eq!(ids.len(), 77);
        assert!(ids[7..].iter().all(|i| *i == 0));
    }

    /// The model from the cache on `device`, when it is downloaded.
    #[cfg(feature = "ort")]
    fn model(device: klipp_ml::DevicePreference) -> Option<Clip> {
        let (Some(vision), Some(text), Some(tok)) = (cached(CLIP_VISION_ID), cached(CLIP_TEXT_ID), cached(CLIP_TOKENIZER_ID)) else {
            eprintln!("skipping: the picture search model is not downloaded");
            return None;
        };
        if crate::runtime_resolve::ensure_runtime_loadable().is_err() {
            eprintln!("skipping: no ONNX Runtime");
            return None;
        }
        Some(Clip::load(&vision, &text, &tok, device).unwrap())
    }

    #[cfg(feature = "ort")]
    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// A red picture is nearer "red" than "blue", and a blue one the
    /// other way round.
    #[cfg(feature = "ort")]
    #[test]
    fn a_colour_is_nearest_its_name() {
        let Some(clip) = model(klipp_ml::DevicePreference::Cpu) else { return };
        let n = (IMAGE_SIZE * IMAGE_SIZE) as usize;
        let fill = |c: [u8; 3]| (0..n).flat_map(|_| c).collect::<Vec<u8>>();
        let pics = clip.embed_images(&[fill([220, 20, 20]), fill([20, 40, 220])]).unwrap();
        let (red, blue) = (clip.embed_text("a red picture").unwrap(), clip.embed_text("a blue picture").unwrap());
        assert!(dot(&pics[0], &red) > dot(&pics[0], &blue));
        assert!(dot(&pics[1], &blue) > dot(&pics[1], &red));
        assert!((dot(&red, &red) - 1.0).abs() < 1e-4);
    }

    /// Pictures with something in them, from ffmpeg's test sources.
    #[cfg(feature = "ort")]
    fn test_pictures() -> Option<Vec<Vec<u8>>> {
        let side = IMAGE_SIZE.to_string();
        let mut pictures = Vec::new();
        for source in ["testsrc2=", "smptehdbars=", "mandelbrot=", "life=mold=10:ratio=0.5:seed=7:"] {
            let out = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("{source}s={side}x{side}"), "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
                .output()
                .ok()?;
            if !out.status.success() || out.stdout.len() != (IMAGE_SIZE * IMAGE_SIZE * 3) as usize {
                return None;
            }
            pictures.push(out.stdout);
        }
        Some(pictures)
    }

    /// On the GPU the embeddings are the CPU's within a cosine of 0.9999,
    /// pictures and words, and the GPU's time a frame is reported (#2245).
    /// Runs where the runtime has a GPU provider; `KLIPP_TEST_REQUIRE_ORT_GPU=1`
    /// makes one required.
    #[cfg(feature = "ort")]
    #[test]
    fn the_gpus_embeddings_are_the_cpus() {
        let Some(gpu) = model(klipp_ml::DevicePreference::GpuPreferCpu) else { return };
        let on = crate::provider::used_by(CLIP_VISION_ID).map(|u| u.provider).unwrap_or("CPU");
        if on == "CPU" {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_ORT_GPU").as_deref(), Ok("1"), "a GPU provider was required");
            eprintln!("skipping: the runtime has no GPU provider");
            return;
        }
        let Some(pictures) = test_pictures() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        // Twice: the first run compiles the GPU's kernels, the time
        // reported is the second's.
        gpu.embed_images(&pictures).unwrap();
        let on_gpu = gpu.embed_images(&pictures).unwrap();
        let line = crate::provider::used_by(CLIP_VISION_ID).map(|u| u.line()).unwrap_or_default();
        assert!(line.contains("ms a frame"), "{line}");
        eprintln!("{line}");
        let cpu = model(klipp_ml::DevicePreference::Cpu).unwrap();
        let on_cpu = cpu.embed_images(&pictures).unwrap();
        for (i, (g, c)) in on_gpu.iter().zip(&on_cpu).enumerate() {
            assert!(dot(g, c) >= 0.9999, "picture {i}: cosine {}", dot(g, c));
        }
        for words in ["a dog on a beach", "close-up of hands", "a colour test card"] {
            let (g, c) = (gpu.embed_text(words).unwrap(), cpu.embed_text(words).unwrap());
            assert!(dot(&g, &c) >= 0.9999, "{words:?}: cosine {}", dot(&g, &c));
        }
    }
}
